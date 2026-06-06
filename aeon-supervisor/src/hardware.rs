//! Hardware introspection — the read-only backing for the GPIO / Hardware
//! dashboard (`GET /api/hardware/state`) and its MCP read tools.
//!
//! Everything here reads the LIVE Pi via tools already on the Raspberry Pi OS
//! base (no new packages): `pinctrl` / `vcgencmd` / `gpiodetect`, the device
//! tree under `/proc/device-tree`, `/sys/kernel/debug/gpio`, and `/sys`. The
//! supervisor runs as root, so it can read debugfs + the device tree directly.
//!
//! Pin CONTROL (write / PWM / I2C / SPI) and bus enablement land in a later
//! phase (needs I2C/SPI `dtparam` + `i2c-tools`, i.e. a v97 image). This module
//! is purely observational.

use crate::api::AppState;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;

// ── The Raspberry Pi 40-pin (J8) header, in physical order ──────────────────

#[derive(Clone, Copy)]
enum PinKind {
    Power3V3,
    Power5V,
    Ground,
    Gpio,
    IdEeprom,
}

impl PinKind {
    fn as_str(self) -> &'static str {
        match self {
            PinKind::Power3V3 => "3v3",
            PinKind::Power5V => "5v",
            PinKind::Ground => "gnd",
            PinKind::Gpio => "gpio",
            PinKind::IdEeprom => "id_eeprom",
        }
    }
}

struct HeaderPin {
    physical: u8,
    kind: PinKind,
    bcm: Option<u8>,
    /// ALT-function hint for the pin (I2C/SPI/UART/PWM) so the UI can flag the
    /// special buses + warn about HAT collisions. "" for plain GPIO/power/gnd.
    bus: &'static str,
}

const fn p(physical: u8, kind: PinKind, bcm: Option<u8>, bus: &'static str) -> HeaderPin {
    HeaderPin { physical, kind, bcm, bus }
}

const HEADER: &[HeaderPin] = &[
    p(1, PinKind::Power3V3, None, ""),
    p(2, PinKind::Power5V, None, ""),
    p(3, PinKind::Gpio, Some(2), "I2C1 SDA"),
    p(4, PinKind::Power5V, None, ""),
    p(5, PinKind::Gpio, Some(3), "I2C1 SCL"),
    p(6, PinKind::Ground, None, ""),
    p(7, PinKind::Gpio, Some(4), "GPCLK0"),
    p(8, PinKind::Gpio, Some(14), "UART TX"),
    p(9, PinKind::Ground, None, ""),
    p(10, PinKind::Gpio, Some(15), "UART RX"),
    p(11, PinKind::Gpio, Some(17), ""),
    p(12, PinKind::Gpio, Some(18), "PCM CLK / PWM0"),
    p(13, PinKind::Gpio, Some(27), ""),
    p(14, PinKind::Ground, None, ""),
    p(15, PinKind::Gpio, Some(22), ""),
    p(16, PinKind::Gpio, Some(23), ""),
    p(17, PinKind::Power3V3, None, ""),
    p(18, PinKind::Gpio, Some(24), ""),
    p(19, PinKind::Gpio, Some(10), "SPI0 MOSI"),
    p(20, PinKind::Ground, None, ""),
    p(21, PinKind::Gpio, Some(9), "SPI0 MISO"),
    p(22, PinKind::Gpio, Some(25), ""),
    p(23, PinKind::Gpio, Some(11), "SPI0 SCLK"),
    p(24, PinKind::Gpio, Some(8), "SPI0 CE0"),
    p(25, PinKind::Ground, None, ""),
    p(26, PinKind::Gpio, Some(7), "SPI0 CE1"),
    p(27, PinKind::IdEeprom, Some(0), "HAT ID SD"),
    p(28, PinKind::IdEeprom, Some(1), "HAT ID SC"),
    p(29, PinKind::Gpio, Some(5), ""),
    p(30, PinKind::Ground, None, ""),
    p(31, PinKind::Gpio, Some(6), ""),
    p(32, PinKind::Gpio, Some(12), "PWM0"),
    p(33, PinKind::Gpio, Some(13), "PWM1"),
    p(34, PinKind::Ground, None, ""),
    p(35, PinKind::Gpio, Some(19), "PCM FS / PWM1 / SPI1 MISO"),
    p(36, PinKind::Gpio, Some(16), "SPI1 CE0"),
    p(37, PinKind::Gpio, Some(26), ""),
    p(38, PinKind::Gpio, Some(20), "PCM DIN / SPI1 MOSI"),
    p(39, PinKind::Ground, None, ""),
    p(40, PinKind::Gpio, Some(21), "PCM DOUT / SPI1 SCLK"),
];

