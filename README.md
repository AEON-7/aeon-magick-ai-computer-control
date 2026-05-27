# AEON Magick AI Computer Control

> _The Universe's Strangest Peripheral_

A Raspberry Pi turned into a sleek little box that pretends to be a monitor,
a keyboard, a mouse, and a trackpad — all at once — and hands the resulting
**see + act** loop to an AI agent (or you).

Plug it between your laptop and a Pi 4. The laptop sees a USB keyboard and
mouse it can trust. The Pi sees the laptop's HDMI through a video capture
device. A web UI streams the laptop's screen back to your browser. An AI
agent reads frames, decides what to do, and sends back keyboard / mouse /
gesture events. The laptop never knows it's been politely possessed.

```
        ┌──────────────────┐                          ┌──────────────────┐
        │   Target host    │                          │  You / your AI   │
        │  (any OS — Mac,  │   HDMI → Cam Link 4K     │   ┌────────────┐ │
        │   Windows, Linux,│ ───────────────────────► │   │ web UI on  │ │
        │   BIOS, FileVault│                          │   │ laptop or  │ │
        │   prompt, …)     │ ◄─────────────────────── │   │ AI agent   │ │
        └──────────────────┘   USB-C OTG ← Pi gadget  │   │            │ │
                                emulating Logitech /  │   │ keyboard + │ │
                                Apple keyboard+mouse  │   │ mouse +    │ │
                                                      │   │ trackpad   │ │
                                                      │   │ gestures   │ │
                                                      │   └────────────┘ │
                                                      └──────────────────┘
```

## Why this exists

Most "AI computer use" projects sit *inside* the operating system they're
trying to drive. Browser plugins. Accessibility APIs. Screen recorders that
need permission. Those work, but they're chatty — every approach reveals
itself, every screen reveals its agent, and you can't drive **anything** at
the BIOS, the FileVault unlock prompt, the Windows OOBE, the firmware
update screen, the wall of "Hi, please type your password" that Linux
distros open with on a fresh install.

This box doesn't care. It looks like a USB keyboard and a USB mouse. To the
target OS it's just hardware. There is nothing to install on the target.
There is nothing to grant permission to. The target doesn't even know
anything is connected besides a perfectly normal-looking peripheral.

It's the strangest USB device in the universe and also the most boring one.

## Personas

`aeon-hid` builds a USB composite HID gadget on the Pi's USB-C OTG port. It
ships three identities you can hot-swap (via API or web UI):

| Persona | What the host sees | Use this when |
|---|---|---|
| `generic-composite` | Boot keyboard + boot mouse, VID `1d6b` (Linux Foundation) | You want maximum compatibility and a small attack surface. |
| `logitech-mx` | Logitech Unifying Receiver (VID `046d`), MX-Keys-style keyboard + MX-Master-style mouse + consumer media keys | You want media keys and extra mouse buttons. The host often has a Logitech driver path that lights up. |
| `apple-magic` (experimental) | Apple VID (`05ac`), Apple keyboard + Magic Trackpad multi-touch | You want **macOS gesture support** — 2/3/4-finger swipes, pinch, rotate — without installing anything on the Mac. See [`docs/design/apple-mt.md`](./docs/design/apple-mt.md) for the not-fully-solved Apple HID descriptor saga. |

Switching persona requires a USB re-enumeration on the target. Takes about
a second. The target host briefly sees the device disappear and a different
one appear in its place.

## Vision

`aeon-streamer` wraps `ustreamer` (we owe PiKVM a beer) with an adaptive
layer that automatically figures out what format and resolution the capture
device is offering and gives ustreamer arguments that actually work. Two
watchdog loops, one based on the v4l2 format-list hash and one based on
ustreamer's own `source.online` signal, kick the streamer to respawn
whenever the source signal changes. Hot-plugging the HDMI cable on the
target and getting fresh frames within ~5 seconds is the design target.

Hardware H.264 encoding is used on Pi 4 (`h264_v4l2m2m`). Pi 5 falls back
to MJPEG / software libx264.

## Agents-first interfaces

Two ways for an AI agent to drive the box:

- **REST + curl** — every input op is an atomic POST under `/api/hid/*`.
  Snapshots are a single GET. Easy to script from any language.
- **MCP (Model Context Protocol)** — the supervisor speaks MCP Streamable
  HTTP at `/api/mcp`, exposing the same op surface as named tools. Drop
  the URL into Claude Desktop, `@modelcontextprotocol/sdk`, or any MCP
  client and you're operating the device with first-class tool calls,
  no wrapper required.

