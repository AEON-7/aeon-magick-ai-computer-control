//! aeon-streamer
//!
//! Wraps and supervises a `ustreamer` subprocess. The novelty over invoking
//! ustreamer directly is the *adaptive layer*:
//!   * sniff `/dev/video0`'s current v4l2 format/resolution and translate
//!     into compatible `--format` / `--resolution` args (Cam Link 4K's
//!     UVC enum varies with the source signal)
//!   * watch v4l2 enum hash for source-signal changes
//!   * watch ustreamer's own `source.online` for stuck states
//!   * pick the best available pixel format (YU12/YUV420 is offered at
//!     every Cam Link resolution including 4K — preferred over NV12 which
//!     ustreamer can't capture, and over YUYV which disappears at 4K)
//!   * use H.264 hardware encoding when running on Pi 4 (h264_v4l2m2m)
//!
//! Exposes its own HTTP API on a unix socket (`/run/aeon/streamer.sock`)
//! that `aeon-supervisor` proxies to the web UI.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::info;

mod capture;
mod config;
mod jpeg_pipe;
mod state;
mod supervise;
mod watchdog;
mod webapi;

#[derive(Parser, Debug)]
#[command(version, about = "Adaptive Cam Link / ustreamer supervisor")]
struct Cli {
    /// Path to TOML config (defaults to /etc/aeon/streamer.toml then $CWD/streamer.toml)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Override `device` from config.
    #[arg(long)]
    device: Option<PathBuf>,

    /// Override unix socket path for the HTTP API.
    #[arg(long)]
    api_sock: Option<PathBuf>,

    /// Log filter (env_logger / tracing_subscriber syntax).
    #[arg(long, default_value = "info")]
    log: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&cli.log))
        .with_target(false)
        .init();

    let cfg = config::load_with_overrides(cli.config, cli.device, cli.api_sock)?;
    info!(
        device = %cfg.device.display(),
        api_sock = %cfg.api_sock.display(),
        platform = ?cfg.platform,
        "starting"
    );

    // Shared, mutable state visible to the supervisor loop, watchdog, and web API.
    let state = state::SharedState::new(cfg.clone());

    // Start supervisor (manages ustreamer subprocess) + watchdog + web API in parallel.
    let sup = tokio::spawn(supervise::run(state.clone()));
    let wd = tokio::spawn(watchdog::run(state.clone()));
    let api = tokio::spawn(webapi::serve(state.clone()));

    tokio::select! {
        r = sup => { tracing::error!(?r, "supervisor exited"); }
        r = wd  => { tracing::error!(?r, "watchdog exited"); }
        r = api => { tracing::error!(?r, "webapi exited"); }
        _ = tokio::signal::ctrl_c() => { info!("SIGINT, shutting down"); }
    }

    state.shutdown().await;
    Ok(())
}
