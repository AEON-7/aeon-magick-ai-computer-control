# aeon-magick image-builder

This directory is a pi-gen stage overlay that turns vanilla Pi OS Lite into
the Aeon Magick AI Computer Control appliance.

## Build

```
git clone https://github.com/RPi-Distro/pi-gen.git ~/pi-gen
cd ~/pi-gen
# Copy our overlay in as stage-aeon
cp -R /path/to/aeon-magick-ai-computer-control/image-builder/stage-aeon .
echo IMG_NAME=aeon-magick > config
# Skip the heavy desktop stages
touch stage3/SKIP stage4/SKIP stage5/SKIP
touch stage3/SKIP_IMAGES stage4/SKIP_IMAGES stage5/SKIP_IMAGES
# Build
sudo ./build-docker.sh
```

Output: `deploy/<date>-aeon-magick.img.xz`. Flash with `rpi-imager`,
`dd`, or BalenaEtcher.

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
