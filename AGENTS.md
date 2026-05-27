# AGENTS.md — Setting up & using AEON Magick AI Computer Control

> _The Universe's Strangest Peripheral. Plug it in, give it eyes, give it
> hands, hand the keys to an AI._

This document is the read-this-first guide for both **humans setting up the
device** and **AI agents that will be operating it**. Two audiences, one
file, because the operations are the same — only the verbs differ.

If you came here expecting "how do I build this from source," look at
[`BUILDING.md`](./BUILDING.md). If you came here for the design rationale,
look at [`ARCHITECTURE.md`](./ARCHITECTURE.md).

---

## TL;DR for the impatient

1. Flash `aeon-magick.img.xz` to an SD card (Raspberry Pi Imager — no
   customization). Don't decompress first; Imager handles `.xz` natively.
2. Put the SD into a Raspberry Pi 4.
3. Plug the Pi's **USB-C** into the target computer (this is where you
   control). Plug an **Elgato Cam Link 4K** (or MS2109 dongle) into any
   USB-3 port on the Pi. Run the target's HDMI out into the Cam Link.
4. Pi boots, generates a random admin password, writes it to
   `aeon-credentials.txt` on the SD card's boot partition.
5. Once the Pi joins WiFi (or you connect to its `aeon-setup` AP for
   first-time WiFi config), open `https://aeon-magick.local/` and log in
   as `admin` with that password.
6. You're in. Web UI streams the target's screen and accepts keyboard +
   mouse input. AI agents talk to the same surface via the REST API.

---

## Hardware you need

| Piece | Notes |
|---|---|
| **Raspberry Pi 4** | 2 GB RAM minimum; 4 GB recommended. Pi 5 also works but H.264 falls back to software. |
| **Power supply** | Official 27 W USB-C PSU. Cam Link 4K is power-hungry; weaker PSUs cause USB drops. |
| **MicroSD card** | 16 GB+ class 10. Larger if you plan to keep frame history. |
| **HDMI capture** | Elgato Cam Link 4K (recommended), or a generic MS2109-chipset USB capture stick (~$15, common on Amazon). Both auto-detected via udev. |
| **HDMI cable** | From the target's HDMI out (or USB-C-to-HDMI adapter for laptops without HDMI) into the capture device. |
| **USB-C data cable** | Pi USB-C → target USB-C/Thunderbolt. **Must carry data** — most charging-only cables won't work. |
| **Ethernet (optional but recommended for first boot)** | Skips the AP-fallback dance for first-time setup. |

The Pi is the box that pretends to be USB peripherals. The target is the
computer you want to control.

---

## First boot

### Path 1 — Easy mode (you have ethernet near the Pi)

1. Plug ethernet into the Pi. Plug power.
2. Wait ~60 seconds.
3. On your laptop: open `https://aeon-magick.local/` in a browser.
4. Your browser will warn about the self-signed TLS cert. Accept it. (We
   regenerate the cert on first boot per-device, so it's unique to your
   Pi.)
5. **Setup wizard.** First visit shows a one-page form asking you to
   choose an admin password. There are no shipped default credentials —
   you set the password yourself. Once submitted, the device transitions
   to "locked" state and your session cookie is set automatically; you
   land on the main UI.
6. SSH uses a separate credential: user `admin`, default password
   `aeon-default-change-me`. Change it with `passwd` after first login.

### Path 2 — On-the-go (no ethernet, configure WiFi via the device's own AP)

1. Plug power into the Pi. Don't plug ethernet.
2. Wait ~2 minutes. After 90 seconds of "no internet" the Pi spins up its
   own WiFi access point: **`aeon-setup`** (password **`aeon-setup-pw`**).
3. From your phone or laptop, join `aeon-setup`. On every modern OS this
   triggers a **captive portal sheet** that auto-opens to the WiFi
   picker — no need to remember the IP. (If it doesn't, browse to
   `http://anything`; we hijack DNS + HTTP to redirect.)
4. The picker live-scans nearby networks with signal-strength bars.
   Click one, type the password, watch the device join — the
   `aeon-setup` AP tears itself down automatically.
5. Reconnect your laptop to your normal WiFi, then open
   `https://aeon-magick.local/` to land on the main UI.

### Path 3 — Pre-configured before first boot (best for fleets)

Before booting the Pi for the first time, drop a file called
**`aeon-setup.toml`** onto the SD card's boot partition (the FAT32
partition macOS auto-mounts as `bootfs`):

```toml
[wifi]
ssid = "your-network"
password = "your-wifi-password"

[tailscale]
auth_key = "tskey-auth-..."   # optional
hostname = "aeon-magick"      # optional, defaults to aeon-magick
```

