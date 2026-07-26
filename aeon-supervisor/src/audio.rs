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
        // dsnoop: multi-open so live listen + /record A/V can share Cam Link.
        // Local mics (wm8960) stay on plughw — plug conversion is more reliable.
        let shared = format!("dsnoop:CARD={name},DEV=0");
        let plug = format!("plughw:CARD={name},DEV=0");
        if low.contains("tc358743") {
            return Some(CapturePick {
                device: shared,
                kind: "hdmi-target",
                rate: 48000,
                channels: 2,
            });
        }
        // Elgato Cam Link 4K (and similar HDMI USB capture) — target HDMI audio.
        if low.contains("c4k") || low.contains("cam link") || low.contains("elgato") {
            return Some(CapturePick {
                device: shared,
                kind: "camlink-hdmi",
                rate: 48000,
                channels: 2,
            });
        }
        if (low.contains("usb") || low.contains("uac")) && usb.is_none() {
            usb = Some(CapturePick {
                device: shared.clone(),
                kind: "usb-mic",
                rate: 48000,
                channels: 2,
            });
        }
        if (low.contains("wm8960") || low.contains("seeed")) && braincraft.is_none() {
            braincraft = Some(CapturePick {
                device: plug.clone(),
                kind: "braincraft-mic",
                rate: 16000,
                channels: 1,
            });
        }
        if other.is_none() && !low.contains("hdmi") {
            other = Some(CapturePick {
                device: shared,
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

/// Best-effort free of prior listen-session ffmpeg processes that hold ALSA
/// capture exclusive. Matches both the low-latency PCM pump and the legacy
/// MP3 pump — never the video streamer.
fn free_stale_listen_ffmpeg() {
    // Narrow patterns: full argv of our listen encoder only.
    for pat in [
        r"ffmpeg.*-f alsa.*-f s16le",
        r"ffmpeg.*-f alsa.*libmp3lame.*-f mp3",
    ] {
        let _ = Command::new("pkill").args(["-9", "-f", pat]).output();
    }
    std::thread::sleep(std::time::Duration::from_millis(80));
}

/// Codec for the shared live-audio hub.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LiveCodec {
    /// Raw little-endian signed 16-bit PCM — low latency for Web Audio.
    PcmS16le,
    /// MP3 for simple `<audio src>` / curl clients (~seconds of browser buffer).
    Mp3,
}

impl LiveCodec {
    fn from_query(q: Option<&str>) -> Self {
        match q.map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("mp3") | Some("mpeg") => Self::Mp3,
            _ => Self::PcmS16le, // default: low-latency
        }
    }
    fn content_type(self) -> &'static str {
        match self {
            Self::PcmS16le => "application/octet-stream",
            Self::Mp3 => "audio/mpeg",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::PcmS16le => "pcm_s16le",
            Self::Mp3 => "mp3",
        }
    }
}

/// One shared ffmpeg → many HTTP listeners. Cam Link ALSA is exclusive under
/// plughw; dsnoop shares. Fan-outs raw PCM (default) or MP3 chunks.
struct LiveAudioHub {
    tx: tokio::sync::broadcast::Sender<bytes::Bytes>,
    kind: &'static str,
    device: String,
    codec: LiveCodec,
    rate: u32,
    channels: u32,
    /// Generation counter so a late pump exit does not clear a newer hub.
    gen: u64,
}

static AUDIO_HUB: std::sync::OnceLock<tokio::sync::Mutex<Option<LiveAudioHub>>> =
    std::sync::OnceLock::new();
static AUDIO_HUB_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn audio_hub() -> &'static tokio::sync::Mutex<Option<LiveAudioHub>> {
    AUDIO_HUB.get_or_init(|| tokio::sync::Mutex::new(None))
}

/// Hub snapshot returned to an HTTP listener.
struct LiveSub {
    rx: tokio::sync::broadcast::Receiver<bytes::Bytes>,
    kind: &'static str,
    device: String,
    codec: LiveCodec,
    rate: u32,
    channels: u32,
}

