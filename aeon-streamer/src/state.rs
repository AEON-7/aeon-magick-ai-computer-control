//! Shared, mutable runtime state. Held in an `Arc` so all tasks see the same
//! view.

use crate::capture::CaptureMode;
use crate::config::Config;
use http_body_util::Empty;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use parking_lot::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::{watch, Notify};

#[derive(Debug, Clone, Default)]
pub struct StreamerSnapshot {
    pub mode: Option<CaptureMode>,
    pub enum_hash: Option<String>,
    pub online: bool,
    pub captured_fps: u32,
    pub last_relaunch: Option<chrono::DateTime<chrono::Utc>>,
    pub relaunch_count: u64,
    /// Which pipeline is currently active: `Some("ustreamer")` for the fast
    /// path that proxies frames from ustreamer's unix socket, or
    /// `Some("ffmpeg")` when the NV12/YU12-fallback ffmpeg pipeline owns
    /// the device. The webapi proxy reads this to decide where to fetch
    /// /snapshot and /stream from.
    pub pipeline_kind: Option<&'static str>,
}

pub struct Shared {
    pub cfg: Config,
    pub snap: Mutex<StreamerSnapshot>,
    pub relaunch_signal: Notify,
    pub shutdown_signal: Notify,
    /// Long-lived hyper-util Client for proxying to ustreamer's unix
    /// socket. Has to outlive any streaming response body it produces —
    /// per-request Clients drop their pool when the function returns,
    /// killing in-flight multipart MJPEG bodies a few seconds later (the
    /// same bug the supervisor had in v14, fixed by moving its Client
    /// into AppState). This is the same pattern, applied to the
    /// streamer→ustreamer hop.
    pub uds_client: Client<hyperlocal::UnixConnector, Empty<bytes::Bytes>>,
    /// Latest complete JPEG frame captured from ffmpeg's stdout pipe.
    /// `None` until the first frame arrives. Single producer (the
    /// jpeg_pipe_reader task spawned by supervise.rs), unbounded
    /// consumers (every /snapshot and /stream request).
    ///
    /// tokio::sync::watch is the canonical "latest value, fan-out to
    /// many readers" channel — readers can either grab the current
    /// value immediately via `frame_rx.borrow()` (for /snapshot) or
    /// `.await` the next change via `frame_rx.changed()` (for /stream's
    /// multipart emission). No locking, no filesystem, no torn reads.
    pub frame_tx: watch::Sender<Option<bytes::Bytes>>,
    pub frame_rx: watch::Receiver<Option<bytes::Bytes>>,
    /// Monotonically increasing count of complete JPEG frames published
    /// on `frame_tx`. The watchdog samples this once per tick and
    /// computes the rate to expose as `captured_fps`. Wrapped in Arc
    /// so jpeg_pipe::run can hold its own reference into the same
    /// counter without the whole `Shared` having to be passed along.
    pub frames_published: Arc<AtomicU64>,
}

#[derive(Clone)]
pub struct SharedState(pub Arc<Shared>);

impl SharedState {
    pub fn new(cfg: Config) -> Self {
        let uds_client: Client<_, Empty<bytes::Bytes>> =
            Client::builder(TokioExecutor::new()).build(hyperlocal::UnixConnector);
        let (frame_tx, frame_rx) = watch::channel(None);
        Self(Arc::new(Shared {
            cfg,
            snap: Mutex::new(StreamerSnapshot::default()),
            relaunch_signal: Notify::new(),
            shutdown_signal: Notify::new(),
            uds_client,
            frame_tx,
            frame_rx,
            frames_published: Arc::new(AtomicU64::new(0)),
        }))
    }

    pub fn read(&self) -> StreamerSnapshot {
        self.0.snap.lock().clone()
    }

    pub fn mutate<F: FnOnce(&mut StreamerSnapshot)>(&self, f: F) {
        let mut s = self.0.snap.lock();
        f(&mut s);
    }

    pub fn signal_relaunch(&self, reason: &str) {
        tracing::info!(reason, "signaling ustreamer relaunch");
        self.0.relaunch_signal.notify_one();
    }

    pub async fn shutdown(&self) {
        self.0.shutdown_signal.notify_waiters();
    }
}

// Make `chrono` available without adding it to Cargo.toml: use std time + manual fmt.
// (Replaced via tiny shim below to keep the crate lean.)
pub mod chrono {
    use serde::Serialize;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Debug, Clone, Copy, Serialize)]
    pub struct DateTime<TZ = Utc> {
        pub epoch_ms: i64,
        _tz: std::marker::PhantomData<TZ>,
    }

    #[derive(Debug, Clone, Copy)]
    pub struct Utc;

    impl<TZ> DateTime<TZ> {
        pub fn now() -> Self {
            let ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            DateTime {
                epoch_ms: ms,
                _tz: std::marker::PhantomData,
            }
        }
    }
}
