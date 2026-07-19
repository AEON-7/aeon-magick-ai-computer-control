//! HID config: persona selection, USB IDs (used per-persona), output device
//! paths.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Persona {
    /// Generic hub / composite keyboard+mouse. Identity rotates each connect.
    GenericComposite,

    /// Logitech MX-class keyboard + mouse. Rotating Logitech VID/PID/serial
    /// (plain HID PIDs only — never Unifying 0xc52b).
    LogitechMx,

    /// Experimental Apple Magic Trackpad multi-touch path (Apple VID + MT
    /// descriptor). Can load AppleUSBMultitouch; prefer apple-magic-stable
    /// for daily Mac use.
    AppleMagic,

    /// Apple Magic Keyboard + Magic Mouse (relative). Rotating Apple
    /// keyboard/mouse PIDs (not Trackpad 0x0265). Recommended for macOS.
    AppleMagicStable,

    /// Absolute pointer themed as a **Wacom tablet** (rotating Wacom
    /// VID/PID/serial). Report path is absolute HID (0..32767) via /move_abs.
    /// Best for agent click_at precision.
    GenericAbsolute,
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
            "generic-absolute" => Some(Self::GenericAbsolute),
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
            Self::GenericAbsolute => "generic-absolute",
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
