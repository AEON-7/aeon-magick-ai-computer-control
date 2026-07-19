//! aeon-hid
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
mod identity;
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

    let mut cfg = config::load(cli.config)?;
    info!(persona = ?cfg.persona, "starting");

    if cli.teardown {
        gadget::teardown(&cfg)?;
        return Ok(());
    }

    // Always teardown first — on first boot this is a harmless no-op
    // (gadget not yet bound), but on systemd-respawn (e.g. after a
    // POST /persona triggered a clean exit) it clears the old persona's
    // descriptors so the fresh setup binds the new persona's tree.
    if let Err(e) = gadget::teardown(&cfg) {
        tracing::debug!(?e, "pre-setup teardown returned non-fatal error (no gadget yet?)");
    }

    // Try to set up the requested persona. If that fails AND the persona
    // was selected via the runtime state file (not the default), fall
    // back to GenericComposite so the API still comes up and the user
    // can pick a different persona from the web UI instead of being
    // stuck in a systemd restart loop with no UI signal.
    //
    // Without this fallback, a buggy persona descriptor (or a kernel
    // change that rejects one) leaves /run/aeon/hid.sock unbound, the
    // supervisor's /api/state reports `hid: null`, and the persona
    // selector disappears entirely from the web UI — no way back without
    // SSH access.
    if let Err(e) = try_setup_persona(&cfg) {
        tracing::error!(
            ?e,
            persona = ?cfg.persona,
            "gadget setup FAILED for requested persona"
        );
        let original = cfg.persona;
        let fallback = config::Persona::GenericComposite;
        if cfg.persona != fallback {
            tracing::warn!(
                from = ?original,
                to = ?fallback,
                "falling back to generic-composite so the API stays available; \
                 record the error above and check `journalctl -u aeon-hid` for details. \
                 The persona.state file has been left intact so you can retry after a fix."
            );
            cfg.persona = fallback;
            // Best-effort teardown of whatever partial state the failed
            // setup may have left in configfs.
            let _ = gadget::teardown(&cfg);
            try_setup_persona(&cfg).map_err(|fallback_err| {
                anyhow::anyhow!(
                    "gadget setup failed for {:?} ({}) AND for fallback {:?} ({})",
                    original, e, fallback, fallback_err,
                )
            })?;
            info!(persona = ?fallback, "gadget online (FALLBACK — selected persona errored)");
        } else {
            // Already on the default and it still failed — nothing more to try.
            return Err(e);
        }
    } else {
        info!(persona = ?cfg.persona, "gadget online");
    }

    if cli.setup_only {
        return Ok(());
    }

    let state = state::SharedState::new(cfg);
    let api = tokio::spawn(api::serve(state.clone()));
    // When the host unplugs (UDC → not attached), rebuild the gadget with a
    // *new* rotating identity so the next plug-in looks like a fresh unit —
    // defeats stale macOS "block this accessory" pins on VID+PID+serial.
    let reconnect = tokio::spawn(identity_reconnect_loop(state.clone()));

    tokio::select! {
        r = api => { tracing::error!(?r, "api exited"); }
        r = reconnect => { tracing::error!(?r, "identity reconnect loop exited"); }
        _ = tokio::signal::ctrl_c() => { info!("SIGINT, shutting down"); }
    }

    Ok(())
}

/// Watch UDC attach state. After a host disconnect, re-run gadget setup with
/// a fresh identity once the port has stayed detached for a short settle.
async fn identity_reconnect_loop(state: state::SharedState) {
    use std::time::Duration;
    let udc = state.0.cfg.udc.clone();
    let state_path = format!("/sys/class/udc/{}/state", udc);
    let mut was_attached = read_udc_attached(&state_path);
    let mut need_rotate = false;

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let attached = read_udc_attached(&state_path);
        if was_attached && !attached {
            info!("USB host detached — will rotate identity before next attach");
            need_rotate = true;
        }
        // Rebind while still detached so the next cable-in sees new IDs.
        if need_rotate && !attached {
            // Brief settle so a flaky cable doesn't thrash.
            tokio::time::sleep(Duration::from_millis(800)).await;
            if read_udc_attached(&state_path) {
                was_attached = true;
                need_rotate = false;
                continue;
            }
            info!("rebuilding gadget with rotated identity for next host connection");
            let cfg = state.0.cfg.clone();
            // Blocking configfs work off the runtime.
            let result = tokio::task::spawn_blocking(move || {
                let _ = gadget::teardown(&cfg);
                try_setup_persona(&cfg)
            })
            .await;
            match result {
                Ok(Ok(())) => {
                    info!("gadget rebound with new identity");
                    need_rotate = false;
                }
                Ok(Err(e)) => tracing::error!(?e, "identity rebind failed"),
                Err(e) => tracing::error!(?e, "identity rebind task join failed"),
            }
        }
        was_attached = attached;
    }
}

fn read_udc_attached(path: &str) -> bool {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let t = s.trim();
            // Kernel uses "configured" / "addressed" / "default" when a host
            // is present; "not attached" when idle.
            t != "not attached" && !t.is_empty()
        }
        Err(_) => false,
    }
}

