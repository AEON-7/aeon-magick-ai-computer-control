# Architecture

A small set of single-purpose Rust daemons coordinated by a supervisor, all
reachable from a SvelteKit web UI.

## Daemons

```
                          ┌───────────────────────────────┐
                          │   aeon-supervisor          │
                          │   :8443 HTTPS                 │  ← Web UI + REST
                          │   Auth, session, routing      │     + Agent API
                          └───────────────┬───────────────┘
                                          │
                ┌─────────────────────────┼─────────────────────────┐
                ▼                         ▼                         ▼
   ┌─────────────────────┐   ┌─────────────────────┐   ┌─────────────────────┐
   │ aeon-streamer    │   │ aeon-hid         │   │ aeon-netd        │
   │ unix:streamer.sock  │   │ unix:hid.sock       │   │ (systemd-networkd + │
   │                     │   │                     │   │  wpa_supplicant +   │
   │ Capture /dev/video0 │   │ ConfigFS USB        │   │  hostapd watchdog)  │
   │ Adaptive format/res │   │  gadget setup       │   │ WiFi AP fallback    │
   │ HW H.264 (Pi 4)     │   │ HID personas        │   └─────────────────────┘
   │ MJPEG/WebRTC out    │   │ Atomic input ops    │
   └─────────────────────┘   └─────────────────────┘
                                          │
                                          ▼
                       ┌────────────────────────────────┐
                       │ /dev/hidg0..n                  │  USB HID gadget
                       │ → USB-C OTG → target Mac/PC    │  visible to host
                       └────────────────────────────────┘
```

## Why split into separate daemons

1. **Failure isolation** — streamer can crash and HID stays up (and vice versa).
2. **Sandboxing** — `aeon-hid` runs as root (configfs needs it), streamer as `kvm:video`, supervisor as `kvm`. Smaller blast radius.
3. **Independent restarts** — bouncing the streamer (e.g., after a Cam Link
   format change) doesn't drop active HID input.

## The lessons that shaped this

These came from the eye-Pi build that preceded this project:

- **Cam Link 4K UVC enumeration changes with the source signal.** At 4K it
  ONLY offers NV12 (which ustreamer can't capture in stock builds); at lower
  res it offers YUYV + YU12 + NV12. Solution baked into `aeon-streamer`:
  swscale-based NV12 native support + a "always-pick-YU12-when-uncertain"
  rule (Cam Link offers it at every resolution).
- **USB UVC devices don't support V4L2 DV-timings.** Adaptive resolution
  needs polling — both a v4l2-enum-hash watchdog AND a "is the streamer
  actually producing frames" watchdog. Both built into the streamer itself.
- **HID press/release pairing must be atomic at the API surface.** Every
  cursed-hid op is logical (`type "hello"`, `key cmd+space`, `click left`).
  No `key_down` followed by separate `key_up` — that's where dropped
  network packets leave keys stuck. We do not expose primitive press/release
  operations to clients.
- **PiKVM's udev whitelist is unhelpful for arbitrary capture devices.** We
  match by VID/PID in our own udev rule.

## HID personas

`aeon-hid` builds a USB composite device via ConfigFS. The function set
and HID descriptors are pinned by the active *persona*. Personas:

| Name | Devices | VID/PID basis |
|------|---------|---------------|
| `logitech-mx` | Boot keyboard + boot mouse + consumer | 046d (Logitech) Unifying Receiver  |
| `apple-magic` | Apple keyboard + multi-touch trackpad | 05ac (Apple) Magic Keyboard / Trackpad |
| `generic-composite` | Boot keyboard + boot mouse | 1d6b (Linux Foundation) |

Switching persona requires un-bind / re-bind of the USB gadget (target host
sees re-enumeration). API exposes this as `POST /api/persona/switch`.

### Apple multi-touch caveat

The Apple Magic Trackpad HID descriptor and report layout are reverse-engineered
from `linux-magicmouse-hidraw`, hid-apple-patched, and usbmon traces. macOS's
gesture engine (Quartz Event Services) routes Apple-VID multi-touch devices
through a privileged path; the Microsoft Precision Touchpad path does not
fire OS-level swipe gestures. We attempt the Apple route first; if it fails
on a given macOS version, fall back to Precision Touchpad + a Karabiner config
the user installs on the target.

This piece is the highest-risk part of the project and is expected to need
hardware-in-the-loop iteration.

## Streamer pipeline

```
/dev/video0 (Cam Link UVC)
   │
   ▼
v4l2 capture (V4L::Capture) ── current source's native format/res
   │
   ▼
swscale convert ── NV12 / YUYV / UYVY / YU12 → I420
   │
   ▼
encoder
   ├─ Pi 4: h264_v4l2m2m  → H.264 Annex B
   ├─ Pi 5: libx264 (sw)  → H.264 Annex B
   └─ Always available: turbojpeg → MJPEG
   │
   ▼
fan-out
   ├─ WebRTC track (low-latency primary)
   ├─ MJPEG HTTP (compat / agent snapshot endpoint)
   └─ JPEG memsink (single-frame snapshot, cheap polling)
```

`aeon-streamer` watches the v4l2 device:
- Polls `--list-formats-ext` hash every 2s; if it changes, reconfigure capture.
- Polls own capture-fps; if 0 for 4+s, reset device.

## Web UI

SvelteKit + TailwindCSS. WebRTC video in a `<canvas>` for pixel-accurate
display. Input capture overlays the canvas and posts to `/api/hid/*`.

First-boot mode: when `aeon-netd` is in AP fallback, the web UI shows a
WiFi setup wizard instead of the KVM session. After WiFi joins, full UI.

## Tailscale

`tailscale` and `tailscaled` are pre-installed in the image. First boot
auto-`tailscale up --hostname=$HOSTNAME` if a TS auth key was provided via
`/boot/firmware/aeon-setup.toml` (the same file used for initial WiFi).
