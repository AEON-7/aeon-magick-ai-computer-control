---
name: aeon-webcam
description: >
  Present one of an AEON Magick Orb's video sources back to its USB-connected
  target host as a standard UVC webcam, so the target machine can pick the Orb
  as a camera in a video call, OBS, etc. This is the inverse of viewing — here
  the controlled machine sees through the Orb's eyes (a Pi camera's physical
  scene, or its own screen as a webcam). Load this only when you want the
  target itself to see through the Orb; for the agent watching the screen, use
  the core aeon-magick vision reference. Requires a Pi whose OTG stack supports
  UVC; check the `supported` flag first.
---
# aeon-webcam

Expose a capture source **to the OTG-connected target host** as a USB Video
Class (UVC) webcam. The target then sees the Orb like any plug-in webcam — pick
it in a call app, OBS, `cheese`. This is distinct from the **console** view
(the core `aeon-magick` skill's vision reference): that's *you* watching the
screen over the network; webcam passthrough is the *target machine* watching a
source over its own USB.

> **Credentials.** Auth = `Authorization: Bearer $AEON_TOKEN` against
> `https://$AEON_HOST` (self-signed → `curl -k`). The token is never in this
> skill — `export AEON_TOKEN=<your token>` (e.g. from your password manager) or
> load `~/.openclaw/aeon-magick.env` (`set -a; . ~/.openclaw/aeon-magick.env; set +a`).
> A `401` means the token is unset/wrong — fix that, don't switch tools.

The PUT is a **Full-tier** op (it reconfigures the OTG gadget); the GET is fine
at Read tier.

## state — what's exposed right now?

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/webcam"
```

```json
{
  "ok": true,
  "enabled": false,
  "source": "off",
  "available_sources": ["camera-csi", "hdmi-csi"],
  "supported": true
}
```

- `enabled` — is a UVC device currently presented to the target.
- `source` — which feed is streamed out (`off` when disabled).
- `available_sources` — sources this Orb can expose, gated by its model (a Pi 4
  with only a Cam Link → `["cam-link-usb"]`; a Pi 5 with CSI + HDMI-CSI → both).
  Same source vocabulary as the `aeon-cameras` skill.
- `supported` — whether this Orb's kernel/OTG stack can do UVC at all. If
  `false`, don't bother PUTting — the hardware can't.

## enable — expose a source

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" -X PUT \
    -H "Content-Type: application/json" \
    -d '{"source": "camera-csi"}' \
    "https://$AEON_HOST/api/webcam"
```

`source` ∈ `camera-csi` | `hdmi-csi` | `cam-link-usb` | `off`. Pick one from the
live `available_sources` — requesting a source this Orb lacks is rejected. The
target re-enumerates USB (a ~1 s blip, like a persona swap) and a new UVC webcam
appears in its device list. What each shows the target:

- `camera-csi` — a Pi-5 CSI camera (e.g. Sony IMX477 HQ) — a **physical view**
  (room, desk, operator). Use to show a face/scene on a call.
- `hdmi-csi` — the Pi-5 X1301 HDMI→CSI bridge — the **controlled machine's own
  screen** as a webcam (screen-share-as-camera).
- `cam-link-usb` — an Elgato Cam Link 4K — whatever HDMI feeds that capture.

## disable

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" -X PUT \
    -H "Content-Type: application/json" -d '{"source": "off"}' \
    "https://$AEON_HOST/api/webcam"
```

`off` tears down the UVC gadget; the webcam disappears from the target. Leave it
`off` when idle — a present-but-idle webcam still holds the camera.

## single-source caveat

Webcam passthrough and the network **console** view draw from the **same
physical capture devices**, and a source generally can't feed two consumers at
once. **Don't point the webcam and the console at the SAME source** (e.g. webcam
on `camera-csi` while the `aeon-cameras` source is also `camera-csi`) — they
contend for the one camera and a feed may stall or go black. Safe pattern: split
them — console on `hdmi-csi` (you watch the screen) while the webcam exposes
`camera-csi` (the target shows the room). Check `/api/streamer/config`
(`aeon-cameras`) before enabling the webcam.

There is **no MCP tool** for the webcam — it's REST-only; curl `/api/webcam`.
