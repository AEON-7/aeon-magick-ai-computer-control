# USB webcam — Cam0 (IMX477) exposed as a UVC gadget

On a Pi 5 Orb, the **Cam0 camera (Raspberry Pi HQ Camera / IMX477)** can be
presented to the target host as a standard **USB webcam (UVC)**, alongside the
HID (keyboard/mouse) and CDC-NCM (USB-ethernet) functions the device already
exposes over its one USB-C OTG port. The target then sees: keyboard + mouse +
network adapter + **webcam**.

(Cam1 carries the TC358743 HDMI-to-CSI "KVM" capture that feeds the *network*
streamer — that's separate from this webcam.)

## Status

| Piece | State |
|---|---|
| Gadget function (`aeon-hid` adds `uvc.usb0`) | ✅ **validated on real hardware** — binds with HID composite, `/dev/videoN` gadget node appears, teardown clean |
| Config toggle (`/etc/aeon/uvc.toml`) + service scaffolding | ✅ in image |
| **Feeder daemon (`uvc-gadget`)** | ⏳ **needs an on-device build + a host test** (below) |

## How it works

- **`aeon-hid`** reads `/etc/aeon/uvc.toml`; when `enabled = true` it adds a
  `uvc.usb0` MJPEG function to the composite gadget (`aeon-hid/src/gadget.rs`
  `add_uvc_function`). After the UDC bind a `/dev/videoN` **gadget sink** node
  appears whose `…/function_name` is `uvc.usb0`.
- **`aeon-uvc`** (the feeder, `aeon-uvc.service` → `/usr/local/bin/aeon-uvc`)
  pumps Cam0 frames into that sink, opening the camera only while the host is
  actively viewing (UVC STREAMON) and releasing it on STREAMOFF.

## ⚠ Cam0 is a single resource — contention

libcamera gives **exclusive** access to a sensor. The network streamer's
`camera-csi` source *also* uses Cam0/IMX477, so **the USB webcam and the
`camera-csi` network source are mutually exclusive**. On a webcam-enabled Orb:

> set `streamer.toml` `[capture] source = "hdmi-csi"` so the agent's network
> view uses the KVM (Cam1) and leaves Cam0 free for the webcam.

The feeder's lazy-open (camera held only while the host streams) minimizes the
window even if that's overridden, but the default-to-`hdmi-csi` policy is what
keeps them from fighting.

## Enabling it

1. `/etc/aeon/uvc.toml` → `enabled = true` (set `width`/`height`/`fps` as desired).
2. `/etc/aeon/streamer.toml` → `[capture] source = "hdmi-csi"` (free Cam0).
3. `sudo systemctl restart aeon-hid` (rebinds the gadget with the webcam) then
   `sudo systemctl restart aeon-uvc`.
4. Plug the OTG-C port into a host. The host should enumerate a camera
   (macOS QuickTime → New Movie → camera dropdown; Linux `v4l2-ctl --list-devices`).

## Building the feeder (`uvc-gadget`) — the remaining step

The kernel UVC function only provides the sink; a userspace daemon must service
the host's UVC PROBE/COMMIT and pump frames. Use **uvc-gadget** (Ideas-on-Board)
with its **libcamera source** — the same tool RPi's official USB-webcam tutorial
uses; it debayers the IMX477 through the ISP and re-encodes MJPEG (correct for
the Pi 5, which has no HW H.264). Build it **on the device** (native; the
libcamera sysroot makes cross-compiling not worth it), then vendor the binary
into `image-builder/stage-aeon/01-base/files/bin/uvc-gadget` so the bake just
installs it:

```sh
sudo apt-get install -y git meson ninja-build libcamera-dev libjpeg-dev
git clone https://gitlab.freedesktop.org/camera/uvc-gadget.git   # mirror: github.com/ABelliqueux/uvc-gadget
cd uvc-gadget && meson setup build && ninja -C build
sudo ninja -C build install && sudo ldconfig
# confirm the libcamera source compiled in:
uvc-gadget --help 2>&1 | grep -i libcamera
```
Then pin the exact invocation in `aeon-uvc.sh` (the wrapper currently calls
`uvc-gadget -c <camera_id> uvc.usb0`).

## Verify on hardware (do once on stable USB-C PD power, OTG plugged into a host)

1. `cat /sys/class/video4linux/video*/function_name` → one node prints `uvc.usb0`.
2. Host enumerates a camera (see step 4 above). Confirm HID + NCM still work.
3. Start `aeon-uvc`, open the webcam on the host → live IMX477 video at 720p30.
   Close it → Cam0 released (`rpicam`/`dmesg`).
4. If the host sees USB disconnects under load, drop `uvc.toml streaming_maxpacket`
   2048 → 1024, or `fps` 30 → 20.

## Endpoint budget note

The Pi 5 dwc2 controller has 8 endpoints (EP0 + 7). 3 HID + UVC = 4; add NCM
(~2–3) → 6–7. That fits, but it's near the ceiling — if the gadget fails to bind
on a 3-HID persona *with* both NCM and UVC, drop to a 2-HID persona or disable
one function. (Validated: 3-HID `logitech-mx` + UVC binds cleanly.)
