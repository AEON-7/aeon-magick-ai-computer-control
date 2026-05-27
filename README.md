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

## What it does, in one bullet list

- 🎹 **Pretends to be a keyboard/mouse/trackpad** via USB-C OTG (Linux gadget framework) — three hot-swappable personas including Apple Magic Keyboard + Trackpad with real multi-touch descriptor
- 🎥 **Captures HDMI** via Elgato Cam Link 4K (or any UVC device), streams MJPEG to a browser at native resolution, 1080p60 capable
- 🌐 **Optional USB ethernet adapter** on the same cable — host pipes its WAN through the Pi (`isolation` / `sharing` / `restricted` modes)
- 🔒 **Optional encrypted DNS** (DNSCrypt v2 + DoH) — Cloudflare / Quad9 / AdGuard / NextDNS / Mullvad — for both the Pi AND USB clients
- 🕳️ **Optional VPN tunnel** — Tailscale · WireGuard · OpenVPN · **Tor** (with bridge presets: direct/obfs4/meek-azure/snowflake/custom) · I2P — with kill-switch + LAN-bypass
- 💿 **Optional USB-CDROM disk** — upload an ISO, expose it to the host as a bootable read-only drive
- 🤖 **REST + MCP** — every operation atomic; macros + prompts shipped; works with any AI agent
- 🎯 **Zero-touch first-boot** — captive-portal WiFi wizard, generated admin password on `/boot/firmware/`
- 🔐 **argon2 + HMAC-signed cookies** — TLS with self-signed cert (or BYO), scoped API tokens (admin/full/macros/read)

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

## Network — three independent layers

The box doubles as a network appliance. All three layers are independent
and any subset can be active.

| Layer | What it does |
|---|---|
| **USB ethernet** (CDC NCM gadget) | The same USB-C that delivers HID adds a virtual ethernet adapter. Three modes: **isolation** (host reaches the Pi + internet via NAT, *cannot* see your LAN — guest-laptop safe), **sharing** (full LAN bridge), **restricted** (WAN only, host can't even see the Pi). 250+ Mbit on USB 3.0. |
| **DNSCrypt** | Optional local `dnscrypt-proxy` on `127.0.2.1:53`. Curated providers (Cloudflare, Quad9, AdGuard, NextDNS, Mullvad, Cloudflare-for-Families). When enabled, both the Pi *and* every USB-connected client resolve via encrypted DoH. Plaintext DNS never leaves the device. |
| **VPN tunnel** | One of: Tailscale (just paste an auth-key) · WireGuard (paste a `.conf`) · OpenVPN (paste a `.ovpn` + optional creds) · **Tor** (transparent proxy + DNS-over-Tor with bridge presets: direct / obfs4 / meek-azure / snowflake / custom) · I2P (garlic-routed, optional outproxy). Built-in **kill-switch** drops WAN if the tunnel falls; LAN-bypass keeps management always reachable. |

Live VPN status panel polls every few seconds — bootstrap %, exit IP +
country (or Tor circuit hops with `Guard → Middle → Exit`), peer count
for Tailscale/WireGuard, handshake age. The "rotate identity" button
sends `SIGNAL NEWNYM` to Tor, force-cycles WireGuard peers, etc.

## First-boot setup wizard

A `aeon-setup` WiFi AP comes up automatically if the device can't reach
the internet for ~90 seconds. Connecting to it triggers the captive
portal flow on every OS (Apple's `hotspot-detect.html`, Android's
`generate_204`, Windows' `ncsi.txt` — all served via tiny HTTP listener
on :80, with DNS wildcard + iptables redirect catching anything else),
which auto-launches a browser pointing at the live-scanning WiFi picker.
Pick a network, type the password, the device joins, the AP tears
itself down. Zero monitor, zero keyboard, zero serial cable.

## USB-CDROM disk drive

Upload an ISO via the web UI; the Pi exposes it as a read-only USB CDROM
the target boots from. Useful for booting installers, recovery images,
or shimming a Linux live-USB onto a sealed device. Multi-GB streaming
uploads with SHA-256 verification + atomic rename so a half-uploaded ISO
can't corrupt your library. Hot-swap the "inserted" disk without
unplugging the USB cable.

## Vision

`aeon-streamer` runs ffmpeg with `-f image2pipe` so MJPEG frames stream
straight into the supervisor's memory via stdout — no intermediate disk
writes, no half-written-frame races, no jpeg corruption when the browser
fetches at exactly the wrong microsecond. A single `tokio::sync::watch`
channel fans the latest frame out to all consumers: the multipart MJPEG
HTTP stream, the snapshot endpoint, and the watchdog. Two watchdog loops
— one based on the v4l2 format-list hash, one based on the watch channel
going stale — kick ffmpeg to respawn whenever the source signal changes.
Hot-plugging the HDMI cable on the target and getting fresh frames within
~5 seconds is the design target.

The capture pipeline auto-detects format and resolution from the v4l2
device (MJPEG passthrough preferred; YUV/RGB sources get encoded), and
adapts on the fly when the source resolution changes (e.g. when the
target laptop wakes from sleep and renegotiates).

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
  aeon-net-services.service                 ← oneshot: applies network.toml (usb_eth + dnscrypt + vpn) on changes
  dnscrypt-proxy.service tor.service        ← lazy-started by aeon-net-services when enabled

/usr/share/aeon/web/
  the SvelteKit single-page app

/usr/local/bin/aeon-firstboot
/usr/local/bin/aeon-netwatch
/usr/local/bin/aeon-net-services             ← applies network config; idempotent

/etc/udev/rules.d/99-aeon-capture.rules    ← Elgato Cam Link 4K + MS2109 → /dev/kvmd-video

/var/lib/aeon/iso/                           ← uploaded ISOs + .meta sidecars

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
