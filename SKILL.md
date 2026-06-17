---
name: aeon-magick
description: >
  Drive a target host's keyboard, mouse, trackpad, AND screen capture through
  an AEON Magick AI Computer Control Orb on the LAN — and pick among several
  Orbs in a fleet. Each Orb's USB-C OTG port emulates a real USB keyboard +
  mouse (+ optional Apple multi-touch trackpad) on its target; the Orb does its
  OWN HID — it both sees AND acts, no external dongle. Vision comes from a
  switchable capture source (Cam Link USB, Pi-5 HDMI→CSI bridge, or a CSI
  camera), so an agent can flip between watching the controlled screen and
  looking at a physical thing. All ops are atomic HTTPS calls — there is no
  `key_down` / `key_up` split that can leave the target with a stuck key after
  a packet drop. Use to operate the USER'S OWN computers, accounts, and
  services.
---
# aeon-magick

The Universe's Strangest Peripheral. One box, two superpowers:
**see** (switchable capture → JPEG) and **act** (USB-HID keyboard + mouse +
optional Apple trackpad → target host). The Orb does its **own** HID over its
USB-C OTG port — it is the keyboard and mouse the target sees, no Flipper, no
external dongle, nothing inert. Both halves live behind one HTTPS endpoint with
one set of credentials.

> **Credentials — read this first.** Every call authenticates with
> `Authorization: Bearer $AEON_TOKEN` against `https://$AEON_HOST` (self-signed
> cert → `curl -k`). The token is **never** written into this skill. Supply it
> however you already hold it — both work:
> - **Inject your own:** `export AEON_HOST=<orb ip>` and `export AEON_TOKEN=<your
>   Orb token>` (e.g. pulled from your password manager) for the session, then run
>   any command below as-is.
> - **Or the shared file:** put them once in `~/.openclaw/aeon-magick.env` and
>   load it: `set -a; . ~/.openclaw/aeon-magick.env; set +a`.
>
> Each Orb has its own token. If a call returns `401`, your `$AEON_TOKEN` is unset
> or wrong for `$AEON_HOST` — fix that first, don't fall back to another tool.

This skill is the Orb's own manifest. It is the **agent's** manifest — it
documents only what an agent should do. Keep this index small; pull a
`references/` file in only when you actually need that capability.

## Environment

There can be **more than one Orb** — they form a decentralized **fleet**, each
at its own host, each independently controlling its own target. You operate one
Orb at a time by pointing `AEON_HOST` at it. To discover what's out there,
read the roster (normal auth, any Orb answers for the whole fleet):

```bash
# fleet roster — every Orb, what it controls, and whether it's online
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/fleet/roster"
```

Each peer entry carries `label` (what it controls), `model` (`pi5`/`pi4`),
`online`, `sources`, `view_source`, `webcam`, `ups`, `health` — enough to pick
the right Orb, then set `AEON_HOST` to its address and operate it. See
the `aeon-fleet` skill. **The two current Orbs:**

| Host | Model | Vision |
|---|---|---|
| `192.168.1.25` | Pi 5 | CSI camera (IMX477) + HDMI→CSI capture — switchable |
| `192.168.1.56` | Pi 4 | Cam Link 4K USB capture |

Every command uses `$AEON_HOST` / `$AEON_TOKEN` — set them yourself (export, per
the Credentials note above) or keep them in the shared `~/.openclaw/aeon-magick.env`:

| Var | Default | Meaning |
|---|---|---|
| `AEON_HOST` | `aeon-magick.local` | the Orb you're operating — mDNS hostname or IP (e.g. `192.168.1.25`). Use the roster to choose. |
| `AEON_TOKEN` | _(required)_ | your API token (`aeon_tok_…`), sent as `Authorization: Bearer $AEON_TOKEN`. Mint it in the Orb's web UI (Settings → API tokens): a **Full**-scope token for input/config, **Read** for view-only. Each Orb has its own token. |
| `AEON_INSECURE_TLS` | `1` | accept the Orb's self-signed cert. Curl: `-k`. Python `requests`: `verify=False`. |

