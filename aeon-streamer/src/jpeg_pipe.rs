//! Read JPEG frames from a child process's stdout pipe (e.g. ffmpeg's
//! `-f image2pipe -c:v mjpeg pipe:1`), parse out complete frames via
//! JPEG SOI/EOI markers, and publish each complete frame on a
//! tokio::sync::watch channel.
//!
//! Replaces the v1-v23 architecture of writing JPEGs to a tmpfs file
//! and polling its mtime — that approach had a race window where ffmpeg
//! had truncated the file but not yet rewritten the bytes, leading to
//! mid-write reads being forwarded to browsers and Chrome's image
//! decoder tearing down the multipart stream after a few corrupt
//! frames. Pipe-based streaming closes that window entirely: a Unix
//! pipe is a byte stream; we only emit a frame after seeing the
//! complete SOI-to-EOI byte sequence.

use bytes::Bytes;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::process::ChildStdout;
use tokio::sync::watch;
use tracing::{debug, trace, warn};

/// SOI marker — every JPEG starts with these two bytes.
const SOI: [u8; 2] = [0xFF, 0xD8];
/// EOI marker — every JPEG ends with these two bytes.
const EOI: [u8; 2] = [0xFF, 0xD9];

/// Maximum buffered bytes we'll accept before assuming the stream is
/// garbage and resyncing. 4 MB covers any reasonable 4K JPEG at high
/// quality with margin.
const MAX_BUFFER: usize = 4 * 1024 * 1024;

/// Read JPEG frames from `stdout` until EOF (or error), publishing each
/// complete frame on `tx`. Runs until the pipe closes.
///
/// `frames_published` is a counter the watchdog samples to compute
/// captured_fps. Atomic so we don't need a lock on the hot path.
pub async fn run(
    mut stdout: ChildStdout,
    tx: watch::Sender<Option<Bytes>>,
    frames_published: Arc<AtomicU64>,
) {
    let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
    let mut read_buf = vec![0u8; 64 * 1024];
    let mut frames_emitted: u64 = 0;

    loop {
        // Read more bytes from ffmpeg
        let n = match stdout.read(&mut read_buf).await {
            Ok(0) => {
                debug!(frames = frames_emitted, "ffmpeg stdout pipe closed");
                return;
            }
            Ok(n) => n,
            Err(e) => {
                warn!(?e, "ffmpeg stdout read error");
                return;
            }
        };
        buf.extend_from_slice(&read_buf[..n]);

        // Drain as many complete JPEG frames from the buffer as we can.
        loop {
            match extract_frame(&buf) {
                ExtractResult::Frame { start, end } => {
                    let frame = Bytes::copy_from_slice(&buf[start..end]);
                    // send() only fails if all receivers dropped. We
                    // always hold a permanent receiver via state.frame_rx
                    // (it's part of Shared), so this can't actually fail
                    // in normal operation. Ignore the result.
                    let _ = tx.send(Some(frame));
                    frames_emitted = frames_emitted.wrapping_add(1);
                    // Bump the shared counter — watchdog samples this
                    // to compute captured_fps.
                    frames_published.fetch_add(1, Ordering::Relaxed);
                    if frames_emitted % 600 == 0 {
                        // Roughly every 20s at 30fps — keep journal noise low.
                        trace!(frames = frames_emitted, buf_len = buf.len(),
                            "jpeg_pipe steady-state");
                    }
                    buf.drain(..end);
                }
                ExtractResult::NeedMore => break,
                ExtractResult::Resync(skip) => {
                    // Lost sync — drop everything up to the next plausible
                    // SOI and try again. Rare in practice; would happen if
                    // ffmpeg ever emitted garbage on stdout.
                    if skip > 0 {
                        warn!(skip, "jpeg_pipe resyncing past garbage");
                        buf.drain(..skip);
                    } else {
                        // No SOI found anywhere — buffer is unrecoverable.
                        // Clear it and hope the next read aligns.
                        warn!(len = buf.len(), "jpeg_pipe: no SOI in buffer, clearing");
                        buf.clear();
                        break;
                    }
                }
            }
        }

        // Backstop: if the buffer keeps growing (frames bigger than
        // expected, or we never find an EOI), force a clear to prevent
        // OOM. 4 MB is enough for any reasonable JPEG; if we exceed
        // that, something is wrong with ffmpeg's output.
        if buf.len() > MAX_BUFFER {
            warn!(len = buf.len(), "jpeg_pipe: buffer exceeded {MAX_BUFFER} bytes, dropping");
            buf.clear();
        }
    }
}