/// Subscribe to the shared live pump, starting ffmpeg if needed.
async fn subscribe_live_audio(pick: &CapturePick, codec: LiveCodec) -> Result<LiveSub, String> {
    use tokio::io::AsyncReadExt;
    use tokio::process::Command as TokioCommand;
    use tokio::time::{timeout, Duration};

    let mut guard = audio_hub().lock().await;
    if let Some(hub) = guard.as_ref() {
        // Reuse only when device + codec match (PCM vs MP3 need different ffmpeg).
        if hub.device == pick.device && hub.codec == codec {
            return Ok(LiveSub {
                rx: hub.tx.subscribe(),
                kind: hub.kind,
                device: hub.device.clone(),
                codec: hub.codec,
                rate: hub.rate,
                channels: hub.channels,
            });
        }
        // Device/codec changed — drop the old hub (receivers lag-out; pump exits).
        *guard = None;
        free_stale_listen_ffmpeg();
    }

    free_stale_listen_ffmpeg();

    let dev = pick.device.clone();
    let rate = pick.rate;
    let ch = pick.channels;
    let rate_s = rate.to_string();
    let ch_s = ch.to_string();

    // Low-latency ALSA → PCM (default) or MP3. PCM + Web Audio is ~100–300 ms;
    // MP3 via <audio> is often multi-second because browsers buffer MPEG.
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-fflags".into(),
        "nobuffer".into(),
        "-flags".into(),
        "low_delay".into(),
        "-probesize".into(),
        "32".into(),
        "-analyzeduration".into(),
        "0".into(),
        "-thread_queue_size".into(),
        "8".into(),
        "-f".into(),
        "alsa".into(),
        "-ar".into(),
        rate_s.clone(),
        "-ac".into(),
        ch_s.clone(),
        "-i".into(),
        dev.clone(),
    ];
    match codec {
        LiveCodec::PcmS16le => {
            args.extend([
                "-c:a".into(),
                "pcm_s16le".into(),
                "-f".into(),
                "s16le".into(),
                "-".into(),
            ]);
        }
        LiveCodec::Mp3 => {
            args.extend([
                "-c:a".into(),
                "libmp3lame".into(),
                "-b:a".into(),
                "96k".into(),
                "-compression_level".into(),
                "0".into(),
                "-reservoir".into(),
                "0".into(),
                "-write_xing".into(),
                "0".into(),
                "-f".into(),
                "mp3".into(),
                "-".into(),
            ]);
        }
    }

    let mut child = TokioCommand::new("ffmpeg")
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn ffmpeg: {e}"))?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ffmpeg stdout missing".to_string())?;

    // Wait for the first audio bytes so we fail fast on busy/missing capture.
    let mut probe = [0u8; 4096];
    let n = match timeout(Duration::from_millis(2000), stdout.read(&mut probe)).await {
        Ok(Ok(0)) => {
            let err = read_ffmpeg_err(&mut child).await;
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(if err.is_empty() {
                format!(
                    "no audio from {} ({}) — capture silent or device busy",
                    pick.kind, pick.device
                )
            } else {
                format!("{err} ({})", pick.device)
            });
        }
        Err(_) => {
            let err = read_ffmpeg_err(&mut child).await;
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(if err.is_empty() {
                format!(
                    "timeout opening {} ({}) — is another process holding the mic?",
                    pick.kind, pick.device
                )
            } else {
                format!("{err} ({})", pick.device)
            });
        }
        Ok(Ok(n)) => n,
        Ok(Err(e)) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(format!("ffmpeg read: {e}"));
        }
    };

    drop(child.stderr.take()); // don't block on a full stderr pipe

    // Small broadcast depth so a slow client cannot build multi-second backlog
    // for everyone else (lagged receivers just skip).
    let (tx, rx) = tokio::sync::broadcast::channel::<bytes::Bytes>(8);
    let first = bytes::Bytes::copy_from_slice(&probe[..n]);
    let _ = tx.send(first);

    let gen = AUDIO_HUB_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let kind = pick.kind;
    let device = pick.device.clone();
    *guard = Some(LiveAudioHub {
        tx: tx.clone(),
        kind,
        device: device.clone(),
        codec,
        rate,
        channels: ch,
        gen,
    });
    drop(guard);

    // Pump: read ffmpeg → broadcast. Idle (no listeners) for ~15s tears the
    // capture down so ALSA is not held forever after mute.
    tokio::spawn(async move {
        let mut stdout = stdout;
        // ~10 ms of stereo s16 @ 48 kHz ≈ 1920 bytes; keep chunks small for low delay.
        let mut buf = [0u8; 2048];
        let mut idle_ticks: u32 = 0;
        loop {
            match timeout(Duration::from_millis(100), stdout.read(&mut buf)).await {
                Ok(Ok(0)) | Ok(Err(_)) => break,
                Err(_) => {
                    // read timeout — still check idle / generation
                }
                Ok(Ok(n)) => {
                    let _ = tx.send(bytes::Bytes::copy_from_slice(&buf[..n]));
                }
            }
            if tx.receiver_count() == 0 {
                idle_ticks += 1;
                // 100ms poll × 150 ≈ 15s with no listeners
                if idle_ticks >= 150 {
                    break;
                }
            } else {
                idle_ticks = 0;
            }
            let still = {
                let g = audio_hub().lock().await;
                g.as_ref().map(|h| h.gen == gen).unwrap_or(false)
            };
            if !still {
                break;
            }
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
        let mut g = audio_hub().lock().await;
        if g.as_ref().map(|h| h.gen == gen).unwrap_or(false) {
            *g = None;
        }
    });

    Ok(LiveSub {
        rx,
        kind,
        device,
        codec,
        rate,
        channels: ch,
    })
}

