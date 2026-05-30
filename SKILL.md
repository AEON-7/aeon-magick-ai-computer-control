---
name: aeon-magick
description: >
  Drive a target host's keyboard, mouse, trackpad, AND screen capture through
  a single AEON Magick AI Computer Control device on the LAN. The device's
  USB-C OTG port emulates a real USB keyboard + mouse (+ optional Apple
  multi-touch trackpad) on the target; its HDMI capture stick sees the
  target's screen. All ops are atomic HTTPS calls — there is no `key_down` /
  `key_up` split that can leave the target with a stuck key after a packet
  drop. Use to operate the USER'S OWN computers, accounts, and services.
---
# aeon-magick

The Universe's Strangest Peripheral. One box, two superpowers:
**see** (HDMI capture → JPEG) and **act** (USB-HID keyboard + mouse + optional
Apple trackpad → target host). Both halves live behind one HTTPS endpoint
with one set of credentials.

This skill is the device's own manifest. Loading it gives an agent everything
it needs to drive a target machine vision-first.

## Environment

The skill reads these from your env (or from a `config.json` if you wrap it
in a script):

| Var | Default | Meaning |
|---|---|---|
| `AEON_HOST` | `aeon-magick.local` | mDNS hostname (or IP if mDNS isn't working) |
| `AEON_USER` | `admin` | HTTP Basic username |
| `AEON_PASSWD` | _(required)_ | password from `aeon-credentials.txt` on the SD card boot partition (or copied from `/boot/firmware/aeon-credentials.txt` on the running device) |
| `AEON_INSECURE_TLS` | `1` | accept the device's self-signed cert. Curl: `-k`. Python `requests`: `verify=False`. |

All endpoints below assume `https://${AEON_HOST}/`.

## Hardware preconditions

Before any of this works:

- Device powered on, joined to LAN (or reachable via Tailscale if you wired
  that in via `aeon-setup.toml`).
- **Device USB-C → target USB-C** with a **data-capable** cable. Charging-only
  cables will let the device draw power from the target but no data flows;
  `keyboard_online` will be false in `state`.
- **Target HDMI out → HDMI capture stick → device USB-A** for vision.
  Elgato Cam Link 4K or any MS2109-based capture stick. Both are auto-detected.

If `state` shows `keyboard_online: false` or `streamer_online: false`, fix
the physical link before issuing input or capture calls — they will silently
no-op or return stale frames.

## Commands

Every command is a single HTTPS call. Returns JSON unless noted.

### Status — what is the device seeing right now?

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" \
    "https://$AEON_HOST/api/state"
```

Returns:
```json
{
  "streamer": {"online": true, "resolution": "1920x1080", "fps": 30, "mode": "h264"},
  "hid":      {"persona": "logitech-mx", "keyboard_online": true, "mouse_online": true}
}
```

Call this first every session. If `keyboard_online: false`, stop and tell
the user — there is no point sending input that won't land.

### Capture — what does the target's screen look like?

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" \
    "https://$AEON_HOST/api/streamer/snapshot" \
    -o /tmp/aeon_capture.jpg
```

One JPEG, current frame. 1920×1080 at default settings; 4K if the source is
4K. Latency ~50–150 ms on LAN. After it returns, `Read` the file path to
look at the frame.

For live MJPEG (e.g. into ffmpeg or OpenCV):

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" \
    "https://$AEON_HOST/api/streamer/stream"
```

Multipart MJPEG. Most agents only need `snapshot`.

### Type — emit a string

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"text": "Hello, world."}' \
    "https://$AEON_HOST/api/hid/type"
```

Press → release per character, paced on-device. Mechanical timing (no
naturalism layer at the API). There is intentionally no `key_down` /
`key_up` split — that's the design lesson inherited from `cursed-hid`.

### Key chord — Cmd+Space, Ctrl+C, F11, etc.

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"keys": ["GUI","SPACE"], "hold_ms": 30}' \
    "https://$AEON_HOST/api/hid/key"
```

Recognized names: `CTRL ALT SHIFT GUI/CMD/WIN ENTER TAB ESC SPACE BACKSPACE
DELETE HOME END PAGEUP PAGEDOWN UP DOWN LEFT RIGHT F1..F12`, plus single
ASCII characters. The whole chord is pressed → held `hold_ms` → released as
one atomic op.

### Click — left/right/middle, single/double/triple

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"button": "left", "count": 1}' \
    "https://$AEON_HOST/api/hid/click"
```

