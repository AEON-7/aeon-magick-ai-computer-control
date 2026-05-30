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
4. Pi boots, drops you onto a one-page setup wizard that asks you to
   choose an admin password.
5. Once the Pi joins WiFi (or you connect to its `aeon-setup` AP for
   first-time WiFi config), open `https://aeon-magick.local/` and log in
   as `admin` with the password you chose.
6. You're in. Web UI streams the target's screen and accepts keyboard +
   mouse input. AI agents talk to the same surface via the REST API or
   MCP endpoint.

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

Top-level pages in the navigation:

| Page | What it covers |
|---|---|
| `/` | Live video + HID input + persona switcher |
| `/macros` | Stored macro library + builder |
| `/prompts` | Stored agent playbook library |
| `/tokens` | Issue/revoke API tokens |
| `/files` | Upload/download files to the target via the optional HTTP server |
| `/clipboard` | Shared text buffer (paste-to-target, copy-from-target) |
| `/storage` | ISO library + USB-CDROM toggle |
| `/network` | DNSCrypt, anonymized relays, Tor, I2P, VPN, firewall, DNS blacklist |
| `/network/vpn-providers` | Mullvad / IVPN / AzireVPN setup wizards |
| `/network/i2p` | I2P daemon status + browser proxy hints |
| `/security` | Blocked-traffic log, audit log |
| `/system` | Pi-level controls — Pi reboot/poweroff, system info |
| `/setup/wifi` | WiFi picker (also reachable via the AP-fallback captive portal) |
| `/target` | Target machine power controls — power tap, hold, WoL, full reboot |

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

Returns the current JPEG. 1920×1080 by default; with `match_source` enabled it
mirrors the source's native resolution (capped at 1080p). 4K sources downscale to fit.
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
on a network is how keys get stuck. Unmappable characters (emoji, smart
quotes) are silently skipped; the response includes `typed` + `skipped`
counters so callers can detect data loss.

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

### Move / click at an absolute position (precise targeting)

Requires the `generic-absolute` persona (see *Switch HID persona* below).
Instead of relative deltas you give a point as a **fraction of the screen** —
`(0,0)` = top-left, `(1,1)` = bottom-right — so the cursor lands exactly where
you computed it from a snapshot (`x = pixel_x / frame_width`,
`y = pixel_y / frame_height`). This sidesteps host pointer-acceleration
entirely and is **the recommended way for an AI agent to click** a specific
element.

```bash
# move the pointer to the centre of the screen (no click)
curl -sk -u admin:$PW -X POST \
    -H "Content-Type: application/json" \
    -d '{"x": 0.5, "y": 0.5}' \
    https://aeon-magick.local/api/hid/move_abs
```

Body: `x`, `y` (0..1, required) plus optional `buttons` (bitmask: 1=left,
2=right, 4=middle) and `wheel` (signed, ±127). Omit `buttons`/`wheel` to just
move. To **click** a point, send it once with the button bit set, then again
with `buttons: 0` at the same coordinates.

### Click-and-drag (held buttons)

Because `move_abs` carries the button mask, a drag is press → move → release:

```bash
# drag from (0.2,0.3) to (0.6,0.7) with the left button held
for step in '{"x":0.2,"y":0.3,"buttons":1}' \
            '{"x":0.6,"y":0.7,"buttons":1}' \
            '{"x":0.6,"y":0.7,"buttons":0}'; do
  curl -sk -u admin:$PW -X POST -H "Content-Type: application/json" \
      -d "$step" https://aeon-magick.local/api/hid/move_abs
done
```

For the *relative* personas, `POST /api/hid/button {"button":"left","down":true|false}`
holds or releases a button at the current position (the web UI uses this to
drag). `release_all` always clears any held button.

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

Valid values: `generic-composite`, `generic-absolute`, `logitech-mx`,
`apple-magic-stable`, `apple-magic`. Triggers a USB re-enumeration on the
target — about a one-second blip.

- **`generic-composite`** — boot keyboard + relative boot mouse, neutral VID
  `1d6b`. Most compatible, smallest attack surface. Safe on Linux hosts.
