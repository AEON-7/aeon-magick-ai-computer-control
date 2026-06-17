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

    tokio::select! {
        r = api => { tracing::error!(?r, "api exited"); }
        _ = tokio::signal::ctrl_c() => { info!("SIGINT, shutting down"); }
    }

    Ok(())
}

/// Build the gadget composite for `cfg.persona`. Pulled out so the main
/// fallback path can call it twice without duplicating the descriptor
/// assembly.
fn try_setup_persona(cfg: &config::Config) -> Result<()> {
    let mut desc = persona::descriptors_for(cfg.persona);
    // Inject a per-device random serial. Persisted across reboots so the
    // host sees a stable identity, but unique per Pi and free of any
    // strings (like "ACURSED-…") that would trip host-side anomaly
    // heuristics. Real USB devices have factory-unique serials; this
    // mimics that behavior.
    desc.serial = load_or_gen_serial();
    info!(serial = %desc.serial, persona = ?cfg.persona, "USB serial");
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
    gadget::setup(cfg, &desc)
}

/// Per-device USB serial.
///
/// Loads from /etc/aeon/usb-serial.state if it exists, else generates a
/// fresh 12-char uppercase alphanumeric string and persists it. The
/// format roughly mimics what Apple and Logitech print on real device
/// hardware (e.g. `F2LV8XLBL311`, `0123456789AB`) so it doesn't stand
/// out to host-side device-fingerprinting or anomaly scanners that flag
/// suspicious-looking strings.
///
/// Stable across reboots; unique per Pi (because file is generated
/// per-device on first boot).
fn load_or_gen_serial() -> String {
    use std::path::Path;
    let path = Path::new("/etc/aeon/usb-serial.state");
    if let Ok(existing) = std::fs::read_to_string(path) {
        let trimmed = existing.trim();
        // Validate it's still a sensible serial (12+ uppercase alnum).
        if trimmed.len() >= 8
            && trimmed
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            return trimmed.to_string();
        }
    }
    // Generate. 12 chars: 36^12 keyspace, ample.
    let alpha = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let serial: String = {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        (0..12)
            .map(|_| {
                let idx = rng.gen_range(0..alpha.len());
                alpha[idx] as char
            })
            .collect()
    };
    // Persist (best-effort — if the dir doesn't exist or we can't write,
    // we still return the freshly-generated value but it won't survive
    // a reboot; next boot will regenerate).
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, &serial);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    serial
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
