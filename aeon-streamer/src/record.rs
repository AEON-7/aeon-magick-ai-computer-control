//! On-demand screen recording.
//!
//! Subscribes to the live H.264 access-unit broadcast (the same fan-out the
//! `/h264` WebSocket uses), starts at the next self-contained keyframe, and
//! muxes the Annex-B stream into an MP4 with `ffmpeg -f h264 -i pipe:0 -c
//! copy` — no re-encode, so it's nearly free on the Pi. A recording ends when
//! its duration elapses (default 30 s, or open-ended capped at 3 h), when the
//! caller stops it, or when the **disk budget** is hit (see below).
//!
//! **Disk budget.** Recordings may occupy at most 50% of the total disk.
//! Oldest recordings are purged first to stay under that, and if the *current*
//! recording alone would exceed the budget it is stopped with a message so the
//! disk can never fill with video. One recording at a time; files (and a
//! first-frame `.jpg` thumbnail) live in `/var/lib/aeon/recordings`. Recording
//! requires the `ffmpeg-h264` pipeline.

use crate::state::H264Au;
use parking_lot::Mutex;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::{broadcast, Notify};
use tracing::{info, warn};

const DEFAULT_DURATION_S: u64 = 30;
/// Hard ceiling so an open-ended ("record until stopped") capture can't run
/// forever — 3 hours.
const MAX_DURATION_S: u64 = 3 * 3600;
/// Recordings may occupy at most this fraction of the total disk.
const DISK_BUDGET_FRACTION: f64 = 0.5;
/// How often an active recording re-checks the disk budget.
const BUDGET_CHECK_S: u64 = 10;

const CAPACITY_MSG: &str =
    "video recording allocation max capacity, use a larger disk for more record time";

#[derive(Clone, Serialize)]
pub struct RecInfo {
    pub id: String,
    pub started_ms: i64,
    pub duration_s: u64,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Whether a first-frame thumbnail exists (GET /recordings/<id>/thumb).
    pub has_thumb: bool,
}

struct Active {
    id: String,
    started_ms: i64,
    started: Instant,
    duration_s: u64,
    stop: Arc<Notify>,
}

pub struct RecordManager {
    dir: PathBuf,
    active: Mutex<Option<Active>>,
    /// Set when a recording was stopped by the disk-capacity guard, so the UI
    /// can surface the message. Cleared on the next start.
    last_note: Mutex<Option<String>>,
}

