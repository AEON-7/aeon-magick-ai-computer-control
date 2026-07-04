<p align="center">
  <img src="docs/images/banner.png" alt="Aeon Magick Orb — see and command any computer, and your entire AI lab" width="100%" />
</p>

# 🔮 Aeon Magick Orb

> ## _The Universe's Strangest Peripheral._
> **Every Aeon master has one.** _(we also just call it the **cursed HID**.)_

### Gaze into the Orb and you **see and command any computer — and your entire AI lab.** It hands any AI agent a **physical presence** at any machine: real eyes, real hands, **zero software on the target.** It just looks like a USB keyboard and mouse.

> 🤖 **Every capability is provisioned to your AI agent over a REST API _and_ a first-class MCP endpoint** — drop-in **agentic control** for *any* model and *any* agent. No SDK, no plugin, no permission prompt.

One small box you can hold in a pocket, and you can:

- 🖥️ **View and control your entire AI infrastructure** — every gateway, DGX Spark, and model server, from one console.
- 🤖 **Grant your AI agent a physical presence** at any computer — vision in, keyboard / mouse / gesture out, from the **BIOS** on up.
- 🛡️ **Secure your network traffic** — one-click VPN · Tor · I2P · encrypted **no-log DNS**, filterable by latency *and* jurisdiction.
- 🌍 **Access it all from anywhere** — one-key Tailscale mesh; carry the Orb to any network and your whole lab comes with it.
- 🧠 **Build up your Pantheon** of AI agent personas — each with a soul, a voice, a corpus, and a face — and summon them to act.
- 💾 …**deploy models**, **orchestrate containers**, **install an OS lights-out**, and **back the whole config up, encrypted**. _(keep scrolling 👇)_

