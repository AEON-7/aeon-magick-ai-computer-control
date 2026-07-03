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

/// Read the current audio state (runs the blocking amixer calls).
fn read_state() -> Value {
    let (card, name) = match pick_card() {
        Some(c) => c,
        None => return json!({ "present": false }),
    };
    let cs = card.to_string();
    let names = scontrol_names(&cs);
    let out_ctl =
        first_present(&names, OUT_ANALOG).or_else(|| first_present(&names, OUT_FALLBACK));
    let in_ctl = first_present(&names, IN_CTRL);
    let playback = out_ctl.and_then(|c| ctl_pct(&cs, c).map(|p| json!({ "control": c, "percent": p })));
    let capture = in_ctl.and_then(|c| {
        amixer(&["-c", &cs, "sget", c]).map(|s| {
            json!({ "control": c, "percent": parse_pct(&s).unwrap_or(0), "muted": s.contains("[off]") })
        })
    });
    json!({
        "present": true,
        "card": { "index": card, "name": name },
        "playback": playback,   // null if no output control
        "capture": capture,     // null if no capture control
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
