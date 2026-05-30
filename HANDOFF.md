# HANDOFF.md — AEON Magick AI Computer Control

> Portable session-handoff for picking the project up in a fresh Claude
> instance (e.g. after a billing/tenant change). Snapshot taken at
> **v65**, git `3b396e4`, on branch `main` (pushed to
> `AEON-7/aeon-magick-ai-computer-control`). Working tree clean.
>
> Read this first, then `AGENTS.md` (operation), `ARCHITECTURE.md`
> (design), `BUILDING.md` (bake recipe).

---

## 1. What this project is

A **Raspberry Pi 4 KVM-over-IP appliance** for AI-agent (or human)
remote control of a *target* computer. The Pi:

- **Pretends to be USB peripherals** to the target (keyboard, mouse,
  absolute pointer, consumer-power button) via the USB-C gadget port.
- **Captures the target's HDMI** through an Elgato Cam Link 4K (or
  MS2109 dongle) on a USB-3 port.
- **Serves an HTTPS web UI + REST API + MCP server** so a human in a
  browser or an AI agent over MCP can see the screen and drive input.
- Adds a deep **privacy/networking stack**: DNSCrypt, anonymized
  relays, Tor (split/transparent), I2P, commercial VPN wizards
  (Mullvad/IVPN/AzireVPN), firewall/NAT editor, USB-ethernet
  passthrough with isolation modes.

The tagline: *"Plug it in, give it eyes, give it hands, hand the keys
to an AI."*

---

## 2. Repo layout

`/Users/albert/aeon-magick-ai-computer-control` — Cargo workspace + web + image overlay.

| Path | What |
|---|---|
| `aeon-supervisor/` | **Rust.** HTTPS frontend: web UI host, REST API, WebSocket, MCP server, auth (argon2 + signed cookies), all the network/VPN/firewall logic. The brain. |
| `aeon-streamer/` | **Rust.** Capture + encode supervisor. Spawns ustreamer (MJPEG fast path) or ffmpeg (NV12→H.264 via `h264_v4l2m2m` on Pi4 / `libx264` elsewhere). `h264_pipe.rs` reads Annex-B access units → broadcast. |
| `aeon-hid/` | **Rust.** USB gadget daemon. Builds the configfs HID descriptors per persona, drains endpoints, applies input ops atomically. |
| `aeon-web/` | **SvelteKit** static build (adapter-static). The dark-theme UI. Built with `npm run build`, output copied into the image. |
| `image-builder/stage-aeon/` | **pi-gen overlay.** `01-base/files/bin/` holds cross-compiled aarch64 binaries; `01-base/files/web/` the SvelteKit build; `02-services/files/` the systemd units + shell scripts (`aeon-net-services.sh`, `aeon-usb-net.sh`, `aeon-netwatch.sh`, `aeon-vpn-status.py`, `streamer.toml`, `hid.toml`). |
| `scripts/` | `build-binaries.sh` (cross-compile via `cross`), `build-web.sh` (npm build + stage), DNSCrypt/relay catalog regenerators. |

**Build toolchain:** `cross` (Docker-based aarch64 cross-compile) for
Rust; `npm` for web. macOS dev box; Docker Desktop required for both
`cross` and the pi-gen bake.

---

## 3. Current state — shipped through v65

Everything below is **built, committed, and in the v65 image**.

- **Input:** keyboard, relative mouse, **absolute pointer**
  (`generic-absolute` persona), click-and-drag (held-button state),
  consumer power button, scroll. Atomic ops — dropped packets can't
  wedge the target.
- **5 HID personas:** `generic-composite` (relative, Linux-safe
  **default-recommended**), `generic-absolute` (abs pointer for agents),
  `logitech-mx`, `apple-magic`, `apple-magic-stable`.
- **Streaming:** MJPEG (ustreamer/ffmpeg) **and H.264-over-WebSocket
  (WebCodecs)** — see §4. `match_source` native-res capture, 15/30/60
  fps (clean divisors of 60 fps capture), NV12 capture path.
- **MCP server:** **55 tools** at `/api/mcp` (Streamable HTTP). Incl.
  `click_at` / `move_pointer` / `drag` (screen-fraction absolute
  targeting), snapshot, type/key/scroll, target power, clipboard,
  files, ISO library, DNSCrypt/Tor/I2P/VPN control, Pi system, WiFi,
  tokens, blocked-log.
- **Networking/privacy:** DNSCrypt (226-resolver catalog, criteria
  auto-pick), anonymized relays (187 catalog), Tor (split-tunnel +
  transparent + over-VPN), I2P, **VPN wizards for Mullvad/IVPN/AzireVPN
  all working end-to-end** (see §5), firewall/NAT/port-forward editor
  with system-rule visibility + diagnostics.
- **WiFi page** (`/wifi`): client/AP/off modes, saved-network mgmt
  (auto-connect toggle + forget), AP credentials (shared with
  boot-fallback AP), radio toggle.
- **Pages:** `/`, `/wifi`, `/network` (+`/vpn-providers`, `/i2p`),
  `/security`, `/dns`, `/storage`, `/files`, `/clipboard`, `/macros`,
  `/prompts`, `/tokens`, `/ssh-keys`, `/audit`, `/system`, `/target`,
  `/setup/wifi`.