> **All it takes:** a **Raspberry Pi 4 or Pi 5**, any HDMI capture stick (tested with an Elgato Cam Link 4K), and a USB-C data cable. [Full hardware list ↓](#hardware-youll-need)

This isn't just Agentic AI. It's **_Robo_-Agentic AI** — the disembodied, given a body.

```
        ┌──────────────────┐                          ┌──────────────────┐
        │   Target host    │                          │  You / your AI   │
        │  (any OS — Mac,  │   HDMI → Cam Link 4K     │   ┌────────────┐ │
        │   Windows, Linux,│ ───────────────────────► │   │ web UI on  │ │
        │   BIOS, FileVault│                          │   │ laptop or  │ │
        │   prompt, …)     │ ◄─────────────────────── │   │ AI agent   │ │
        └──────────────────┘   USB-C OTG ← Pi gadget  │   │ kbd+mouse+ │ │
                                emulating Logitech /  │   │ trackpad + │ │
                                Apple keyboard+mouse  │   │ gestures   │ │
                                                      │   └────────────┘ │
                                                      └──────────────────┘
```

Nothing to install. Nothing to grant. **Pull the plug and access is instantly,
physically gone.**

---

## Everything the Orb does, at a glance

One box is a computer-control rig, an AI-infrastructure jump box, a persona studio,
a privacy router, and a lights-out KVM. Tap any capability to jump to the deep dive.

| | Superpower | Why it's a superpower |
|---|---|---|
| 🖥️ | **[Zero-software computer control](#zero-software-computer-control-for-any-agent)** | See real pixels, move the mouse, type, click — on **any** machine, **any** OS, even **pre-OS**. Nothing installed on the target, ever. |
| 🔌 | **[The cursed HID](#the-cursed-hid--it-just-looks-like-a-keyboard)** | To the target it's a boring USB keyboard + mouse. No SDK, no agent in your files, no permission prompt. Powered over the same cable — no PSU. |
| 🧠 | **[Build a pantheon of AI personas](#build-your-pantheon-of-ai-personas)** | Each agent gets a Soul, Identity, corpus, profile picture, and its **own designed-or-cloned voice**. Summon one specialist, or call several in parallel and let them talk. |
| 🛰️ | **[Centralized AI-infra jump box](#the-ultimate-ai-jump-box)** | One console over every gateway, DGX Spark, and model server — live GPU / CPU / RAM, temperature, container counts, reboot / wake. |
| 📊 | **[Agent dashboard + telemetry](#agent-dashboard--token-telemetry)** | A bird's-eye view of resource consumption across your whole lab, plus per-agent token burn over any time span. |
| 🔗 | **[Tailscale mesh, carry-anywhere](#reach-it-from-anywhere--one-key-tailscale-mesh)** | One-key enrollment puts the box on your tailnet forever. Reach it — and everything it can SSH to — from anywhere, at a stable address that never changes. |
| 🚪 | **[Privacy-stack exit node](#exit-node--share-the-whole-privacy-stack)** | Flip the box into a Tailscale exit node and every device behind it inherits its VPN **and** encrypted DNS **and** Tor / I2P. |
| 🕶️ | **[Surveillance-resistance appliance](#one-click-surveillance-resistance)** | One-click VPN · Tor · I2P, filterable by **latency and jurisdiction**. Choose **No-Eyes** for the most off-grid path you can build. |
| 🔒 | **[Encrypted, no-log DNS](#encrypted-no-log-dns)** | DNSCrypt v2 / DoH with no plaintext leaving the box — filter resolvers by latency and by countries outside 5 / 9 / 14-Eyes, to match your threat model. |
| 🖧 | **[Multi-terminal window manager](#multi-terminal-window-manager)** | Concurrent SSH panes — many systems at once, or many shells on one — without leaving the dashboard. |
| 🚀 | **[One-click model deploy](#one-click-model-deploy-to-your-dgx-spark)** | Push an AEON model to your DGX Spark and it pulls the container + weights and spins it up. Or hand the keys to an agent and let it deploy for you. |
| 🐳 | **[Container orchestration](#container-orchestration--monitoring)** | Every compose project and container — even the ones you've shut down — with live stats and up / down control. Housekeeping, sorted. |
| 💿 | **[Lights-out KVM](#lights-out-kvm--mount-an-iso-boot-to-bios-install-an-os)** | Mount an ISO, reboot into the BIOS / boot menu, and install an **entire operating system remotely**. True out-of-band control — for you and your agents. |
| 🎛️ | **[Hot-swap HID personas](#hid-personas)** | Five identities, including an **absolute pointer** for pixel-perfect agent clicks and an Apple Magic multi-touch trackpad. |
| 🔑 | **[Scoped keys + SSH, provision/revoke](#scoped-access--provision-or-revoke-in-one-click)** | Per-agent API tokens (instant audit trail) and SSH grants at the privilege level you choose. Revoke a key — or pull the plug — and it's over. |
| 🛡️ | **[Full audit + security console](#full-audit-trails--security-console)** | Every privileged action attributed to an actor, beside live firewall counters and an intrusion-heuristics monitor. |
| 🤖 | **[Agents-first: REST + MCP](#agents-first-interfaces-rest--mcp)** | Every capability is a `curl` POST **and** a first-class MCP tool. Drop one URL into your MCP client and your agent operates the whole rig. |

---

## Why the Orb matters

AI agents today are **trapped behind the integrations someone wired for them** — an
API here, a browser plugin there. They can touch the handful of apps you connected,
and nothing else. The Orb hands an agent a **universal body**: real eyes and hands on
*any* machine, *any* OS, even *pre-OS*, with **nothing installed** and nothing to
detect. That's the leap from *"the apps we integrated"* to **"literally any
computer"** — the missing limb of agentic AI.

And the same box is a **single pane of glass over your whole lab** — live GPU / CPU /
RAM, per-agent token burn, model deploys, container control, terminals, audit. So it
isn't just *reach*: it's **observability + orchestration + control**, unified in one
thing you can carry. Because every capability is reachable over **REST + MCP**, your
agents don't just *watch* the infrastructure — they can *run* it.

Most "AI computer use" tools live *inside* the machine they drive — a browser
extension, an accessibility shim, a screen-recorder daemon, an SDK linked into your
app. They're **chatty**: every screen reveals the agent, every approach asks
permission, and none can touch the BIOS, the FileVault unlock, the Windows OOBE, the
firmware updater, or the login wall a fresh Linux install opens with. The Orb drives
the machine **itself** — vision-first, from the outside.

| Typical agent tool | Aeon Magick Orb |
|---|---|
| Installs software / an SDK on the target | **Nothing installed.** The target sees a USB keyboard + mouse. |
| Needs OS APIs, permissions, a logged-in session | **Works pre-OS / BIOS / lock screen** — it's just hardware. |
| An agent lives in your files; revoking it is fiddly | **Failsafe kill switch:** pull the plug or revoke the key → access is *instantly* gone. |
| Screen-scrapes through accessibility trees | **Sees real pixels** over HDMI; clicks exact coordinates. |
| Drives one app, one browser | **Drives the whole machine** — and a whole fleet of them. |

**Full agent empowerment, monitoring, and control** is the through-line of
everything below.

---

## Zero-software computer control for any agent

Plug the box between any machine and the capture side. The target gets a USB-HID
keyboard + mouse / trackpad it trusts; the box gets the target's HDMI through a
video-capture device. The web UI streams the screen to your browser; an AI agent
reads frames and sends back keyboard / mouse / gesture events. The target never
knows it's been politely possessed — there is **nothing to install and nothing to
grant permission to.**

![The live computer-control view — HDMI in, USB-HID out, streamed to the browser](docs/images/control.png)

- 🎹 **Keyboard / mouse / trackpad over USB-C OTG** — five hot-swappable HID
  personas, including an **absolute pointer** (`generic-absolute`, ideal for AI
  agents) and an Apple Magic Keyboard + Trackpad multi-touch descriptor.
- 🎥 **HDMI capture** via Elgato Cam Link 4K (or any UVC device) — MJPEG or
  low-latency **H.264 / WebCodecs**, native source resolution, 1080p60 capable.
  Two watchdogs respawn the pipeline within ~5 s of an HDMI hot-plug or resolution
  change, so the feed heals itself when the target wakes from sleep.
- 🎬 **Record the session** to MP4 on demand — proof of exactly what an agent did.
- ⚡ **Works pre-OS** — BIOS, FileVault, Windows OOBE, firmware screens. To the
  target it's just a perfectly boring peripheral.

> **This is an agent-control device first.** Plug the USB into a data-capable port,
> the capture card into the target's video out — and that's it. The box is **powered
> over that same USB link** (tested with an Elgato Cam Link), so there's **no
> external power supply** to find. Want to revoke an agent? **Pull the plug.** No
> diving into files to rotate keys on the client (though if you suspect the target
> is compromised, that's never a bad idea too).

---

## The cursed HID — it just looks like a keyboard

The most subversive thing about the Orb is how *ordinary* it looks. The target
enumerates a **USB-HID keyboard, mouse, and trackpad** — a Logitech Unifying
Receiver, or an Apple Magic Keyboard, depending on the persona you pick — and
nothing else. There is no kernel module to load, no agent process to spot in a task
manager, no entry in an MDM inventory. Just the world's most boring peripheral,
quietly handing an AI full **video, keyboard, and mouse** access to whatever it's
plugged into.

That's the whole trick, and it's why it works where everything else can't: a BIOS
doesn't run your SDK, a FileVault prompt doesn't expose an API, a freshly-imaged
Linux box has no agent installed — but every one of them trusts a USB keyboard.

---

## Build your pantheon of AI personas

Your gateway's whole **pantheon** — every agent persona — is a live, clickable
roster in the console, and spinning up a brand-new one is a single **+ New
persona** away.

![The pantheon — your full roster of personas at a glance; open any card to build it out, or + New persona to add one](docs/images/pantheon.png)

Open any persona and build it out, end to end:

- 📜 **Soul** (`SOUL.md`) — the system prompt that *is* the agent.
- 🪪 **Identity** (`IDENTITY.md`) — name, era, domain, emoji.
- 📚 **Corpus** — a private, persistent knowledge base you can **upload, edit,
  browse, and selectively prune** straight from the console (large files stream in
  with no size cap).
- 🖼️ **Profile picture** → pushed straight to the agent's **Matrix avatar.**
- 🗣️ **A voice of its own** — see below.

![A persona's editor (top) — Matrix avatar, a scoped Orb API key, a human-only SSH grant, and skills](docs/images/agent-detail.png)

### A real voice — designed or cloned

Give each persona its own voice, two ways:

- **Voice designer** — write a natural-language descriptor (e.g. *"Lofty, measured,
  eloquent male voice; teacherly and grand"*) and the TTS engine synthesizes a
  matching voice.
- **Voice cloning** — upload a short, clean `.wav` (~15–30 s is the sweet spot) and
  the persona speaks in that cloned voice. Pick from the clone pool or drop in a new
  sample — it lands on the TTS host and the agent is pointed at it automatically.

![A persona's Voice management + Corpus — a designer descriptor or a cloned .wav, and a browsable knowledge vault](docs/images/persona-build.png)

### They can actually talk — together

Pair the pantheon with **a Matrix server you host** (a custom WebRTC server and
**per-agent looped virtual audio interfaces** — setup lives in a separate repo,
linked below). Now every persona can be **called like a person**: summon one for a
focused task, ring several **in parallel** for a real conversation, or tell them to
**collaborate with each other** on something bigger. Because each persona
specializes around *who they are*, you always know the right one to call for the
job — and they each speak in their own voice.

> 🔧 **Self-hosted Matrix + WebRTC voice stack** — see [**Voice: Real-time Speech AI
> for DGX Spark**](https://github.com/AEON-7#%EF%B8%8F-voice--real-time-speech-ai-for-dgx-spark)
> on the AEON-7 org page (custom WebRTC server + per-agent looped virtual audio).

### Auto skill-deploy on provisioning

Grant an agent an **Orb API key** and the supervisor drops a scoped access
file straight into that agent's gateway workspace — instant, first-class access via
a provisioned **MCP server and/or REST API.** Add capabilities by clicking
ready-made skill chips or uploading a `SKILL.md` / `.tar` of your own.

---

## The ultimate AI jump box

A bird's-eye view of — and control plane over — your **entire AI infrastructure**:
gateways, DGX Sparks, model servers, and anything else in your personal lab. Live
GPU / CPU / RAM per host, container counts, temperature, and **reboot / shutdown /
wake** on every box, from one console.

![Agent dashboard — connected systems, GPU/CPU/RAM, token usage, and the pantheon](docs/images/agent-overview.png)

Adding a system is a single SSH login away — and if you've meshed your lab with
Tailscale first, the box enrolls a secure SSH key over the tailnet and keeps
**persistent visibility and access to every machine**, even when the Orb
itself moves to a remote network.

---

## Agent dashboard + token telemetry

The same console that watches your hardware watches your **agents.** See total token
consumption across any span of time — last 24 h, 7 days, 90 days, a year — or the
**all-time, per-agent comparison** of who's burning what. It's the difference
between *running* a pantheon and actually *understanding* it.

---

## Reach it from anywhere — one-key Tailscale mesh

The box is most useful when you can reach it — and *everything it can see* — from
any device, anywhere, with **no port-forwarding, no static IP, no VPN gymnastics.**
**Tailscale** (a zero-config WireGuard mesh) gives you exactly that in a single shot:

1. **Network → Tailscale.** Paste a **one-time auth key** from
   [login.tailscale.com](https://login.tailscale.com) (*Settings → Keys → Generate
   auth key*), name it (defaults to `aeon-magick`), and Save.
2. The Pi runs `tailscale up` **once** — and the daemon keeps the resulting node key
   forever.

That's the whole enrollment. The box is now a permanent member of your tailnet:

- 🌍 **Local *and* remote, one address.** Reach it at a stable `aeon-magick` /
  `100.x.y.z` from your laptop on the couch or your phone across the planet — the web
  UI, the REST / MCP API, and the multi-pane terminal all just work.
- 🔁 **Survives moving networks.** Reboot it, carry it to hotel WiFi, a phone
  hotspot, a different office — it pops back onto your tailnet at the *same* name.
  The underlying LAN IP can churn all it likes; your address for it never changes.
- 🤝 **It cuts both ways.** From the Agent Dashboard, **"+ Add device from
  Tailscale"** lists every machine on your tailnet — pick one, give an SSH login, and
  it's added as a managed system reachable by its stable `100.x` address (not a LAN
  hostname that only resolves on one network). **Mesh your whole lab onto Tailscale
  first**, enroll each system over the tailnet, and you keep access to all of it even
  when the box is plugged in somewhere far away.

Pair this with the jump-box view and **your entire AI infrastructure becomes
reachable through one small box you can carry in a pocket.**

---

## Exit node — share the whole privacy stack

Flip one switch and the Pi becomes a **Tailscale exit node** for your tailnet. Now
any device that routes through it inherits **everything the box is running** —
because the exit traffic rides the Pi's *normal* outbound path: your configured
**VPN**, your **encrypted no-log DNS**, and your **Tor / I2P** overlay, all at once.
The mesh between your devices stays direct and fast; only the internet-bound hops
take the scenic, hardened route. One small box, and your phone's traffic comes out
the other side of a VPN-over-Tor tunnel with encrypted DNS — no client config on the
phone at all.

---

## One-click surveillance resistance

VPN, **Tor**, **I2P**, and **DNSCrypt** — layered over your traffic with a button.
The box doubles as a privacy router: the same USB-C that delivers HID can add a
virtual ethernet adapter and pipe a connected host's WAN through the Pi. Built for
privacy, and for operating under oppressive regimes.

![Privacy stack — VPN / Tor (bridge presets) / I2P / DNSCrypt, with a kill-switch](docs/images/network-privacy.png)

- 🕳️ **VPN tunnel** — WireGuard · OpenVPN · **Tor** (transparent proxy + DNS-over-Tor,
  bridge presets: direct / obfs4 / meek-azure / snowflake / custom) · I2P. Commercial
  wizards for **Mullvad / IVPN / AzireVPN / AirVPN** (AirVPN even auto-pulls its
  config, with SSL/SSH stealth modes for Tor-over-VPN). A built-in **kill-switch**
  drops WAN if the tunnel falls; LAN-bypass keeps management reachable.
- 🛂 **Filter by latency *and* by surveillance jurisdiction.** Pick-fastest probes
  every server by RTT — and **No-Eyes** restricts the field to operators outside the
  Five / Nine / Fourteen-Eyes alliances, so you can tune the trade-off between speed
  and exposure to **your** threat model. The provider catalog carries HQ country,
  Eyes tier, audit history, and a trust score.
- 🧅 **Tor-over-VPN, done right.** Nest Tor (or I2P) on top of a commercial VPN so the
  exit IP isn't a known Tor node — with watchdogs that recover the box if a transparent
  Tor circuit can't bootstrap.

---

## Encrypted, no-log DNS

Local `dnscrypt-proxy` speaking **true DNSCrypt v2 + DoH** — **no plaintext DNS ever
leaves the device**, for the Pi *and* every USB-connected client. Auto-pick a
resolver by your criteria — **no-logs**, **DNSSEC**, **no filtering**, **outside
5 / 9 / 14-Eyes**, minimum trust score — or pin specific servers. Add **anonymized
relays** (DNSCrypt's onion-routing-for-DNS) chosen for operator diversity, layer on
**subscription blocklists** (StevenBlack, OISD, …) and your own allow/deny rules, and
watch the live query log. Encrypted DNS, filterable by latency and by alliance — your
threat model, your rules.

---

## Multi-terminal window manager

Concurrent SSH windows — across one system or a mix of systems — for hands-on control
of your whole fleet without leaving the dashboard. Open many shells on a single box,
or one shell on each of many boxes, and tile them. Backed by a PTY WebSocket bridge
and `xterm.js`.

![Terminal — concurrent SSH panes across multiple systems](docs/images/terminal.png)

---

## One-click model deploy to your DGX Spark

Pick a model from **AEON-7's live model + container catalog** (tokenless,
auto-updating straight from Hugging Face + GHCR), tune the flags that matter, and hit
**Deploy** — the image pull + start runs in the background with a progress bar. Or
**hand an agent the keys and let it deploy for you.**

![Easy Deploy — AEON-7 catalog with a GPU% slider + context/batch/concurrency flags](docs/images/easy-deploy.png)

The headline control is a **GPU VRAM slider** (`--gpu-memory-utilization`), with
**context length** (`--max-model-len`), **GPU devices** (tensor-parallel), **max
batch** (`--max-num-batched-tokens`), and **max concurrent sessions**
(`--max-num-seqs`) right beside it.

---

## Container orchestration + monitoring

See **every** registered docker-compose project and container across your connected
hosts — including the ones you've stopped or aren't using, so housekeeping is easy.
Live per-container resource stats tell you at a glance whether a system is healthy,
and **start / stop / restart** plus **compose up / down** and an in-browser compose
editor put it all under one roof.

![Containers — live stats across hosts, compose control, logs](docs/images/containers.png)

---

## Lights-out KVM — mount an ISO, boot to BIOS, install an OS

This is the part that makes it a true **lights-out management** solution. Upload an
ISO through the web UI and the Pi exposes it as a **read-only USB-CDROM** the target
boots from — multi-GB streaming uploads with SHA-256 verification, hot-swap the
"inserted" disk without unplugging. Combine that with the **video feed**, the
**keyboard**, and **out-of-band power control**, and you can reboot a machine into
its **BIOS / boot menu** and **install an entire operating system remotely** —
provided the USB connection supplies passthrough power. A full KVM-over-IP, for both
you *and* your agents — no IPMI card, no datacenter, just the cursed HID.

---

## HID personas

`aeon-hid` builds a USB composite HID gadget on the Pi's USB-C OTG port. It ships
five identities you can hot-swap (via API or web UI):

| Persona | What the host sees | Use this when |
|---|---|---|
| `generic-composite` | Boot keyboard + relative boot mouse, VID `1d6b` (Linux Foundation) | Maximum compatibility, small attack surface. |
| `generic-absolute` | Boot keyboard + **absolute pointer**, VID `1d6b` | You're driving with an **AI agent** — `click_at` / `move_abs` land on exact coordinates, no relative drift. Linux-safe. |
| `logitech-mx` | Logitech Unifying Receiver (VID `046d`), MX-Keys + MX-Master + media keys | You want media keys and extra mouse buttons. |
| `apple-magic-stable` | Apple VID (`05ac`), Apple keyboard + working trackpad | macOS target, reliable Apple-flavored keyboard + pointer. |
| `apple-magic` (experimental) | Apple VID (`05ac`), Apple keyboard + Magic Trackpad multi-touch | macOS **gesture support** — 2/3/4-finger swipes, pinch, rotate. See [`docs/design/apple-mt.md`](./docs/design/apple-mt.md). |

Switching persona triggers a ~1-second USB re-enumeration on the target.

---

## Scoped access — provision or revoke in one click

Empower an agent exactly as much as you mean to, and no more:

- 🔑 **Per-agent API tokens** with four scopes — **admin / full / macros / read** —
  minted, scoped, and revoked from the Tokens page. Each agent connecting with its
  own token gives you a clean **audit trail**: who connected, when, and what they
  did.
- 🔐 **Scoped SSH grants** to the Pi — a per-agent login you can hand out at the
  privilege level you choose (and toggle sudo on). **Granting SSH is a deliberate,
  human-admin action** — never exposed to agents over the API or MCP.
- 🧯 **Two kill switches.** Revoke a key from the console, or — the failsafe nobody
  can race — **pull the plug.** Access is instantly, physically gone.

![API tokens — scoped keys for agents, REST, and MCP](docs/images/tokens.png)

---

## Full audit trails + security console

Know exactly **when** each agent touched the system and **what** it did. Every login,
logout, password / token change, scope denial, persona swap, and non-GET mutation is
logged with the authenticated actor, the HTTP method + path, and the outcome.

![Audit log — every privileged action, attributed to an actor](docs/images/audit.png)

A live **Security Console** sits alongside it: inbound / outbound throughput, iptables
DROP / REJECT counters, a heuristic anomaly detector (traffic spikes, blacklisted-domain
attempts, SYN floods), and active-client conntrack.

![Security console — firewall counters + intrusion heuristics](docs/images/security.png)

---

## Agents-first interfaces (REST + MCP)

Two ways for an AI agent to drive the box, sharing TLS + auth so credentials issued at
first boot work for both:

- **REST + curl** — every input op is an atomic POST under `/api/hid/*`; snapshots are
  a single GET. Scriptable from any language.
- **MCP (Model Context Protocol)** — the supervisor speaks MCP Streamable HTTP at
  `/api/mcp`, exposing **58 named tools** (scope-gated to the calling token): vision (`snapshot`, `state`, recording),
  input (`type_text`, `key_chord`, `click_at`, `drag`, `set_persona`), target power,
  clipboard + files, the **full network/privacy surface** (VPN / Tor / I2P / DNSCrypt /
  WiFi / firewall), ISO control, tokens, and read-only audit/security. Drop the URL
  into Claude Desktop or any MCP client and operate the device with first-class tool
  calls.

Scoped **API tokens** (admin / full / macros / read) gate access. Stored **macros**
(`/etc/aeon/macros/*.toml`) and **prompts** (`/etc/aeon/prompts/*.md`) round it out:
keyboard-driven sequences you don't want to re-author, and short playbooks an agent
can fetch to prime itself.

> Atomic-op discipline (inherited from `cursed-hid`): the input API exposes `type`,
> `key chord`, `click`, `move` — never a `key_down` that a dropped packet could leave
> stuck on the wire.

---

## More that's in the box

- 📋 **Shared clipboard** — a two-way 64 KB text buffer that survives reboots; type it
  onto the target with one call (great for pasting a long credential an agent shouldn't
  fumble character-by-character).
- 📁 **File staging + optional target file-server** — stage files on the Pi, or expose a
  small HTTP drop to the connected host over the USB-ethernet link.
- 🌐 **USB ethernet modes** — **isolation** (host reaches the internet via NAT, can't
  see your LAN — guest-laptop safe), **sharing** (full LAN bridge), **restricted**
  (WAN only). 250+ Mbit on USB 3.0.
- 🎯 **Zero-touch first boot** — an `aeon-setup` WiFi AP comes up automatically if the
  device can't reach the internet for ~90 s. Connecting triggers the captive-portal flow
  on every OS and launches a **live-scanning WiFi picker** — pick a nearby network, type
  the password, the AP tears itself down. Zero monitor, zero keyboard, zero serial cable.
- 🔐 **Hardened + unique-per-device by default** — argon2 password + HMAC-signed cookies,
  TLS (self-signed or BYO), and a **random admin password generated at first boot** and
  dropped on the SD card's boot partition. No two flashed devices share keys.

---

## What's in the image

```
aeon (system user), running:
  /usr/local/bin/aeon-streamer       ← v4l2 capture, MJPEG/H.264 via ustreamer
  /usr/local/bin/aeon-hid            ← USB gadget configfs + atomic-op API
  /usr/local/bin/aeon-supervisor     ← HTTPS frontend (rustls), routes /api/*,
                                       MCP server at /api/mcp, macros + prompts,
                                       agent dash, containers, terminal, serves /

/etc/aeon/
  streamer.toml hid.toml supervisor.toml    ← runtime config
  auth.toml                                 ← argon2 admin password (first boot)
  cert.pem key.pem                          ← self-signed TLS (first boot)
  macros/ scripts/ prompts/                 ← user-editable action library

/usr/share/aeon/web/                         ← the SvelteKit single-page app
/etc/systemd/system/                         ← aeon-{streamer,hid,supervisor},
                                               firstboot, netwatch, net-services,
                                               dnscrypt-proxy, tor

In /boot/firmware/ after first boot:
  aeon-credentials.txt   ← per-device admin password (mode 0600; delete once noted)
```

Tailscale is preinstalled but disabled by default. Drop an `aeon-setup.toml` onto the
boot partition before first boot to wire up WiFi + Tailscale automatically (see
[`AGENTS.md`](./AGENTS.md)).

---

## Project layout

```
aeon-streamer/    Rust — adaptive capture supervisor + watchdog
aeon-hid/         Rust — USB gadget + persona + atomic-op input API
aeon-supervisor/  Rust — HTTPS frontend, /api/* + MCP, argon2 auth, agent dash,
                         containers, terminal, network/privacy, audit
aeon-web/         SvelteKit — dark UI, live stream canvas, input capture
image-builder/    pi-gen overlay producing aeon-magick.img.xz
docs/             Design notes + showcase screenshots
scripts/          Cross-compile + web-build helpers
AGENTS.md         Setup + day-to-day usage, for humans AND AIs
SKILL.md          Manifest an AI agent loads to operate the device (+ references/)
ARCHITECTURE.md   How the daemons split work and why
BUILDING.md       Compile + build the image on macOS or Linux
```

---

## Hardware you'll need

A small, cheap bill of materials — most of it you may already own:

| Part | What / why | Notes |
|---|---|---|
| **Raspberry Pi 4 or Pi 5** (2 GB+) | The appliance. Its USB-C port runs **USB-OTG gadget mode** to emulate a keyboard + mouse + trackpad to the target. | **Two flavors.** The **Pi 4 image (Pi OS Bookworm)** is this branch — USB capture (Cam Link), hardware H.264, longest-tested. The **Pi 5 image (Pi OS Trixie)** — the `feat/pi5-vision-voice-ups-av` track — adds the Pi 5 suite: **built-in Hailo AI HAT support** (AI Kit Hailo-8/8L *and* AI HAT+ 2 Hailo-10H; the right runtime installs chip-aware from the Hailo super-app), on-device **screen OCR + VLM grounding**, HDMI-to-CSI capture, voice, and UPS power. Everything model-specific is auto-detected either way (UDC name; H.264 encoder: Pi 4 hardware `h264_v4l2m2m`, Pi 5 software `libx264`), so the Bookworm image will boot a Pi 5 in a pinch. **Pi 5 cabling:** a kernel bug (≥6.6.42) breaks gadget enumeration over C-to-C cables — connect the Pi 5's USB-C to a **USB-A port** on the target (A-to-C cable). |
| **microSD card** (16 GB+) | Boots the Aeon Magick Orb image. | A fast A1/A2 card helps stream latency. |
| **HDMI video-capture device** | The "eyes" — pipes the target's HDMI into the Pi as a USB camera. | **Elgato Cam Link 4K** (rock-solid 1080p60 / 4K30) **or any ~$10 MS2109-based HDMI→USB stick**. Both auto-detected (UVC) — no drivers. |
| **USB-C data cable** | Pi-C → target-C — carries the emulated keyboard/mouse and **powers the Pi from the target.** | Must be **data-capable**; a charge-only cable powers the Pi but enumerates no HID. **Pi 5: use an A-to-C cable** (target USB-A → Pi USB-C) — C-to-C enumeration is broken by an upstream kernel bug. |
| **HDMI cable** | Target's HDMI-out → the capture device. | |
| *(optional)* **Tailscale** | Reach the box — and everything it can see — from anywhere in the world. | Enabled during enrollment. |

Nothing is installed on the **target** — it only ever sees a USB keyboard/mouse and an
HDMI sink. Works on macOS, Windows, Linux, and even pre-OS (BIOS, FileVault, Windows
OOBE). The Pi is powered over the USB-C link by the target (or via its GPIO 5V pins);
the capture device draws power from the Pi's USB-A.

---

## Quick start

1. **Flash the image.** Grab the latest `image_vNN-aeon-magick.img.xz` from the
   [Releases](../../releases) page and write it with **Raspberry Pi Imager** ("Use
   custom" → the `.img.xz`) or:
   `xzcat image_vNN-aeon-magick.img.xz | sudo dd of=/dev/diskN bs=4M status=progress`
2. **First boot → join WiFi.** On first power-up the Pi becomes its own WiFi access
   point (**`aeon-setup`**). Connect to it from a laptop/phone; a captive-portal wizard
   opens — pick your home WiFi + password. *(On the Pi 4, WiFi beats Ethernet for
   stream latency — its Ethernet sits behind a USB bridge while WiFi is on a PCIe
   lane. On the Pi 5 both are fast; use whichever is convenient.)*
3. **Open the web UI.** The Pi joins your network at `https://<pi-ip>/` (or
   `https://aeon-magick.local/`). Accept the self-signed cert and set/confirm the
   **admin password** — a random one is generated at first boot and written to
   `aeon-credentials.txt` on the SD card's boot partition.
4. **Wire the target.** Pi **USB-C → target USB-C** (data cable) for keyboard/mouse;
   target **HDMI-out → capture device → Pi USB-A** for vision.
5. **Drive it — or hand it to an AI.** The UI now streams the target's screen: type,
   click, run macros. To empower an agent, mint a token on the **API Keys** page (or
   provision one from the **Agent Dash**), point it at `https://<pi>/api` (REST) or
   `https://<pi>/api/mcp` (MCP), and the skill auto-deploys into its workspace.

Zero software on the target, full control from your browser or your AI — and a hard
kill switch: unplug it or revoke the key and access is instantly gone.

For the deep dive (the agent skill, macros, personas, the privacy stack) read
[`AGENTS.md`](./AGENTS.md); to rebuild from source see [`BUILDING.md`](./BUILDING.md).
The screenshots above were captured from the live UI and **redacted** by
[`scripts/capture-screenshots.js`](./scripts/capture-screenshots.js) (re-runnable
against your own device).

---

## Lineage

Descended from [`cursed-hid`](https://github.com/albert/cursed-hid), an ESP32-S2 dongle
that emulated a Logitech Unifying Receiver and accepted only **logical operations** over
WebSocket — never raw press/release primitives that could leave a key stuck if a packet
dropped. The Orb inherits that lesson. It borrows configfs USB-gadget patterns and
the ustreamer v4l2 capture loop from [PiKVM](https://github.com/pikvm/pikvm) — but we're
not a fork; we're a sibling project with a different goal: **agent-grade computer
control**, not remote KVM administration.

## License

MIT. See [`LICENSE`](./LICENSE).

## Hard rules (non-negotiable)

This software exists to drive **the user's own computers**, the **user's own accounts**,
the **user's own services**. It is not a tool to bypass fraud controls, automate access
to other people's accounts, or evade device-attestation on a service the user is not
authorized to operate against. The naturalism layer (when enabled) is for accessibility
and reliability — not cover for impersonation. If you build something sketchy on top of
this, that's on you, and we will not help.