// ── small helpers ───────────────────────────────────────────────────────────

fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Read a device-tree property as a trimmed, NUL-stripped string (None if empty/absent).
fn dt_string(path: &str) -> Option<String> {
    std::fs::read(path).ok().and_then(|b| {
        let s = String::from_utf8_lossy(&b).trim_end_matches('\0').trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    })
}

fn glob_dev(prefix: &str) -> Vec<String> {
    std::fs::read_dir("/dev")
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    n.starts_with(prefix).then(|| format!("/dev/{n}"))
                })
                .collect()
        })
        .unwrap_or_default()
}

// ── GPIO ────────────────────────────────────────────────────────────────────

/// `pinctrl get 0-27` → BCM → (mode, pull, level, func).
fn read_pin_states() -> HashMap<u8, (String, String, String, String)> {
    let out = run("pinctrl", &["get", "0-27"]);
    let mut map = HashMap::new();
    for line in out.lines() {
        // " 0: ip    pu | hi // ID_SDA/GPIO0 = input"
        let (head, func_raw) = match line.split_once("//") {
            Some((h, f)) => (h, f.trim()),
            None => continue,
        };
        let (num, rest) = match head.split_once(':') {
            Some((n, r)) => (n.trim(), r.trim()),
            None => continue,
        };
        let bcm: u8 = match num.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let mut halves = rest.split('|');
        let left: Vec<&str> = halves.next().unwrap_or("").split_whitespace().collect();
        let mode = left.first().copied().unwrap_or("").to_string();
        let pull = left.get(1).copied().unwrap_or("").to_string();
        let level = halves.next().unwrap_or("").trim().to_string();
        // func_raw = "GPIO14 = TXD0" → take the right of '='
        let func = func_raw
            .split_once('=')
            .map(|(_, f)| f.trim())
            .unwrap_or(func_raw)
            .to_string();
        map.insert(bcm, (mode, pull, level, func));
    }
    map
}

