---
name: aeon-fleet
description: >
  See every AEON Magick Orb on the network at once and pick which one to
  operate. Each Orb is a peer in a decentralized roster; one roster call
  returns every Orb's identity, what it controls, its capture sources, battery
  and health — so you choose the right Orb, then drive it directly with the
  core aeon-magick skill against that Orb's own endpoint. Load this only when
  there may be more than one Orb; for single-device work just set AEON_HOST and
  use the core skill.
---
# aeon-fleet

Each Orb is a self-contained see+act device on its own HTTPS endpoint. The fleet
layer makes an Orb a **peer in a decentralized roster** so one agent can
discover every Orb, read its state, and decide which to drive — then talk
directly to that Orb's `https://<addr>/api/*` for input and vision (the core
`aeon-magick` skill). There's no central controller: the roster is each Orb's
view of the swarm, polled directly over the tailnet/LAN.

> **Credentials.** Auth = `Authorization: Bearer $AEON_TOKEN` against
> `https://$AEON_HOST` (self-signed → `curl -k`). The token is never in this
> skill — `export AEON_TOKEN=<your token>` (e.g. from your password manager) or
> load `~/.openclaw/aeon-magick.env` (`set -a; . ~/.openclaw/aeon-magick.env; set +a`).
> A `401` means the token is unset/wrong — fix that, don't switch tools.

The roster GET is Read-tier.

## roster — the whole fleet in one call

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
    "https://$AEON_HOST/api/fleet/roster"
```

```json
{
  "configured": true,
  "self": {
    "id": "orb-a1b2c3", "hostname": "aeon-orb-1", "label": "Pi5 CSI rig",
    "model": "pi5", "lan_ip": "192.168.1.25",
    "addrs": ["192.168.1.25", "100.92.x.y"], "is_self": true, "online": true,
    "sources": ["camera-csi", "hdmi-csi"], "view_source": "hdmi-csi",
    "webcam": {"enabled": false, "source": "off"},
    "ups": {"present": true, "charge": 96, "on_battery": false},
    "health": {"cpu_temp_c": 47, "uptime_seconds": 8123}, "version": "0.1.0",
    "seeds": ["192.168.1.56"]
  },
  "peers": [
    {
      "id": "orb-d4e5f6", "hostname": "aeon-orb-2", "label": "Pi4 Cam Link",
      "model": "pi4", "addr": "192.168.1.56", "online": true,
      "sources": ["cam-link-usb"], "view_source": "cam-link-usb",
      "webcam": {"enabled": false, "source": "off"},
      "ups": {"present": false}, "health": {"cpu_temp_c": 51}, "version": "0.1.0"
    }
  ]
}
```

`configured: false` means fleet peering was never set up — you have a lone Orb;
just operate `$AEON_HOST` directly and ignore the rest.

The `peers` set is **static `seeds` ∪ tailnet-discovered IPs (when `tailscale` is
on), minus this Orb's own addresses**. Each peer is fetched concurrently with a
**3 s timeout**, so a down or slow peer comes back as an `online: false` stub
rather than blocking the roster — one unreachable Orb never stalls the call.

## picking an Orb

Each entry is everything you need to choose without touching the device:

| Field | What it tells you |
|---|---|
| `label` | **what this Orb controls** — the thing you're really selecting on. Match the user's intent against this first. |
| `model` | `pi5` / `pi4` — gates which capture `sources` exist (Pi 5 adds CSI camera + HDMI-CSI; see `aeon-cameras`) |
| `addr` / `lan_ip` / `addrs` | the Orb's address(es) — **target this directly** for input/vision. `addrs` lists *every* address it answers on (LAN + tailnet); `lan_ip`/`addr` is just the first. Use whichever is reachable from where you're dispatching. |
| `online` | reachable on the last heartbeat. **Don't dispatch to an offline Orb.** (`self` is implicitly online.) |
| `sources` / `view_source` | available sources, and what the console is watching right now |
| `webcam` | what it's exposing to its target as a UVC webcam (see `aeon-webcam`) |
| `ups` | battery state — warn/skip on `on_battery: true` / low `charge` (see `aeon-power`) |
| `seeds` (on `self`) | this Orb's configured peer list — the swarm it expects, without a separate `/config` call |
| `health` / `version` | quick triage + capability sanity |

**Flow:** roster → filter `online` (+ healthy) → match `label` (and `model` if
you need Pi-5-only sources) → take that peer's `addr` → from there it's an
ordinary single-Orb session against `https://<addr>/api/*` with the core skill.
Each Orb authenticates **independently** — use *that* Orb's own token (the
config can hold per-Orb tokens; see the master env), not necessarily
`$AEON_HOST`'s.

## the two current Orbs

| addr | model | label | capture |
|---|---|---|---|
| `192.168.1.25` | `pi5` | Pi5 CSI rig | `camera-csi` (Sony IMX477 HQ) + `hdmi-csi` (Geekworm X1301) |
| `192.168.1.56` | `pi4` | Pi4 Cam Link | `cam-link-usb` (Elgato Cam Link 4K) |

Each is configured on its own — the `.25` Pi 5 flips between the screen it
controls (`hdmi-csi`) and a physical camera (`camera-csi`); the `.56` Pi 4 has
the single USB capture path. The fleet doesn't unify them; it lets you **see and
reach both** from one roster call.

## what's operator-only

- `GET /api/fleet/status` is the **internal peer-to-peer heartbeat** (gated by
  an `X-Fleet-Token`, bypasses user auth on purpose). You use `/roster` — that's
  the heartbeat data already assembled. Only reach for `status` when debugging
  why a peer shows `online: false`.
- `GET/PUT /api/fleet/config` manages peering — the seed list, the **`tailscale`
  tailnet auto-discovery toggle**, and the shared token. With `tailscale: true`
  the roster also pulls in peers discovered over the tailnet, so an Orb can show
  up with **no seed entry at all**. PUT accepts `label`, `seeds`, and
  `tailscale`; it never reads or writes the token. That's human setup, not an
  agent surface.
- **No MCP tool** for the fleet — roster/status/config are REST-only.
