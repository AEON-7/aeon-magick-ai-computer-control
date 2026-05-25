//! HID report descriptors per persona, plus the USB device-level identity
//! (VID/PID, strings) each persona presents.
//!
//! Report descriptors are dense byte streams in the USB HID standard format.
//! See "Device Class Definition for HID 1.11" §6.2.2.

use crate::config::Persona;

/// One HID function within a composite gadget.
#[derive(Debug, Clone)]
pub struct HidFunction {
    /// configfs function instance name, e.g. `hid.kbd`
    pub name: &'static str,
    /// HID protocol code (1 = keyboard, 2 = mouse, 0 = none)
    pub protocol: u8,
    /// HID subclass (1 = boot interface, 0 = no subclass)
    pub subclass: u8,
    /// EP IN packet size for this function
    pub report_length: u16,
    /// Report descriptor bytes
    pub report_desc: &'static [u8],
    /// `/dev/hidgN` purpose hint (informational, used in logs).
    pub kind: HidKind,
}

#[derive(Debug, Clone, Copy)]
pub enum HidKind {
    Keyboard,
    Mouse,
    Consumer,
    Trackpad,
}

#[derive(Debug, Clone)]
pub struct PersonaDescriptors {
    pub id_vendor: u16,
    pub id_product: u16,
    pub bcd_device: u16,
    pub manufacturer: &'static str,
    pub product: &'static str,
    pub serial: &'static str,
    pub functions: Vec<HidFunction>,
}

pub fn descriptors_for(p: Persona) -> PersonaDescriptors {
    match p {
        Persona::GenericComposite => generic(),
        Persona::LogitechMx => logitech_mx(),
        Persona::AppleMagic => apple_magic(),
    }
}

// ── Standard boot keyboard (8-byte report) ────────────────────────────────
//
// Byte 0: modifier mask (Ctrl/Shift/Alt/GUI L+R)
// Byte 1: reserved
// Bytes 2-7: up to 6 simultaneously-pressed keys (HID usage codes)
const BOOT_KEYBOARD_DESC: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xA1, 0x01, // Collection (Application)
    0x05, 0x07, //   Usage Page (Key Codes)
    0x19, 0xE0, //   Usage Minimum (LeftControl)
    0x29, 0xE7, //   Usage Maximum (RightGUI)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x08, //   Report Count (8)
    0x81, 0x02, //   Input (Data, Var, Abs) — modifier bitmap
    0x95, 0x01, //   Report Count (1)
    0x75, 0x08, //   Report Size (8)
    0x81, 0x01, //   Input (Const) — reserved byte
    0x95, 0x05, //   Report Count (5)
    0x75, 0x01, //   Report Size (1)
    0x05, 0x08, //   Usage Page (LEDs)
    0x19, 0x01, //   Usage Minimum (NumLock)
    0x29, 0x05, //   Usage Maximum (Kana)
    0x91, 0x02, //   Output (Data, Var, Abs) — LED state
    0x95, 0x01, //   Report Count (1)
    0x75, 0x03, //   Report Size (3)
    0x91, 0x01, //   Output (Const) — LED padding
    0x95, 0x06, //   Report Count (6)
    0x75, 0x08, //   Report Size (8)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0xFF, //   Logical Maximum (255)
    0x05, 0x07, //   Usage Page (Key Codes)
    0x19, 0x00, //   Usage Minimum (0)
    0x29, 0xFF, //   Usage Maximum (255)
    0x81, 0x00, //   Input (Data, Array) — 6 keycodes
    0xC0, // End Collection
];