/// `/sys/kernel/debug/gpio` → BCM → consumer label (what claimed the line).
fn read_consumers() -> HashMap<u8, String> {
    let mut map = HashMap::new();
    let text = std::fs::read_to_string("/sys/kernel/debug/gpio").unwrap_or_default();
    // gpiochip0 base, e.g. header line "gpiochip0: GPIOs 512-569, ..."
    let base: i64 = text
        .lines()
        .find(|l| l.trim_start().starts_with("gpiochip0:"))
        .and_then(|l| l.split("GPIOs ").nth(1))
        .and_then(|s| s.split('-').next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(512);
    for line in text.lines() {
        let l = line.trim();
        let Some(rest) = l.strip_prefix("gpio-") else { continue };
        let n: i64 = match rest.split(|c: char| !c.is_ascii_digit()).next().and_then(|s| s.parse().ok()) {
            Some(n) => n,
            None => continue,
        };
        let bcm = n - base;
        if !(0..=27).contains(&bcm) {
            continue;
        }
        // " gpio-554 (STATUS_LED_G_CLK |ACT) out lo" → consumer = "ACT"
        if let Some(open) = l.find('(') {
            if let Some(rel_close) = l[open..].find(')') {
                let inner = &l[open + 1..open + rel_close];
                if let Some((_, cons)) = inner.split_once('|') {
                    let cons = cons.trim();
                    if !cons.is_empty() {
                        map.insert(bcm as u8, cons.to_string());
                    }
                }
            }
        }
    }
    map
}

fn gpio_section() -> Value {
    let states = read_pin_states();
    let consumers = read_consumers();
    let pins: Vec<Value> = HEADER
        .iter()
        .map(|hp| {
            let mut v = json!({
                "physical": hp.physical,
                "kind": hp.kind.as_str(),
                "bus": hp.bus,
            });
            match hp.bcm {
                Some(bcm) => {
                    v["bcm"] = json!(bcm);
                    v["name"] = json!(format!("GPIO{bcm}"));
                    if let Some((mode, pull, level, func)) = states.get(&bcm) {
                        let dir = match mode.as_str() {
                            "ip" => "input",
                            "op" => "output",
                            m if m.starts_with('a') => "alt",
                            other => other,
                        };
                        v["mode"] = json!(mode);
                        v["direction"] = json!(dir);
                        v["pull"] = json!(match pull.as_str() {
                            "pu" => "up",
                            "pd" => "down",
                            "pn" => "none",
                            x => x,
                        });
                        v["level"] = json!(if level == "hi" { 1 } else { 0 });
                        v["function"] = json!(func);
                        // "active" = something other than a plain default input,
                        // OR a driver has claimed the line.
                        v["active"] = json!(dir != "input" || consumers.contains_key(&bcm));
                    }
                    if let Some(c) = consumers.get(&bcm) {
                        v["consumer"] = json!(c);
                    }
                }
                None => {
                    v["name"] = json!(match hp.kind {
                        PinKind::Power3V3 => "3V3",
                        PinKind::Power5V => "5V",
                        PinKind::Ground => "GND",
                        _ => "",
                    });
                }
            }
            v
        })
        .collect();
    json!({ "header": "40-pin J8", "pins": pins })
}

// ── power / thermal ─────────────────────────────────────────────────────────

fn num_after_eq(s: &str) -> String {
    s.trim()
        .rsplit('=')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.')
        .to_string()
}

fn power_section() -> Value {
    let throttled = run("vcgencmd", &["get_throttled"]); // "throttled=0x0"
    let bits = throttled
        .trim()
        .rsplit('=')
        .next()
        .map(|h| h.trim().trim_start_matches("0x"))
        .and_then(|h| u64::from_str_radix(h, 16).ok())
        .unwrap_or(0);
    let volts = run("vcgencmd", &["measure_volts", "core"]); // "volt=0.8688V"
    let temp = run("vcgencmd", &["measure_temp"]); // "temp=33.1'C"
    json!({
        "throttled_raw": format!("0x{bits:x}"),
        "healthy": bits == 0,
        "undervolt_now": bits & 0x1 != 0,
        "freq_capped_now": bits & 0x2 != 0,
        "throttled_now": bits & 0x4 != 0,
        "soft_temp_limit_now": bits & 0x8 != 0,
        "undervolt_occurred": bits & 0x1_0000 != 0,
        "throttle_occurred": bits & 0x4_0000 != 0,
        "core_volts": num_after_eq(&volts),
        "temp_c": num_after_eq(&temp),
        // Per-rail current (pmic_read_adc) is Pi 5 only — absent on this Pi 4.
        "per_rail_current": false,
    })
}

// ── HAT ─────────────────────────────────────────────────────────────────────

fn hat_section() -> Value {
    if !std::path::Path::new("/proc/device-tree/hat").exists() {
        return json!({ "present": false });
    }
    json!({
        "present": true,
        "vendor": dt_string("/proc/device-tree/hat/vendor"),
        "product": dt_string("/proc/device-tree/hat/product"),
        "product_id": dt_string("/proc/device-tree/hat/product_id"),
        "product_ver": dt_string("/proc/device-tree/hat/product_ver"),
        "uuid": dt_string("/proc/device-tree/hat/uuid"),
    })
}

// ── IO (network + USB + serial) ─────────────────────────────────────────────

fn iface_type(n: &str) -> &'static str {
    if n.starts_with("eth") {
        "ethernet"
    } else if n.starts_with("wlan") {
        "wifi"
    } else if n.starts_with("usb") {
        "usb-gadget"
    } else if n.starts_with("tun") || n.starts_with("wg") {
        "vpn"
    } else if n.starts_with("tailscale") {
        "tailnet"
    } else {
        "other"
    }
}

