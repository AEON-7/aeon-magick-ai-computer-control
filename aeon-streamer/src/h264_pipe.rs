//! Read an H.264 Annex-B byte stream from a child process's stdout pipe
//! (ffmpeg `-c:v h264_v4l2m2m -f h264 pipe:1`), split it into NAL units on
//! start codes, group NALs into access units (one decodable picture each),
//! and broadcast each access unit on a tokio broadcast channel.
//!
//! This is the H.264 sibling of [`crate::jpeg_pipe`]. The framing differs:
//! where MJPEG has clean SOI/EOI markers per frame, H.264 is a sequence of
//! NAL units delimited by 3- or 4-byte start codes (`00 00 01` /
//! `00 00 00 01`). An *access unit* (one picture) is, for the
//! single-slice-per-frame output that hardware encoders produce, an
//! optional run of parameter/SEI NALs followed by exactly one VCL slice
//! NAL. We detect an AU boundary as "a new NAL arriving after we've
//! already buffered a VCL slice", and we make every keyframe
//! self-contained by prepending the most recently seen SPS+PPS if the
//! encoder didn't already inline them.
//!
//! Why broadcast and not watch: a `watch` channel keeps only the latest
//! value, which is correct for independent JPEG frames but fatal for
//! H.264 — P-frames reference earlier frames, so a consumer must see
//! every AU, in order. `broadcast` fans every AU out to all subscribers;
//! one that falls behind the channel capacity gets a `Lagged` error and
//! resyncs at the next keyframe.
//!
//! NOTE (hardware-in-the-loop): the exact bytes h264_v4l2m2m emits — start
//! code lengths, whether SPS/PPS are inlined per IDR, whether an AUD is
//! present — want validation against the real Pi 4 encoder. The parser is
//! written to tolerate all of those (3- or 4-byte codes; SPS/PPS inlined
//! or not), but on-device confirmation is the source of truth.

use crate::state::H264Au;
use bytes::Bytes;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::process::ChildStdout;
use tokio::sync::broadcast;
use tracing::{debug, trace, warn};

/// Backstop: a single access unit (e.g. a 4K IDR) should never exceed this.
/// If the buffer blows past it we assume garbage and resync.
const MAX_BUFFER: usize = 8 * 1024 * 1024;

// NAL unit types (lower 5 bits of the first byte after the start code).
const NAL_NON_IDR: u8 = 1;
const NAL_IDR: u8 = 5;
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;