Press → release at the current cursor position. There is no
half-pressed-button failure mode possible.

### Move — relative cursor delta

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"dx": 100, "dy": -40}' \
    "https://$AEON_HOST/api/hid/move"
```

Boot-mouse semantics: relative deltas in HID int8 range (−127..127). For
larger or smoother moves, split client-side into multiple calls (the device
deliberately does **not** auto-segment — the agent stays in charge of the
path, and a dropped packet only drops one segment, not a whole gesture).

### Move / click at an absolute point — `move_abs` (recommended for agents)

With the `generic-absolute` persona, give a point as a fraction of the screen
(`(0,0)` top-left … `(1,1)` bottom-right) and the cursor lands there exactly —
no relative-acceleration drift. Compute it from a snapshot as
`x = pixel_x / frame_width`, `y = pixel_y / frame_height`.

```bash
# move (and optionally click) at an absolute point
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"x": 0.5, "y": 0.5, "buttons": 0}' \
    "https://$AEON_HOST/api/hid/move_abs"
```

`buttons` is a bitmask (1=left, 2=right, 4=middle); `wheel` is optional. A
click = send with the button bit, then again with `buttons:0`. Over MCP this
is the `click_at` / `move_pointer` / `drag` tools — no math needed, just pass
`x`,`y`.

### Drag — hold a button across a move

`move_abs` carries the button mask, so press → move → release drags. For the
relative personas, `POST /api/hid/button {"button":"left","down":true}` /
`…"down":false` holds/releases at the current spot. `release_all` clears it.

### Scroll

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"dy": -3}' \
    "https://$AEON_HOST/api/hid/scroll"
```

Positive `dy` = standard wheel up. macOS users with "natural scrolling" see
the inverse — that's the host OS, not the device.

### Persona — hot-swap which keyboard/mouse the target sees

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"persona": "logitech-mx"}' \
    "https://$AEON_HOST/api/hid/persona"
```

Valid values:

- `generic-composite` — boot keyboard + relative boot mouse, neutral VID.
  Most compatible, smallest attack surface. Linux-safe.
- `generic-absolute` — boot keyboard + **absolute pointer**, neutral VID.
  Reports absolute screen coords → enables `move_abs` / `click_at`. **Best for
  AI agents** (precise pointing, no drift). Linux-safe.
- `logitech-mx` — Logitech VID, MX-Keys + MX-Master flavor (media keys, extra
  buttons). Can wedge `aeon-hid` on Linux targets — prefer a `generic-*`
  persona there.
- `apple-magic-stable` — Apple VID, Apple keyboard + working trackpad (pointer
  + keys + modifiers). Use for macOS targets.
- `apple-magic` (experimental) — Apple VID, multi-touch; macOS gestures WIP and
  the pointer is currently unreliable — see `docs/design/apple-mt.md`.

Switching triggers a USB re-enumeration on the target (about 1 s blip).

### Release-all — panic button

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    "https://$AEON_HOST/api/hid/release_all"
```

Sends release events for every modifier and mouse button, then fires a HID
reset. Run this if the target is misbehaving (e.g. continuous letter
repeats, or "modifier is held" behavior). Atomic-op design makes this
rarely needed, but it's there.

### Macros — run a stored action sequence

```bash
# List
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/macros"

# Run with params
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST \
    -H "Content-Type: application/json" \
    -d '{"params": {"query": "Slack"}}' \
    "https://$AEON_HOST/api/macros/open-spotlight/run"
```

Shipped on the device: `open-spotlight`, `cmd-tab`, `screenshot-region`,
`see-then-click`. Author your own by PUTing a TOML body to
`/api/macros/<name>`. Schema lives in `AGENTS.md`. Use macros for
deterministic keyboard-driven sequences — your reasoning loop drives the
moments that need vision.

### Prompts — stored agent playbooks

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/prompts"
curl -sk -u "$AEON_USER:$AEON_PASSWD" \
    "https://$AEON_HOST/api/prompts/agent-quickstart"