Load the config before any call, then every command picks up `$AEON_HOST` /
`$AEON_TOKEN`:

```bash
set -a; . ~/.openclaw/aeon-magick.env; set +a
```

All endpoints below assume `https://${AEON_HOST}/`. Every command in this skill
is a single HTTPS call and returns JSON unless noted.

Your Aeon Magick API token is provisioned at one of two tiers:
**Read** (view-only — GET snapshots / state / status / roster / ups) or **Full**
(interactive — HID input + capture-source / webcam / network config). That's
the whole agent surface. Managing *other* systems (corpus / voice / personas,
terminal, containers, compose, deploy) and all API-key / SSH-key provisioning
are operator/human-only and are deliberately **not** part of this skill or the
MCP server.

## Hardware preconditions

Before any of this works, for the Orb you're operating:

- Orb powered on, joined to LAN (or reachable via Tailscale if you wired
  that in via `aeon-setup.toml`). It may be running on its UPS HAT battery —
  check `GET /api/ups` (see the `aeon-power` skill).
- **Orb USB-C → target USB-C** with a **data-capable** cable. The Orb's USB-C
  OTG port is where it presents itself to the target as a keyboard + mouse.
  Charging-only cables let the Orb draw power from the target but pass no data;
  `keyboard_online` will be false in `state`.
- **A live capture source** for vision. Which sources exist depends on the
  Orb's `platform` (`pi4`/`pi5`) — query `GET /api/streamer/config`:
  - `cam-link-usb` — an Elgato Cam Link 4K (or MS2109) USB stick, target
    HDMI → capture → Orb USB-A. (The only source on the Pi-4 Orb.)
  - `hdmi-csi` — a Pi-5 Geekworm X1301 HDMI→CSI bridge: watch the target's
    HDMI directly.
  - `camera-csi` — a Pi-5 CSI camera (e.g. Sony IMX477 HQ Camera): look at a
    physical thing, not a screen.

  Switch the live view between them with `PUT /api/streamer/config {source}`
  (see the `aeon-cameras` skill).

If `state` shows `keyboard_online: false` or `streamer_online: false`, fix
the physical link before issuing input or capture calls — they will silently
no-op or return stale frames.

## Core workflow — vision-driven navigation

The whole loop is **state → snapshot → act → snapshot again**:

1. **`state`** — confirm `keyboard_online`, `mouse_online`, `streamer_online`
   are all `true`. If anything is false, **stop and report to the user** — it's
   almost always a cable, not software. There's no point sending input that
   won't land. (Also worth a glance: `GET /api/ups` — if `on_battery: true`
   with a low `charge`, the Orb may be about to power down.)
2. **Know what you're looking at.** `GET /api/streamer/config` tells you the
   current `source`. If you need to see the controlled screen, use a screen
   source (`hdmi-csi` / `cam-link-usb`); to look at a physical thing, switch to
   `camera-csi`. `PUT` the new source to flip the live view (brief blip while
   the streamer restarts).
3. **`snapshot`** → save to a tmp path, then `Read` the JPEG to inspect it. But
   you often don't need to eyeball pixels at all:
   - **`screen_find("Save")`** — when you already know the label you want to
     click, skip the visual hunt: it reads the on-device OCR and hands back the
     button's clickable **`center` {x,y}` in 0..1 fractions** — feed that
     straight into `click_at` / `click`. The fast path for "click the Save
     button / Sign in link / Settings".
   - **`describe_screen`** — when the frame is unfamiliar, get a one-line
     semantic read ("which dialog is open?", "is the upload finished?") from the
     on-device vision-language model instead of reasoning over raw pixels.
   - **`screen_text`** — when you want *all* the OCR text + boxes at once.
4. From the frame (or the OCR/VLM output above), identify the click target in
   target space. You are reasoning over the captured frame directly — there is
   no "where is the cursor now" state to track, because every input is relative
   or chord-based.