---

## 4. ⭐ The latency story (read before touching streaming)

**The H.264 feature the user keeps asking for is ALREADY BUILT and
shipped in v65.** Do not re-implement it. The remaining work is making
it the *default* and verifying it on the user's hardware.

How it works today:
- Streamer produces H.264 only when `streamer.toml` has
  `format = "h264"`. **The shipped default is still `format = "mjpeg"`.**
- The web client (`+page.svelte`) auto-selects H.264 whenever the
  browser exposes `VideoDecoder` (WebCodecs) — i.e. **Chrome / Brave /
  Edge YES, Safari / Firefox NO**. It connects to
  `wss://…/api/streamer/ws`, decoded by `H264Canvas.svelte`, with
  automatic `fallback` to MJPEG if WebCodecs is missing.
- `bridge_h264` in `aeon-supervisor/src/proxy.rs` forwards only the
  latest keyframe-anchored tail (hard cap 60 frames, drop-to-newest) so
  congestion can't balloon latency. This was the v65 fix.

**Why the user still saw 2–3 s** while testing fps/quality sliders: those
sliders (`PUT /api/streamer/config`) only touch `fps` + `jpeg_quality`,
which are **MJPEG-only knobs**. If you're tuning them and seeing effects,
you're on the MJPEG path — either on `format=mjpeg` (the default) or in a
non-WebCodecs browser (Safari).

**Empirically established (v65 session):**
- The dominant latency factor was the **Ethernet network path**, not the
  codec or the 4K scale. Moving the Pi from Ethernet (.55) to **WiFi
  (192.168.1.56) dropped latency to ~0** with CPU unchanged.
- JPEG quality has ~no effect on latency; **frame rate does** (lower fps
  = less buffer depth to drain). This is a buffering problem, not a
  bandwidth problem.

**So the path to low latency is:** (a) `format = "h264"`, (b) a WebCodecs
browser (Brave/Chrome), (c) prefer WiFi over the current Ethernet path.
The concrete next task is flipping the default + adding a UI/runtime
format switch — see §7.

---

## 5. VPN wizard status — DONE (verified at HEAD)

All three commercial providers work end-to-end with just a credential
paste. Endpoints were wrong in v59 and fixed in v63.1 (confirmed live):