```

Shipped: `agent-quickstart`, `macos-shortcuts`. Useful to fetch at session
start to remind yourself of device idioms.

## MCP transport (alternative to curl)

If your agent speaks MCP (Claude Desktop, anything on
`@modelcontextprotocol/sdk`), skip the curl recipes entirely. Point your
MCP client at:

```
url:      https://${AEON_HOST}/api/mcp
auth:     Basic admin:<password>
tlsVerify: false
```

The server uses Streamable HTTP transport and exposes these tools (same
shape as the curl endpoints above):

`state`, `snapshot`, `type_text`, `key_chord`, `click`, `move_cursor`,
`click_at`, `move_pointer`, `drag`, `scroll`, `set_persona`, `release_all`,
`list_macros`, `run_macro`.

It also advertises stored macros + prompts as MCP **resources**
(`aeon://macros/<name>`, `aeon://prompts/<name>`) so a UI like Claude
Desktop's resource picker lets the user browse them without a separate
filesystem mount.

Pick one transport per session — mixing curl and MCP works, but creates
ambiguity in audit logs.

## Workflow for vision-driven navigation

1. **`state`** — confirm `keyboard_online`, `mouse_online`,
   `streamer_online` are all `true`. If anything is false, stop and report
   to the user (it's almost always a cable, not software).
2. **`snapshot`** → save to a tmp path, then read the JPEG to inspect.
3. From the frame, identify the click target (in target pixel space). Your
   model is reasoning over the captured frame directly — there is no
   "where is the cursor right now" state to track because every input is
   relative or chord-based.
4. On the `generic-absolute` persona, **`click_at`** the target with its
   fractional coords (`x = pixel_x / width`, `y = pixel_y / height`) — the
   reliable path. (On a relative persona, drive there with segmented `move`
   instead, splitting deltas larger than ±127.)
5. **`click_at`** / **`click`** (or `key`, `type`, `scroll`, `drag`) to act.
6. **`snapshot`** again to verify the expected change happened. If not,
   loop back to step 3 with the new frame.

For text entry: focus the input field (move + click), then `type`.

For multi-segment gestures: send several short `move`s in sequence. The
device fires each as soon as it arrives; there's no client-side pacing
needed unless the target OS smooths poorly (rare).

## Differences from the older skills

| | hid-operator (Flipper) | pikvm-operator | **aeon-magick** |
|---|---|---|---|
| Devices needed | 2 (Flipper + PiKVM) | 1 (PiKVM) | **1 (aeon-magick)** |
| Input transport | WebSocket | HTTPS REST | **HTTPS REST** |
| Vision transport | PiKVM snapshot | PiKVM snapshot | **same device, same auth** |
| Mouse semantics | relative + naturalistic | absolute | **relative *or* absolute, atomic** |
| Personas | one (Logitech) | one (generic) | **5, hot-swappable** |
| Apple gestures | no | no | **yes (experimental)** |
| Pre-OS / BIOS HID | no | yes | **yes** |
| Atomic input ops | yes (firmware) | no | **yes (API design)** |

For agents that already speak `hid-operator` or `pikvm-operator`, the
verbs are similar enough that high-level macros (`type`, `key`, `click`,
`move`, `scroll`, `snapshot`) port over with minimal rewriting.

## Hard rules (non-negotiable)

- This tool drives the USER'S OWN computers, the USER'S OWN accounts, the
  USER'S OWN services. Never use it to defeat fraud controls on a service
  the user is not authorized to operate against.
- BEFORE any irreversible action (sending an email, submitting a form,
  paying for something, deleting files, running `rm`, force-pushing, etc.),
  confirm with the user in chat first — unless they have already explicitly
  authorized that specific action this session.
- Don't use it to install software, change system settings on the target,
  or modify other agents' configs without the user's explicit go-ahead.
- If the persona supports gestures or media keys, those exist for
  accessibility and ergonomics — not as cover for impersonation. Don't
  enable `apple-magic` to make a session "look more human" on a service
  the user isn't authorized to operate.

## Troubleshooting cheatsheet

| Symptom | Likely cause | Fix |
|---|---|---|
| `keyboard_online: false` | Charging-only USB-C cable | Swap to a data cable |
| `streamer_online: false` | HDMI capture stick not enumerated | Re-seat the capture stick; check the target is outputting HDMI |
| Continuous letter repeats on target | Lost release event (rare) | `release_all` |
| Snapshot returns same frame forever | Target went to sleep, or display put to standby | Wake the target (try sending a SHIFT key chord), then snapshot again |
| `aeon-magick.local` doesn't resolve | mDNS blocked on your network | Use the IP directly; set `AEON_HOST` to it |
| TLS handshake fails | Cert regenerated after a re-flash | Re-accept; the cert is unique per device per first-boot |

When in doubt: `state` first, `snapshot` second, then act. The device's
state surface is small enough that those two calls explain almost every
"why isn't this working" question without needing SSH.
