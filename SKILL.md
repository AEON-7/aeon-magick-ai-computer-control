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

This skill is the device's own manifest. It is the **agent's** manifest — it
documents only what an agent should do. Keep this index small; pull a
`references/` file in only when you actually need that capability.

## Environment

The skill reads these from your env (or from a `config.json` if you wrap it
in a script):

| Var | Default | Meaning |
|---|---|---|
| `AEON_HOST` | `aeon-magick.local` | mDNS hostname (or IP if mDNS isn't working) |
| `AEON_USER` | `admin` | HTTP Basic username |
| `AEON_PASSWD` | _(required)_ | password from `aeon-credentials.txt` on the SD card boot partition (or copied from `/boot/firmware/aeon-credentials.txt` on the running device) |
| `AEON_INSECURE_TLS` | `1` | accept the device's self-signed cert. Curl: `-k`. Python `requests`: `verify=False`. |

All endpoints below assume `https://${AEON_HOST}/`. Every command in this skill
is a single HTTPS call and returns JSON unless noted.

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

## Core workflow — vision-driven navigation

The whole loop is **state → snapshot → act → snapshot again**:

1. **`state`** — confirm `keyboard_online`, `mouse_online`, `streamer_online`
   are all `true`. If anything is false, **stop and report to the user** — it's
   almost always a cable, not software. There's no point sending input that
   won't land.
2. **`snapshot`** → save to a tmp path, then `Read` the JPEG to inspect it.
3. From the frame, identify the click target in target pixel space. You are
   reasoning over the captured frame directly — there is no "where is the
   cursor now" state to track, because every input is relative or chord-based.
4. On the `generic-absolute` persona, **`click_at`** the target with its
   fractional coords (`x = pixel_x / width`, `y = pixel_y / height`) — the
   reliable path, no acceleration drift. (On a relative persona, drive there
   with segmented `move` instead, splitting deltas larger than ±127.)
5. **`click_at`** / **`click`** (or `key`, `type`, `scroll`, `drag`) to act.
   For text entry: focus the field (move + click), then `type`.
6. **`snapshot`** again to verify the change happened. If not, loop to step 3
   with the new frame.

Latency budget: snapshot ~50–150 ms (LAN); reasoning is up to your model; HID
action ~10–30 ms.

The two foundational calls:

```bash
# state — what is the device seeing right now? (call this first every session)
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/state"

# snapshot — one JPEG of the target's current screen
curl -sk -u "$AEON_USER:$AEON_PASSWD" \
    "https://$AEON_HOST/api/streamer/snapshot" -o /tmp/aeon_capture.jpg
```

See `references/vision.md` and `references/input.md` for the full surface.

## Capability index

Load exactly one reference when you need that capability — don't read them all
up front.

| Reference | Load this when… |
|---|---|
| [`references/vision.md`](references/vision.md) | you need to **see** the target — `state`, `snapshot`, or the live MJPEG/H.264 stream. |
| [`references/input.md`](references/input.md) | you need to **act** — type, key chords, click, move (relative + absolute `click_at`), drag, scroll, persona switching, or `release_all`. |
| [`references/macros.md`](references/macros.md) | you want to run (or author) a stored **macro** sequence, or fetch a stored agent-playbook **prompt**. |
| [`references/recording.md`](references/recording.md) | you need to **record the target's screen** to MP4 and list/download recordings. |
| [`references/clipboard.md`](references/clipboard.md) | you need to read/write the device's text **clipboard** or type its contents onto the target (e.g. pasting a long credential). |
| [`references/files.md`](references/files.md) | you need to **stage files** for the target or manage the **ISO** library (OS-install workflows). |
| [`references/network.md`](references/network.md) | you're setting up **DNS / VPN / Tor / I2P / WiFi** on the device's outbound path. |
| [`references/mcp.md`](references/mcp.md) | your agent speaks **MCP** and you want the Streamable-HTTP transport + the full tool list instead of curl. |

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
- **This is the agent's manifest.** Some device actions are
  **human-admin-only** and are deliberately NOT agent capabilities: managing
  the device's SSH keys, powering / rebooting / waking the **target or the Pi
  itself**, and issuing / revoking API tokens. None of those appear in this
  skill. If a task seems to need one, ask the human to do it in the web UI.

## Troubleshooting cheatsheet

| Symptom | Likely cause | Fix |
|---|---|---|
| `keyboard_online: false` | Charging-only USB-C cable | Swap to a data cable |
| `streamer_online: false` | HDMI capture stick not enumerated | Re-seat the capture stick; check the target is outputting HDMI |
| Continuous letter repeats on target | Lost release event (rare) | `release_all` (see `references/input.md`) |
| Snapshot returns same frame forever | Target went to sleep, or display put to standby | Wake the target (try sending a SHIFT key chord), then snapshot again |
| `click_at` lands in the wrong place | Not on the `generic-absolute` persona | Switch persona first (see `references/input.md`) |
| `recording requires H.264 stream mode` | Stream isn't in h264 mode | Set the stream format to h264, then record (see `references/recording.md`) |
| `aeon-magick.local` doesn't resolve | mDNS blocked on your network | Use the IP directly; set `AEON_HOST` to it |
| TLS handshake fails | Cert regenerated after a re-flash | Re-accept; the cert is unique per device per first-boot |

When in doubt: `state` first, `snapshot` second, then act. The device's state
surface is small enough that those two calls explain almost every "why isn't
this working" question.