5. On the `generic-absolute` persona, **`click_at`** the target with its
   fractional coords (`x = pixel_x / width`, `y = pixel_y / height`) — the
   reliable path, no acceleration drift. (On a relative persona, drive there
   with segmented `move` instead, splitting deltas larger than ±127.)
6. **`click_at`** / **`click`** (or `key`, `type`, `scroll`, `drag`) to act.
   For text entry: focus the field (move + click), then `type`.
7. **`snapshot`** again to verify the change happened. If not, loop to step 4
   with the new frame.

Latency budget: snapshot ~50–150 ms (LAN); reasoning is up to your model; HID
action ~10–30 ms.

The two foundational calls:

```bash
# state — what is the Orb seeing right now? (call this first every session)
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/state"

# snapshot — one JPEG of the current capture source
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
    "https://$AEON_HOST/api/streamer/snapshot" -o /tmp/aeon_capture.jpg
```

### Reading the screen without eyeballing pixels (on-device vision)

On a Hailo AI HAT+ Orb, the on-device vision stack lets you turn the frame into
text and coordinates directly — `screen_text`, `screen_find`, and
`describe_screen` are all first-class **MCP tools** (`tools/list` + `tools/call`)
*and* REST endpoints, exactly like each other.

```bash
# screen_text — ALL the OCR text on screen right now, with boxes (already exists)
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/vision/detections"

# screen_find — find a SPECIFIC label and get its clickable center (FAST PATH)
#   center {x,y} is in 0..1 FRACTIONS of the frame → feed straight into click_at.
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
    "https://$AEON_HOST/api/vision/find?query=Save"
# → {"ok":true,"query":"Save","count":1,
#    "matches":[{"text":"Save","match_score":1.0,"ocr_conf":0.98,
#                "center":{"x":0.13,"y":0.22}}],
#    "hint":"feed center into click(x,y)"}

# describe_screen — a NATURAL-LANGUAGE read of the whole screen (SEMANTIC)
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
    -H "Content-Type: application/json" -X POST \
    "https://$AEON_HOST/api/vision/describe" \
    -d '{"prompt":"which dialog is open?","max_tokens":64}'
# → {"ok":true,"description":"The screen shows a message that says
#    \"Can't Connect to Server.\"","gen_ms":2629}
```

**`screen_find(query)`** ranks matches from the live OCR (text, `match_score`,
`ocr_conf`, fractional `center`) and returns `{ok:false}` if vision/OCR is off
or nothing matches. It just reads the **aeon-vision OCR daemon's** output, so it
needs **no NPU GenAI slot** — it runs fine alongside the local LLM. This is the
fast path: when you know the label, don't snapshot-and-stare, just
`screen_find("Sign in")` → `click_at(center)`.

**`describe_screen(prompt?, max_tokens?)`** runs the on-device **Qwen2-VL** model
on the Hailo AI HAT+ for semantic understanding ("what *is* this screen?"); pass
an optional `prompt` to ask a specific visual question. ~3 s warm, ~13 s cold
(first call loads the 2.2 GB VLM, which then stays resident and idle-unloads
after ~180 s). It shares the NPU's **single GenAI slot** with the local
hailo-ollama LLM, so it's available when the local LLM isn't resident — the
normal online case, where *you* (the external agent) are the brain. If the slot
is taken it returns `{ok:false}` with guidance to free it. Use
`screen_text`/`screen_find` for exact text + coordinates instead.

Both need the on-device vision stack (aeon-vision OCR for
`screen_text`/`screen_find`; the deployed Qwen2-VL `.hef` for `describe_screen`)
on a Hailo AI HAT+ Orb.

See `references/vision.md` and `references/input.md` for the full surface.

## Capability index

Load exactly one reference when you need that capability — don't read them all
up front.

This skill's own references (the core see+act surface):

