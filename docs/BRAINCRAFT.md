# BrainCraft HAT — optional enhancement (AI face · viewfinder · voice)

The Aeon Magick Orb runs fully headless without any HAT. As an **optional
enhancement**, an **Adafruit BrainCraft HAT** gives an Orb a small face and a
voice: a 240×240 ST7789 TFT, three buttons + a joystick, and (with a speaker/mic)
a local-or-hosted voice assistant. It's managed from the **BrainCraft** super-app
tile in the web console and is **off by default** — the `aeon-braincraft` daemon
self-idles when the feature is disabled or no HAT is present, so the image is
harmless on a bare Pi.

> Unlike the Hailo AI HAT, the BrainCraft is **not Pi-5-only** — it ships in
> **both** the `aeon-magick-pi4` and `aeon-magick-pi5` releases. The daemon
> auto-detects the GPIO controller (Pi 5 → `gpiochip4` via RP1, Pi 4 →
> `gpiochip0`) and the audio device. The AI-detection viewfinder overlay is
> richer on the Pi 5 (where the Hailo/vision feed lives), but the core modes work
> on either.

## Full on-HAT menu (joystick + button)

The BrainCraft has **one face button** (BCM 17) and a **5-way joystick**
(select=16, left=22, up=23, right=24, down=27). **Joystick press** opens the
mode menu; **↑↓** navigate; **button** (or right) selects; **left** backs out.
You can also set the mode from the web console **BrainCraft** tile.

| Mode | What it does | Controls |
|---|---|---|
| **AI face** *(default)* | Idle "face": backend / persona, listen/speak state, last reply | Button opens menu |
| **Camera capture** | Live preview from the streamer snapshot socket (never opens CSI twice) | **Button** = photo · **←→** = start/stop record · **↑** = toggle overlay |
| **AI video analysis** | Same preview with vision/OCR boxes always on + caption strip | **Button** = photo |

### Display orientation (side-held HAT)

The whole TFT (menus, AI face, volume, camera) uses ST7789 `display_rotation`
in `/etc/aeon/braincraft.toml` — default **`270`** (90° left of the old
button-edge-down `180` layout). Restart `aeon-braincraft` after changing it:

```toml
display_rotation = 270   # 0 | 90 | 180 | 270  (CCW)
viewfinder_rotate = 0    # extra camera-only rotate; leave 0 unless CSI is still wrong
```

The joystick **d-pad is remapped** to the same `display_rotation`, so ↑ / ↓ / ← / →
always match the on-screen menu (joystick press and the face button stay fixed).
| **AI voice chat** | Push-to-talk: mic → ASR → LLM → TTS → speaker | **Button** = PTT |
| **Volume** | Speaker + mic levels via `amixer` (WM8960) | **↑↓** speaker · **←→** mic |

Captures land under `/var/lib/aeon/captures/`. Volume changes are persisted with
`alsactl store` and match the web console `/api/audio/volume` sliders.

> **libgpiod v2:** the daemon uses `request_lines` (Bookworm/Trixie). Older
> `get_line` / `LINE_REQ_*` code left buttons dead (`buttons.ok: false`).

## Local ↔ hosted voice (personas)

The voice backend is selectable in the BrainCraft tile:

| Backend | ASR | LLM | TTS | Use |
|---|---|---|---|---|
| **Local** | Vosk (small) | on-device (Hailo `hailo-ollama` :8000 if present, else CPU) | Piper | Fully offline; no network |
| **Hosted** | `asr_url` (qwen3-asr) | `llm_url` (vLLM, OpenAI-compatible) | `tts_url` (qwen3-tts) | Calls a **DGX Spark** or other host with a chosen **persona** voice |

Hosted mode turns the Orb into a voice endpoint for any of your personas — set the
`persona` field and point the three endpoints at the host (defaults target the
home-lab DGX Spark at `192.168.1.116`). Switch back to local at any time.

## Stacking with the Hailo AI HAT+ — no pin conflict

The BrainCraft and the **Raspberry Pi AI HAT+** (Hailo) **coexist with no GPIO
rerouting**. The Hailo board talks over the **PCIe FFC ribbon**, not the 40-pin
header — from the header it uses only power/ground plus its ID EEPROM:

| | AI HAT+ (Hailo) | BrainCraft HAT |
|---|---|---|
| Data path | PCIe FFC ribbon (off-GPIO) | SPI + I²S + I²C + GPIO on the header |
| Header pins | 2/4 (5V) · 6/9 (GND) · 27/28 (ID EEPROM) | SPI 19/23/24 · I²S 12/35/38/40 · I²C 3/5 · GPIO buttons/joystick/DotStar/fan |
| Overlap | — | only 2/4/6/9 (shared power + ground rails) |

BrainCraft never touches pins **27/28**, so even the HAT auto-ID stays clean. The
only real constraint is **mechanical**: the Hailo heatsink is tall, so when
stacked the BrainCraft must sit on a **~16 mm header + standoffs** to clear it
(AI HAT+ on the bottom with its ribbon to the Pi 5's PCIe port, BrainCraft up top).

## Audio: USB by default, WM8960 experimental

The BrainCraft's **WM8960 I²S codec is unverified on the Pi 5 (RP1)** — RP1 exposes
I²S differently than older Pis. The daemon therefore defaults to **USB audio**
(`device = "auto"` prefers a USB sound device for both playback and capture). Set
`device = "wm8960"` to attempt the on-board codec, or pin a specific ALSA device
by name. A plain USB speakerphone is the supported, no-surprises path.

## Installing the voice/display stack (button-driven)

The heavy bits — **Piper TTS**, **Vosk ASR**, and the display/audio/GPIO Python
libraries (~100–150 MB) — are **not baked** into the image. Open the BrainCraft
tile → **Install voice stack**; `aeon-voice` installs them on demand and streams
progress (the same non-blocking, resume-on-return pattern as the Hailo runtime
install). Until then the daemon idles and the tile offers the install button.

## Config reference — `/etc/aeon/braincraft.toml`

The daemon re-reads this every loop, so a change from the web console (or a hand
edit) takes effect within a tick — no restart needed.

```toml
enabled = false        # master switch; daemon idles until true
mode    = "ai"         # ai | viewfinder | voice
overlay = true         # draw vision.json detections in viewfinder

[audio]
device = "auto"        # auto | usb | wm8960 | <alsa device name>
tts    = "piper"
asr    = "vosk"

[backend]
mode    = "local"      # local | hosted
persona = ""           # hosted persona voice (qwen3-tts / gateway persona)

[hosted]
llm_url = "http://192.168.1.116:8000/v1"   # vLLM (OpenAI-compatible)
tts_url = "http://192.168.1.116:8002"      # qwen3-tts
asr_url = "http://192.168.1.116:8001"      # qwen3-asr
```

## Release tracks

Shipped on **both** tracks (`aeon-magick-pi4` and `aeon-magick-pi5`). See
`docs/HAILO.md` → "Release tracks" and `AGENTS.md` → "Rebuilding the image" for
the `AEON_TARGET=pi5|pi4` bake split (the BrainCraft install is *not* gated on it).
