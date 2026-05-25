//! acursed-hid
//!
//! Manages a USB composite HID gadget on the Pi's USB-C OTG port via Linux
//! ConfigFS, exposes a small HTTP+WS API that takes ONLY logical input
//! operations (type a string, send a chord, click, move) — never raw
//! press/release primitives that can be left dangling.
//!
//! Personas swap which functions are loaded into the composite and which
//! HID report descriptors they advertise. Switching persona re-enumerates
//! the USB gadget on the target.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::info;

mod api;
mod config;
mod gadget;
mod input;
mod persona;
mod state;

#[derive(Parser, Debug)]
#[command(version, about = "USB HID gadget supervisor with persona switching")]
struct Cli {
    #[arg(short, long)]
    config: Option<PathBuf>,

    #[arg(long, default_value = "info")]
    log: String,

    /// Set up the gadget and exit (for testing). Does NOT start the API.
    #[arg(long)]
    setup_only: bool,

    /// Tear down the gadget and exit.
    #[arg(long)]
    teardown: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&cli.log))
        .with_target(false)
        .init();

    let cfg = config::load(cli.config)?;
    info!(persona = ?cfg.persona, "starting");

    if cli.teardown {
        gadget::teardown(&cfg)?;
        return Ok(());
    }

    // Build the USB gadget for the configured persona.
    gadget::setup(&cfg, &persona::descriptors_for(cfg.persona))?;
    info!("gadget online");

    if cli.setup_only {
        return Ok(());
    }

    let state = state::SharedState::new(cfg);
    let api = tokio::spawn(api::serve(state.clone()));

    tokio::select! {
        r = api => { tracing::error!(?r, "api exited"); }
        _ = tokio::signal::ctrl_c() => { info!("SIGINT, shutting down"); }
    }

    Ok(())
}