On first boot, `aeon-firstboot.service` reads this file, applies the
config, then **deletes the file** so secrets don't sit on a public-readable
partition.

---

## After login — the web UI

Dark theme. Top bar shows live/offline status, current capture resolution,
current FPS, current HID persona. Center: live video. Click in the canvas
to focus and start sending keyboard input. Drag = mouse. Wheel = scroll.

Buttons:
- **release all keys** — panic button. Sends every modifier release + a
  HID reset. Use this if a key gets stuck (mostly impossible with our
  atomic-op API, but it's there).
- **relaunch streamer** — force-respawn `aeon-streamer`. Picks up new
  capture parameters from scratch. Useful if you change the target's
  display resolution and want a clean restart.

---

## Talking to it as an AI agent

The whole device speaks a small REST API behind authentication. Three ways
to authenticate, in order of typical use:

- **API token** (recommended for agents) — issue via the web UI's `/tokens`
  page or `POST /api/auth/tokens`. Each token has a scope (`admin`, `full`,
  `macros`, or `read`). Use as `Authorization: Bearer aeon_tok_…` or
  `X-Aeon-Token: aeon_tok_…`.
- **Session cookie** (web UI only) — set by `POST /api/login` with username
  + password. HMAC-signed with a per-device key.
- **HTTP Basic** — `Authorization: Basic <base64(admin:password)>`. The
  password you set in the first-boot wizard. Convenient for quick curl.

The API surface is shaped specifically so dropped network packets cannot
leave the target host in a weird state — every input op is **atomic** at
the API layer.

### Snapshot one frame

```bash
curl -sk -u admin:$PW \
    "https://aeon-magick.local/api/streamer/snapshot" \
    -o frame.jpg
```

Returns the current JPEG. 1920×1080 by default; 4K if the source is 4K.
Pi 4 hardware JPEG encoding keeps frame latency around 40-80 ms on LAN.

### Live MJPEG stream

```bash
curl -sk -u admin:$PW "https://aeon-magick.local/api/streamer/stream"
```

Multipart MJPEG. Feed straight into ffmpeg, OpenCV, or a `<img>` tag.

### Type a string

```bash
curl -sk -u admin:$PW -X POST \
    -H "Content-Type: application/json" \
    -d '{"text": "Hello, world."}' \
    https://aeon-magick.local/api/hid/type
```

`/api/hid/type` is the typing endpoint. Each character: press → release,
paced. There is no `/api/hid/keydown` — by design. Press without release
on a network is how keys get stuck.

### Send a chord (Cmd+Space, Ctrl+C, F11, etc.)

```bash
curl -sk -u admin:$PW -X POST \
    -H "Content-Type: application/json" \
    -d '{"keys": ["GUI", "SPACE"], "hold_ms": 30}' \
    https://aeon-magick.local/api/hid/key
```

Recognized names: `CTRL ALT SHIFT GUI/CMD/WIN ENTER TAB ESC SPACE
BACKSPACE DELETE HOME END PAGEUP PAGEDOWN UP DOWN LEFT RIGHT F1..F12`, plus
single ASCII characters.

### Click

```bash
curl -sk -u admin:$PW -X POST \
    -H "Content-Type: application/json" \
    -d '{"button": "left", "count": 1}' \
    https://aeon-magick.local/api/hid/click
```

`button` is `left`, `right`, or `middle`. Press → release happens
server-side; no half-pressed-button failure modes.

### Move the mouse (relative)

```bash
curl -sk -u admin:$PW -X POST \
    -H "Content-Type: application/json" \
    -d '{"dx": 100, "dy": -40}' \
    https://aeon-magick.local/api/hid/move
```

Boot mouse semantics — relative deltas, clamped to int8 range
(−127..127). For multi-segment moves, split client-side.

### Scroll

```bash
curl -sk -u admin:$PW -X POST \
    -H "Content-Type: application/json" \
    -d '{"dy": -3}' \
    https://aeon-magick.local/api/hid/scroll
```

Positive `dy` = standard wheel up. macOS users with "natural scrolling"
enabled will see the inverse — that's the host OS's job, not ours.

### Switch HID persona

The web UI has a persona dropdown in the top bar — pick a value and confirm
the re-enumeration prompt. Or hit the API directly:

```bash
curl -sk -u admin:$PW -X POST \
    -H "Content-Type: application/json" \
    -d '{"persona": "logitech-mx"}' \
    https://aeon-magick.local/api/hid/persona
```

Valid values: `generic-composite`, `logitech-mx`, `apple-magic`. Triggers a
USB re-enumeration on the target — about a one-second blip.

**Persistence:** the selection is written to `/etc/aeon/persona.state`
and survives reboots. To revert to the default, SSH in and `sudo rm
/etc/aeon/persona.state` then `sudo systemctl restart aeon-hid`. The
default falls back to whatever `persona = ` is set to in
`/etc/aeon/hid.toml` (which ships as `generic-composite`).

**How the switch is implemented:** the supervisor writes the new persona
slug to `persona.state`, the aeon-hid daemon exits cleanly, systemd
respawns it, the fresh process reads `persona.state` on startup and
binds the gadget under the new HID descriptor. Cleaner than trying to
tear down + rebuild configfs in-process.

### Status

```bash
curl -sk -u admin:$PW https://aeon-magick.local/api/state
```

Returns: streamer mode/resolution/fps/online, current HID persona,
keyboard online flag (= USB-C connected and target sees us), mouse online.

### Panic button — release all keys/buttons

```bash
curl -sk -u admin:$PW -X POST \
    https://aeon-magick.local/api/hid/release_all
```

### Macros — store and run named action sequences

The device ships with a small macro library at `/usr/share/aeon/macros/` and
accepts user-authored TOML at `/etc/aeon/macros/`. User-editable shadows
shipped on name collision.

List, fetch, store, delete, run:

```bash
curl -sk -u admin:$PW https://aeon-magick.local/api/macros
curl -sk -u admin:$PW https://aeon-magick.local/api/macros/open-spotlight
curl -sk -u admin:$PW -X PUT --data-binary @my-macro.toml \
    https://aeon-magick.local/api/macros/my-macro
curl -sk -u admin:$PW -X DELETE https://aeon-magick.local/api/macros/my-macro
curl -sk -u admin:$PW -X POST -H "Content-Type: application/json" \
    -d '{"params": {"query": "Slack"}}' \
    https://aeon-magick.local/api/macros/open-spotlight/run
```

Macro TOML schema:

```toml
name = "open-spotlight"
description = "Open Spotlight, type a query, hit Enter."

[[params]]
name = "query"
required = true

[[steps]]
type = "key"
keys = ["GUI", "SPACE"]
hold_ms = 30

[[steps]]
type = "wait"
ms = 350

[[steps]]
type = "type"
text = "{{query}}"

[[steps]]
type = "key"
keys = ["ENTER"]
```

Step types: `type`, `key`, `click`, `move`, `scroll`, `wait`, `persona`,
`snapshot`, `release_all`. `{{var}}` placeholders in any string field are
replaced from the `params` object passed at run time. `snapshot` steps
write JPEGs to `/run/aeon/snapshots/` and the paths come back in the
response under `snapshots`.

### Prompts — stored agent playbooks

```bash
curl -sk -u admin:$PW https://aeon-magick.local/api/prompts
curl -sk -u admin:$PW https://aeon-magick.local/api/prompts/agent-quickstart
curl -sk -u admin:$PW -X PUT --data-binary @playbook.md \
    https://aeon-magick.local/api/prompts/playbook
curl -sk -u admin:$PW -X DELETE https://aeon-magick.local/api/prompts/playbook
```

Prompts are plain `.md` or `.txt`. The device ships `agent-quickstart` and
`macos-shortcuts` you can crib from.

### API tokens (issue, list, revoke)

```bash
# List active tokens
curl -sk -u admin:$PW https://aeon-magick.local/api/auth/tokens

# Issue a new token (returns plaintext ONCE)
curl -sk -u admin:$PW -X POST \
    -H "Content-Type: application/json" \
    -d '{"name": "claude-desktop", "scope": "full"}' \
    https://aeon-magick.local/api/auth/tokens
# → {"ok":true, "id":"a1b2c3d4", "name":"claude-desktop", "scope":"full",
#    "token":"aeon_tok_xxxx..."}    ← copy this; server stores only the hash.

# Use the token instead of admin password
TOK=aeon_tok_xxxx...
curl -sk -H "Authorization: Bearer $TOK" \
    https://aeon-magick.local/api/streamer/snapshot -o frame.jpg

# Revoke
curl -sk -u admin:$PW -X DELETE \
    https://aeon-magick.local/api/auth/tokens/a1b2c3d4
```

**Scopes:**

| Scope | Allowed |
|---|---|
| `admin` | Everything including token management and password change |
| `full` | All HID input, macros, snapshots; cannot manage tokens |
| `macros` | Run pre-stored macros + read state/snapshots; no raw HID |
| `read` | Read state, snapshots, list macros/prompts; no mutations |

Token management endpoints (`/api/auth/tokens`, `/api/auth/change-password`)
require Admin scope or a session cookie from web login.

### MCP (Model Context Protocol) endpoint

For agents that speak MCP (Claude Desktop, the MCP SDK clients, anything
built on `@modelcontextprotocol/sdk`), the same TLS + Basic-auth endpoint
also speaks Streamable HTTP MCP at `/api/mcp`. Configure your client to
point at:

```
https://aeon-magick.local/api/mcp
```

with Basic auth `admin:<password>` and TLS verification off (self-signed
cert). The server advertises these tools:

| Tool | What it does |
|---|---|
| `state` | combined streamer + HID status |
| `snapshot` | one JPEG of the target's screen, returned as an MCP image block |
| `type_text` | type a string |
| `key_chord` | fire a key combo |
| `click` | left/right/middle click |
| `move_cursor` | relative cursor move (int8 deltas) |
| `scroll` | wheel scroll |
| `set_persona` | hot-swap HID identity |
| `release_all` | panic-release everything |
| `list_macros` | enumerate stored macros |
| `run_macro` | execute a stored macro by name |

It also exposes stored macros + prompts as MCP **resources**
(`aeon://macros/<name>`, `aeon://prompts/<name>`) and prompts as MCP
**prompts** for the `prompts/list` + `prompts/get` methods.

---

## Workflow for vision-driven AI agents

The proven pattern (used by Celina, our local OpenClaw anchor agent):

1. **`/api/state`** to confirm everything is reachable + the right
   persona is loaded.
2. **`/api/streamer/snapshot`** to fetch a frame.
3. The agent reasons over the frame and decides what to do.
4. **`/api/hid/*`** to act — type, click, move, scroll. Each call is
   atomic; the agent does not need to track partial press state.
5. Optionally wait briefly, then **`/api/streamer/snapshot`** again to
   confirm what changed.
6. Loop.

Latency budget: snapshot ~50-150 ms (LAN), reasoning is up to your model,
HID action ~10-30 ms.

---

## SSH access

`ssh admin@aeon-magick.local`. Same admin password from
`aeon-credentials.txt`. Use it to inspect logs:

```bash
journalctl -u aeon-streamer -f
journalctl -u aeon-hid -f
journalctl -u aeon-supervisor -f
journalctl -u aeon-firstboot
journalctl -t aeon-netwatch
```

The five Aeon services. Pretty self-explanatory.

---

## Resetting an unhappy device

If something goes off the rails:

```bash
# Panic-release all HID state without restarting anything
curl -sk -u admin:$PW -X POST https://aeon-magick.local/api/hid/release_all

# Restart just the streamer (e.g., capture is stuck on an old format)
sudo systemctl restart aeon-streamer

# Restart just HID (re-enumerates the USB gadget on the target host)
sudo systemctl restart aeon-hid

# Forget WiFi and re-trigger the AP setup
sudo nmcli con delete <your-ssid>
sudo systemctl restart NetworkManager
# Watcher will spin up `aeon-setup` AP within 90 seconds.
```

Full factory reset: re-flash the SD card. Generates fresh password + cert.

---

## Three things that will trip you up

1. **Cam Link 4K and macOS conspire to negotiate 4K.** macOS Display
   settings hide a "Scaled" Retina-style option that says "Looks like
   1920×1080" but secretly outputs 4K. The Pi can capture 4K but the
   adaptive layer prefers UYVY/YUYV at lower resolutions for cleaner
   software encoding. If you want crisp 1080p, hold Option while clicking
   the resolution picker in System Settings → Displays, and pick the
   `1920×1080` entry **without "Looks like" in front of it.** Or use
   `displayplacer` CLI for full control.
2. **USB-C cable matters.** A charging-only USB-C cable will let the Pi
   power-on from the target but no data flows. You'll see `keyboard.online
   = false` in `/api/state` until you swap to a data-capable cable.
3. **Apple gestures are experimental.** The `apple-magic` persona ships
   a Microsoft Precision Touchpad descriptor as a placeholder. Three- and
   four-finger gestures on macOS require Apple's proprietary multi-touch
   report format which is partially reverse-engineered. Stable for click,
   2-finger scroll, pinch — `docs/design/apple-mt.md` has the full saga
   and the plan to finish it with usbmon captures.

---

## Hard rules

Same as the README: this is for your own computers, your own accounts,
your own services. The naturalism layer (if you enable it via the persona
config) exists for accessibility and reliability, **not** as cover for
impersonation. If you ship something sketchy on top of this, that's on
you.
