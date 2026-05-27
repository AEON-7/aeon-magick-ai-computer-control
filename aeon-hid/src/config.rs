//! HID config: persona selection, USB IDs (used per-persona), output device
//! paths.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Persona {
    /// Generic boot keyboard + boot mouse. Universally accepted, no fancy
    /// features. Safe default.
    GenericComposite,

    /// Boot keyboard + boot mouse + consumer page, advertised as a
    /// Logitech Unifying Receiver. Lets media keys + extra mouse buttons
    /// route through OS-side Logitech driver paths when present.
    LogitechMx,

    /// Apple Magic Keyboard + Magic Trackpad (multi-touch) descriptors.
    /// macOS routes Apple-VID multi-touch devices through its gesture
    /// engine — this is the only path to programmatic 3/4-finger swipes.
    /// EXPERIMENTAL — requires hardware-in-the-loop validation.
    /// Power-unstable on Mac USB-C due to AppleUSBMultitouch driver
    /// chatter (see apple-magic-stable for the practical alternative).
    AppleMagic,

    /// Apple-themed sub-interface labels, but the device's USB VID:PID
    /// is generic (Linux Foundation composite) so macOS uses generic
    /// HID handling instead of loading AppleUSBMultitouch. Stable under
    /// Mac USB-C power. Recommended for everyday use; loses the
    /// programmatic Apple-VID gesture path (use the experimental
    /// `apple-magic` persona on a robust power supply for that).
    AppleMagicStable,
}

impl Default for Persona {
    fn default() -> Self {
        Self::GenericComposite
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub persona: Persona,
    pub udc: String, // e.g., "fe980000.usb" on Pi 4
    pub configfs_root: PathBuf,
    pub gadget_name: String,
    pub api_sock: PathBuf,
    /// File where the runtime-selected persona is persisted (so it survives
    /// reboots without baking a new config). Written by POST /persona,
    /// read on startup. Overrides whatever's in hid.toml.
    pub persona_state_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            persona: Persona::GenericComposite,
            udc: detect_udc().unwrap_or_else(|| "dwc2".to_string()),
            configfs_root: PathBuf::from("/sys/kernel/config/usb_gadget"),
            gadget_name: "aeon".to_string(),
            api_sock: PathBuf::from("/run/aeon/hid.sock"),
            persona_state_path: PathBuf::from("/etc/aeon/persona.state"),
        }
    }
}

impl Persona {
    /// Parse the on-disk persona.state value back into a Persona.
    pub fn from_slug(s: &str) -> Option<Self> {
        match s.trim() {
            "generic-composite" => Some(Self::GenericComposite),
            "logitech-mx" => Some(Self::LogitechMx),
            "apple-magic" => Some(Self::AppleMagic),
            "apple-magic-stable" => Some(Self::AppleMagicStable),
            _ => None,
        }
    }
    /// kebab-case slug used in API + state file.
    pub fn as_slug(self) -> &'static str {
        match self {
            Self::GenericComposite => "generic-composite",
            Self::LogitechMx => "logitech-mx",
            Self::AppleMagic => "apple-magic",
            Self::AppleMagicStable => "apple-magic-stable",
        }
    }
}

pub fn load(explicit: Option<PathBuf>) -> Result<Config> {
    let candidates = match explicit {
        Some(p) => vec![p],
        None => vec![
            PathBuf::from("/etc/aeon/hid.toml"),
            PathBuf::from("hid.toml"),
        ],
    };
    let mut cfg = Config::default();
    for p in &candidates {
        if p.exists() {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("reading {}", p.display()))?;
            cfg = toml::from_str(&text)
                .with_context(|| format!("parsing {}", p.display()))?;
            tracing::info!(path = %p.display(), "loaded config");
            break;
        }
    }

    // Persisted-persona override. If POST /persona has been called, that
    // wrote the new persona slug to persona_state_path and exited the
    // process. systemd respawns us; we pick up the new persona here.
    if cfg.persona_state_path.exists() {
        match std::fs::read_to_string(&cfg.persona_state_path) {
            Ok(s) => {
                if let Some(p) = Persona::from_slug(&s) {
                    tracing::info!(
                        from_state = ?p,
                        path = %cfg.persona_state_path.display(),
                        "applying persisted persona override",
                    );
                    cfg.persona = p;
                } else {
                    tracing::warn!(
                        contents = %s.trim(),
                        path = %cfg.persona_state_path.display(),
                        "persona.state has unknown value, ignoring",
                    );
                }
            }
            Err(e) => tracing::warn!(?e, "could not read persona.state"),
        }
    }

    Ok(cfg)
}

fn detect_udc() -> Option<String> {
    let entries = std::fs::read_dir("/sys/class/udc").ok()?;
    for e in entries.flatten() {
        if let Some(name) = e.file_name().to_str() {
            return Some(name.to_string());
        }
    }
    None
}
