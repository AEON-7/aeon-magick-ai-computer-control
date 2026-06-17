# Raspberry Pi 5 — dual-CSI vision (X1301 HDMI bridge + HQ Camera)

From **v103**, the aeon image is a single unified build that boots on **both Pi 4
and Pi 5**. On a Pi 5 it adds two new vision sources over the board's two MIPI
CSI connectors, so an AI vision agent can either:

- **`hdmi-csi`** — watch the **HDMI output of a system it controls**, captured by
  a **Geekworm X1301** (Toshiba **TC358743** HDMI-to-CSI-2 bridge, 1080p60), or
- **`camera-csi`** — watch a **live camera feed** from a **Raspberry Pi HQ Camera**
  (Sony **IMX477**) to check on something physically.

The Pi 4 path (Elgato Cam Link over USB) is **unchanged** — `source = "auto"`
resolves to it exactly as before.

> ⚠️ **Hardware-in-the-loop.** This was built and the config validated against
> docs + the existing code, but **not** tested on a physical Pi 5 + X1301 (none
> available at build time). Everything uncertain is auto-discovered at boot or
> exposed as an on-device knob (config.txt / cmdline.txt / streamer.toml) so a
> wrong guess is fixable on the flashed card **without a re-bake**. Run the
> **post-flash checklist** below before relying on it.

## Wiring

| Connector | Device | Overlay (config.txt `[pi5]`) |
|-----------|--------|------------------------------|
| **CAM1** | Geekworm X1301 (TC358743 HDMI bridge) | `dtoverlay=tc358743,cam1` |
| **CAM0** | Raspberry Pi HQ Camera (IMX477) | `dtoverlay=imx477,cam0` |

Both are pinned (`camera_auto_detect=0`). The camera line is **enabled by
default**; comment it out in `/boot/firmware/config.txt` if no camera is fitted.
Swap `imx477` → `imx708` (Camera Module 3) / `imx219` (Camera v2) for a different
module.

## How an agent switches source

The capture input lives in `/etc/aeon/streamer.toml` under `[capture]` and is
hot-switchable via the supervisor (writes the TOML + restarts `aeon-streamer`):

```bash
# View the controlled machine's HDMI:
curl -XPUT .../api/streamer/config -d '{"source":"hdmi-csi"}'
# Look at the live camera:
curl -XPUT .../api/streamer/config -d '{"source":"camera-csi"}'
# Back to Cam Link USB (Pi 4 default):
curl -XPUT .../api/streamer/config -d '{"source":"auto"}'
```

`GET /api/streamer/config` reports the current `source` plus `platform`
(`pi4`/`pi5`/`other`) so the UI/agent knows which sources are even available
(`hdmi-csi` / `camera-csi` are Pi-5-only). Drive **one source at a time** — the
hardware can have both *configured*, but the streamer streams one (CMA/bandwidth
budget).

## What the image does at boot (Pi 5 only)

`aeon-hdmi-csi.service` (oneshot, self-skips on Pi 4 / no X1301) runs
`/usr/local/bin/aeon-hdmi-csi`, which:

1. Finds the `/dev/mediaN` that owns the `tc358743`.
2. Loads the EDID (`/etc/aeon/hdmi-edid.txt`) so the HDMI source negotiates a
   mode (native **1080p60**; also 1080p50/30, 720p60/50).
3. Latches the incoming DV timings and wires the Media-Controller graph as UYVY.
4. Publishes the resolved nodes to `/run/aeon/hdmi-{video,subdev,media}` and a
   **stable `/dev/aeon-hdmi` symlink** the streamer points at (so re-numbering
   across boots doesn't matter).

The streamer then captures `/dev/aeon-hdmi` through the existing v4l2 → ffmpeg
H.264 pipeline (the TC358743 offers UYVY, already supported; Pi 5 software-encodes
via libx264 since it has no HW H.264 block). The `camera-csi` source instead runs
`rpicam-vid` (MJPEG) piped into ffmpeg, producing the same H.264 + JPEG-snapshot
sinks.

## Post-flash verification checklist (run on the real Pi 5 + X1301)

Run in order; each line gives the expected result and the on-device fix.

1. `cat /proc/device-tree/model` → `Raspberry Pi 5 ...`
2. `dmesg | grep -i tc358743` → `tc358743 4-000f: found`.
   **Empty →** edit `/boot/firmware/config.txt`, change `,cam1` → `,cam0`, reboot.
3. `systemctl status aeon-hdmi-csi` → `active (exited)`; journal shows
   `aeon-hdmi-csi: ready: /dev/aeon-hdmi -> /dev/videoN`.
4. With a live HDMI source plugged into the X1301:
   `v4l2-ctl -d "$(cat /run/aeon/hdmi-subdev)" --query-dv-timings` → `1920x1080p60`
   (or p50/p30). **0×0 →** replug HDMI, then `systemctl restart aeon-hdmi-csi`.
5. `v4l2-ctl -d "$(cat /run/aeon/hdmi-video)" --list-formats-ext` → shows `UYVY`.
6. `v4l2-ctl -d "$(cat /run/aeon/hdmi-video)" --stream-mmap=3 --stream-count=300 --stream-to=/dev/null`
   → ~59.94 fps. **`dma-buf alloc failed` →** add `cma=128M` to
   `/boot/firmware/cmdline.txt`, reboot.
7. `curl -XPUT .../api/streamer/config -d '{"source":"hdmi-csi"}'`, then check
   `/api/streamer/snapshot` shows live HDMI. Green/magenta cast → color-range
   (the streamer uses `in_range=full:out_range=tv`; flip if needed).
8. **Camera:** `rpicam-hello --list-cameras` → lists the **IMX477**.
   `rpicam-vid --camera 0 -t 1000 -o /tmp/c.h264 && ls -l /tmp/c.h264` → non-empty.
   Then `{"source":"camera-csi"}`, confirm the camera feed.
9. `ffmpeg -hide_banner -encoders | grep libx264` → present (Pi 5 has no
   `h264_v4l2m2m`). Watch CPU under streaming — software encode is the cost.

## Known on-device knobs (fix without re-baking)

| Symptom | Fix |
|---------|-----|
| `dmesg` shows no `tc358743 found` | `tc358743,cam1` → `,cam0` in config.txt, reboot |
| Want 4-lane headroom (only if your X1301 wires 4 lanes) | add `,4lane=1` to the tc358743 overlay |
| `dma-buf alloc failed` / pipeline won't start | add `cma=128M` to cmdline.txt |
| HDMI capture stays black | the `media-ctl` pad wiring in `aeon-hdmi-csi` is the tuning point |
| `rpicam-vid` not found | `sudo apt install rpicam-apps` (image tries this at build, non-fatally) |
| No camera fitted | comment out `dtoverlay=imx477,cam0` in config.txt |
