# aeon-magick image-builder

This directory is a pi-gen stage overlay that turns vanilla Pi OS Lite into
the Aeon Magick AI Computer Control appliance.

## Build

```
git clone https://github.com/RPi-Distro/pi-gen.git ~/pi-gen
cd ~/pi-gen
# Copy our overlay in as stage-aeon
cp -R /path/to/aeon-magick-ai-computer-control/image-builder/stage-aeon .
printf 'IMG_NAME=aeon-magick\nARM64=1\n' > config   # Pi 4 flavor (Bookworm); Pi 5 flavor bakes with RELEASE=trixie
# Skip the heavy desktop stages
touch stage3/SKIP stage4/SKIP stage5/SKIP
touch stage3/SKIP_IMAGES stage4/SKIP_IMAGES stage5/SKIP_IMAGES
# Build
sudo ./build-docker.sh
```

Output: `deploy/<date>-aeon-magick.img.xz`. Flash with `rpi-imager`,
`dd`, or BalenaEtcher.

**Two flavors share this overlay.** The **Pi 4 image** bakes on Pi OS
**Bookworm** (this branch's default config); the **Pi 5 image** bakes on
Pi OS **Trixie** (`RELEASE=trixie` + matching pi-gen branch) and carries
the Pi 5 suite — the Hailo AI HAT super-app, HDMI-to-CSI capture, voice,
UPS (`feat/pi5-vision-voice-ups-av` track). The stage scripts never
hardcode the codename. Model-specific behavior is auto-detected at
runtime either way: `aeon-hid` reads the UDC from `/sys/class/udc`
(`fe980000.usb` vs `1000480000.usb`), the streamer picks the encoder per
platform (Pi 4 hardware `h264_v4l2m2m`, Pi 5 software `libx264`), and
config.txt carries a `[pi5]`-scoped `usb_max_current_enable=1` so a
target-powered Pi 5 still feeds a USB capture stick full current.

## What goes into the image

| Stage script | Effect |
|---|---|
| `01-base/00-run.sh` | Install runtime packages (ustreamer, v4l-utils, network-manager, tailscale, etc.) |
| `01-base/01-copy-binaries.sh` | Drop the three Rust daemons (built ahead of time for `aarch64-unknown-linux-gnu`) and the SvelteKit static build into `/usr/local/bin` and `/usr/share/aeon/web` |
| `02-services/00-run.sh` | Install systemd units, enable them |
| `02-services/01-network.sh` | NetworkManager + WiFi-or-AP-fallback watchdog |
| `02-services/02-udev.sh` | Cam Link udev rule (Elgato 0fd9:0066 → /dev/kvmd-video) |
| `03-firstboot/00-run.sh` | First-boot oneshot: generate auth password, prompt for WiFi if not configured, start daemons |

## Cross-compilation note

Daemons are compiled to `aarch64-unknown-linux-gnu` using
`scripts/build-binaries.sh` (in the repo root). The image-builder expects
them in `image-builder/bin/aarch64/`. Build them on your dev box before
running pi-gen.