| Provider | Auth | Key endpoint | Notes |
|---|---|---|---|
| **Mullvad** | 16-digit account # | `POST /accounts/v1/devices` (after `/auth/v1/token`); relays `GET /public/relays/wireguard/v1` | Was already correct. |
| **IVPN** | account ID `ivpn-…` | `POST /v4/session/new` (NOT v5 — v5 is 404). Body field `wg_public_key`, response `wg_ip`. | Fixed in v63.1. |
| **AzireVPN** | dashboard API token | `POST /v3/ips` with `{"key": "<pubkey>"}` (NOT `/v3/keys` — that's 404; the endpoint is confusingly called "IPs"). Servers `GET /v3/locations` (`pool` hostname used as WG Endpoint). | Fixed in v63.1 — the GL.iNet router firmware uses the same `/v3/ips` path. |

The interrupted standalone v63.1 bake **does not matter** — v64 and v65
both built on top of v63.1 and incorporate these fixes. `ivpn.rs` is on
`/v4` and `azirevpn.rs` is on `/v3/ips` at HEAD. ✓

---

## 6. Hard-won landmines (don't re-learn these the hard way)

1. **`h264_v4l2m2m` + `nv12` = green macroblocks.** Capture NV12 (saves
   ~170% CPU at 1080p vs YUYV software-convert) but the encoder input
   must stay **`yuv420p`**. Feeding nv12 straight to the Pi4 encoder
   corrupts output.
2. **`logitech-mx` persona wedges `aeon-hid` on Linux targets** —
   `hid-logitech-dj` claims the device but doesn't drain endpoints.
   `generic-composite` is Linux-safe. **`hid.toml` still ships
   `logitech-mx` as default** — open item to flip it (§7).
3. **Charging-only USB-C cables** power the Pi but pass no data →
   `keyboard.online=false`. Needs a data-capable USB-A→C or C→C cable.
4. **Cam Link negotiates 4K with macOS** "Looks like 1080p" Retina
   scaling. Capture works but wastes CPU; pick a true 1080p mode or use
   `match_source`.
5. **Safari + iCloud Private Relay + browser DoH break Tor mode** and
   block WebCodecs (so no H.264). For both Tor and low-latency H.264,
   use **Brave/Chrome/Firefox** (Firefox lacks WebCodecs though). The
   `/network` page has a browser-tuning callout.
6. **Tor iptables rules tagged `aeon-vpn`, not `aeon-tor`** — the
   firewall system-rule listing matches any `aeon-*` comment except
   `aeon-fw` (user rules). If "0 system rules" shows, check the
   `diagnostics` block: zero total = can't reach iptables; non-zero
   total but zero aeon = nothing aeon-tagged installed (VPN/Tor/USB-net
   all off).
7. **Svelte reactive ordering:** a `$:` block that reads another `$:`
   var *through a function call* hides the dependency from Svelte's
   tracker → runs before the dep is initialized → blank page. Declare
   derived state in dependency order and read deps directly. (This
   caused the v62 black-screen, fixed in v62.1.)
8. **pi-gen bake copy-out exits 1** because `~/pi-gen/deploy` is
   root-owned. **Benign** — extract the image via `docker cp` instead.

---

## 7. Open to-do list (prioritized for the new instance)

**P0 — finish the latency win (the user's top ask):**
1. **Flip `format = "h264"` default** in
   `image-builder/stage-aeon/02-services/files/streamer.toml`, OR better,
   **add a runtime format switch** to `streamer_config.rs`
   `put_config` (currently only fps + jpeg_quality) + a UI toggle on
   `/system`. Then the device defaults to the low-latency path on
   Chrome/Brave.
2. Verify H.264 end-to-end on the user's Pi (WiFi 192.168.1.56, Brave) —
   confirm sub-second and that the drop-to-newest bridge holds under
   congestion.

**P1 — known correctness fix:**
3. **Default persona → `generic-composite`** in `hid.toml` (currently
   `logitech-mx`, which wedges on Linux targets). Needs a re-bake.

**P2 — UX completeness:**
4. **Stage-2 web pointer:** wire the canvas pointer → `/hid/move_abs`
   and add `generic-absolute` to the persona dropdown, so point-and-click
   works for *humans* in the browser (today abs pointer is agent/API-only).
5. **Web terminal (PTY in browser)** — task #59, pending since v27. The
   one long-standing unbuilt feature.

**Housekeeping before any bake:**
6. **Bump `IMG_DATE`** in `~/pi-gen/config` (pinned to `v65`) to the new
   version, or the output filename collides.

---

## 8. Coordinates / cheat-sheet

| Item | Value |
|---|---|
| Repo | `/Users/albert/aeon-magick-ai-computer-control` |
| Remote | `AEON-7/aeon-magick-ai-computer-control` (GitHub), branch `main` |
| HEAD | `3b396e4` (docs sync) — source is `5f7db06` (v65) |
| Device | Pi 4, **WiFi 192.168.1.56**, persona `generic-composite` |
| Current image | `~/Documents/aeon-magick/aeon-magick-v65.img.xz` · 595 MB · md5 `94cf982cbb52d9dfd5ad0400f916256b` |
| Image archive | `~/Documents/aeon-magick/aeon-magick-v*.img.xz` (v60→v65) |
| pi-gen dir | `~/pi-gen` (config has `IMG_NAME=aeon-magick`, `IMG_DATE="v65"`) |
| SSH | `admin@aeon-magick.local` (or `.56`), default pw `aeon-default-change-me` |
| Admin portal pw | stashed in `~/.config/aeon/env` (mode 600) — never paste in chat |
| MCP endpoint | `https://aeon-magick.local/api/mcp` (Basic auth, TLS verify off) |
| Tool count | 55 MCP tools |

**Build + bake recipe (confirmed working):**
```bash
cd /Users/albert/aeon-magick-ai-computer-control
bash scripts/build-binaries.sh          # cross-compile aarch64 (needs `cross` + Docker)
bash scripts/build-web.sh               # npm build + stage into image-builder
rsync -a --delete image-builder/stage-aeon/ ~/pi-gen/stage-aeon/
# bump IMG_DATE in ~/pi-gen/config first!
cd ~/pi-gen && sudo CLEAN=1 ./build-docker.sh
# copy-out may exit 1 (root-owned deploy/) — benign. Extract:
#   docker cp pigen_work:/pi-gen/deploy/<img>.img.xz ~/Documents/aeon-magick/
# verify: xz -t <img>.img.xz ; md5 -q <img>.img.xz
```

**Local verify before bake:** `cargo check -p aeon-supervisor` and
`cd aeon-web && npm run check` (both must be clean — svelte-check
catches the reactive-ordering class of bug).

---

## 9. Conventions / preferences (carry forward)

- Version every shippable change `vNN`, tag it, one image per version,
  archive with md5. Commit messages are detailed (what + why + landmine).
- Co-author trailer on commits: `Claude Opus 4.x`.
- Secrets live in local `.env` / `~/.config/aeon/env` — **never pasted in
  chat**; argon2 hashing + HMAC-signed cookies for auth.
- Never amend/force-push shared history; never skip git hooks.
- Personas synthesize in-voice by default (user preference from memory).

---

## 10. Model recommendation for vision-driven control (FYI)

For an AI agent *driving the GUI*, the deciding capability is **GUI
grounding (coordinate accuracy)**, not general image IQ — generalist
VLMs score <2% on ScreenSpot-Pro vs ~28%+ for grounding-tuned models.
**Recommended: Qwen3-VL** (30B-A3B MoE for speed, or 32B dense for max
grounding) over Gemma/generalists — its native coordinate output maps
directly onto `click_at`'s screen-fraction API. Can be benched on the
user's DGX Spark + vLLM.
