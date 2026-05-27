//! aeon-supervisor
//!
//! HTTPS frontend for the Aeon Magick AI Computer Control. Serves:
//!   - The SvelteKit web UI (static, baked into the image at build time)
//!   - REST API that wraps the streamer + HID daemon unix sockets
//!   - WebSocket for live state pushes (streamer status, persona changes)
//!   - First-boot setup wizard when in WiFi AP fallback mode
//!
//! Auth: HTTP Basic against an argon2 hash stored in /etc/aeon/auth.toml.
//! Sessions: signed cookie issued on POST /api/login.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::info;

mod api;
mod auth;
mod macros;
mod mcp;
mod network;
mod proxy;
mod tls;
mod webui;

#[derive(Parser, Debug)]
struct Cli {
    #[arg(long, default_value = "/etc/aeon/supervisor.toml")]
    config: PathBuf,

    #[arg(long, default_value = "info")]
    log: String,

    /// First-boot helper: write a fresh auth.toml with a randomly generated
    /// admin password to PATH (mode 0600), print the plaintext password to
    /// stdout, and exit. Used by /usr/local/bin/aeon-firstboot.
    #[arg(long, value_name = "PATH")]
    generate_auth: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&cli.log))
        .with_target(false)
        .init();

    if let Some(path) = cli.generate_auth {
        let pw = auth::generate_default(&path)?;
        println!("{pw}");
        return Ok(());
    }

    let cfg = api::Config::load(cli.config)?;
    let tls_config = tls::ensure_cert(&cfg).await?;

    info!(listen = cfg.listen, "starting HTTPS");
    let app = api::build_router(cfg.clone());
    let addr: std::net::SocketAddr = cfg.listen.parse()?;
    axum_server::bind_rustls(addr, tls_config).serve(app.into_make_service()).await?;
    Ok(())
}
