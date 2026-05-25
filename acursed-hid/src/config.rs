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
    AppleMagic,
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            persona: Persona::GenericComposite,
            udc: detect_udc().unwrap_or_else(|| "dwc2".to_string()),
            configfs_root: PathBuf::from("/sys/kernel/config/usb_gadget"),
            gadget_name: "acursed".to_string(),
            api_sock: PathBuf::from("/run/acursed/hid.sock"),
        }
    }
}

pub fn load(explicit: Option<PathBuf>) -> Result<Config> {
    let candidates = match explicit {
        Some(p) => vec![p],
        None => vec![
            PathBuf::from("/etc/acursed/hid.toml"),
            PathBuf::from("hid.toml"),
        ],
    };
    for p in &candidates {
        if p.exists() {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("reading {}", p.display()))?;
            let cfg: Config = toml::from_str(&text)
                .with_context(|| format!("parsing {}", p.display()))?;
            tracing::info!(path = %p.display(), "loaded config");
            return Ok(cfg);
        }
    }
    Ok(Config::default())
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