impl RecordManager {
    pub fn new(dir: PathBuf) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&dir);
        Arc::new(Self {
            dir,
            active: Mutex::new(None),
            last_note: Mutex::new(None),
        })
    }

    pub fn is_recording(&self) -> bool {
        self.active.lock().is_some()
    }

    /// Begin a recording. `duration_s`: `None` → 30 s; `Some(0)` → open-ended
    /// (record until stopped, capped at 3 h); `Some(n)` → `min(n, 3h)`.
    pub fn start(
        self: &Arc<Self>,
        h264_tx: &broadcast::Sender<H264Au>,
        ffmpeg_bin: PathBuf,
        fps: u32,
        audio_device: String,
        audio_rate: u32,
        audio_channels: u32,
        duration_s: Option<u64>,
    ) -> Result<RecInfo, String> {
        let mut guard = self.active.lock();
        if guard.is_some() {
            return Err("a recording is already in progress".into());
        }
        let cap = match duration_s {
            None => DEFAULT_DURATION_S,
            Some(0) => MAX_DURATION_S,
            Some(n) => n.min(MAX_DURATION_S),
        };
        *self.last_note.lock() = None;
        // Make room: drop oldest recordings if we're already at/over budget.
        self.enforce_budget(None);
        let budget = self.disk_budget();
        if budget > 0 && self.dir_total_bytes() >= budget {
            return Err(CAPACITY_MSG.into());
        }

        let now = now_ms();
        let id = format!("rec_{now}");
        let out = self.dir.join(format!("{id}.mp4"));
        let stop = Arc::new(Notify::new());
        let rx = h264_tx.subscribe();
        let fps = fps.clamp(1, 60);

        let me_loop = Arc::clone(self);
        let me_done = Arc::clone(self);
        let id_task = id.clone();
        let out_task = out.clone();
        let stop_task = stop.clone();
        tokio::spawn(async move {
            match record_loop(&me_loop, &id_task, rx, &out_task, &ffmpeg_bin, fps,
                               audio_device, audio_rate, audio_channels, cap, stop_task)
                .await
            {
                Ok(bytes) => {
                    info!(id = %id_task, bytes, "recording complete");
                    me_loop.make_thumb(&id_task, &ffmpeg_bin);
                }
                Err(e) => {
                    warn!(id = %id_task, err = %e, "recording failed");
                    let _ = std::fs::remove_file(&out_task);
                }
            }
            me_done.active.lock().take();
        });

        *guard = Some(Active {
            id: id.clone(),
            started_ms: now,
            started: Instant::now(),
            duration_s: cap,
            stop,
        });
        Ok(RecInfo {
            id,
            started_ms: now,
            duration_s: cap,
            status: "recording".into(),
            elapsed_s: Some(0),
            size_bytes: None,
            has_thumb: false,
        })
    }

    pub fn stop(&self) -> Result<RecInfo, String> {
        let guard = self.active.lock();
        match guard.as_ref() {
            Some(a) => {
                a.stop.notify_waiters();
                Ok(a.info())
            }
            None => Err("no recording in progress".into()),
        }
    }

    pub fn active_info(&self) -> Option<RecInfo> {
        self.active.lock().as_ref().map(|a| a.info())
    }

    /// A one-shot note for the UI (e.g. the disk-capacity stop message).
    pub fn note(&self) -> Option<String> {
        self.last_note.lock().clone()
    }

    /// Finished recordings on disk, newest first.
    pub fn list(&self) -> Vec<RecInfo> {
        let mut out = Vec::new();
        let active_id = self.active.lock().as_ref().map(|a| a.id.clone());
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for ent in rd.flatten() {
                let path = ent.path();
                if path.extension().and_then(|e| e.to_str()) != Some("mp4") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if active_id.as_deref() == Some(id) {
                    continue;
                }
                let size = ent.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(RecInfo {
                    id: id.to_string(),
                    started_ms: id_to_ms(id),
                    duration_s: 0,
                    status: "complete".into(),
                    elapsed_s: None,
                    size_bytes: Some(size),
                    has_thumb: self.dir.join(format!("{id}.jpg")).is_file(),
                });
            }
        }
        out.sort_by(|a, b| b.started_ms.cmp(&a.started_ms));
        out
    }

    pub fn path(&self, id: &str) -> Option<PathBuf> {
        if !is_safe_id(id) {
            return None;
        }
        let p = self.dir.join(format!("{id}.mp4"));
        p.is_file().then_some(p)
    }

    pub fn thumb_path(&self, id: &str) -> Option<PathBuf> {
        if !is_safe_id(id) {
            return None;
        }
        let p = self.dir.join(format!("{id}.jpg"));
        p.is_file().then_some(p)
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        if !is_safe_id(id) {
            return Err("bad id".into());
        }
        if self.active.lock().as_ref().map(|a| a.id.as_str()) == Some(id) {
            return Err("can't delete a recording that's still in progress".into());
        }
        let _ = std::fs::remove_file(self.dir.join(format!("{id}.jpg")));
        std::fs::remove_file(self.dir.join(format!("{id}.mp4")))
            .map_err(|e| format!("delete: {e}"))
    }

    /// 50% of the recordings filesystem's total size, in bytes (0 if unknown).
    fn disk_budget(&self) -> u64 {
        (disk_total_bytes(&self.dir) as f64 * DISK_BUDGET_FRACTION) as u64
    }

    /// Total bytes of everything in the recordings dir (mp4s + thumbnails).
    fn dir_total_bytes(&self) -> u64 {
        let mut total = 0u64;
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for ent in rd.flatten() {
                total += ent.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        total
    }

    /// Drop oldest recordings (mp4 + thumb) until the dir total ≤ budget.
    /// `keep` is the in-progress recording id, never purged. Returns the
    /// resulting total bytes.
    fn enforce_budget(&self, keep: Option<&str>) -> u64 {
        let budget = self.disk_budget();
        if budget == 0 {
            return self.dir_total_bytes();
        }
        let mut recs: Vec<(String, i64, u64)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for ent in rd.flatten() {
                let p = ent.path();
                if p.extension().and_then(|e| e.to_str()) != Some("mp4") {
                    continue;
                }
                let Some(id) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                    continue;
                };
                if keep == Some(id.as_str()) {
                    continue;
                }
                let mp4 = ent.metadata().map(|m| m.len()).unwrap_or(0);
                let thumb = std::fs::metadata(self.dir.join(format!("{id}.jpg")))
                    .map(|m| m.len())
                    .unwrap_or(0);
                recs.push((id, id_to_ms(p.file_stem().and_then(|s| s.to_str()).unwrap_or("")), mp4 + thumb));
            }
        }
        recs.sort_by(|a, b| a.1.cmp(&b.1)); // oldest first
        let mut total = self.dir_total_bytes();
        for (id, _, sz) in recs {
            if total <= budget {
                break;
            }
            let _ = std::fs::remove_file(self.dir.join(format!("{id}.mp4")));
            let _ = std::fs::remove_file(self.dir.join(format!("{id}.jpg")));
            total = total.saturating_sub(sz);
            info!(id = %id, "purged oldest recording (disk budget)");
        }
        total
    }

    /// Extract the first frame of a finished recording as a small JPEG thumb.
    fn make_thumb(&self, id: &str, ffmpeg_bin: &Path) {
        let mp4 = self.dir.join(format!("{id}.mp4"));
        let jpg = self.dir.join(format!("{id}.jpg"));
        let _ = std::process::Command::new(ffmpeg_bin)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&mp4)
            .args(["-frames:v", "1", "-vf", "scale=320:-1"])
            .arg(&jpg)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Active {
    fn info(&self) -> RecInfo {
        RecInfo {
            id: self.id.clone(),
            started_ms: self.started_ms,
            duration_s: self.duration_s,
            status: "recording".into(),
            elapsed_s: Some(self.started.elapsed().as_secs()),
            size_bytes: None,
            has_thumb: false,
        }
    }
}

