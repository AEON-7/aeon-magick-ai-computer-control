---
name: aeon-cameras
description: >
  Choose what an AEON Magick Orb's eye is pointed at. An Orb can have several
  video inputs wired to it — a USB HDMI-capture stick, a Pi-5 HDMI→CSI bridge
  (watch the controlled machine's screen), and a Pi-5 CSI camera (look at a
  physical scene through a lens). This skill reads the available capture
  sources and switches the live source so the console snapshot/stream shows
  what you need. Pi-5-only sources (camera-csi, hdmi-csi) are gated by the
  Orb's model. Load this when you need to switch between "see the screen I
  control" and "see a physical thing"; for plain screen viewing use the core
  aeon-magick skill's vision reference.
---
# aeon-cameras

Switch an Orb's **capture source** — what its console (snapshot / stream /
recording) is looking at. The core `aeon-magick` skill's `vision` reference
shows you the *current* feed; this skill picks *which input* that feed comes
from.

> **Credentials.** Auth = `Authorization: Bearer $AEON_TOKEN` against
> `https://$AEON_HOST` (self-signed → `curl -k`). The token is never in this
> skill — `export AEON_TOKEN=<your token>` (e.g. from your password manager) or
> load `~/.openclaw/aeon-magick.env` (`set -a; . ~/.openclaw/aeon-magick.env; set +a`).
> A `401` means the token is unset/wrong — fix that, don't switch tools.

Switching a source is a **Full-tier** op.

## the source set

The active source lives in `/etc/aeon/streamer.toml [capture] source` and is
exposed over the API. The vocabulary is `{auto, cam-link-usb, hdmi-csi, camera-csi}`:

| source | hardware | what you see |
|---|---|---|
| `auto` | — | first available source automatically (safe default) |
| `cam-link-usb` | Elgato Cam Link 4K USB capture | the **controlled machine's HDMI** screen, over a USB stick |
| `hdmi-csi` | Pi-5 Geekworm X1301 HDMI→CSI bridge | the **controlled machine's HDMI** screen, over the CSI ribbon (**Pi 5 only**) |
| `camera-csi` | Pi-5 CSI camera (e.g. Sony IMX477 HQ Camera) | a **physical scene** through a lens — a thing in the room, not a screen (**Pi 5 only**) |

`cam-link-usb` and `hdmi-csi` both watch *the screen you're driving*; flip to
`camera-csi` when you need to **look at something physical** (a status LED, a
phone, the rack, a printout) instead of the controlled desktop.

## read the current source

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
    "https://$AEON_HOST/api/streamer/config"
```

```json
{
  "ok": true,
  "source": "camera-csi",
  "platform": "pi5",
  "available_sources": ["auto", "hdmi-csi", "camera-csi"],
  "fps": 30,
  "width": 1920,
  "height": 1080,
  "format": "h264",
  "hw_accel": false,
  "jpeg_quality": 70,
  "rotation": 0,
  "hflip": false,
  "vflip": false
}
```

`rotation` / `hflip` / `vflip` echo the current **server-side orientation** (see
"rotate / flip the view" below).

`platform` is `pi4`, `pi5`, or `other`, and it **gates which sources exist**:
`hdmi-csi` and `camera-csi` are Pi-5-only, so a Pi-4 Orb usually lists only
`auto` + `cam-link-usb`. **Never assume a source is selectable — pick only from
`available_sources`** for that specific Orb. (In a fleet, each peer reports its
own `model` and `sources`; see the `aeon-fleet` skill.)

## switch the source live

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" -X PUT \
    -H "Content-Type: application/json" \
    -d '{"source": "camera-csi"}' \
    "https://$AEON_HOST/api/streamer/config"
```

The PUT rewrites the config and **restarts the streamer** to bring up the new
pipeline — expect a brief feed blip, then `snapshot`/`stream`/`ws` show the new
source. Send just `source` to switch the eye and leave everything else alone
(the same PUT also accepts `fps` and `jpeg_quality` to tune encoding, and
`rotation`/`hflip`/`vflip` to re-orient — see below). Re-read
`/api/streamer/config` (or take a snapshot) after switching to confirm it took.

A typical vision agent flips between *"see the screen I control"*
(`hdmi-csi` / `cam-link-usb`) and *"see a physical thing"* (`camera-csi`) as the
task demands — switch, snapshot, reason, switch back.

## rotate / flip the view

A camera is often mounted sideways or upside-down. Correct it **server-side** on
the same `/api/streamer/config` endpoint — `rotation` (degrees **clockwise**,
snapped to `0 | 90 | 180 | 270`) plus `hflip` (mirror left↔right) / `vflip`
(mirror top↔bottom):

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" -X PUT \
    -H "Content-Type: application/json" \
    -d '{"rotation": 90, "hflip": false, "vflip": false}' \
    "https://$AEON_HOST/api/streamer/config"
```

The correction is **baked into the capture pipeline** (an ffmpeg `transpose` /
`hflip,vflip` *before* the encode split), so it applies identically to the
**snapshot, the live stream, recordings, AND the vision/OCR feed** — flip a
sideways Pi-camera mount once and everything downstream (including `screen_text`
/ `screen_find` boxes) reads upright. A `90`/`270` quarter-turn **swaps the
reported `width`↔`height`**, so the resolution and the 0..1 coordinate space
follow the rotated view. Send orientation keys alone to re-orient without
touching the source or encoding; like a source switch it restarts the streamer
(brief blip).

Orientation is mainly for **`camera-csi`** (a physical camera on a turned mount).
Don't rotate a **screen** source you drive with HID (`cam-link-usb`/`hdmi-csi`) —
the host's screen isn't rotated, so a rotated view only confuses your click math.

## caveats

- **Single-camera contention.** The `aeon-webcam` passthrough draws from the
  same physical inputs. Don't point the console *and* the webcam at the same
  source (e.g. both on `camera-csi`) — they'll contend and one may go black.
- **No MCP tool** — source switching is REST-only; an MCP agent curls
  `/api/streamer/config` directly.
