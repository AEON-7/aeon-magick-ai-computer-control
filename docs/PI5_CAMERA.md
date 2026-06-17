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

## Runtime contention (unchanged by the swap)

libcamera is **single-consumer** per sensor. The streamer's `camera-csi` source and
the USB-webcam feeder (`aeon-uvc`, also CAM0) are mutually exclusive — the second to
open CAM0 gets "camera in use". On a webcam-enabled Orb, set the streamer's view to
`hdmi-csi` (CAM1) so the network view uses the KVM bridge and leaves CAM0 free for
the UVC webcam. `aeon-vision` and `aeon-braincraft` never open the camera directly
(they tap the streamer's snapshot socket), so they never fight the lock.