/// Read NAL units from `stdout` until EOF, assemble access units, and
/// broadcast each on `tx`. Bumps `units_published` per AU (the watchdog
/// samples it for captured_fps + liveness, same counter the JPEG path uses).
pub async fn run(
    mut stdout: ChildStdout,
    tx: broadcast::Sender<H264Au>,
    units_published: Arc<AtomicU64>,
) {
    let mut buf: Vec<u8> = Vec::with_capacity(512 * 1024);
    let mut read_buf = vec![0u8; 64 * 1024];

    // The access unit currently being assembled (Annex-B, with start codes).
    let mut au: Vec<u8> = Vec::with_capacity(256 * 1024);
    let mut au_has_vcl = false;
    let mut au_is_idr = false;
    let mut au_has_sps = false;
    let mut au_has_pps = false;

    // Most-recently-seen parameter sets (each WITH its start code), used to
    // make keyframes self-contained for clients that join mid-stream.
    let mut last_sps: Option<Vec<u8>> = None;
    let mut last_pps: Option<Vec<u8>> = None;

    let mut aus_emitted: u64 = 0;

    loop {
        let n = match stdout.read(&mut read_buf).await {
            Ok(0) => {
                debug!(aus = aus_emitted, "h264 stdout pipe closed");
                return;
            }
            Ok(n) => n,
            Err(e) => {
                warn!(?e, "h264 stdout read error");
                return;
            }
        };
        buf.extend_from_slice(&read_buf[..n]);

        // Drain every NAL we can fully delimit. A NAL is "complete" only
        // once the start code that FOLLOWS it is visible — so the trailing
        // partial NAL waits in `buf` for the next read (same need-more
        // discipline as jpeg_pipe's SOI/EOI).
        loop {
            let Some(first) = find_start_code(&buf, 0) else {
                break; // no start code yet — read more
            };
            // `first` points at the `00 00 01`; the NAL header byte follows.
            let nal_start = first + 3;
            let Some(next) = find_start_code(&buf, nal_start) else {
                // This NAL isn't terminated yet. Drop any leading garbage so
                // the buffer head sits on a start code, then wait for more.
                if first > 0 {
                    buf.drain(..first);
                }
                break;
            };

            let nal_type = buf.get(nal_start).map(|b| b & 0x1F).unwrap_or(0);

            // Is this a *continuation* slice of the picture we're already
            // assembling? libx264 with sliced-threads (the `-tune zerolatency`
            // low-latency path) splits ONE picture into multiple slices — first
            // slice has first_mb_in_slice == 0, the rest have it > 0. In the
            // H.264 slice header first_mb_in_slice is the leading ue(v), which is
            // 0 iff the first slice-header byte's MSB is set (ue 0 == a single
            // '1' bit). Treating each slice as its own access unit would (a) make
            // captured_fps count slices not pictures — a 30fps 4-slice stream
            // reads as 120fps — and (b) hand WebCodecs partial pictures. So we
            // group all slices of a picture into ONE AU.
            let is_vcl = nal_type == NAL_IDR || nal_type == NAL_NON_IDR;
            let is_continuation_slice = is_vcl
                && au_has_vcl
                && buf.get(nal_start + 1).map(|b| b & 0x80 == 0).unwrap_or(false);

            // AU boundary: a new NAL after we already hold a VCL slice means the
            // previous picture is complete — UNLESS this is another slice of the
            // SAME picture, which we append rather than flush.
            if au_has_vcl && !is_continuation_slice {
                flush_au(
                    &tx, &units_published, &au, au_is_idr, au_has_sps, au_has_pps,
                    &last_sps, &last_pps,
                );
                aus_emitted = aus_emitted.wrapping_add(1);
                if aus_emitted % 600 == 0 {
                    trace!(aus = aus_emitted, buf_len = buf.len(), "h264_pipe steady-state");
                }
                au.clear();
                au_has_vcl = false;
                au_is_idr = false;
                au_has_sps = false;
                au_has_pps = false;
            }

            // Cache parameter sets for keyframe priming; track AU contents.
            match nal_type {
                NAL_SPS => {
                    last_sps = Some(buf[first..next].to_vec());
                    au_has_sps = true;
                }
                NAL_PPS => {
                    last_pps = Some(buf[first..next].to_vec());
                    au_has_pps = true;
                }
                NAL_IDR => {
                    au_has_vcl = true;
                    au_is_idr = true;
                }
                NAL_NON_IDR => {
                    au_has_vcl = true;
                }
                _ => {} // SEI(6), AUD(9), etc. — carried along as leading NALs
            }

            au.extend_from_slice(&buf[first..next]);
            buf.drain(..next);
        }

        if buf.len() > MAX_BUFFER {
            warn!(len = buf.len(), "h264_pipe: buffer exceeded {MAX_BUFFER} bytes, resyncing");
            buf.clear();
            au.clear();
            au_has_vcl = false;
            au_is_idr = false;
            au_has_sps = false;
            au_has_pps = false;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn flush_au(
    tx: &broadcast::Sender<H264Au>,
    units_published: &Arc<AtomicU64>,
    au: &[u8],
    is_idr: bool,
    has_sps: bool,
    has_pps: bool,
    last_sps: &Option<Vec<u8>>,
    last_pps: &Option<Vec<u8>>,
) {
    if au.is_empty() {
        return;
    }
    // Keyframes must be self-contained: if this IDR AU didn't already carry
    // SPS/PPS inline, prepend the most recent ones so a client joining here
    // can configure its decoder from this access unit alone.
    let data: Bytes = if is_idr && !(has_sps && has_pps) {
        let mut v = Vec::with_capacity(au.len() + 64);
        if !has_sps {
            if let Some(s) = last_sps {
                v.extend_from_slice(s);
            }
        }
        if !has_pps {
            if let Some(p) = last_pps {
                v.extend_from_slice(p);
            }
        }
        v.extend_from_slice(au);
        Bytes::from(v)
    } else {
        Bytes::copy_from_slice(au)
    };

    // send() errors only when there are zero subscribers — that's the
    // normal idle case (nobody watching). Ignore it.
    let _ = tx.send(H264Au { data, key: is_idr });
    units_published.fetch_add(1, Ordering::Relaxed);
}

/// Find the next Annex-B start code (`00 00 01`, the common tail of both the
/// 3- and 4-byte forms) at or after `from`. Returns the index of its first
/// `0x00`. For a 4-byte code `00 00 00 01` this returns the index of the
/// second `0x00`; the leading `0x00` is harmless (a legal trailing zero byte
/// on the preceding NAL).
fn find_start_code(buf: &[u8], from: usize) -> Option<usize> {
    if buf.len() < 3 {
        return None;
    }
    let mut i = from;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_3byte_start_code() {
        let b = [0u8, 0, 1, 0x65, 9, 9];
        assert_eq!(find_start_code(&b, 0), Some(0));
    }

    #[test]
    fn finds_4byte_code_at_second_zero() {
        // 00 00 00 01 — returns index 1 (the 00 00 01 tail).
        let b = [0u8, 0, 0, 1, 0x67];
        assert_eq!(find_start_code(&b, 0), Some(1));
    }

    #[test]
    fn none_when_no_start_code() {
        let b = [1u8, 2, 3, 4];
        assert_eq!(find_start_code(&b, 0), None);
    }

    #[test]
    fn respects_from_offset() {
        let b = [0u8, 0, 1, 9, 0, 0, 1, 9];
        assert_eq!(find_start_code(&b, 3), Some(4));
    }
}
