//! Rotating USB device identity — VID/PID/bcd/serial/product strings.
//!
//! Goal: each gadget bind (and each host reconnect) presents a plausible
//! commercial peripheral identity so host-side accessory allow-lists and
//! "known bad serial" blocks don't stick across sessions.
//!
//! Themes (slugs unchanged for API compatibility):
//!   generic-composite  → plain composite / hub-class device
//!   generic-absolute   → Wacom-style absolute tablet
//!   logitech-mx        → Logitech MX-class keyboard+mouse receiver
//!   apple-magic-stable → Apple Magic Keyboard + Magic Mouse (relative)
//!   apple-magic        → Apple Magic Keyboard + Trackpad (experimental MT)
//!
//! When USB ethernet (NCM) is enabled, the *device-level* identity is
//! overridden to a Belkin USB-C dock theme (composite dock + HID + net).

use crate::config::Persona;
use crate::persona::PersonaDescriptors;
use rand::Rng;
use tracing::info;

/// One pick from a persona's identity pool.
#[derive(Clone, Copy)]
struct IdPick {
    vid: u16,
    pid: u16,
    /// BCD firmware rev (nibbles must be 0..=9 — configfs rejects A–F).
    bcd: u16,
    manufacturer: &'static str,
    product: &'static str,
}

// ── Identity pools ────────────────────────────────────────────────────────

/// Generic composite — Linux Foundation multifunction / anonymous hubs.
const GENERIC_POOL: &[IdPick] = &[
    IdPick {
        vid: 0x1d6b,
        pid: 0x0104,
        bcd: 0x0100,
        manufacturer: "Generic",
        product: "USB Composite Device",
    },
    IdPick {
        vid: 0x1d6b,
        pid: 0x0105,
        bcd: 0x0101,
        manufacturer: "Generic",
        product: "USB Multifunction Adapter",
    },
    IdPick {
        vid: 0x05e3, // Genesys Logic (common in cheap hubs)
        pid: 0x0610,
        bcd: 0x9230, // BCD-valid? 9,2,3,0 — ok
        manufacturer: "Generic",
        product: "USB2.0 Hub",
    },
    IdPick {
        vid: 0x2109, // VIA Labs
        pid: 0x2817,
        bcd: 0x0501,
        manufacturer: "Generic",
        product: "USB3.0 Hub",
    },
];

/// Absolute tablet theme for `generic-absolute`.
///
/// **Important (Windows):** We do **not** use real Wacom VID `0x056a`.
/// Machines with official Wacom drivers (this studio box has them) claim that
/// VID and remap the absolute range into a tablet rectangle — agent `click_at`
/// fractions then miss UI by tens of pixels even on mirrored 1080p.
///
/// Instead: Linux Foundation multifunction VID + absolute pointer report
/// (0..32767, QEMU usb-tablet style). Product strings still say "Absolute
/// Tablet" so the host tree is readable. Report path is unchanged (Wacom-style
/// absolute, not relative mouse).
const WACOM_POOL: &[IdPick] = &[
    IdPick {
        vid: 0x1d6b, // Linux Foundation — avoid Wacom driver hijack on Windows
        pid: 0x0104,
        bcd: 0x0103,
        manufacturer: "Generic",
        product: "USB Absolute Tablet",
    },
    IdPick {
        vid: 0x1d6b,
        pid: 0x0105,
        bcd: 0x0107,
        manufacturer: "Generic",
        product: "USB Absolute Pointer",
    },
    IdPick {
        vid: 0x1d6b,
        pid: 0x0106,
        bcd: 0x0102,
        manufacturer: "Generic",
        product: "Absolute HID Tablet",
    },
    IdPick {
        vid: 0x1d6b,
        pid: 0x0107,
        bcd: 0x0110,
        manufacturer: "Generic",
        product: "Agent Absolute Tablet",
    },
    IdPick {
        vid: 0x1d6b,
        pid: 0x0108,
        bcd: 0x0105,
        manufacturer: "Generic",
        product: "USB Tablet Absolute",
    },
];

/// Logitech MX Universal Receiver theme — product string always reads as
/// "MX Universal Receiver" (what macOS users expect). PIDs stay plain HID
/// (NOT Unifying 0xc52b — that binds hid-logitech-dj on Linux and wedges).
const LOGITECH_POOL: &[IdPick] = &[
    IdPick {
        vid: 0x046d,
        pid: 0xc31c,
        bcd: 0x1200,
        manufacturer: "Logitech",
        product: "MX Universal Receiver",
    },
    IdPick {
        vid: 0x046d,
        pid: 0xc32b,
        bcd: 0x1210,
        manufacturer: "Logitech",
        product: "MX Universal Receiver",
    },
    IdPick {
        vid: 0x046d,
        pid: 0xc33f,
        bcd: 0x1301,
        manufacturer: "Logitech",
        product: "MX Universal Receiver",
    },
    IdPick {
        vid: 0x046d,
        pid: 0xc534,
        bcd: 0x1101,
        manufacturer: "Logitech",
        product: "MX Universal Receiver",
    },
    IdPick {
        vid: 0x046d,
        pid: 0xc52f, // nano receiver, often plain HID on macOS
        bcd: 0x1201,
        manufacturer: "Logitech",
        product: "MX Universal Receiver",
    },
];

