# Aeon Cursed KVM

A KVM-over-IP for AI agents and humans, descended from `cursed-hid`.

Built specifically for:

- **Elgato Cam Link 4K + any resolution** — adaptive capture pipeline auto-handles 720p / 1080p / 4K NV12 / YUYV / YU12 without manual reconfig.
- **Identity-flexible HID** — present to the target as a Logitech MX-class keyboard+mouse, an Apple Magic-class keyboard+trackpad, or a generic composite device. Pick a persona per session.
- **Multi-touch gesture support on macOS targets** (experimental) — Magic Trackpad emulation lets an agent do 2/3/4-finger gestures.
- **Hardware H.264 on Pi 4** for sub-100ms LAN latency; HEVC + libx264 fallback on Pi 5.
- **Tailscale built in** — your KVM is reachable from anywhere on your tailnet, no port forwarding.
- **WiFi AP fallback for setup** — drop the device anywhere, if it can't find its known networks it spins up `aeon-setup` SSID so you can configure WiFi from your phone.
- **Sleek SvelteKit web UI** — dark, fast, no Java applet ancestry.

Initial target: **Raspberry Pi 4 with Elgato Cam Link 4K**. Pi 5 second.

## Project layout

```
acursed-streamer/   Video capture from Cam Link, HW/SW H.264 encode, MJPEG/WebRTC delivery
acursed-hid/        USB gadget configfs management + HID personas + HTTP/WS input API
acursed-supervisor/ Top-level coordinator + REST API the web UI talks to
acursed-web/        SvelteKit web UI
image-builder/      pi-gen overlay producing aeon-cursed-kvm.img.xz
scripts/            One-off helpers (provisioning, debugging, etc.)
docs/               Design notes, HID descriptor references, etc.
```

See [`ARCHITECTURE.md`](./ARCHITECTURE.md) for the design rationale.

## Status

Phase 1 — image, streamer, Tailscale, WiFi AP fallback : in progress
Phase 2 — HID with Logitech persona : in progress
Phase 3 — Apple persona + multi-touch scaffold : in progress
Phase 5 — Web UI : in progress

(Phase 4 — Hailo annotation overlay — deferred.)

## License

MIT.