| Reference | Load this when… |
|---|---|
| [`references/vision.md`](references/vision.md) | you need to **see** the target — `state`, `snapshot`, the live MJPEG/H.264 stream, and the on-device vision tools (`screen_text`, `screen_find`, `describe_screen`). |
| [`references/input.md`](references/input.md) | you need to **act** — type, key chords, click, move (relative + absolute `click_at`), drag, scroll, persona switching, or `release_all`. |
| [`references/macros.md`](references/macros.md) | you want to run (or author) a stored **macro** sequence, or fetch a stored agent-playbook **prompt**. |
| [`references/recording.md`](references/recording.md) | you need to **record the current capture source** to MP4 and list/download recordings. |
| [`references/clipboard.md`](references/clipboard.md) | you need to read/write the Orb's text **clipboard** or type its contents onto the target (e.g. pasting a long credential). |
| [`references/files.md`](references/files.md) | you need to **stage files** for the target or manage the **ISO** library (OS-install workflows). |
| [`references/network.md`](references/network.md) | you're setting up **DNS / VPN / Tor / I2P / WiFi** on the Orb's outbound path. |
| [`references/mcp.md`](references/mcp.md) | your agent speaks **MCP** and you want the Streamable-HTTP transport + the full tool list instead of curl. |

### Companion skills (separate skills in the collection — load independently)

The Orb's distinct / Pi-5-specific features live as their **own skills**, so the
model loads each only when its task needs it — never all at once:

| Skill | Load this when… |
|---|---|
| `aeon-cameras` | you need to **switch what the Orb sees** — capture-source switching + the Pi-5 CSI camera / HDMI-CSI bridge. |
| `aeon-webcam` | you want the Orb to present a source to its OTG target **as a UVC webcam**. |
| `aeon-power` | you need an Orb's **battery / UPS** state (`GET /api/ups`). |
| `aeon-fleet` | you want to **see every Orb at once** and pick which to operate (`GET /api/fleet/roster`). |

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
- **This is the agent's manifest.** Some Orb actions are
  **human-admin-only** and are deliberately NOT agent capabilities: managing
  the Orb's SSH keys, powering / rebooting / waking the **target or the Pi
  itself**, and issuing / revoking API tokens. None of those appear in this
  skill. If a task seems to need one, ask the human to do it in the web UI.
  (Reading the **UPS** state is allowed at the Read tier — it's a GET.)

## Troubleshooting cheatsheet

| Symptom | Likely cause | Fix |
|---|---|---|
| `keyboard_online: false` | Charging-only USB-C cable | Swap to a data cable |
| `streamer_online: false` | Capture source not enumerated | Re-seat the capture source; confirm the target is outputting HDMI; check `GET /api/streamer/config` `available_sources` |
| Continuous letter repeats on target | Lost release event (rare) | `release_all` (see `references/input.md`) |
| Snapshot frozen + `ups.on_battery: true` | Orb may be powering down on a draining battery | Check `GET /api/ups`; restore mains power (see the `aeon-power` skill) |
| Snapshot returns same frame forever | Target went to sleep, or display put to standby | Wake the target (try sending a SHIFT key chord), then snapshot again |
| Seeing the wrong thing (camera vs. screen) | Wrong capture `source` selected | `PUT /api/streamer/config {source}` to the source you want (see the `aeon-cameras` skill) |
| `click_at` lands in the wrong place | Not on the `generic-absolute` persona | Switch persona first (see `references/input.md`) |
| `recording requires H.264 stream mode` | Stream isn't in h264 mode | Set the stream format to h264, then record (see `references/recording.md`) |
| Can't reach an Orb you expected | It's offline, or you have the wrong address | Check `GET /api/fleet/roster` `online` + addresses (see the `aeon-fleet` skill) |
| `aeon-magick.local` doesn't resolve | mDNS blocked on your network | Use the IP directly; set `AEON_HOST` to it |
| TLS handshake fails | Cert regenerated after a re-flash | Re-accept; the cert is unique per Orb per first-boot |

When in doubt: `state` first, `snapshot` second, then act. The Orb's state
surface is small enough that those two calls explain almost every "why isn't
this working" question.