/// Apple Magic Keyboard / Mouse themed — Apple VID with keyboard-safe PIDs
/// (avoid Magic Trackpad 0x0265 which loads AppleUSBMultitouch and can
/// brown-out ports). Strings rotate between Keyboard / Mouse product names.
const APPLE_STABLE_POOL: &[IdPick] = &[
    IdPick {
        vid: 0x05ac,
        pid: 0x024f, // Aluminum Keyboard (ANSI)
        bcd: 0x0074, // BCD 0.7.4
        manufacturer: "Apple Inc.",
        product: "Magic Keyboard",
    },
    IdPick {
        vid: 0x05ac,
        pid: 0x0250,
        bcd: 0x0075,
        manufacturer: "Apple Inc.",
        product: "Magic Keyboard",
    },
    IdPick {
        vid: 0x05ac,
        pid: 0x0267, // Magic Keyboard (2015-ish)
        bcd: 0x0201,
        manufacturer: "Apple Inc.",
        product: "Magic Keyboard",
    },
    IdPick {
        vid: 0x05ac,
        pid: 0x030d, // Magic Mouse 2
        bcd: 0x0107,
        manufacturer: "Apple Inc.",
        product: "Magic Mouse",
    },
    IdPick {
        vid: 0x05ac,
        pid: 0x0263,
        bcd: 0x0103,
        manufacturer: "Apple Inc.",
        product: "Magic Mouse 2",
    },
    IdPick {
        vid: 0x05ac,
        pid: 0x029c,
        bcd: 0x0202,
        manufacturer: "Apple Inc.",
        product: "Magic Keyboard with Numeric Keypad",
    },
];

/// Experimental Apple trackpad path — keep MT-oriented PIDs but rotate serials.
const APPLE_MT_POOL: &[IdPick] = &[
    IdPick {
        vid: 0x05ac,
        pid: 0x0265,
        bcd: 0x0119,
        manufacturer: "Apple Inc.",
        product: "Magic Trackpad 2",
    },
    IdPick {
        vid: 0x05ac,
        pid: 0x030e,
        bcd: 0x0102,
        manufacturer: "Apple Inc.",
        product: "Magic Trackpad",
    },
    IdPick {
        vid: 0x05ac,
        pid: 0x0324,
        bcd: 0x0101,
        manufacturer: "Apple Inc.",
        product: "Magic Keyboard with Trackpad",
    },
];

/// Belkin USB-C dock identity — used when NCM ethernet (or mass storage / UVC
/// dock functions) make the composite look like a multiport hub.
const BELKIN_DOCK_POOL: &[IdPick] = &[
    IdPick {
        vid: 0x050d,
        pid: 0x1109,
        bcd: 0x0100,
        manufacturer: "Belkin",
        product: "USB-C Multiport Adapter",
    },
    IdPick {
        vid: 0x050d,
        pid: 0x3090,
        bcd: 0x0101,
        manufacturer: "Belkin",
        product: "Connect USB-C 5-in-1 Hub",
    },
    IdPick {
        vid: 0x050d,
        pid: 0x0237,
        bcd: 0x0200,
        manufacturer: "Belkin",
        product: "USB-C Dock",
    },
    IdPick {
        vid: 0x050d,
        pid: 0x1106,
        bcd: 0x0102,
        manufacturer: "Belkin International",
        product: "USB-C Multimedia Hub",
    },
    IdPick {
        vid: 0x2109, // some Belkin rebrands use VL chips
        pid: 0x0817,
        bcd: 0x0500,
        manufacturer: "Belkin",
        product: "USB-C Hub",
    },
];

fn pick(pool: &[IdPick]) -> IdPick {
    let mut rng = rand::thread_rng();
    pool[rng.gen_range(0..pool.len())]
}

/// Fresh serial every bind — format mimics factory alphanumerics.
/// Not persisted: each connection is a "new unit" to host allow-lists.
pub fn fresh_serial(len: usize) -> String {
    let alpha = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no I/O/0/1 — more factory-like
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| alpha[rng.gen_range(0..alpha.len())] as char)
        .collect()
}

/// Random BCD-safe device revision (each nibble 0..=9).
fn fresh_bcd(base: u16) -> u16 {
    let mut rng = rand::thread_rng();
    // Keep high byte from base-ish, randomize low nibbles within BCD.
    let major = ((base >> 8) & 0x0f).min(9);
    let minor = rng.gen_range(0u16..10);
    let patch = rng.gen_range(0u16..10);
    let rev = rng.gen_range(0u16..10);
    (major << 12) | (minor << 8) | (patch << 4) | rev
}

