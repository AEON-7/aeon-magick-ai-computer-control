//! On-demand screen recording.
//!
//! Subscribes to the live H.264 access-unit broadcast (the same fan-out the
//! `/h264` WebSocket uses), starts at the next self-contained keyframe, and
//! muxes the Annex-B stream into an MP4 with `ffmpeg -f h264 -i pipe:0 -c
//! copy` — no re-encode, so it's nearly free on the Pi. A recording ends when
//! its duration elapses (default 30 s, caller-customizable, 1 h hard cap) or
//! when the caller stops it manually, whichever comes first.
//!
//! One recording at a time. Files land in `/var/lib/aeon/recordings/<id>.mp4`;
//! the newest `KEEP_RECORDINGS` are retained and older ones pruned on start so
//! the SD card can't fill silently. Recording requires the `ffmpeg-h264`
//! pipeline (the H.264 broadcast); in MJPEG modes `start` returns an error.

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
/// Hard ceiling so an open-ended ("manual stop only") recording can't run
/// away and fill the card. Applies to every recording.
const MAX_DURATION_S: u64 = 3600;
/// Keep at most this many finished recordings; prune oldest on start.
const KEEP_RECORDINGS: usize = 20;

/// Public metadata for one recording (active or finished).
#[derive(Clone, Serialize)]
pub struct RecInfo {
    pub id: String,
    pub started_ms: i64,
    /// Requested cap in seconds (the effective auto-stop deadline).
    pub duration_s: u64,
    /// "recording" | "complete"
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
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
}

impl RecordManager {
    pub fn new(dir: PathBuf) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&dir);
        Arc::new(Self {
            dir,
            active: Mutex::new(None),
        })
    }

    pub fn is_recording(&self) -> bool {
        self.active.lock().is_some()
    }

    /// Begin a recording. `duration_s`: `None` → 30 s default; `Some(0)` →
    /// open-ended (manual stop, capped at 1 h); `Some(n)` → `min(n, 1 h)`.
    /// Errors if one is already in progress.
    pub fn start(
        self: &Arc<Self>,
        h264_tx: &broadcast::Sender<H264Au>,
        ffmpeg_bin: PathBuf,
        fps: u32,
        duration_s: Option<u64>,
    ) -> Result<RecInfo, String> {
        let mut guard = self.active.lock();
        if guard.is_some() {
            return Err("a recording is already in progress".into());
        }
        // Effective auto-stop deadline. 0/None map to default/open-ended,
        // everything clamped to the 1 h ceiling.
        let cap = match duration_s {
            None => DEFAULT_DURATION_S,
            Some(0) => MAX_DURATION_S,
            Some(n) => n.min(MAX_DURATION_S),
        };
        self.prune_old();

        let now = now_ms();
        let id = format!("rec_{now}");
        let out = self.dir.join(format!("{id}.mp4"));
        let stop = Arc::new(Notify::new());
        let rx = h264_tx.subscribe();
        let fps = fps.clamp(1, 60);

        let me = Arc::clone(self);
        let id_for_task = id.clone();
        let stop_for_task = stop.clone();
        tokio::spawn(async move {
            match record_loop(rx, &out, &ffmpeg_bin, fps, cap, stop_for_task).await {
                Ok(bytes) => info!(id = %id_for_task, bytes, "recording complete"),
                Err(e) => {
                    warn!(id = %id_for_task, err = %e, "recording failed");
                    let _ = std::fs::remove_file(&out); // don't leave a broken stub
                }
            }
            me.active.lock().take(); // back to idle
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
        })
    }

    /// Stop the in-progress recording (finalizes the MP4). Errors if idle.
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

    /// The active recording's live info (with elapsed), if any.
    pub fn active_info(&self) -> Option<RecInfo> {
        self.active.lock().as_ref().map(|a| a.info())
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
                    continue; // listed separately as the active recording
                }
                let size = ent.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(RecInfo {
                    id: id.to_string(),
                    started_ms: id_to_ms(id),
                    duration_s: 0,
                    status: "complete".into(),
                    elapsed_s: None,
                    size_bytes: Some(size),
                });
            }
        }
        out.sort_by(|a, b| b.started_ms.cmp(&a.started_ms));
        out
    }

    /// Absolute path of a finished recording's MP4, if it exists and the id is
    /// well-formed (no path traversal).
    pub fn path(&self, id: &str) -> Option<PathBuf> {
        if !is_safe_id(id) {
            return None;
        }
        let p = self.dir.join(format!("{id}.mp4"));
        p.is_file().then_some(p)
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        if !is_safe_id(id) {
            return Err("bad id".into());
        }
        if self.active.lock().as_ref().map(|a| a.id.as_str()) == Some(id) {
            return Err("can't delete a recording that's still in progress".into());
        }
        std::fs::remove_file(self.dir.join(format!("{id}.mp4")))
            .map_err(|e| format!("delete: {e}"))
    }

    /// Drop the oldest finished recordings beyond `KEEP_RECORDINGS`.
    fn prune_old(&self) {
        let mut files: Vec<(PathBuf, i64)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for ent in rd.flatten() {
                let path = ent.path();
                if path.extension().and_then(|e| e.to_str()) == Some("mp4") {
                    let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    files.push((path.clone(), id_to_ms(id)));
                }
            }
        }
        if files.len() < KEEP_RECORDINGS {
            return;
        }
        files.sort_by(|a, b| b.1.cmp(&a.1)); // newest first
        for (path, _) in files.into_iter().skip(KEEP_RECORDINGS - 1) {
            let _ = std::fs::remove_file(path);
        }
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
        }
    }
}

/// The core capture loop: spawn ffmpeg, wait for the first keyframe, then pipe
/// every AU to its stdin until the deadline, a manual stop, or stream end.
/// Returns the final file size on success.
async fn record_loop(
    mut rx: broadcast::Receiver<H264Au>,
    out: &Path,
    ffmpeg_bin: &Path,
    fps: u32,
    cap_s: u64,
    stop: Arc<Notify>,
) -> Result<u64, String> {
    let mut child = tokio::process::Command::new(ffmpeg_bin)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-fflags",
            "+genpts",
            "-f",
            "h264",
            "-framerate",
            &fps.to_string(),
            "-i",
            "pipe:0",
            "-c",
            "copy",
            "-movflags",
            "+faststart",
            "-y",
        ])
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn ffmpeg: {e}"))?;

    let mut stdin = child.stdin.take().ok_or("no ffmpeg stdin")?;
    let deadline = tokio::time::sleep(Duration::from_secs(cap_s));
    tokio::pin!(deadline);
    let mut armed = false; // start writing only once we've seen a keyframe

    loop {
        tokio::select! {
            _ = &mut deadline => break,
            _ = stop.notified() => break,
            au = rx.recv() => match au {
                Ok(au) => {
                    if !armed {
                        if au.key { armed = true; } else { continue; }
                    }
                    if stdin.write_all(&au.data).await.is_err() {
                        break; // ffmpeg went away
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

/// Parse the epoch-ms back out of a `rec_<ms>` id (0 if it doesn't match).
fn id_to_ms(id: &str) -> i64 {
    id.strip_prefix("rec_")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

/// Ids are `rec_<digits>` — reject anything else to prevent path traversal.
fn is_safe_id(id: &str) -> bool {
    id.strip_prefix("rec_")
        .map(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or(false)
}