// ── Standard boot mouse (4-byte report) ───────────────────────────────────
//
// Byte 0: button bitmap (L, R, M, 4, 5)
// Byte 1: X delta (signed)
// Byte 2: Y delta (signed)
// Byte 3: wheel delta (signed)
const BOOT_MOUSE_DESC: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x02, // Usage (Mouse)
    0xA1, 0x01, // Collection (Application)
    0x09, 0x01, //   Usage (Pointer)
    0xA1, 0x00, //   Collection (Physical)
    0x05, 0x09, //     Usage Page (Buttons)
    0x19, 0x01, //     Usage Minimum (Button 1)
    0x29, 0x05, //     Usage Maximum (Button 5)
    0x15, 0x00, //     Logical Minimum (0)
    0x25, 0x01, //     Logical Maximum (1)
    0x95, 0x05, //     Report Count (5)
    0x75, 0x01, //     Report Size (1)
    0x81, 0x02, //     Input (Data, Var, Abs) — buttons
    0x95, 0x01, //     Report Count (1)
    0x75, 0x03, //     Report Size (3)
    0x81, 0x01, //     Input (Const) — padding to byte boundary
    0x05, 0x01, //     Usage Page (Generic Desktop)
    0x09, 0x30, //     Usage (X)
    0x09, 0x31, //     Usage (Y)
    0x09, 0x38, //     Usage (Wheel)
    0x15, 0x81, //     Logical Minimum (-127)
    0x25, 0x7F, //     Logical Maximum (127)
    0x75, 0x08, //     Report Size (8)
    0x95, 0x03, //     Report Count (3)
    0x81, 0x06, //     Input (Data, Var, Rel) — X, Y, wheel
    0xC0, //   End Collection
    0xC0, // End Collection
];

// ── HID consumer page (16-bit usage codes for media keys etc.) ───────────
//
// 2-byte report. Usage codes from "HID Usage Tables" §15 (Consumer page).
const CONSUMER_DESC: &[u8] = &[
    0x05, 0x0C, // Usage Page (Consumer)
    0x09, 0x01, // Usage (Consumer Control)
    0xA1, 0x01, // Collection (Application)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xFF, 0x03, //   Logical Maximum (1023)
    0x19, 0x00, //   Usage Minimum (0)
    0x2A, 0xFF, 0x03, //   Usage Maximum (1023)
    0x75, 0x10, //   Report Size (16)
    0x95, 0x01, //   Report Count (1)
    0x81, 0x00, //   Input (Data, Array)
    0xC0, // End Collection
];

// ── Apple Magic Trackpad multi-touch (EXPERIMENTAL) ──────────────────────
//
// Descriptor is a minimal Microsoft Precision Touchpad-style multi-touch
// descriptor as a placeholder. The Apple-VID gesture-engine path requires
// the trackpad to ALSO speak Apple's proprietary HID report format which
// has been partially reverse-engineered in `linux-magicmouse-hidraw` and
// `hid-apple-patched`. We bring the Precision Touchpad descriptor up first
// (works cleanly on Windows / Linux) and iterate to Apple format with
// usbmon traces from a real Magic Trackpad. See docs/design/apple-mt.md.
//
// Supports up to 5 contacts simultaneously (enough for 4-finger gestures
// + thumb).
const APPLE_TOUCHPAD_DESC: &[u8] = &[
    0x05, 0x0D, // Usage Page (Digitizers)
    0x09, 0x05, // Usage (Touch Pad)
    0xA1, 0x01, // Collection (Application)
    //
    // Repeat the following contact block 5 times for 5 fingers. To keep this
    // sample readable we'll emit ONE contact and rely on macOS's Precision
    // Touchpad reporter handling single + chord taps. The full 5-contact
    // version is in `personas/apple_trackpad_full.bin` once captured.
    0x09, 0x22, //   Usage (Finger)
    0xA1, 0x02, //   Collection (Logical)
    0x09, 0x42, //     Usage (Tip Switch)
    0x15, 0x00, //     Logical Minimum (0)
    0x25, 0x01, //     Logical Maximum (1)
    0x75, 0x01, //     Report Size (1)
    0x95, 0x01, //     Report Count (1)
    0x81, 0x02, //     Input (Data, Var, Abs)
    0x09, 0x32, //     Usage (In Range)
    0x81, 0x02, //     Input (Data, Var, Abs)
    0x95, 0x06, //     Report Count (6) — padding to byte
    0x81, 0x03, //     Input (Const, Var, Abs)
    0x05, 0x01, //     Usage Page (Generic Desktop)
    0x26, 0xFF, 0x7F, //     Logical Maximum (32767)
    0x75, 0x10, //     Report Size (16)
    0x95, 0x01, //     Report Count (1)
    0x09, 0x30, //     Usage (X)
    0x81, 0x02, //     Input (Data, Var, Abs)
    0x09, 0x31, //     Usage (Y)
    0x81, 0x02, //     Input (Data, Var, Abs)
    0xC0, //   End Collection (Finger)
    //
    // Button (click) — physical trackpad click
    0x05, 0x09, //   Usage Page (Buttons)
    0x09, 0x01, //   Usage (Button 1)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x95, 0x01, //   Report Count (1)
    0x75, 0x01, //   Report Size (1)
    0x81, 0x02, //   Input (Data, Var, Abs)
    0x95, 0x07, //   Report Count (7) — padding
    0x81, 0x03, //   Input (Const)
    0xC0, // End Collection
];