/// Build the gadget composite for `cfg.persona`. Pulled out so the main
/// fallback path can call it twice without duplicating the descriptor
/// assembly.
///
/// **Identity is rotated on every call** (fresh VID/PID/bcd/serial from
/// the persona's commercial-peripheral pool). Call this again after a
/// host disconnect if you want the next plug-in to look like a new unit.
fn try_setup_persona(cfg: &config::Config) -> Result<()> {
    let mut desc = persona::descriptors_for(cfg.persona);

    if let Some(ecm) = load_ecm_config() {
        info!(host_mac=%ecm.host_mac, dev_mac=%ecm.dev_mac, "ECM enabled (USB ethernet passthrough)");
        desc.ecm = Some(ecm);
    }
    if let Some(ms) = load_mass_storage_config() {
        info!(iso = %ms.iso_path, "mass-storage CDROM enabled");
        desc.mass_storage = Some(ms);
    }
    if let Some(uvc) = load_uvc_config() {
        info!(w = uvc.width, h = uvc.height, "UVC webcam enabled (Cam0 → USB webcam)");
        desc.uvc = Some(uvc);
    }

    // Dock-mode identity (Belkin) when multi-function beyond HID-only.
    let dock_mode = desc.ecm.is_some() || desc.mass_storage.is_some() || desc.uvc.is_some();
    identity::apply_rotating_identity(&mut desc, cfg.persona, dock_mode);
    identity::write_identity_audit(&desc);

    gadget::setup(cfg, &desc)
}

/// Read /etc/aeon/network.toml. Returns an EcmConfig if usb_ethernet is
/// enabled there, else None (no ECM function attached → HID-only gadget).
fn load_ecm_config() -> Option<persona::EcmConfig> {
    let raw = std::fs::read_to_string("/etc/aeon/network.toml").ok()?;
    let v: toml::Value = toml::from_str(&raw).ok()?;
    let usb = v.get("usb_ethernet")?;
    if !usb.get("enabled").and_then(|b| b.as_bool()).unwrap_or(false) {
        return None;
    }
    Some(persona::EcmConfig {
        host_mac: usb.get("host_mac").and_then(|s| s.as_str())?.to_string(),
        dev_mac: usb.get("dev_mac").and_then(|s| s.as_str())?.to_string(),
    })
}

/// Read /etc/aeon/storage.toml. Returns a MassStorageConfig if an ISO
/// is marked active AND the file exists at /var/lib/aeon/iso/<slug>.iso,
/// else None.
///
/// Storage config schema:
/// ```toml
/// [mass_storage]
/// active = "debian-12-netinst"   # slug, or "" to detach
/// ```
///
/// Setting `active` to a non-empty slug requires aeon-hid to restart
/// for the change to take effect — the supervisor's /api/storage/active
/// endpoint triggers `systemctl restart aeon-hid` after writing the
/// new state. From the host's perspective this is a brief USB
/// re-enumerate (~1s blip), then the new disk appears in the boot menu.
fn load_mass_storage_config() -> Option<persona::MassStorageConfig> {
    let raw = std::fs::read_to_string("/etc/aeon/storage.toml").ok()?;
    let v: toml::Value = toml::from_str(&raw).ok()?;
    let ms = v.get("mass_storage")?;
    let active = ms.get("active").and_then(|s| s.as_str())?;
    if active.is_empty() {
        return None;
    }
    // Sanitise the slug — no path traversal, alphanumerics + hyphens
    // + underscores + dots only.
    if !active.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        tracing::warn!(active, "storage.toml: active slug contains invalid characters, ignoring");
        return None;
    }
    let iso_path = format!("/var/lib/aeon/iso/{}.iso", active);
    if !std::path::Path::new(&iso_path).exists() {
        tracing::warn!(
            slug = active,
            path = %iso_path,
            "storage.toml: active ISO file missing — disk drive not attached"
        );
        return None;
    }
    Some(persona::MassStorageConfig { iso_path })
}

/// Read /etc/aeon/uvc.toml. Returns a UvcConfig if `enabled = true`, else None
/// (no webcam function → the gadget stays HID/ethernet only). This only
/// decides whether the gadget ADVERTISES a webcam to the host; the actual
/// frames are pumped by the separate `aeon-uvc` (uvc-gadget) daemon, which
/// opens the Cam0 IMX477 only while the host is streaming.
///
/// Schema (top-level keys):
/// ```toml
/// enabled = true
/// width = 1280
/// height = 720
/// fps = 30
/// streaming_maxpacket = 2048
/// ```
fn load_uvc_config() -> Option<persona::UvcConfig> {
    let raw = std::fs::read_to_string("/etc/aeon/uvc.toml").ok()?;
    let v: toml::Value = toml::from_str(&raw).ok()?;
    if !v.get("enabled").and_then(|b| b.as_bool()).unwrap_or(false) {
        return None;
    }
    let width = v.get("width").and_then(|x| x.as_integer()).unwrap_or(1280).clamp(2, 4096) as u16;
    let height = v.get("height").and_then(|x| x.as_integer()).unwrap_or(720).clamp(2, 4096) as u16;
    let fps = v.get("fps").and_then(|x| x.as_integer()).unwrap_or(30).clamp(1, 120) as u32;
    // dwc2 (Pi 5 OTG) in USB-2.0 high-speed caps isochronous wMaxPacketSize at
    // 1024 and has no high-bandwidth iso, so 2048/3072 endpoints never enable and
    // the host can't start streaming (Windows: 0x80070006). Default to and clamp
    // at 1024 so a stale uvc.toml can't re-break it.
    let streaming_maxpacket =
        v.get("streaming_maxpacket").and_then(|x| x.as_integer()).unwrap_or(1024).clamp(1, 1024) as u16;
    // dwFrameInterval is in 100ns units (1s = 10_000_000). Advertise the
    // configured fps, plus a 15fps fallback so a bandwidth-constrained host
    // can negotiate a lighter rate.
    let mut frame_intervals = vec![10_000_000u32 / fps.max(1)];
    if fps > 15 {
        frame_intervals.push(10_000_000 / 15);
    }
    Some(persona::UvcConfig {
        width,
        height,
        frame_intervals,
        streaming_maxpacket,
    })
}