enum ExtractResult {
    /// Found a complete frame at `buf[start..end]`.
    Frame { start: usize, end: usize },
    /// Need more bytes from the pipe before we can decide.
    NeedMore,
    /// Lost sync. Caller should drain `skip` bytes (which moves the
    /// buffer head to a plausible SOI or empties it entirely if no SOI
    /// is present).
    Resync(usize),
}

fn extract_frame(buf: &[u8]) -> ExtractResult {
    if buf.len() < 4 {
        return ExtractResult::NeedMore;
    }
    // Must start with SOI. If not, the producer got out of sync —
    // find the next SOI in the buffer.
    if buf[0..2] != SOI {
        if let Some(pos) = find_marker(buf, SOI, 0) {
            return ExtractResult::Resync(pos);
        }
        // No SOI anywhere — fully resync (clear)
        return ExtractResult::Resync(0);
    }
    // We have a SOI at offset 0. Find the next EOI starting after the
    // SOI (skip the SOI bytes themselves so we don't match SOI-as-EOI
    // in a malformed stream — though that can't happen since SOI != EOI).
    match find_marker(buf, EOI, 2) {
        Some(eoi_pos) => {
            // Frame spans [0, eoi_pos+2) — include the EOI bytes.
            ExtractResult::Frame { start: 0, end: eoi_pos + 2 }
        }
        None => ExtractResult::NeedMore,
    }
}

/// Find the first occurrence of `marker` (a 2-byte sequence) at or
/// after position `from` in `buf`.
fn find_marker(buf: &[u8], marker: [u8; 2], from: usize) -> Option<usize> {
    if from >= buf.len().saturating_sub(1) {
        return None;
    }
    buf[from..]
        .windows(2)
        .position(|w| w == marker)
        .map(|p| p + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_single_frame() {
        // SOI + body + EOI
        let buf = b"\xFF\xD8some payload here\xFF\xD9";
        match extract_frame(buf) {
            ExtractResult::Frame { start, end } => {
                assert_eq!(start, 0);
                assert_eq!(end, buf.len());
            }
            _ => panic!("expected Frame"),
        }
    }

    #[test]
    fn extract_needs_more_when_no_eoi() {
        let buf = b"\xFF\xD8some payload";
        match extract_frame(buf) {
            ExtractResult::NeedMore => {}
            _ => panic!("expected NeedMore"),
        }
    }

    #[test]
    fn extract_resyncs_on_no_soi() {
        let buf = b"\x00\x01\xFF\xD8payload\xFF\xD9";
        match extract_frame(buf) {
            ExtractResult::Resync(skip) => assert_eq!(skip, 2),
            _ => panic!("expected Resync(2)"),
        }
    }

    #[test]
    fn extract_resyncs_clears_when_no_soi_at_all() {
        let buf = b"\x00\x01\x02\x03";
        match extract_frame(buf) {
            ExtractResult::Resync(skip) => assert_eq!(skip, 0),
            _ => panic!("expected Resync(0) — full clear"),
        }
    }

    #[test]
    fn extract_short_buffer() {
        match extract_frame(b"\xFF\xD8") {
            ExtractResult::NeedMore => {}
            _ => panic!("expected NeedMore on too-short buffer"),
        }
    }
}
