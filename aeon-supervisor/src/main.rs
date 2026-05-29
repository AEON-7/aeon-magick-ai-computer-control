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
mod audit;
mod auth;
mod blocked_log;
mod captive;
mod dns_log;
mod dnscrypt_relays;
mod dnscrypt_servers;
mod firewall;
mod i2p;
mod macros;
mod mcp;
mod network;
mod proxy;
mod security_metrics;
mod clipboard;
mod file_xfer;
mod ssh_keys;
mod storage;
mod system;
mod target;
mod tls;
mod webui;
mod wifi;

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

    // Spawn the captive-portal listener on port 80. It serves OS
    // captive-probe URLs (Apple/Android/Microsoft/etc.) with responses
    // that trigger the device's "this network needs sign-in" sheet,
    // and redirects everything else to https://gateway/setup/wifi.
    // Only relevant when aeon-netwatch has installed the iptables
    // PREROUTING redirect from wlan0 80/443 → gateway, but binding
    // unconditionally is cheap and means we don't need a separate
    // service lifecycle.
    tokio::spawn(captive::serve());
    // Background refresh loop for DNS blacklist subscription sources.
    // Runs forever; checks every 5 min for stale lists and re-fetches.
    tokio::spawn(dns_log::run_refresh_loop());
    // Ensure the AEON_DROP iptables chain exists at startup so every
    // DROP rule we apply (user or system) gets logged on the way down.
    // This is what populates the "Blocked traffic" panel.
    firewall::ensure_drop_chain();

    // If the operator has enabled the target-facing HTTP file server,
    // spawn it now. Listener binds 0.0.0.0:<configured-port>; the
    // iptables INPUT rules from aeon-usb-net (in isolation/restricted
    // modes) gate which clients can actually reach it. Off by default;
    // toggle via PUT /api/files/config.
    file_xfer::maybe_spawn_target_server().await;

    axum_server::bind_rustls(addr, tls_config).serve(app.into_make_service()).await?;
    Ok(())
}