/// Apply a rotating identity to `desc` for this bind.
///
/// `dock_mode` = true when USB ethernet / mass-storage / UVC make the
/// composite a multi-function dock → Belkin identity overlay.
pub fn apply_rotating_identity(
    desc: &mut PersonaDescriptors,
    persona: Persona,
    dock_mode: bool,
) {
    let pool = if dock_mode {
        BELKIN_DOCK_POOL
    } else {
        match persona {
            Persona::GenericComposite => GENERIC_POOL,
            Persona::GenericAbsolute => WACOM_POOL,
            Persona::LogitechMx => LOGITECH_POOL,
            Persona::AppleMagicStable => APPLE_STABLE_POOL,
            Persona::AppleMagic => APPLE_MT_POOL,
        }
    };

    let id = pick(pool);
    desc.id_vendor = id.vid;
    desc.id_product = id.pid;
    desc.bcd_device = fresh_bcd(id.bcd);
    desc.manufacturer = id.manufacturer;
    desc.product = id.product;
    // Serial length: Apple-ish 12, Logitech-ish 12, Wacom often 8–10.
    let serial_len = if dock_mode {
        12
    } else {
        match persona {
            Persona::GenericAbsolute => 10,
            Persona::AppleMagic | Persona::AppleMagicStable => 12,
            _ => 12,
        }
    };
    desc.serial = fresh_serial(serial_len);

    // Theme interface labels to match product family (HID functions only).
    if dock_mode {
        relabel_interfaces(desc, "Dock Keyboard", "Dock Mouse", "Dock Controls", None);
    } else {
        match persona {
            Persona::GenericAbsolute => {
                // Avoid "Wacom" in interface strings — some Wacom host software
                // matches names even when VID is not 0x056a.
                relabel_interfaces(
                    desc,
                    "Absolute Keyboard",
                    "Absolute Pointer",
                    "Consumer Control",
                    None,
                );
            }
            Persona::LogitechMx => {
                relabel_interfaces(
                    desc,
                    "Logitech MX Keys Keyboard",
                    "Logitech MX Master Mouse",
                    "Logitech Consumer Control",
                    None,
                );
            }
            Persona::AppleMagicStable => {
                // Product string may be Keyboard or Mouse — keep both sub-labels Apple.
                relabel_interfaces(
                    desc,
                    "Apple Magic Keyboard",
                    "Apple Magic Mouse",
                    "Apple Consumer Control",
                    None,
                );
            }
            Persona::AppleMagic => {
                relabel_interfaces(
                    desc,
                    "Apple Magic Keyboard",
                    "Apple Magic Trackpad",
                    "Apple Consumer Control",
                    Some("Apple Magic Trackpad"),
                );
            }
            Persona::GenericComposite => {
                relabel_interfaces(desc, "Keyboard", "Pointing Device", "Consumer Control", None);
            }
        }
    }

    info!(
        vid = format_args!("{:04x}", desc.id_vendor),
        pid = format_args!("{:04x}", desc.id_product),
        bcd = format_args!("{:04x}", desc.bcd_device),
        manufacturer = desc.manufacturer,
        product = desc.product,
        serial = %desc.serial,
        dock_mode,
        ?persona,
        "rotated USB identity for this connection"
    );
}

fn relabel_interfaces(
    desc: &mut PersonaDescriptors,
    kbd: &'static str,
    mouse: &'static str,
    consumer: &'static str,
    trackpad: Option<&'static str>,
) {
    for f in &mut desc.functions {
        match f.name {
            "hid.kbd" => f.interface_label = Some(kbd),
            "hid.mouse" => f.interface_label = Some(mouse),
            "hid.consumer" => f.interface_label = Some(consumer),
            "hid.trackpad" => {
                f.interface_label = Some(trackpad.unwrap_or(mouse));
            }
            _ => {}
        }
    }
}

/// Persist last identity for diagnostics only (hosts never reuse this file).
pub fn write_identity_audit(desc: &PersonaDescriptors) {
    let path = std::path::Path::new("/run/aeon/hid-identity.json");
    let body = format!(
        "{{\"vid\":\"{:04x}\",\"pid\":\"{:04x}\",\"bcd\":\"{:04x}\",\"manufacturer\":{},\"product\":{},\"serial\":{}}}\n",
        desc.id_vendor,
        desc.id_product,
        desc.bcd_device,
        serde_json::to_string(desc.manufacturer).unwrap_or_else(|_| "\"?\"".into()),
        serde_json::to_string(desc.product).unwrap_or_else(|_| "\"?\"".into()),
        serde_json::to_string(&desc.serial).unwrap_or_else(|_| "\"?\"".into()),
    );
    let _ = std::fs::write(path, body);
}