async fn read_ffmpeg_err(child: &mut tokio::process::Child) -> String {
    use tokio::io::AsyncReadExt;
    use tokio::time::{timeout, Duration};
    let Some(mut err) = child.stderr.take() else {
        return String::new();
    };
    let mut buf = vec![0u8; 1024];
    match timeout(Duration::from_millis(150), err.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => String::from_utf8_lossy(&buf[..n]).trim().to_string(),
        _ => String::new(),
    }
}

/// GET /api/audio/stream — continuous live capture audio for the operator
/// console and agents. Default is **raw s16le PCM** (low latency, for Web
/// Audio); `?codec=mp3` keeps the old MPEG stream for simple players.
///
/// Query:
/// - `codec=pcm` (default) | `mp3`
///
/// One shared ffmpeg per preferred capture device fans out to all listeners.
pub async fn stream_audio(
    State(_state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    use axum::body::Body;
    use axum::http::{header, HeaderMap, HeaderValue, StatusCode as SC};

    let codec = LiveCodec::from_query(q.get("codec").map(|s| s.as_str()));

    let Some(pick) = capture_alsa_device() else {
        return (
            SC::SERVICE_UNAVAILABLE,
            Json(json!({
                "ok": false,
                "err": "no capture sound card (need Cam Link / tc358743-audio for HDMI, or USB/BrainCraft mic)"
            })),
        )
            .into_response();
    };

    let sub = match subscribe_live_audio(&pick, codec).await {
        Ok(v) => v,
        Err(e) => {
            return (
                SC::SERVICE_UNAVAILABLE,
                Json(json!({"ok": false, "err": e, "device": pick.device, "kind": pick.kind})),
            )
                .into_response();
        }
    };

    // Async fan-out of the shared broadcast into an HTTP body stream.
    // Lagged → drop (prefer live edge over backlog — keeps A/V closer).
    let stream = futures::stream::unfold(sub.rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(b) => return Some((Ok::<_, std::io::Error>(b), rx)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(sub.codec.content_type()),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache"),
    );
    headers.insert(
        header::HeaderName::from_static("accept-ranges"),
        HeaderValue::from_static("none"),
    );
    // X-Accel / proxy: disable buffering if anything sits in front.
    headers.insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("{}; device=\"{}\"", sub.kind, sub.device)) {
        headers.insert(header::HeaderName::from_static("x-aeon-audio-source"), v);
    }
    if let Ok(v) = HeaderValue::from_str(sub.codec.label()) {
        headers.insert(header::HeaderName::from_static("x-aeon-audio-codec"), v);
    }
    if let Ok(v) = HeaderValue::from_str(&sub.rate.to_string()) {
        headers.insert(header::HeaderName::from_static("x-aeon-audio-rate"), v);
    }
    if let Ok(v) = HeaderValue::from_str(&sub.channels.to_string()) {
        headers.insert(header::HeaderName::from_static("x-aeon-audio-channels"), v);
    }

    (SC::OK, headers, Body::from_stream(stream)).into_response()
}
