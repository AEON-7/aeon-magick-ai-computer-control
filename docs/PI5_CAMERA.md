# Pi 5 CSI camera — swapping modules (IMX708 Cam 3 / IMX477 HQ / IMX219 …)

The Pi 5 Orb has **two** CSI ports. The Orb's vision stack uses them as:

- **CAM1** → the **tc358743 HDMI-to-CSI bridge** (Geekworm X1301) — the KVM "view the
  controlled machine's HDMI" path. Pinned with `dtoverlay=tc358743,cam1,4lane=1`.
- **CAM0** → an optional **Raspberry Pi camera** — the live-camera vision source
  (the `camera-csi` streamer source).

Because there are only two ports, you can run **one** Pi camera **or** the HDMI
bridge per port — the camera and the bridge coexist fine (different ports, different
drivers), but you can't have the HQ cam **and** the Cam 3 fitted at the same time.

## Any single camera works — no re-flash, no code change

The streamer's `camera-csi` source is **sensor-agnostic** — it streams whatever
libcamera enumerates (IMX477, IMX708, IMX219, …) with no code change. The Camera
Module 3 **Wide** is just the **Wide lens on the same IMX708 sensor** — same overlay.

The one thing that must match the fitted sensor is the **device-tree overlay** in
`config.txt`. The Orb can't use firmware auto-detect here: the tc358743 bridge isn't
auto-detectable, which forces `camera_auto_detect=0`, so the camera must be named
explicitly. To avoid hand-editing `config.txt`, the image ships a selector:

### Swap a camera with a one-word file

On the FAT **`/boot/firmware`** partition (readable from any computer, like the
`aeon-force-ap` flag), create/edit **`aeon-camera`** with the sensor name and reboot:

```sh
echo imx708 > /boot/firmware/aeon-camera   # Camera Module 3 (+ Wide)  ← baked default
echo imx477 > /boot/firmware/aeon-camera   # HQ Camera
echo imx219 > /boot/firmware/aeon-camera   # Camera v2
echo off    > /boot/firmware/aeon-camera   # no CSI camera fitted
```

`aeon-camera-select.service` reads it at boot, rewrites the `dtoverlay=<sensor>,cam0`
line if it differs, and **reboots once** to apply. It's **loop-safe** — it parses the
currently-active sensor each boot and only acts on a real change, re-verifying the
edit landed before rebooting (and restoring a backup if it didn't). It never touches
the tc358743 bridge line. With **no** flag file the baked default (`imx708`) is used.

Supported names: `imx708 imx477 imx219 imx519 imx296 imx290 imx327 imx462 imx500
ov5647 ov9281 ov64a40 off`.

## After swapping — quick checks

- `rpicam-vid --list-cameras` should list the new sensor (e.g. `imx708`). If it's
  empty even though `dmesg | grep imx708` shows the driver bound, the running
  libcamera lacks that sensor's tuning file (current Bookworm ships IMX708's).
- In the web console, the **view-source picker** still shows `camera-csi`; select it
  to stream the camera. The detection just matches any `imx*`/`ov*` video node.
- IMX708 has **autofocus** (VCM); it's on by default. To lock focus, add `,vcm=off`
  to the overlay (hand-edit `config.txt`).

## Runtime contention — auto handoff

libcamera is **single-consumer** per sensor. The streamer's `camera-csi` source and
the USB-webcam feeder (`aeon-uvc`, also CAM0) are mutually exclusive — the second to
open CAM0 gets "Pipeline handler in use by another process" and the console stream
loops forever.

**Console view wins:** `PUT /api/streamer/config` with `"source":"camera-csi"` (or
the web **view** picker) **auto-disables UVC** when the webcam was holding the same
source, stops `aeon-uvc`, and restarts HID so the gadget drops the webcam. Conversely,
`PUT /api/webcam` **refuses** to enable a source that is already the console view
(HTTP 409) — switch the console to `hdmi-csi` first if you need the USB webcam on CAM0.

`aeon-vision` and `aeon-braincraft` never open the camera directly (they tap the
streamer's snapshot socket), so they never fight the lock.

## Camera encode: resolution + fps (separate from HDMI)

The Pi camera does **not** share HDMI / Cam Link `[output].fps` / `width` /
`height`. It has its own knobs under `[capture]`:

| Key | Default | Notes |
|-----|---------|--------|
| `camera_width` / `camera_height` | 1280×720 | Or 1920×1080 for 1080p |
| `camera_fps` | **24** | 15 / 24 / 30 via the UI |
| `camera_pipe` | `yuv420` | Raw planar → one software H.264 pass. Set `mjpeg` only as a fallback |

Live switch (restarts streamer):

```bash
# 720p @ 24 (default — smooth balance on Pi 5 software encode)
curl -sk -u admin:$PW -X PUT -H 'Content-Type: application/json' \
  -d '{"camera_mode":"720p","camera_fps":24}' \
  https://aeon-magick.local/api/streamer/config

# 1080p @ 15 (sharper; more CPU — aim ~15 fps sustained)
curl -sk -u admin:$PW -X PUT -H 'Content-Type: application/json' \
  -d '{"camera_mode":"1080p","camera_fps":15}' \
  https://aeon-magick.local/api/streamer/config
```

The live console shows **cam 720p/1080p** and **cam N fps** pickers only while
the view source is `camera-csi`. HDMI view still uses the global `[output]` fps.

Pipeline: `rpicam-vid --codec yuv420` → ffmpeg `libx264` (Pi 5 has no HW H.264).
The old MJPEG intermediate double-encode path is disabled by default.

## Audio with camera / HDMI

| Source | Recording audio (`audio_device`) | Live web listen (`GET /api/audio/stream`) |
|---|---|---|
| `hdmi-csi` | Auto / default: `plughw:CARD=tc358743,DEV=0` when `dtoverlay=tc358743-audio` is loaded | **HDMI target PCM** (same card) — prefer for KVM |
| `camera-csi` | Auto-fills BrainCraft WM8960 / seeed when present | BrainCraft mics (or USB) |

The live H.264 video pipe is still **video-only**; audio is a **parallel** MP3
stream for the console ("listen" button) and an AAC track in MP4 recordings when
`audio_device` is set.

**HDMI audio requirements (X1301):**

1. `dtoverlay=tc358743-audio` (I2S on GPIO 18/19/20; conflicts with BrainCraft WM8960 I2S).
2. EDID that advertises basic audio + LPCM (`/etc/aeon/hdmi-edid.txt` ships with this).
3. On the **target OS**, set the sound **output** device to the HDMI/display the
   capture card presents (otherwise `audio_present` stays 0 and capture is silence).
4. Check: `v4l2-ctl -d /dev/v4l-subdev2 -C audio_present` → `1`, then
   `arecord -D plughw:CARD=tc358743,DEV=0 -f S32_LE -r 48000 -c 2 -d 3 t.wav`.

## HDMI *out* (display a custom feed)

The **X1301 / Cam Link capture path is input-only** — you cannot reverse it to push
camera or a recording back out the capture card's HDMI port. To show a feed on a
monitor, use the **Pi's own HDMI** (`vc4-hdmi-0` / `vc4-hdmi-1`): e.g. `mpv` /
`ffplay` a recording, or a future `aeon-hdmi-out` service that composites the
streamer snapshot / camera onto the framebuffer. That is separate from KVM capture.
