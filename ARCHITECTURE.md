# Architecture

A small set of single-purpose Rust daemons coordinated by a supervisor, all
reachable from a SvelteKit web UI.

## Daemons

```
                       ┌──────────────────────────────────┐
                       │   aeon-supervisor                │
                       │   :8443 HTTPS (rustls)           │  ← Web UI + REST
                       │   Auth, sessions, /api/* routing │     + Agent API
                       │   MCP server at /api/mcp         │     + MCP
                       │   /api/network /api/storage      │
                       │   /api/wifi /api/streamer/*      │
                       └──────────────┬───────────────────┘
                                      │ Unix sockets in /run/aeon/
                ┌─────────────────────┼─────────────────────┐
                ▼                     ▼                     ▼
   ┌────────────────────┐ ┌────────────────────┐ ┌────────────────────┐
   │ aeon-streamer      │ │ aeon-hid           │ │ shell oneshots     │
   │ streamer.sock      │ │ hid.sock           │ │ (root, idempotent) │
   │                    │ │                    │ │                    │
   │ Capture /dev/video0│ │ ConfigFS USB       │ │ aeon-net-services  │
   │ ffmpeg pipe→memory │ │  gadget setup      │ │  (usb_eth + dnscrypt│
   │ JPEG SOI/EOI parse │ │ HID personas       │ │   + VPN apply)     │
   │ watch channel      │ │ Mass-storage CDROM │ │ aeon-netwatch      │
   │ MJPEG HTTP fan-out │ │ Atomic input ops   │ │  (AP fallback)     │
   └────────────────────┘ └────────────────────┘ │ aeon-firstboot     │
                                      │          │  (creds + setup)   │
                                      ▼          └────────────────────┘
                       ┌────────────────────────────────┐
                       │ /dev/hidg0..n                  │  USB HID gadget
                       │ + CDC NCM ethernet (usb0)      │  + optional eth
                       │ + mass_storage.0 (CDROM)       │  + optional disk
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
  ONLY offers NV12; at lower res it offers YUYV + YU12 + NV12. Solution baked
  into `aeon-streamer`: it auto-detects whichever pixel format the v4l2 device
  is presenting and **prefers planar NV12/YU12** (the H.264 encoder ingests it
  with no per-frame software conversion — picking packed YUYV instead pegged
  the CPU at 1080p). Output is MJPEG or low-latency **H.264/WebCodecs**; the
  supervisor's H.264 WebSocket bridge drops to the newest keyframe-anchored
  frame under congestion, so a slow link can't balloon latency.
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
| `generic-composite` | Boot keyboard + relative boot mouse | 1d6b (Linux Foundation) |
| `generic-absolute` | Boot keyboard + **absolute pointer** (X/Y 0..32767) | 1d6b (Linux Foundation) |
| `logitech-mx` | Boot keyboard + boot mouse + consumer | 046d (Logitech) Unifying Receiver |
| `apple-magic-stable` | Apple keyboard + working trackpad | 05ac (Apple) Magic Keyboard / Trackpad |
| `apple-magic` | Apple keyboard + multi-touch trackpad (experimental) | 05ac (Apple) Magic Keyboard / Trackpad |

Switching persona requires un-bind / re-bind of the USB gadget (target host
sees re-enumeration). API exposes this as `POST /api/hid/persona`. The
`generic-absolute` pointer is driven via `POST /api/hid/move_abs` (normalized
0..1 X/Y + button mask); relative personas use `/api/hid/move` + `/api/hid/button`.

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
/dev/video0 (Cam Link UVC, MS2109, …)
   │
   ▼
ffmpeg ─ auto-detected format / resolution
   │   Linux v4l2 input, MJPEG passthrough preferred,
   │   YUV/RGB sources transcoded to MJPEG via libjpeg.
   │
   │   -f image2pipe -c:v mjpeg pipe:1
   ▼
stdout pipe → aeon-streamer (jpeg_pipe.rs)
   │   Parses JPEG SOI (0xFFD8) / EOI (0xFFD9) markers,
   │   extracts one Bytes per frame, drops partial buffers
   │   on resync. No intermediate disk writes — frames live
   │   in process memory only.
   ▼
tokio::sync::watch<Option<Bytes>> ── single source of truth
   ├─ HTTP multipart MJPEG stream (`/api/streamer/stream`)
   │   long-lived connection, re-emits latest frame on watch
   │   change + heartbeat every 1.5s if source stalls.
   ├─ HTTP snapshot (`/api/streamer/snapshot`)
   │   reads `.borrow().clone()`, returns immediately.
   └─ Watchdog
       checks `has_changed()` — restarts ffmpeg if no new
       frame for N seconds.
```

`aeon-streamer` watches the v4l2 device:
- Polls `--list-formats-ext` hash every 2s; if it changes, reconfigure capture.
- Polls own watch channel; if no new frame for 4+s, restart ffmpeg.

The pipe-based design replaced an earlier file-based one (v15-v23 wrote
`live.jpg` with `-update 1 -atomic_writing 1`, but Chrome's multipart
parser occasionally caught a half-written frame and discarded the rest
of the stream). In-memory pipe eliminates the race entirely.

## Web UI

SvelteKit + TailwindCSS. The live stream is a long-lived multipart MJPEG
`<img>` fed by `/api/streamer/stream`. Input capture overlays the image
container and posts to `/api/hid/*`. Snapshot ops (`/api/streamer/snapshot`)
share the same in-memory frame channel — no separate v4l2 grab.

Pages:
- `/` — main KVM session (live stream + persona + input)
- `/network` — USB ethernet + DNSCrypt + VPN (Tailscale/WireGuard/OpenVPN/Tor/I2P)
- `/storage` — USB-CDROM library: upload ISO, set active, eject
- `/tokens` — issue scoped API tokens (admin/full/macros/read)
- `/setup` — first-boot admin password
- `/setup/wifi` — live-scanning WiFi picker (served from the captive AP)

First-boot mode: when `aeon-netwatch` is in AP fallback, the
`/setup/wifi` route is served unauthenticated (state == `open`) and
captive-portal HTTP responders + DNS wildcard auto-launch this page
on every modern OS.

## Tailscale

`tailscale` and `tailscaled` are pre-installed in the image. First boot
auto-`tailscale up --hostname=$HOSTNAME` if a TS auth key was provided via
`/boot/firmware/aeon-setup.toml` (the same file used for initial WiFi).
