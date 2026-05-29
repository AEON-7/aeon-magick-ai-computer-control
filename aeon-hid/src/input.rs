//! Write HID reports to `/dev/hidgN` to actuate keyboard / mouse / trackpad
//! input.
//!
//! All public operations here are **atomic logical actions** — they
//! guarantee that any internal "press" state is paired with a "release"
//! before returning, even on error paths. This matches the cursed-hid
//! design lesson: a client never gets to leave a key half-pressed.

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

/// Maps logical persona function names to /dev/hidg device paths.
/// On a Pi with our gadget bound, devices appear in symlink order:
///   hid.kbd      → /dev/hidg0
///   hid.mouse    → /dev/hidg1
///   hid.consumer → /dev/hidg2
/// (or with trackpad replacing mouse for apple-magic persona)
pub struct Hid {
    kbd: Mutex<PathBuf>,
    mouse: Mutex<Option<PathBuf>>,
    consumer: Mutex<Option<PathBuf>>,
    /// Reserved for the Apple Magic persona's trackpad device path. Multi-
    /// touch report writers will live in a follow-on commit.
    #[allow(dead_code)]
    trackpad: Mutex<Option<PathBuf>>,
}

impl Hid {
    pub fn new() -> Self {
        Self {
            kbd: Mutex::new(PathBuf::from("/dev/hidg0")),
            mouse: Mutex::new(Some(PathBuf::from("/dev/hidg1"))),
            consumer: Mutex::new(Some(PathBuf::from("/dev/hidg2"))),
            trackpad: Mutex::new(None),
        }
    }

    // ─── Keyboard ─────────────────────────────────────────────────────────