- **`generic-absolute`** — boot keyboard + **absolute pointer** (VID `1d6b`).
  Same compatibility, but the mouse reports absolute screen coordinates, which
  unlocks `move_abs` / `click_at` (point at an exact spot). **Best choice for
  AI agents** — precise, no relative-acceleration drift. Also Linux-safe.
- **`logitech-mx`** — Logitech VID `046d`, MX-Keys + MX-Master flavor (media
  keys, extra buttons). Can wedge `aeon-hid` on **Linux** targets (the
  `hid-logitech-dj` driver claims it but doesn't drain reports) — prefer a
  `generic-*` persona on Linux.
- **`apple-magic-stable`** — Apple VID, Apple keyboard + working trackpad
  (pointer + keys + modifiers). Use for macOS targets.
- **`apple-magic`** (experimental) — Apple multi-touch descriptor; gestures are
  a work-in-progress and the pointer is currently unreliable. See
  `docs/design/apple-mt.md`.

**Persistence:** the selection is written to `/etc/aeon/persona.state`
and survives reboots. To revert to the default, SSH in and `sudo rm
/etc/aeon/persona.state` then `sudo systemctl restart aeon-hid`. The
default falls back to whatever `persona = ` is set to in
`/etc/aeon/hid.toml` (which ships as `logitech-mx`; for Linux targets or
precise agent control, set it to `generic-absolute`).

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

---

## Target machine power controls

The device emulates a USB **Consumer Control** descriptor in addition to
keyboard + mouse, so it can push the target machine's power button. Plus,
once the target's MAC is in our ARP table (it shows up as soon as the
target DHCPs from the Pi's USB ethernet), Wake-on-LAN works automatically.

These endpoints affect the **target**, not the Pi. They're the right
primitives for an AI agent driving an OS installation that needs to reboot
between steps.

| Endpoint | Effect |
|---|---|
| `GET /api/target/info` | Known MAC + iface, available actions. |
| `POST /api/target/power_tap` | 200 ms HID power button press — graceful OS shutdown dialog. |
| `POST /api/target/power_hold` | 8 s HID power button hold — hard power-off, bypasses OS. |
| `POST /api/target/wake` | WoL magic packet — boots the target from S5/S4. |
| `POST /api/target/reboot` | 8 s hold → 5 s wait → WoL packet. Canonical "reboot now" for agents. |

Quick test:

```bash
curl -sk -u admin:$PW -X POST https://aeon-magick.local/api/target/reboot
```

The target's MAC is auto-discovered when it DHCPs. If you change targets,
the supervisor picks up the new MAC on next ARP refresh.

---

## Shared clipboard

Two-way text channel between agent and target, persisting across reboots.
64 KB cap. Useful for handing long credentials, log snippets, or context
to the target without going through the OS clipboard (which doesn't work
over USB-HID anyway).

```bash
# Get current clipboard contents
curl -sk -u admin:$PW https://aeon-magick.local/api/clipboard

# Write to clipboard
curl -sk -u admin:$PW -X PUT \
    -H "Content-Type: application/json" \
    -d '{"text": "secret-token-xyz"}' \
    https://aeon-magick.local/api/clipboard

# Type clipboard contents on the target via HID keyboard
# (skips emoji/smart-quotes; response has typed + skipped counters)
curl -sk -u admin:$PW -X POST \
    https://aeon-magick.local/api/clipboard/type
```

---

## File transfer

Two flavors:

**Pi-side staging area** at `/var/lib/aeon/files/` — files you upload via
the web UI or REST land here. Optionally exposed to the target as an HTTP
server on the USB ethernet (off by default; toggle via PUT
`/api/files/config`).

```bash
# List staged files
curl -sk -u admin:$PW https://aeon-magick.local/api/files

# Upload a file
curl -sk -u admin:$PW -X POST \
    -F "file=@local-file.bin" \
    https://aeon-magick.local/api/files

# Download a staged file
curl -sk -u admin:$PW \
    https://aeon-magick.local/api/files/local-file.bin -o copy.bin

# Delete
curl -sk -u admin:$PW -X DELETE \
    https://aeon-magick.local/api/files/local-file.bin
```

**USB-CDROM ISO library** at `/var/lib/aeon/iso/` — separate library used
to expose a chosen ISO to the target as a USB-CDROM device. Used for OS
install workflows.

```bash
# List ISOs
curl -sk -u admin:$PW https://aeon-magick.local/api/storage/isos

# Activate one (target sees a fresh CDROM insertion)
curl -sk -u admin:$PW -X PUT \
    -H "Content-Type: application/json" \
    -d '{"slug": "ubuntu-24.04.iso"}' \
    https://aeon-magick.local/api/storage/active

# Eject (empty slug)
curl -sk -u admin:$PW -X PUT \
    -H "Content-Type: application/json" \
    -d '{"slug": ""}' \
    https://aeon-magick.local/api/storage/active
```

---

## Privacy posture (DNS, Tor, I2P, VPN)

The device's outbound network can be layered: DNSCrypt-over-TCP for query
privacy, anonymized DNSCrypt relays so the resolver never sees client IPs,
Tor and/or I2P for transport anonymity, and a clearnet VPN (Mullvad, IVPN,
AzireVPN, generic WireGuard/OpenVPN, or Tailscale) as the outermost wrapper.

Each layer is independently togglable; the supervisor handles policy
routing and iptables so the layers compose cleanly. Endpoints all live
under `/api/network/` and `/api/firewall/`.

### Read overall state

```bash
curl -sk -u admin:$PW https://aeon-magick.local/api/network
```

Returns the full network config: USB ethernet mode, DNSCrypt settings,
anonymized relay state, Tor state, I2P state, current VPN provider, kill
switch, and currently-picked DNS servers + relays if auto-pick is on.

### DNSCrypt — the default DNS layer

DNSCrypt v2 wraps DNS queries in an authenticated, encrypted tunnel to a
chosen resolver. Unlike DoH, it doesn't leak SNI to the resolver's hosting
provider. We ship the full upstream resolver catalog (~226 entries) scored
on no-logs, no-filter, DNSSEC, jurisdiction, and trust level (1-5).

Two modes:

- **Auto** (default): the supervisor filters the catalog by criteria you
  set (no_logs, dnssec, outside-Five-Eyes, etc.) and picks ~5 candidates
  with operator diversity. dnscrypt-proxy's `lb_strategy = p2` then
  routes each query through the lowest-latency match.
- **Specific**: pin to one named resolver (e.g. `mullvad-doh`,
  `cloudflare`, `quad9-doh-ip4-port443-filter-pri`).

```bash
# Switch to auto + strict criteria + min trust 4
curl -sk -u admin:$PW -X PUT \
    -H "Content-Type: application/json" \
    -d '{
      "dnscrypt": {
        "enabled": true,
        "server_mode": "auto",
        "auto_criteria": {
          "no_logs": true,
          "dnssec": true,
          "no_filter": true,
          "outside_five_eyes": true,
          "outside_fourteen_eyes": false,
          "tor_friendly_port": false,
          "min_trust_score": 4
        }
      }
    }' \
    https://aeon-magick.local/api/network
```

If you flip `vpn.provider = "tor"`, the supervisor automatically sets
`force_tcp = true` in dnscrypt-proxy.toml and tightens `tor_friendly_port`
so we only pick resolvers whose stamps speak DNSCrypt over TCP on
ports Tor can actually reach (53/443/853).

### Anonymized DNSCrypt relays

Anonymized DNSCrypt routes queries through one of ~187 community relays
so the resolver never sees client IPs. Independent of base DNSCrypt
selection.

```bash
curl -sk -u admin:$PW -X PUT \
    -H "Content-Type: application/json" \
    -d '{
      "dnscrypt": {
        "anon_relays": {
          "enabled": true,
          "mode": "auto",
          "criteria": {
            "no_logs": true,
            "outside_five_eyes": true,
            "dnssec": true
          }
        }
      }
    }' \
    https://aeon-magick.local/api/network
```

Three-pass operator-diversity algorithm makes sure CryptoStorm or DNSCry.pt
don't monopolize the picks.

### Tor

Two routing modes:

- **split_tunnel** (default when enabled): only `.onion` lookups go via
  Tor's DNSPort, everything else routes normally. .onion works in any
  browser — no Tor Browser required.
- **transparent**: ALL TCP from the Pi+USB-clients is REDIRECTed through
  Tor's TransPort. Except `i2pd` itself, which keeps a direct outbound
  via a UID-based exemption.

Plus `over_vpn`: if a clearnet VPN is active, Tor's entry guard
connections route via the VPN tunnel (fwmark 0x100 → custom routing table
100). Nest depth: client → VPN → Tor.

```bash
# Enable split-tunnel Tor over an already-configured VPN
curl -sk -u admin:$PW -X PUT \
    -H "Content-Type: application/json" \
    -d '{
      "tor": {
        "enabled": true,
        "mode": "split_tunnel",
        "over_vpn": true
      }
    }' \
    https://aeon-magick.local/api/network
```

### I2P

I2P is always proxy-based — browsers must point at the daemon's HTTP
proxy explicitly. No transparent mode (the protocol doesn't lend itself
to it). Daemon binds on the USB ethernet so USB-attached clients can
use it too.

```bash
# Read i2pd status (installed, running, bound addresses, browser-hint URLs)
curl -sk -u admin:$PW https://aeon-magick.local/api/i2p/status

# Enable + set over-VPN
curl -sk -u admin:$PW -X PUT \
    -H "Content-Type: application/json" \
    -d '{
      "i2p": {
        "enabled": true,
        "over_vpn": false,
        "outproxy": "exit.stormycloud.i2p"
      }
    }' \
    https://aeon-magick.local/api/network
```

Set browser HTTP proxy to `http://<aeon-ip>:4444` to use it. The
`/network/i2p` page shows the exact URL the proxy is bound on plus a
copyable PAC-file URL.

### Clearnet VPN providers

Independent of Tor/I2P. Switch via the top-level provider setting.

```bash
# Switch to Mullvad (must have run the wizard first)
curl -sk -u admin:$PW -X PUT \
    -H "Content-Type: application/json" \
    -d '{
      "vpn": {
        "provider": "mullvad",
        "enabled": true,
        "kill_switch": true,
        "lan_bypass": true
      }
    }' \
    https://aeon-magick.local/api/network
```

Valid `provider` values: `none`, `tailscale`, `wireguard`, `openvpn`,
`mullvad`, `ivpn`, `azirevpn`.

### Provider wizards (Mullvad / IVPN / AzireVPN)

For Mullvad/IVPN/AzireVPN you run a one-shot wizard that
registers your WireGuard pubkey with the provider's API, caches their
server list, and lets you pick by country + Eyes-tier + privacy badges.

Full-process from CLI:

```bash
# 1. Cache the provider catalog (HQ country, Eyes tier, audit history)
curl -sk -u admin:$PW \
    https://aeon-magick.local/api/vpn/providers/catalog

# 2. Register your account (creates WG keypair, posts to provider, saves
#    secrets to /etc/aeon/vpn-providers/<name>.toml mode 0600)
curl -sk -u admin:$PW -X POST \
    -H "Content-Type: application/json" \
    -d '{"account_number": "1234567890123456"}' \
    https://aeon-magick.local/api/vpn/providers/mullvad/register

# 3. Fetch server list
curl -sk -u admin:$PW \
    https://aeon-magick.local/api/vpn/providers/mullvad

# 4. TCP-probe all servers, ranked by RTT
curl -sk -u admin:$PW -X POST \
    https://aeon-magick.local/api/vpn/providers/mullvad/pick_fastest

# 5. Select a specific server (writes /etc/wireguard/aeon0.conf,
#    restarts wg-quick@aeon0)
curl -sk -u admin:$PW -X PUT \
    -H "Content-Type: application/json" \
    -d '{"server_id": "se-sto-wg-001", "mode": "manual"}' \
    https://aeon-magick.local/api/vpn/providers/mullvad/select
```

Trust ratings per HQ country are based on Eyes-tier membership
(Five / Nine / Fourteen / outside) plus published audit history. The
`/network/vpn-providers` page shows them as badges.

---

## Firewall, NAT, port-forwards, DNS blacklist

All editable via REST + UI under `/network`.

```bash
# List user-defined firewall rules with hit counters
curl -sk -u admin:$PW https://aeon-magick.local/api/firewall/rules

# Add a port-forward
curl -sk -u admin:$PW -X POST \
    -H "Content-Type: application/json" \
    -d '{
      "name": "ssh-to-target",
      "kind": "port_forward",
      "proto": "tcp",
      "external_port": 2222,
      "dest_ip": "10.55.0.2",
      "dest_port": 22
    }' \
    https://aeon-magick.local/api/firewall/rules

# Delete by id
curl -sk -u admin:$PW -X DELETE \
    https://aeon-magick.local/api/firewall/rules/<id>

# DNS blacklist
curl -sk -u admin:$PW https://aeon-magick.local/api/dns/blacklist
curl -sk -u admin:$PW https://aeon-magick.local/api/dns/sources

# Subscribe to a curated source
curl -sk -u admin:$PW -X PUT \
    -H "Content-Type: application/json" \
    -d '{"enabled": true}' \
    https://aeon-magick.local/api/dns/sources/oisd
```

---

## Security console

Two read-only feeds for spotting weirdness:

```bash
# Last ~2h of iptables drops, parsed from kernel journal
# Each entry has src, dst, proto, ports, cause_tag (rule that dropped it)
curl -sk -u admin:$PW https://aeon-magick.local/api/security/blocked

# Audit log of all mutating ops + logins (newest first)
curl -sk -u admin:$PW "https://aeon-magick.local/api/security/audit?limit=200"

# Filter by actor or action prefix
curl -sk -u admin:$PW \
    "https://aeon-magick.local/api/security/audit?actor=admin&action=login"
```

The blocked-log UI exposes an "Allow this traffic" button per row that
deep-links to `/network` with a pre-populated firewall rule form.

---

## WiFi management

```bash
# Current state (connected SSID, radio on/off, AP-fallback flag)
curl -sk -u admin:$PW https://aeon-magick.local/api/wifi

# Scan
curl -sk -u admin:$PW -X POST https://aeon-magick.local/api/wifi/scan

# Connect (persisted across reboots)
curl -sk -u admin:$PW -X POST \
    -H "Content-Type: application/json" \
    -d '{"ssid": "my-home", "psk": "secret"}' \
    https://aeon-magick.local/api/wifi/connect

# Forget a network
curl -sk -u admin:$PW -X DELETE \
    https://aeon-magick.local/api/wifi/my-home
```

---

## Pi-side system controls (vs target controls)

Separate from `/api/target/*` (which acts on the connected target). These
act on the Raspberry Pi running this software.

```bash
# Pi health: uptime, CPU temp °C, load avg, mem, CPU count
curl -sk -u admin:$PW https://aeon-magick.local/api/system

# Reboot the Pi itself (web UI is gone for 30-60s)
curl -sk -u admin:$PW -X POST \
    https://aeon-magick.local/api/system/reboot

# Power off the Pi
curl -sk -u admin:$PW -X POST \
    https://aeon-magick.local/api/system/poweroff
```

Agents almost never want these — they want `/api/target/reboot` instead.
This is for the human operator.

---

## Macros — store and run named action sequences

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

---

## Prompts — stored agent playbooks

```bash
curl -sk -u admin:$PW https://aeon-magick.local/api/prompts
curl -sk -u admin:$PW https://aeon-magick.local/api/prompts/agent-quickstart
curl -sk -u admin:$PW -X PUT --data-binary @playbook.md \
    https://aeon-magick.local/api/prompts/playbook
curl -sk -u admin:$PW -X DELETE https://aeon-magick.local/api/prompts/playbook
```

Prompts are plain `.md` or `.txt`. The device ships `agent-quickstart` and
`macos-shortcuts` you can crib from.

---

## API tokens (issue, list, revoke)

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
| `admin` | Everything including token management, network mutations, password change, Pi reboot |
| `full` | All HID input, target power, clipboard, files, macros, snapshots; cannot manage tokens or reboot Pi |
| `macros` | Run pre-stored macros + read state/snapshots; no raw HID |
| `read` | Read state, snapshots, list macros/prompts; no mutations |

Token management endpoints (`/api/auth/tokens`, `/api/auth/change-password`)
require Admin scope or a session cookie from web login.

---

## MCP (Model Context Protocol) endpoint

For agents that speak MCP (Claude Desktop, the MCP SDK clients, anything
built on `@modelcontextprotocol/sdk`), the same TLS + Basic-auth endpoint
also speaks Streamable HTTP MCP at `/api/mcp`. Configure your client to
point at:

```
https://aeon-magick.local/api/mcp
```

with Basic auth `admin:<password>` and TLS verification off (self-signed
cert). The server advertises 55 tools, grouped below.

### Live control + state

| Tool | What it does |
|---|---|
| `state` | combined streamer + HID status |
| `snapshot` | one JPEG of the target's screen, returned as an MCP image block |
| `type_text` | type a string |
| `key_chord` | fire a key combo |
| `click` | left/right/middle click |
| `move_cursor` | relative cursor move (int8 deltas) |
| `click_at` | click at an absolute screen point — `x`,`y` as fractions 0..1; needs `generic-absolute` |
| `move_pointer` | move the absolute pointer to a point without clicking (hover) |
| `drag` | press → move → release between two absolute points |
| `scroll` | wheel scroll |
| `set_persona` | hot-swap HID identity |
| `release_all` | panic-release everything |
| `list_macros` | enumerate stored macros |
| `run_macro` | execute a stored macro by name |

### Target machine power

| Tool | What it does |
|---|---|
| `target_info` | MAC, iface, available actions |
| `target_power_tap` | 200 ms HID power press — graceful shutdown dialog |
| `target_power_hold` | 8 s HID power hold — hard power-off |
| `target_wake` | Send WoL magic packet |
| `target_reboot` | Hold + wait + WoL, canonical "reboot now" |

### Clipboard + files

| Tool | What it does |
|---|---|
| `get_clipboard` | Read shared text buffer |
| `set_clipboard` | Write to it |
| `type_clipboard` | Paste clipboard contents to target via HID |
| `list_files` | Enumerate `/var/lib/aeon/files/` |
| `read_file` | Read a staged file as base64 |
| `delete_file` | Remove a staged file |
| `list_isos` | Enumerate ISO library |
| `set_active_iso` | Mount or eject the USB-CDROM |

### DNS, Tor, I2P, VPN

| Tool | What it does |
|---|---|
| `dnscrypt_state` | Full DNSCrypt + relay catalog + current picks |
| `set_dnscrypt_criteria` | Update auto-picker criteria |
| `set_anonymized_relays` | Update anonymized-relay config |
| `set_tor` | Toggle Tor + mode + over_vpn |
| `set_i2p` | Toggle I2P + over_vpn |
| `i2p_status` | i2pd daemon state + bound addresses |
| `vpn_state` | Clearnet VPN config |
| `set_vpn_provider` | Switch provider |
| `vpn_providers_catalog` | Static provider catalog with privacy badges |
| `vpn_provider_state` | Setup state of one provider |
| `vpn_provider_select` | Pick a specific server |
| `vpn_provider_pick_fastest` | TCP-probe ranking |

### System + security

| Tool | What it does |
|---|---|
| `pi_system_info` | Pi health: temp, load, mem, uptime |
| `pi_reboot` | Reboot the Pi (rare; use `target_reboot` instead) |
| `wifi_state` | Current SSID + radio + AP-fallback flag |
| `wifi_scan` | Nearby SSIDs |
| `wifi_connect` | Join a network |
| `network_status` | Combined network-layer snapshot |
| `security_metrics` | Throughput + drop counters + top clients |
| `firewall_rules` | User-defined rules + hit counters |
| `dns_blacklist` | Blacklist + recent query log |
| `dns_sources` | Subscription sources |
| `ssh_keys` | Trusted SSH keys |
| `audit_log` | Mutation + login history, filterable |
| `blocked_log` | Recent iptables drops with cause attribution |
| `list_tokens` | Active API tokens |
| `issue_token` | Create a new token (plaintext returned ONCE) |
| `revoke_token` | Delete by id |

It also exposes stored macros + prompts as MCP **resources**
(`aeon://macros/<name>`, `aeon://prompts/<name>`) and prompts as MCP
**prompts** for the `prompts/list` + `prompts/get` methods.

---

## Recipes for AI agents

### Recipe — vision-driven control loop

The proven pattern (used by Celina, our local OpenClaw anchor agent):

1. **`state`** to confirm everything is reachable + the right persona is loaded.
2. **`snapshot`** to fetch a frame.
3. Reason over the frame and decide what to do.
4. **`type_text` / `key_chord` / `click_at` / `scroll`** to act. To click a
   specific element, switch to the `generic-absolute` persona once and use
   **`click_at`** with the target's fractional coordinates — far more reliable
   than relative `move_cursor`. Each call is atomic; you don't track partial
   press state.
5. Optionally wait briefly, then **`snapshot`** again to confirm what changed.
6. Loop.

Latency budget: snapshot ~50-150 ms (LAN), reasoning is up to your model,
HID action ~10-30 ms.

### Recipe — OS install from ISO

1. **`list_isos`** to find what's available.
2. **`set_active_iso`** with the chosen slug. Target sees a CDROM
   insertion on next USB enumeration.
3. **`target_reboot`** to power-cycle the target.
4. Loop on **`snapshot`** + **`key_chord`** to drive BIOS/UEFI to boot
   from the CDROM.
5. Continue the visual loop through the installer.
6. After post-install reboot: **`set_active_iso`** with empty slug to
   eject, then **`target_reboot`** to boot from the freshly-installed OS.

### Recipe — paste a long credential

USB HID can't access the target's OS clipboard, but you can stage text
on-device and type it through HID:

1. **`set_clipboard`** with the credential text.
2. Focus the password field on the target (`click` at coords).
3. **`type_clipboard`** — the device types the stored buffer via HID.

Pros over `type_text` directly: clipboard persists if you need to retry,
and the credential isn't in your prompt history if a token-scope
restriction means you can't see it after staging.

### Recipe — privacy-paranoid setup

Goal: VPN → Tor split-tunnel for `.onion`, DNSCrypt over Mullvad's DoH
endpoint with anonymized relays, DNS blacklist on.

1. **`set_vpn_provider`** with `mullvad` (after wizard via REST).
2. **`set_dnscrypt_criteria`** with `min_trust_score=4`, `no_logs=true`,
   `dnssec=true`, `outside_fourteen_eyes=true`.
3. **`set_anonymized_relays`** with `enabled=true`, `mode=auto`, same criteria.
4. **`set_tor`** with `enabled=true`, `mode=split_tunnel`, `over_vpn=true`.
5. **`network_status`** to confirm public IP is the VPN exit and DNS picks
   match the criteria.

### Recipe — diagnose a "target unreachable" report

1. **`state`** — is `keyboard.online`/`mouse.online` true? If false, USB-C
   data cable is the suspect.
2. **`target_info`** — has the MAC been auto-discovered? If not, the
   target hasn't DHCPed yet, meaning either USB enumeration failed or
   it hasn't booted past BIOS.
3. **`snapshot`** — what's the screen showing? Black = no HDMI signal
   reaching the capture device.
4. **`security_metrics`** + **`blocked_log`** — has the firewall been
   accidentally dropping target traffic?

### Recipe — schedule a reboot during a long task

Use the macro system to chain a reboot + wait + snapshot:

```toml
name = "controlled-reboot"
description = "Reboot target, wait for login screen, take a snapshot."

[[steps]]
type = "snapshot"

[[steps]]
type = "key"
keys = ["CTRL", "ALT", "DELETE"]   # Windows graceful prompt

[[steps]]
type = "wait"
ms = 90000  # 90 s for cold boot

[[steps]]
type = "snapshot"
```

Then trigger via `run_macro` from the agent.

---

## SSH access

`ssh admin@aeon-magick.local`. Default password
`aeon-default-change-me` — change it with `passwd` after first login.

Use SSH for inspecting logs:

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

# Reset network config to ship defaults
sudo cp /usr/share/aeon/defaults/network.toml /etc/aeon/network.toml
sudo systemctl restart aeon-net-services aeon-supervisor

# Forget WiFi and re-trigger the AP setup
sudo nmcli con delete <your-ssid>
sudo systemctl restart NetworkManager
# Watcher will spin up `aeon-setup` AP within 90 seconds.
```

Full factory reset: re-flash the SD card. Generates fresh cert; the
operator picks a new password in the setup wizard.

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