/// The capture loop: spawn ffmpeg, wait for a keyframe, pipe AUs until the
/// deadline / manual stop / stream end / disk-budget guard. Returns final size.
#[allow(clippy::too_many_arguments)]
async fn record_loop(
    me: &Arc<RecordManager>,
    id: &str,
    mut rx: broadcast::Receiver<H264Au>,
    out: &Path,
    ffmpeg_bin: &Path,
    fps: u32,
    audio_device: String,
    audio_rate: u32,
    audio_channels: u32,
    cap_s: u64,
    stop: Arc<Notify>,
) -> Result<u64, String> {
    // Video always arrives as H.264 Annex-B on stdin (the broadcast pipe).
    // With an `audio_device` set we add an ALSA input and mux an AAC track into
    // the MP4 — video stays `-c copy` (already-encoded NALs) so the live
    // broadcast pipe is untouched. Empty `audio_device` reproduces the exact
    // historical video-only command, byte-for-byte.
    let audio = !audio_device.trim().is_empty();
    let mut args: Vec<String> =
        vec!["-hide_banner".into(), "-loglevel".into(), "error".into()];
    if audio {
        // Audio = input 0. thread_queue_size absorbs ALSA buffering while
        // ffmpeg waits for the first video keyframe.
        args.extend([
            "-thread_queue_size".into(), "1024".into(),
            "-f".into(), "alsa".into(),
            "-ar".into(), audio_rate.to_string(),
            "-ac".into(), audio_channels.to_string(),
            "-i".into(), audio_device.clone(),
        ]);
    }
    // Video = input 1 (or 0 when no audio).
    args.extend([
        "-fflags".into(), "+genpts".into(),
        "-f".into(), "h264".into(),
        "-framerate".into(), fps.to_string(),
        "-i".into(), "pipe:0".into(),
    ]);
    if audio {
        args.extend([
            "-map".into(), "1:v:0".into(), "-map".into(), "0:a:0".into(),
            "-c:v".into(), "copy".into(),
            "-c:a".into(), "aac".into(), "-b:a".into(), "128k".into(),
            "-async".into(), "1".into(), "-shortest".into(),
        ]);
    } else {
        args.extend(["-c".into(), "copy".into()]);
    }
    args.extend(["-movflags".into(), "+faststart".into(), "-y".into()]);

    let mut child = tokio::process::Command::new(ffmpeg_bin)
        .args(&args)
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn ffmpeg: {e}"))?;

    let mut stdin = child.stdin.take().ok_or("no ffmpeg stdin")?;
    let deadline = tokio::time::sleep(Duration::from_secs(cap_s));
    tokio::pin!(deadline);
    let mut budget_tick = tokio::time::interval(Duration::from_secs(BUDGET_CHECK_S));
    budget_tick.tick().await; // consume the immediate first tick
    let mut armed = false; // start writing only once we've seen a keyframe

    loop {
        tokio::select! {
            _ = &mut deadline => break,
            _ = stop.notified() => break,
            _ = budget_tick.tick() => {
                // Rotating purge keeps the total under budget; if the CURRENT
                // recording alone still exceeds it, stop with the message.
                let budget = me.disk_budget();
                let total = me.enforce_budget(Some(id));
                if budget > 0 && total > budget {
                    *me.last_note.lock() = Some(CAPACITY_MSG.to_string());
                    warn!(id, "stopping recording — disk budget reached");
                    break;
                }
            }
            au = rx.recv() => match au {
                Ok(au) => {
                    if !armed {
                        if au.key { armed = true; } else { continue; }
                    }
                    if stdin.write_all(&au.data).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    drop(stdin); // EOF → ffmpeg writes the moov atom and exits
    let _ = child.wait().await;
    let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        return Err(format!(
            "recording produced no data — is {} writable by the streamer (aeon)?",
            out.display()
        ));
    }
    Ok(size)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn id_to_ms(id: &str) -> i64 {
    id.strip_prefix("rec_")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

fn is_safe_id(id: &str) -> bool {
    id.strip_prefix("rec_")
        .map(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or(false)
}

/// Total bytes of the filesystem holding `dir`, via `df` (no extra crates).
fn disk_total_bytes(dir: &Path) -> u64 {
    std::process::Command::new("df")
        .args(["-B1", "--output=size"])
        .arg(dir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.lines().nth(1).and_then(|l| l.trim().parse::<u64>().ok()))
        .unwrap_or(0)
}
