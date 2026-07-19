//! GET/PUT /api/audio/volume — playback (speaker/headphone) + capture (mic)
//! levels via `amixer`, targeting the BrainCraft WM8960 codec (or any ALSA card
//! that exposes pvolume/cvolume controls). Returns `{"present": false}` when the
//! only sound cards are control-less ones (e.g. the Pi's vc4-hdmi outputs), so
//! the UI hides the sliders on a mains-only / no-codec Orb.
//!
//! The WM8960 has several gain stages; for a single intuitive "output" slider we
//! drive the analog `Speaker`/`Headphone` controls and force the digital
//! `Playback`/`PCM` path fully open behind them so the slider value maps to what
//! you actually hear. "Input" drives the `Capture` PGA.

use crate::api::AppState;
use axum::{extract::State, response::IntoResponse, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Command;

// Preference order. First control present on the card wins.
const OUT_ANALOG: &[&str] = &["Speaker", "Headphone"]; // the slider drives these
const OUT_FALLBACK: &[&str] = &["Master", "PCM", "Playback"]; // USB cards: one combined
const OUT_DIGITAL: &[&str] = &["Playback", "PCM"]; // path-opener behind the analog stage
// DAPM routing switches that connect the DAC into the analog output mixer.
// On the WM8960 these default OFF, so the DAC never reaches Speaker/Headphone
// even with the analog volumes up and the digital `Playback` open — the
// classic "every level looks set but it's dead silent" trap. Flip them on
// (all present ones) whenever we drive the analog outputs. Absent on USB
// cards, so the name-match guard simply skips them there.
const OUT_ROUTE: &[&str] = &["Left Output Mixer PCM", "Right Output Mixer PCM"];
const IN_CTRL: &[&str] = &["Capture", "Mic", "Mic Boost"];
// WM8960 mic input path behind the `Capture` PGA. The input-boost-mixer GAIN
// (`… Input Boost Mixer …`) defaults to 0 = muted, so the ADC reads pure
// silence even with `Capture` up and the connect switches on; the connect
// switches (`… Input Mixer Boost`, `… Boost Mixer LINPUTn`) route LINPUT1/
// RINPUT1 in. We open all present ones when the capture level is set.
const IN_ROUTE_GAIN: &[&str] =
    &["Left Input Boost Mixer LINPUT1", "Right Input Boost Mixer RINPUT1"];
const IN_ROUTE_SW: &[&str] = &[
    "Left Input Mixer Boost",
    "Right Input Mixer Boost",
    "Left Boost Mixer LINPUT1",
    "Right Boost Mixer RINPUT1",
];

fn amixer(args: &[&str]) -> Option<String> {
    let out = Command::new("amixer").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Pick the ALSA card index that actually has volume controls — prefer a named
/// codec (wm8960 / USB-Audio) over the HDMI cards (which expose none).
fn pick_card() -> Option<(i32, String)> {
    let cards = std::fs::read_to_string("/proc/asound/cards").ok()?;
    let mut fallback: Option<(i32, String)> = None;
    for line in cards.lines() {
        // " 0 [wm8960soundcard]: simple-card - wm8960-soundcard"
        let idx: i32 = match line.trim_start().split_whitespace().next().and_then(|s| s.parse().ok()) {
            Some(i) => i,
            None => continue,
        };
        let name = line
            .split('[')
            .nth(1)
            .and_then(|s| s.split(']').next())
            .unwrap_or("")
            .trim()
            .to_string();
        let cs = idx.to_string();
        let has_controls = amixer(&["-c", &cs, "scontrols"])
            .map(|s| s.contains("Simple mixer control"))
            .unwrap_or(false);
        if !has_controls {
            continue;
        }
        let low = name.to_lowercase();
        if low.contains("hdmi") || low.contains("vc4") {
            fallback.get_or_insert((idx, name)); // last resort
        } else {
            return Some((idx, name)); // real codec — prefer it
        }
    }
    fallback
}

/// The simple-control names present on a card.
fn scontrol_names(card: &str) -> Vec<String> {
    amixer(&["-c", card, "scontrols"])
        .map(|s| {
            s.lines()
                .filter_map(|l| l.split('\'').nth(1).map(|n| n.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn first_present(names: &[String], prefs: &[&'static str]) -> Option<&'static str> {
    prefs.iter().copied().find(|p| names.iter().any(|n| n == p))
}

/// First `[NN%]` in an `amixer sget` dump.
fn parse_pct(sget: &str) -> Option<i64> {
    let mut from = 0;
    while let Some(rel) = sget[from..].find('[') {
        let start = from + rel + 1;
        if let Some(p) = sget[start..].find('%') {
            if let Ok(v) = sget[start..start + p].trim().parse::<i64>() {
                return Some(v);
            }
        }
        from = start;
    }
    None
}

fn ctl_pct(card: &str, ctl: &str) -> Option<i64> {
    amixer(&["-c", card, "sget", ctl]).and_then(|s| parse_pct(&s))
}

/// All ALSA cards with a coarse role tag (codec / hdmi / other).
fn list_cards() -> Vec<Value> {
    let text = match std::fs::read_to_string("/proc/asound/cards") {
        Ok(t) => t,
        Err(_) => return vec![],
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let idx: i32 = match line
            .trim_start()
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
        {
            Some(i) => i,
            None => continue,
        };
        let name = line
            .split('[')
            .nth(1)
            .and_then(|s| s.split(']').next())
            .unwrap_or("")
            .trim()
            .to_string();
        let low = name.to_lowercase();
        let role = if low.contains("wm8960") {
            "braincraft"
        } else if low.contains("usb") {
            "usb"
        } else if low.contains("hdmi") || low.contains("vc4") {
            "hdmi-out" // playback only — never use for mic
        } else {
            "other"
        };
        out.push(json!({
            "index": idx,
            "name": name,
            "role": role,
            "plughw": format!("plughw:CARD={name},DEV=0"),
        }));
    }
    out
}

/// Read the current audio state (runs the blocking amixer calls).
fn read_state() -> Value {
    let (card, name) = match pick_card() {
        Some(c) => c,
        None => {
            return json!({
                "present": false,
                "devices": list_cards(),
                "note": "no codec card with volume controls (BrainCraft WM8960 / USB)",
            });
        }
    };
    let cs = card.to_string();
    let names = scontrol_names(&cs);
    let out_ctl =
        first_present(&names, OUT_ANALOG).or_else(|| first_present(&names, OUT_FALLBACK));
    let in_ctl = first_present(&names, IN_CTRL);
    let speaker = if names.iter().any(|n| n == "Speaker") {
        ctl_pct(&cs, "Speaker").map(|p| json!({ "control": "Speaker", "percent": p }))
    } else {
        None
    };
    let headphone = if names.iter().any(|n| n == "Headphone") {
        ctl_pct(&cs, "Headphone").map(|p| json!({ "control": "Headphone", "percent": p }))
    } else {
        None
    };
    let playback = out_ctl.and_then(|c| ctl_pct(&cs, c).map(|p| json!({ "control": c, "percent": p })));
    let capture = in_ctl.and_then(|c| {
        amixer(&["-c", &cs, "sget", c]).map(|s| {
            json!({ "control": c, "percent": parse_pct(&s).unwrap_or(0), "muted": s.contains("[off]") })
        })
    });
    // Canonical device strings — match /etc/asound.conf + braincraft probe_audio.
    let plughw = format!("plughw:CARD={name},DEV=0");
    let pcm_default = if name.to_lowercase().contains("wm8960") && std::path::Path::new("/etc/asound.conf").exists() {
        "aeon"
    } else {
        plughw.as_str()
    };
    json!({
        "present": true,
        "card": { "index": card, "name": name },
        "playback": playback,   // primary slider (Speaker preferred)
        "speaker": speaker,
        "headphone": headphone,
        "capture": capture,     // null if no capture control
        // How apps should open the card (never use bare hw:N for mono/16k).
        "devices": {
            "playback": pcm_default,
            "capture": pcm_default,
            "plughw": plughw,
            "named": if name.to_lowercase().contains("wm8960") {
                json!(["aeon", "aeon_play", "aeon_cap"])
            } else {
                json!([])
            },
            "cards": list_cards(),
        },
        "mapping": {
            "speakers": "WM8960 SPK_L/SPK_R (class-D JST)",
            "headphone": "WM8960 HP jack",
            "mics": "WM8960 stereo electrets (LINPUT1/RINPUT1 + MICBIAS)",
            "hdmi": "vc4-hdmi is output-only — not used for capture",
        },
    })
}

pub async fn get_audio(State(_state): State<AppState>) -> impl IntoResponse {
    let v = tokio::task::spawn_blocking(read_state)
        .await
        .unwrap_or_else(|_| json!({ "present": false }));
    Json(v).into_response()
}

#[derive(Deserialize)]
pub struct VolPatch {
    /// Output (speaker/headphone) volume 0–100.
    pub playback: Option<i64>,
    /// Input (mic) volume 0–100.
    pub capture: Option<i64>,
    /// Mute/unmute the mic capture.
    pub capture_muted: Option<bool>,
}

fn apply(p: &VolPatch) {
    let (card, _name) = match pick_card() {
        Some(c) => c,
        None => return,
    };
    let cs = card.to_string();
    let names = scontrol_names(&cs);

    if let Some(v) = p.playback {
        let pv = format!("{}%", v.clamp(0, 100));
        let mut set_analog = false;
        for c in OUT_ANALOG {
            if names.iter().any(|n| n == c) {
                amixer(&["-c", &cs, "sset", c, &pv]);
                set_analog = true;
            }
        }
        if set_analog {
            // Open the digital DAC path so the analog slider is audible.
            for c in OUT_DIGITAL {
                if names.iter().any(|n| n == c) {
                    amixer(&["-c", &cs, "sset", c, "100%"]);
                }
            }
            // Connect the DAC into the analog output mixer (DAPM switch,
            // defaults off — without it the slider above moves a dead net).
            for c in OUT_ROUTE {
                if names.iter().any(|n| n == c) {
                    amixer(&["-c", &cs, "sset", c, "on"]);
                }
            }
        } else if let Some(c) = first_present(&names, OUT_FALLBACK) {
            amixer(&["-c", &cs, "sset", c, &pv]);
        }
    }
    if let Some(v) = p.capture {
        if let Some(c) = first_present(&names, IN_CTRL) {
            amixer(&["-c", &cs, "sset", c, &format!("{}%", v.clamp(0, 100)), "cap"]);
            // Open the mic path behind the PGA: un-mute the input-boost-mixer
            // gain (0 by default) and close the LINPUT/RINPUT connect switches.
            for c in IN_ROUTE_GAIN {
                if names.iter().any(|n| n == c) {
                    amixer(&["-c", &cs, "sset", c, "100%"]);
                }
            }
            for c in IN_ROUTE_SW {
                if names.iter().any(|n| n == c) {
                    amixer(&["-c", &cs, "sset", c, "on"]);
                }
            }
        }
    }
    if let Some(m) = p.capture_muted {
        if let Some(c) = first_present(&names, IN_CTRL) {
            amixer(&["-c", &cs, "sset", c, if m { "nocap" } else { "cap" }]);
        }
    }
    // Persist so the levels survive reboot (alsa-restore.service).
    Command::new("alsactl").arg("store").output().ok();
}

pub async fn put_audio(
    State(_state): State<AppState>,
    Json(patch): Json<VolPatch>,
) -> impl IntoResponse {
    let v = tokio::task::spawn_blocking(move || {
        apply(&patch);
        read_state()
    })
    .await
    .unwrap_or_else(|_| json!({ "present": false }));
    Json(v).into_response()
}

/// Resolved capture source for live web passthrough.
#[derive(Clone, Debug)]
struct CapturePick {
    /// ALSA device string (prefer `plughw:` so format/rate convert works).
    device: String,
    /// Human label for UI / `X-Aeon-Audio-Source` header.
    kind: &'static str,
    /// ffmpeg input sample rate hint (plughw still converts).
    rate: u32,
    channels: u32,
}

/// Resolve an ALSA capture device for live web passthrough.
///
/// Preference (KVM-first Orb):
/// 1. **tc358743** — HDMI audio from the target (X1301 I2S; needs
///    `dtoverlay=tc358743-audio` and an EDID that advertises audio).
/// 2. USB capture mic
/// 3. BrainCraft WM8960 / seeed (local mics — same I2S pins as HDMI audio;
///    usually not co-present with tc358743-audio)
///
/// Skips Pi onboard HDMI *outputs* (`vc4-hdmi*`).
fn capture_alsa_device() -> Option<CapturePick> {
    let cards = std::fs::read_to_string("/proc/asound/cards").ok()?;
    let mut usb: Option<CapturePick> = None;
    let mut braincraft: Option<CapturePick> = None;
    let mut other: Option<CapturePick> = None;

    for line in cards.lines() {
        let name = line
            .split('[')
            .nth(1)
            .and_then(|s| s.split(']').next())
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() {
            continue;
        }
        let low = name.to_lowercase();
        // Pi HDMI *outputs* (speakers on a monitor) — not capture sources.
        if low.contains("vc4") {
            continue;
        }
        let dev = format!("plughw:CARD={name},DEV=0");
        if low.contains("tc358743") {
            return Some(CapturePick {
                device: dev,
                kind: "hdmi-target",
                rate: 48000,
                channels: 2,
            });
        }
        // Elgato Cam Link 4K (and similar HDMI USB capture) — target HDMI audio.
        if low.contains("c4k") || low.contains("cam link") || low.contains("elgato") {
            return Some(CapturePick {
                device: dev,
                kind: "camlink-hdmi",
                rate: 48000,
                channels: 2,
            });
        }
        if (low.contains("usb") || low.contains("uac")) && usb.is_none() {
            usb = Some(CapturePick {
                device: dev.clone(),
                kind: "usb-mic",
                rate: 48000,
                channels: 2,
            });
        }
        if (low.contains("wm8960") || low.contains("seeed")) && braincraft.is_none() {
            braincraft = Some(CapturePick {
                device: dev.clone(),
                kind: "braincraft-mic",
                rate: 16000,
                channels: 1,
            });
        }
        if other.is_none() && !low.contains("hdmi") {
            other = Some(CapturePick {
                device: dev,
                kind: "alsa-capture",
                rate: 48000,
                channels: 2,
            });
        }
    }
    // Prefer /etc/asound.conf `aeon` only when it maps to a real capture card we know.
    if std::path::Path::new("/etc/asound.conf").exists() {
        if cards.to_ascii_lowercase().contains("tc358743") {
            return Some(CapturePick {
                device: "aeon".into(),
                kind: "hdmi-target",
                rate: 48000,
                channels: 2,
            });
        }
    }
    usb.or(braincraft).or(other)
}

/// GET /api/audio/stream — continuous low-latency MP3 for the operator console
/// and agents: **HDMI target audio** when `tc358743` is present, otherwise USB /
/// BrainCraft mic. The web `<audio>` element plays this beside the video stream.
///
/// Implementation: `ffmpeg -f alsa -i … -f mp3 -` piped as an HTTP body.
pub async fn stream_audio(State(_state): State<AppState>) -> impl IntoResponse {
    use axum::body::Body;
    use axum::http::{header, HeaderMap, HeaderValue, StatusCode as SC};
    use tokio::process::Command as TokioCommand;
    use tokio_util::io::ReaderStream;

    let Some(pick) = capture_alsa_device() else {
        return (
            SC::SERVICE_UNAVAILABLE,
            Json(json!({
                "ok": false,
                "err": "no capture sound card (need tc358743-audio for HDMI, or USB/BrainCraft mic)"
            })),
        )
            .into_response();
    };
    let dev = pick.device.clone();
    let rate = pick.rate.to_string();
    let ch = pick.channels.to_string();

    let mut child = match TokioCommand::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "alsa",
            "-ar",
            &rate,
            "-ac",
            &ch,
            "-i",
            &dev,
            "-c:a",
            "libmp3lame",
            "-b:a",
            "128k",
            "-f",
            "mp3",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                SC::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": format!("spawn ffmpeg: {e}")})),
            )
                .into_response();
        }
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill().await;
            return (
                SC::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": "ffmpeg stdout missing"})),
            )
                .into_response();
        }
    };

    // Keep the child alive for the life of the stream by moving it into a
    // background task that waits until the pipe ends (client disconnect or
    // ffmpeg exit). kill_on_drop on the child is NOT enough once we move
    // stdout out — so we explicitly kill when the wait finishes.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });

    let stream = ReaderStream::new(stdout);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("audio/mpeg"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache"),
    );
    // Expose source kind so the UI can show "HDMI target" vs "BrainCraft mic".
    if let Ok(v) = HeaderValue::from_str(&format!(
        "{}; device=\"{}\"",
        pick.kind, pick.device
    )) {
        headers.insert(header::HeaderName::from_static("x-aeon-audio-source"), v);
    }

    (SC::OK, headers, Body::from_stream(stream)).into_response()
}
