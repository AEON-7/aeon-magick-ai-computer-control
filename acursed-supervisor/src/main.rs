//! acursed-supervisor
//!
//! HTTPS frontend for the Aeon Cursed KVM. Serves:
//!   - The SvelteKit web UI (static, baked into the image at build time)
//!   - REST API that wraps the streamer + HID daemon unix sockets
//!   - WebSocket for live state pushes (streamer status, persona changes)
//!   - First-boot setup wizard when in WiFi AP fallback mode
//!
//! Auth: HTTP Basic against an argon2 hash stored in /etc/acursed/auth.toml.
//! Sessions: signed cookie issued on POST /api/login.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::info;

mod api;
mod auth;
mod proxy;
mod tls;
mod webui;

#[derive(Parser, Debug)]
struct Cli {
    #[arg(long, default_value = "/etc/acursed/supervisor.toml")]
    config: PathBuf,

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

    let cfg = api::Config::load(cli.config)?;
    let tls_config = tls::ensure_cert(&cfg)?;

    info!(listen = cfg.listen, "starting HTTPS");
    let app = api::build_router(cfg.clone());
    let addr: std::net::SocketAddr = cfg.listen.parse()?;
    axum_server::bind_rustls(addr, tls_config).serve(app.into_make_service()).await?;
    Ok(())
}