Both transports share TLS + Basic auth, so credentials issued at first
boot work for both. Stored **macros** (`/etc/aeon/macros/*.toml`) and
**prompts** (`/etc/aeon/prompts/*.md`) round it out: keyboard-driven
sequences you don't want to re-author every session, and short
playbooks an agent can fetch to prime itself.

## What's in the box (the image)

```
aeon (system user), running:
  /usr/local/bin/aeon-streamer       ← v4l2 capture, MJPEG/H.264 via ustreamer
  /usr/local/bin/aeon-hid            ← USB gadget configfs + atomic-op API
  /usr/local/bin/aeon-supervisor     ← HTTPS frontend (rustls), routes /api/*,
                                       MCP server at /api/mcp, macros + prompts,
                                       and serves /

/etc/aeon/
  streamer.toml hid.toml supervisor.toml    ← runtime config
  auth.toml                                 ← argon2 admin password, regenerated on first boot
  cert.pem key.pem                          ← self-signed TLS, regenerated on first boot
  macros/   scripts/   prompts/             ← user-editable action library

/usr/share/aeon/
  web/        ← SvelteKit single-page app
  macros/     ← shipped read-only macros (open-spotlight, cmd-tab, see-then-click, …)
  prompts/    ← shipped agent playbooks (agent-quickstart, macos-shortcuts)

/etc/systemd/system/
  aeon-streamer.service  aeon-hid.service  aeon-supervisor.service
  aeon-firstboot.service                    ← oneshot: generates admin password, applies aeon-setup.toml
  aeon-netwatch.service + .timer            ← polls connectivity; spins up `aeon-setup` WiFi AP if offline for 90s+

/usr/share/aeon/web/
  the SvelteKit single-page app

/usr/local/bin/aeon-firstboot
/usr/local/bin/aeon-netwatch

/etc/udev/rules.d/99-aeon-capture.rules    ← Elgato Cam Link 4K + MS2109 → /dev/kvmd-video

In /boot/firmware/ on the SD card after first boot:
  aeon-credentials.txt   ← admin password (mode 0600, delete it once you've noted it)
```

Tailscale is preinstalled but disabled by default. Drop an
`aeon-setup.toml` onto the boot partition before first boot to wire it up
automatically (see [`AGENTS.md`](./AGENTS.md)).

## Project layout

```
aeon-streamer/    Rust — adaptive capture supervisor + watchdog
aeon-hid/         Rust — USB gadget + persona + atomic-op input API
aeon-supervisor/  Rust — HTTPS frontend, /api/* proxy, argon2 auth
aeon-web/         SvelteKit — dark UI, live stream canvas, input capture
image-builder/    pi-gen overlay producing aeon-magick.img.xz
docs/             Design notes (Apple multi-touch, the cursed-hid lineage)
scripts/          Cross-compile + web-build helpers
AGENTS.md         Setup + day-to-day usage, for humans AND AIs
SKILL.md          Manifest an AI agent loads to operate the device
ARCHITECTURE.md   How the daemons split work and why
BUILDING.md       Compile + build the image on macOS or Linux
```

## Getting started

[`AGENTS.md`](./AGENTS.md) is the read-this-first document. It walks
through flashing the SD card, first-boot, finding your generated admin
password, opening the web UI, and pairing an AI agent.

[`BUILDING.md`](./BUILDING.md) is for when you want to rebuild from source.

## Lineage

Descended from [`cursed-hid`](https://github.com/albert/cursed-hid), an
ESP32-S2 dongle that emulated a Logitech Unifying Receiver. Cursed-hid's
firmware did all the input naturalism on-device (Bezier mouse paths,
log-normal keystroke timing) and accepted only **logical operations** over
WebSocket — never raw press/release primitives that could leave a key
stuck on the wire if a packet dropped. AEON Magick AI Computer Control
inherits that design lesson: the input HTTP API exposes `type`, `key
chord`, `click`, `move` — never `key_down` followed by a separate `key_up`.

Borrows configfs USB-gadget patterns and the ustreamer v4l2 capture loop
from [PiKVM](https://github.com/pikvm/pikvm). We are not a fork; we are a
sibling project with a different goal: agent-grade computer control, not
remote KVM administration.

## License

MIT. See [`LICENSE`](./LICENSE).

## Hard rules (non-negotiable)

This software exists to drive **the user's own computers**, the **user's
own accounts**, the **user's own services**. It is not a tool to bypass
fraud controls, automate access to other people's accounts, or evade
device-attestation on a service the user is not authorized to operate
against. The naturalism layer (when enabled) is for accessibility and
reliability — not as cover for impersonation. If you build something
sketchy on top of this, that's on you, and we will not help.