    /// Type a string. Each printable-ASCII character gets a paired
    /// press → release with a short pacing gap. Characters that don't
    /// map to a USB HID scancode (emoji, smart quotes, em-dashes,
    /// accented letters, etc.) are SKIPPED — the function keeps going
    /// rather than aborting the whole typing run. Returns
    /// `(typed, skipped)` so the caller can surface a useful UI
    /// message ("typed 24 chars, skipped 2 unmappable").
    pub fn type_str(&self, s: &str) -> Result<(usize, usize)> {
        let mut typed = 0usize;
        let mut skipped = 0usize;
        for ch in s.chars() {
            // ascii_to_hid returns None for anything outside the
            // mapped printable-ASCII range — skip rather than fail.
            if ascii_to_hid(ch).is_none() {
                skipped += 1;
                continue;
            }
            // tap_char can still fail (e.g. /dev/hidg* write error).
            // Propagate that — it's a real device-level fault, not a
            // user-input issue.
            self.tap_char(ch)?;
            typed += 1;
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
        Ok((typed, skipped))
    }

    /// Single character: build the HID keyboard report, send press, then
    /// release. `finally` ensures the release fires even if write_press
    /// returned Err.
    pub fn tap_char(&self, ch: char) -> Result<()> {
        let (modifier, keycode) = ascii_to_hid(ch).ok_or_else(|| anyhow!("unmappable char {ch:?}"))?;
        let press = [modifier, 0, keycode, 0, 0, 0, 0, 0];
        let release = [0u8; 8];
        let path = self.kbd.lock().clone();
        let press_result = write_report(&path, &press);
        // ALWAYS release, even if press failed mid-write.
        let _ = write_report(&path, &release);
        press_result
    }

    /// Send a keyboard chord (modifiers + up to 6 simultaneously held
    /// keys), held briefly, then released — all in one call. If anything
    /// fails mid-way, an all-zero release report still fires.
    pub fn chord(&self, modifier: u8, keys: &[u8], hold_ms: u32) -> Result<()> {
        let mut press = [0u8; 8];
        press[0] = modifier;
        for (i, &k) in keys.iter().take(6).enumerate() {
            press[2 + i] = k;
        }
        let path = self.kbd.lock().clone();
        let press_result = write_report(&path, &press);
        std::thread::sleep(std::time::Duration::from_millis(hold_ms.max(1) as u64));
        let _ = write_report(&path, &[0u8; 8]);
        press_result
    }

    // ─── Mouse ────────────────────────────────────────────────────────────

    /// Move + click in one atomic call. Always releases.
    pub fn click(&self, button_mask: u8, count: u32) -> Result<()> {
        let path = self
            .mouse
            .lock()
            .clone()
            .ok_or_else(|| anyhow!("mouse function not present in current persona"))?;
        let mut result = Ok(());
        for _ in 0..count {
            let press = [button_mask, 0, 0, 0];
            let release = [0u8; 4];
            if let Err(e) = write_report(&path, &press) {
                result = Err(e);
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
            let _ = write_report(&path, &release);
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
        result
    }

    /// Send a relative mouse delta. Boot mouse is relative-only; for
    /// absolute positioning the agent should track virtual cursor state and
    /// translate.
    pub fn move_rel(&self, dx: i8, dy: i8) -> Result<()> {
        let path = self
            .mouse
            .lock()
            .clone()
            .ok_or_else(|| anyhow!("mouse function not present"))?;
        let report = [0u8, dx as u8, dy as u8, 0];
        write_report(&path, &report)
    }

    pub fn scroll(&self, dy: i8) -> Result<()> {
        let path = self
            .mouse
            .lock()
            .clone()
            .ok_or_else(|| anyhow!("mouse function not present"))?;
        let report = [0u8, 0, 0, dy as u8];
        write_report(&path, &report)
    }

    // ─── Consumer Control (power button, media keys, …) ──────────────────

    /// Send a HID Consumer Page usage code (16-bit) and hold it for
    /// `hold_ms` before releasing. Used for the target machine's power
    /// button (usage 0x30), sleep (0x32), wake (0x83), and standard
    /// media keys (vol up/down, play/pause, etc. from HID Usage Tables
    /// §15).
    ///
    /// For the power button specifically:
    ///   - short tap  (~200ms) → OS-managed: Windows shows power menu,
    ///     macOS shows shutdown dialog, Linux usually starts shutdown.
    ///   - hard hold  (~8000ms) → forces hardware-level power off on
    ///     every modern motherboard.
    ///
    /// The release always fires even if the press write failed
    /// mid-flight — leaving a stuck consumer key is much worse than a
    /// failed power tap, because subsequent OS events would be
    /// misinterpreted.
    pub fn consumer_press(&self, usage: u16, hold_ms: u32) -> Result<()> {
        let path = self
            .consumer
            .lock()
            .clone()
            .ok_or_else(|| anyhow!("consumer function not present in current persona"))?;
        // 2-byte little-endian report matching CONSUMER_DESC.
        let press = [(usage & 0xFF) as u8, ((usage >> 8) & 0xFF) as u8];
        let release = [0u8, 0u8];
        let press_result = write_report(&path, &press);
        // Cap the hold at 30s — anything longer is almost certainly a
        // misuse and ties up the calling thread.
        let hold = hold_ms.min(30_000) as u64;
        std::thread::sleep(std::time::Duration::from_millis(hold));
        let _ = write_report(&path, &release);
        press_result
    }

    // ─── Recovery ─────────────────────────────────────────────────────────

    /// Panic button. Force-release every HID interface by writing an
    /// all-zero report to each available device. Equivalent to PiKVM's
    /// /api/hid/reset.
    pub fn release_all(&self) -> Result<()> {
        let _ = write_report(&self.kbd.lock(), &[0u8; 8]);
        if let Some(p) = self.mouse.lock().as_ref() {
            let _ = write_report(p, &[0u8; 4]);
        }
        if let Some(p) = self.consumer.lock().as_ref() {
            let _ = write_report(p, &[0u8; 2]);
        }
        Ok(())
    }
}

fn write_report(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.write_all(bytes)
        .with_context(|| format!("writing {} bytes → {}", bytes.len(), path.display()))?;
    Ok(())
}

/// Map an ASCII character to (modifier byte, HID keycode). Returns None for
/// chars outside the printable ASCII subset (the agent should sanitize
/// upstream — we don't try to handle UTF-8 here).
fn ascii_to_hid(ch: char) -> Option<(u8, u8)> {
    const MOD_SHIFT: u8 = 0x02;
    match ch {
        'a'..='z' => Some((0, 4 + (ch as u8 - b'a'))),
        'A'..='Z' => Some((MOD_SHIFT, 4 + (ch as u8 - b'A'))),
        '1'..='9' => Some((0, 30 + (ch as u8 - b'1'))),
        '0' => Some((0, 39)),
        '\n' | '\r' => Some((0, 40)),  // Enter
        '\t' => Some((0, 43)),          // Tab
        ' ' => Some((0, 44)),           // Space
        '-' => Some((0, 45)),
        '_' => Some((MOD_SHIFT, 45)),
        '=' => Some((0, 46)),
        '+' => Some((MOD_SHIFT, 46)),
        '[' => Some((0, 47)),
        '{' => Some((MOD_SHIFT, 47)),
        ']' => Some((0, 48)),
        '}' => Some((MOD_SHIFT, 48)),
        '\\' => Some((0, 49)),
        '|' => Some((MOD_SHIFT, 49)),
        ';' => Some((0, 51)),
        ':' => Some((MOD_SHIFT, 51)),
        '\'' => Some((0, 52)),
        '"' => Some((MOD_SHIFT, 52)),
        '`' => Some((0, 53)),
        '~' => Some((MOD_SHIFT, 53)),
        ',' => Some((0, 54)),
        '<' => Some((MOD_SHIFT, 54)),
        '.' => Some((0, 55)),
        '>' => Some((MOD_SHIFT, 55)),
        '/' => Some((0, 56)),
        '?' => Some((MOD_SHIFT, 56)),
        '!' => Some((MOD_SHIFT, 30)),
        '@' => Some((MOD_SHIFT, 31)),
        '#' => Some((MOD_SHIFT, 32)),
        '$' => Some((MOD_SHIFT, 33)),
        '%' => Some((MOD_SHIFT, 34)),
        '^' => Some((MOD_SHIFT, 35)),
        '&' => Some((MOD_SHIFT, 36)),
        '*' => Some((MOD_SHIFT, 37)),
        '(' => Some((MOD_SHIFT, 38)),
        ')' => Some((MOD_SHIFT, 39)),
        _ => None,
    }
}

pub fn modifier_from_name(name: &str) -> Option<u8> {
    match name.to_uppercase().as_str() {
        "CTRL" | "CONTROL" | "LCTRL" => Some(0x01),
        "SHIFT" | "LSHIFT" => Some(0x02),
        "ALT" | "OPTION" | "LALT" => Some(0x04),
        "GUI" | "CMD" | "WIN" | "COMMAND" | "META" => Some(0x08),
        "RCTRL" => Some(0x10),
        "RSHIFT" => Some(0x20),
        "RALT" => Some(0x40),
        "RGUI" | "RCMD" => Some(0x80),
        _ => None,
    }
}

pub fn keycode_from_name(name: &str) -> Option<u8> {
    match name.to_uppercase().as_str() {
        "ENTER" | "RETURN" => Some(40),
        "ESC" | "ESCAPE" => Some(41),
        "BACKSPACE" | "BSP" => Some(42),
        "TAB" => Some(43),
        "SPACE" => Some(44),
        "F1" => Some(58), "F2" => Some(59), "F3" => Some(60), "F4" => Some(61),
        "F5" => Some(62), "F6" => Some(63), "F7" => Some(64), "F8" => Some(65),
        "F9" => Some(66), "F10" => Some(67), "F11" => Some(68), "F12" => Some(69),
        "HOME" => Some(74), "PAGEUP" | "PGUP" => Some(75),
        "DELETE" | "DEL" => Some(76), "END" => Some(77),
        "PAGEDOWN" | "PGDN" => Some(78),
        "RIGHT" => Some(79), "LEFT" => Some(80), "DOWN" => Some(81), "UP" => Some(82),
        s if s.len() == 1 => {
            let ch = s.chars().next().unwrap();
            if ch.is_ascii_alphanumeric() {
                ascii_to_hid(ch.to_ascii_lowercase()).map(|(_, k)| k)
            } else {
                ascii_to_hid(ch).map(|(_, k)| k)
            }
        }
        _ => None,
    }
}
