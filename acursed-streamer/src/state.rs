//! Shared, mutable runtime state. Held in an `Arc` so all tasks see the same
//! view.

use crate::capture::CaptureMode;
use crate::config::Config;
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Debug, Clone, Default)]
pub struct StreamerSnapshot {
    pub mode: Option<CaptureMode>,
    pub enum_hash: Option<String>,
    pub online: bool,
    pub captured_fps: u32,
    pub last_relaunch: Option<chrono::DateTime<chrono::Utc>>,
    pub relaunch_count: u64,
}

pub struct Shared {
    pub cfg: Config,
    pub snap: Mutex<StreamerSnapshot>,
    pub relaunch_signal: Notify,
    pub shutdown_signal: Notify,
}

#[derive(Clone)]
pub struct SharedState(pub Arc<Shared>);

impl SharedState {
    pub fn new(cfg: Config) -> Self {
        Self(Arc::new(Shared {
            cfg,
            snap: Mutex::new(StreamerSnapshot::default()),
            relaunch_signal: Notify::new(),
            shutdown_signal: Notify::new(),
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