fn io_section() -> Value {
    let mut ifaces = vec![];
    if let Ok(rd) = std::fs::read_dir("/sys/class/net") {
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "lo")
            .collect();
        names.sort();
        for name in names {
            let base = format!("/sys/class/net/{name}");
            let read = |f: &str| {
                std::fs::read_to_string(format!("{base}/{f}"))
                    .ok()
                    .map(|s| s.trim().to_string())
            };
            ifaces.push(json!({
                "name": name,
                "type": iface_type(&name),
                "operstate": read("operstate"),
                "carrier": read("carrier").map(|c| c == "1"),
                "speed_mbps": read("speed").and_then(|s| s.parse::<i64>().ok()),
                "mac": read("address"),
            }));
        }
    }
    // USB devices (drop the root hubs).
    let usb: Vec<Value> = run("lsusb", &[])
        .lines()
        .filter_map(|l| {
            let pos = l.find(" ID ")?;
            let rest = &l[pos + 4..];
            let (id, name) = rest.split_once(' ').unwrap_or((rest, ""));
            if id.starts_with("1d6b:") {
                return None; // Linux Foundation root hubs
            }
            Some(json!({ "id": id, "name": name.trim() }))
        })
        .collect();
    let serial = ["/dev/serial0", "/dev/serial1", "/dev/ttyAMA0", "/dev/ttyUSB0"]
        .iter()
        .filter(|p| std::path::Path::new(p).exists())
        .map(|p| json!(p))
        .collect::<Vec<_>>();
    json!({ "interfaces": ifaces, "usb": usb, "serial": serial })
}

// ── buses ───────────────────────────────────────────────────────────────────

fn buses_section() -> Value {
    let i2c = glob_dev("i2c-");
    let spi = glob_dev("spidev");
    let gpiochips = run("gpiodetect", &[])
        .lines()
        .filter(|l| l.contains("gpiochip"))
        .count();
    json!({
        "i2c": { "enabled": !i2c.is_empty(), "devices": i2c },
        "spi": { "enabled": !spi.is_empty(), "devices": spi },
        "uart": std::path::Path::new("/dev/serial0").exists()
            || std::path::Path::new("/dev/ttyAMA0").exists(),
        "gpiochips": gpiochips,
        // Enabling I2C/SPI needs config.txt dtparam + a reboot (v97 image work).
        "note": "I2C/SPI buses are toggled via config.txt dtparam (needs a reboot).",
    })
}

// ── camera ──────────────────────────────────────────────────────────────────

fn camera_section() -> Value {
    let cam = run("vcgencmd", &["get_camera"]); // "supported=0 detected=0, libcamera interfaces=0"
    let field = |k: &str| -> i64 {
        cam.split_whitespace()
            .find_map(|t| {
                t.strip_prefix(k)
                    .and_then(|v| v.trim_start_matches('=').trim_end_matches(',').parse::<i64>().ok())
            })
            .unwrap_or(0)
    };
    json!({
        "csi": {
            "supported": field("supported") == 1,
            "detected": field("detected") == 1,
            "libcamera_interfaces": field("libcamera"),
        },
        "video_devices": glob_dev("video"),
        "note": "The live feed is the USB HDMI-capture device (configure under the streamer). CSI is for an optional Pi camera module on the ribbon.",
    })
}

// ── handler ─────────────────────────────────────────────────────────────────

/// GET /api/hardware/state — a full snapshot of the board's hardware:
/// model, attached HAT, power/thermal, the 40-pin GPIO header (live), IO
/// (net/USB/serial), buses, and camera. Read-scope (GET).
pub async fn hardware_state(State(_state): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| {
        json!({
            "ok": true,
            "model": dt_string("/proc/device-tree/model"),
            "hat": hat_section(),
            "power": power_section(),
            "gpio": gpio_section(),
            "io": io_section(),
            "buses": buses_section(),
            "camera": camera_section(),
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "introspection task failed"}));
    Json(v)
}