fn generic() -> PersonaDescriptors {
    PersonaDescriptors {
        id_vendor: 0x1d6b,  // Linux Foundation
        id_product: 0x0104, // Multifunction Composite Gadget
        bcd_device: 0x0100,
        manufacturer: "aeon-cursed-kvm",
        product: "Aeon Cursed KVM",
        serial: "ACURSED-001",
        functions: vec![
            HidFunction {
                name: "hid.kbd",
                protocol: 1,
                subclass: 1,
                report_length: 8,
                report_desc: BOOT_KEYBOARD_DESC,
                kind: HidKind::Keyboard,
            },
            HidFunction {
                name: "hid.mouse",
                protocol: 2,
                subclass: 1,
                report_length: 4,
                report_desc: BOOT_MOUSE_DESC,
                kind: HidKind::Mouse,
            },
        ],
    }
}

fn logitech_mx() -> PersonaDescriptors {
    PersonaDescriptors {
        id_vendor: 0x046d,  // Logitech
        id_product: 0xc52b, // Unifying Receiver
        bcd_device: 0x1210,
        manufacturer: "Logitech",
        product: "USB Receiver",
        serial: "ACURSED-MX",
        functions: vec![
            HidFunction {
                name: "hid.kbd",
                protocol: 1,
                subclass: 1,
                report_length: 8,
                report_desc: BOOT_KEYBOARD_DESC,
                kind: HidKind::Keyboard,
            },
            HidFunction {
                name: "hid.mouse",
                protocol: 2,
                subclass: 1,
                report_length: 4,
                report_desc: BOOT_MOUSE_DESC,
                kind: HidKind::Mouse,
            },
            HidFunction {
                name: "hid.consumer",
                protocol: 0,
                subclass: 0,
                report_length: 2,
                report_desc: CONSUMER_DESC,
                kind: HidKind::Consumer,
            },
        ],
    }
}

fn apple_magic() -> PersonaDescriptors {
    PersonaDescriptors {
        id_vendor: 0x05ac,  // Apple
        id_product: 0x0265, // Magic Trackpad (gen 2 wired)
        bcd_device: 0x0100,
        manufacturer: "Apple Inc.",
        product: "Magic Trackpad",
        serial: "ACURSED-MT",
        functions: vec![
            HidFunction {
                name: "hid.kbd",
                protocol: 1,
                subclass: 1,
                report_length: 8,
                report_desc: BOOT_KEYBOARD_DESC,
                kind: HidKind::Keyboard,
            },
            HidFunction {
                name: "hid.trackpad",
                protocol: 0,
                subclass: 0,
                report_length: 8, // placeholder; Apple's real report is longer
                report_desc: APPLE_TOUCHPAD_DESC,
                kind: HidKind::Trackpad,
            },
            HidFunction {
                name: "hid.consumer",
                protocol: 0,
                subclass: 0,
                report_length: 2,
                report_desc: CONSUMER_DESC,
                kind: HidKind::Consumer,
            },
        ],
    }
}
