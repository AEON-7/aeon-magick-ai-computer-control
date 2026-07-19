#!/bin/bash -e
# Install systemd units + default configs.

THIS_DIR="$(dirname "$0")"

install -d "${ROOTFS_DIR}/etc/systemd/system"
install -d "${ROOTFS_DIR}/etc/aeon"

# Passwordless sudo for the login user. Bookworm's pi-gen dropped a first-user
# nopasswd file automatically; the Trixie pi-gen path did NOT, so a fresh Trixie
# image prompted for a password on every `sudo` over SSH. Restore it explicitly
# (both tracks; harmless if a pi-gen-provided one already exists).
install -d -m 0755 "${ROOTFS_DIR}/etc/sudoers.d"
printf '%s ALL=(ALL) NOPASSWD: ALL\n' "${FIRST_USER_NAME:-admin}" \
    > "${ROOTFS_DIR}/etc/sudoers.d/010-aeon-nopasswd"
chmod 0440 "${ROOTFS_DIR}/etc/sudoers.d/010-aeon-nopasswd"

# Systemd units
for u in \
    aeon-streamer.service \
    aeon-hid.service \
    aeon-supervisor.service \
    aeon-firstboot.service \
    aeon-netwatch.service \
    aeon-netwatch.timer \
    aeon-usb-net.service \
    aeon-net-services.service \
    aeon-undervolt-watchdog.service \
    aeon-undervolt-watchdog.timer \
    aeon-orb-glow.service \
    aeon-hdmi-csi.service \
    aeon-hdmi-csi-watch.service \
    aeon-ups.service \
    aeon-ups-trend.service \
    aeon-ups-trend.timer \
    aeon-uvc.service \
    aeon-vision.service \
    aeon-vision-vlm.service \
    aeon-braincraft.service \
    aeon-audio-init.service \
    dnscrypt-proxy.service; do
    install -m 0644 "${THIS_DIR}/files/${u}" "${ROOTFS_DIR}/etc/systemd/system/${u}"
done

# Default configs
install -m 0644 "${THIS_DIR}/files/streamer.toml"      "${ROOTFS_DIR}/etc/aeon/streamer.toml"
install -m 0644 "${THIS_DIR}/files/hid.toml"           "${ROOTFS_DIR}/etc/aeon/hid.toml"
install -m 0644 "${THIS_DIR}/files/supervisor.toml"    "${ROOTFS_DIR}/etc/aeon/supervisor.toml"
install -m 0600 "${THIS_DIR}/files/network.toml"       "${ROOTFS_DIR}/etc/aeon/network.toml"  # 0600: holds the Tailscale auth_key + VPN creds
install -m 0644 "${THIS_DIR}/files/storage.toml"       "${ROOTFS_DIR}/etc/aeon/storage.toml"
# Fleet membership (Phase 1) — ships empty (no token) so a fresh Orb shows only
# itself in the Fleet tab until enrolled. 0600: will hold the shared fleet token.
install -m 0600 "${THIS_DIR}/files/fleet.toml"         "${ROOTFS_DIR}/etc/aeon/fleet.toml"
install -m 0755 "${THIS_DIR}/files/aeon-usb-net.sh"    "${ROOTFS_DIR}/usr/local/bin/aeon-usb-net"
install -m 0755 "${THIS_DIR}/files/aeon-net-services.sh" "${ROOTFS_DIR}/usr/local/bin/aeon-net-services"
# USB webcam (Pi 5 Cam0 IMX477 → UVC gadget). The toggle config + the feeder
# wrapper. OFF by default; aeon-hid adds the gadget function only when
# uvc.toml enabled=true. The uvc-gadget feeder binary is built/vendored
# separately (see docs/UVC_WEBCAM.md) — the wrapper self-skips without it.
install -m 0644 "${THIS_DIR}/files/uvc.toml"          "${ROOTFS_DIR}/etc/aeon/uvc.toml"
install -m 0755 "${THIS_DIR}/files/aeon-uvc.sh"       "${ROOTFS_DIR}/usr/local/bin/aeon-uvc"
# Pi 5 HDMI-over-CSI (Geekworm X1301 / TC358743): the EDID the bridge presents
# to the HDMI source + the boot-time pipeline-setup script. No-op on Pi 4.
install -m 0644 "${THIS_DIR}/files/hdmi-edid.txt"      "${ROOTFS_DIR}/etc/aeon/hdmi-edid.txt"
install -m 0755 "${THIS_DIR}/files/aeon-hdmi-csi.sh"   "${ROOTFS_DIR}/usr/local/bin/aeon-hdmi-csi"
install -m 0755 "${THIS_DIR}/files/aeon-hdmi-csi-watch.sh" "${ROOTFS_DIR}/usr/local/bin/aeon-hdmi-csi-watch"
# Waveshare UPS HAT (E) battery monitor + low-battery safe-shutdown daemon.
# No-op (idles) on a Pi without the HAT (probes I2C 0x2D, finds nothing).
install -m 0755 "${THIS_DIR}/files/aeon-ups.py"        "${ROOTFS_DIR}/usr/local/bin/aeon-ups"
# Once-a-minute UPS/power trend logger (survives hard power-cuts; journald
# alone is not enough on Pi OS which ships Storage=volatile by default).
install -m 0755 "${THIS_DIR}/files/aeon-ups-trend.sh"  "${ROOTFS_DIR}/usr/local/bin/aeon-ups-trend"
# Keep journals across reboots so UPS / undervolt / CSI storms leave a trail.
# Pi OS ships Storage=volatile in 40-rpi-volatile-storage.conf; conf.d files are
# merged in lexicographic order regardless of /etc vs /usr, so we both drop a
# late 99- override AND shadow the rpi file under the same basename in /etc.
install -d "${ROOTFS_DIR}/etc/systemd/journald.conf.d"
install -m 0644 "${THIS_DIR}/files/aeon-journald-persistent.conf" \
    "${ROOTFS_DIR}/etc/systemd/journald.conf.d/99-aeon-persistent.conf"
printf '[Journal]\nStorage=persistent\n' \
    > "${ROOTFS_DIR}/etc/systemd/journald.conf.d/40-rpi-volatile-storage.conf"
# On-device live vision (OCR / Hailo AI HAT+ detection over the capture feed).
# OFF by default; self-idles without the streamer/Hailo. Drop .hef models into
# the vision dir once the AI HAT+ is enumerating (see vision.toml).
install -m 0755 "${THIS_DIR}/files/aeon-vision.py"     "${ROOTFS_DIR}/usr/local/bin/aeon-vision"
install -m 0644 "${THIS_DIR}/files/vision.toml"        "${ROOTFS_DIR}/etc/aeon/vision.toml"
install -d "${ROOTFS_DIR}/usr/share/aeon/vision"
# describe_screen VLM service (Qwen2-VL on the Hailo NPU). Gated by its unit on
# /dev/hailo0 + the VLM .hef, and the VLM is loaded lazily on the first
# /describe — so it's a no-op until the model is deployed.
install -m 0755 "${THIS_DIR}/files/aeon-vision-vlm"    "${ROOTFS_DIR}/usr/local/bin/aeon-vision-vlm"
# BrainCraft HAT (Adafruit): 240x240 ST7789 TFT + buttons + WM8960/USB audio. The
# aeon-braincraft daemon drives the AI-face / camera-viewfinder / voice modes
# (auto-detecting gpiochip4 on Pi 5 / gpiochip0 on Pi 4 + the audio device);
# aeon-voice is the button-driven installer for the Piper TTS + Vosk ASR +
# display/audio stack (NOT baked — provisioned on demand via POST
# /api/braincraft/voice/install, exactly like the Hailo runtime). Works on BOTH
# tracks (no AEON_TARGET gate). OFF by default; self-idles without the HAT.
install -m 0755 "${THIS_DIR}/files/aeon-braincraft"    "${ROOTFS_DIR}/usr/local/bin/aeon-braincraft"

# WM8960 MICBIAS overlay for the BrainCraft codec — enabled by 01-run.sh's
# `dtoverlay=wm8960-mic` ([pi5]); it adds the MICB DAPM routes mainline wm8960.c
# omits so the mic-bias powers during capture. (.dts kept beside it for source.)
install -d "${ROOTFS_DIR}/boot/firmware/overlays"
install -m 0644 "${THIS_DIR}/files/wm8960-mic.dtbo" "${ROOTFS_DIR}/boot/firmware/overlays/wm8960-mic.dtbo"
install -m 0755 "${THIS_DIR}/files/aeon-voice"         "${ROOTFS_DIR}/usr/local/bin/aeon-voice"
install -m 0644 "${THIS_DIR}/files/braincraft.toml"    "${ROOTFS_DIR}/etc/aeon/braincraft.toml"
# Boot-time WM8960 audio init: opens the DAC->output-mixer routing (off at chip
# default = silent speaker on a fresh flash) + sane levels. Self-skips with no
# codec. Ships on both tracks (BrainCraft is not Pi-5-gated).
install -m 0755 "${THIS_DIR}/files/aeon-audio-init.sh" "${ROOTFS_DIR}/usr/local/bin/aeon-audio-init"
# System-wide ALSA map: default + aeon/aeon_play/aeon_cap → WM8960 by card id
# (never HDMI; never bare hw:N). Safe no-op when the card is absent.
install -m 0644 "${THIS_DIR}/files/asound.conf" "${ROOTFS_DIR}/etc/asound.conf"
# Hailo-10H AI accelerator (Pi AI HAT+ 2). The supervisor's hailo.rs shells out
# to /usr/local/bin/aeon-hailo for all device-specific work (status/install/
# stats/models/deploy/unload) and offers the curated library.json (installed in
# 01-base). OFF by default; the HAT runtime is NOT baked — install is button-
# driven at runtime via POST /api/hailo/install. There is no systemd unit: the
# script is invoked on demand, not run as a daemon. The model library lives at
# /usr/share/aeon/hailo/ (library.json shipped by 01-base; aeon-hailo creates the
# hef/ marker dir + the /run/aeon ledger at runtime).
# Pi-5-ONLY (the AI HAT+ is PCIe / Pi 5). Gated on AEON_TARGET so the Pi 4 release
# omits the Hailo super-app entirely (defaults to pi5 → included unless a pi4 bake).
if [ "${AEON_TARGET:-pi5}" = "pi5" ]; then
    install -m 0755 "${THIS_DIR}/files/aeon-hailo"         "${ROOTFS_DIR}/usr/local/bin/aeon-hailo"
    install -m 0644 "${THIS_DIR}/files/hailo.toml"         "${ROOTFS_DIR}/etc/aeon/hailo.toml"
    install -d "${ROOTFS_DIR}/usr/share/aeon/hailo"
    # Hailo GenAI server (hailo-ollama, REST :8000) — enabled but self-skips
    # (ConditionPathExists /dev/hailo0 + the binary) until the 10H runtime + the
    # GenAI deb are installed. The button-driven aeon-hailo install lays those down.
    install -m 0644 "${THIS_DIR}/files/aeon-hailo-ollama.service" "${ROOTFS_DIR}/etc/systemd/system/aeon-hailo-ollama.service"
    on_chroot <<'HAILOOLLAMA'
systemctl enable aeon-hailo-ollama.service
HAILOOLLAMA
fi

# Pi 5 CSI camera selector. The [pi5] config.txt pins ONE camera overlay on CAM0
# (camera_auto_detect is forced off by the non-auto-detectable tc358743 bridge),
# so this boot service lets a single image support any Pi camera (IMX708 Cam 3,
# IMX477 HQ, IMX219 v2, …) by reading /boot/firmware/aeon-camera and rewriting the
# overlay + rebooting once if it differs. Pi-5-ONLY (the CSI camera overlays are in
# the [pi5] section; the Pi 4 uses USB Cam Link). Loop-safe; no-op without the flag.
if [ "${AEON_TARGET:-pi5}" = "pi5" ]; then
    install -m 0755 "${THIS_DIR}/files/aeon-camera-select.sh"      "${ROOTFS_DIR}/usr/local/bin/aeon-camera-select"
    install -m 0644 "${THIS_DIR}/files/aeon-camera-select.service" "${ROOTFS_DIR}/etc/systemd/system/aeon-camera-select.service"
    on_chroot <<'CAMSEL'
systemctl enable aeon-camera-select.service
CAMSEL
fi

# /run/aeon is the unix-socket rendezvous. tmpfiles.d ensures it exists on
# every boot before any aeon-* service tries to bind there. Without this,
# aeon-streamer logs "No such file or directory" on its socket bind and
# half-starts.
install -d "${ROOTFS_DIR}/etc/tmpfiles.d"
cat > "${ROOTFS_DIR}/etc/tmpfiles.d/aeon.conf" <<'EOF'
d /run/aeon 0755 aeon aeon -
d /run/aeon/snapshots 0755 aeon aeon -
# Persistent ISO library for the USB-CDROM mass-storage gadget.
# Lives in /var so uploaded ISOs survive reboots. Owned by root because
# the supervisor (which writes here via /api/storage/upload) runs as
# root for port-80 binding.
d /var/lib/aeon 0755 root root -
d /var/lib/aeon/iso 0755 root root -
# DNS blacklist subscription caches — one file per source, populated by
# the supervisor's background refresh task.
d /var/lib/aeon/dns-sources 0755 root root -
# File-transfer staging dir: HTTP file server on usb0 reads/writes here.
# Off by default — only the directory exists; the target-facing
# listener doesn't bind until /etc/aeon/file-xfer.toml enables it.
d /var/lib/aeon/files 0755 root root -
# BrainCraft viewfinder captures (photos + video clips). Only used when the
# HAT is present and viewfinder mode takes a photo / records; the dir just
# needs to pre-exist. Root-owned (aeon-braincraft runs as root, like aeon-vision).
d /var/lib/aeon/captures 0755 root root -
# Audit log file is created on first append by the supervisor; only the
# parent dir needs to exist. (Covered by /var/lib/aeon above.)
EOF

# Helper scripts
install -m 0755 "${THIS_DIR}/files/aeon-netwatch.sh"          "${ROOTFS_DIR}/usr/local/bin/aeon-netwatch"
install -m 0755 "${THIS_DIR}/files/aeon-undervolt-watchdog.sh" "${ROOTFS_DIR}/usr/local/bin/aeon-undervolt-watchdog"
install -m 0755 "${THIS_DIR}/files/aeon-vpn-status.py"        "${ROOTFS_DIR}/usr/local/bin/aeon-vpn-status"
install -m 0755 "${THIS_DIR}/files/aeon-vpn-rotate.py"        "${ROOTFS_DIR}/usr/local/bin/aeon-vpn-rotate"
# Sense HAT "alive" heartbeat glow (drives the 8x8 LED matrix via the rpi-sense
# framebuffer; no-op if no Sense HAT / framebuffer is present).
install -m 0755 "${THIS_DIR}/files/aeon-orb-heartbeat.py"    "${ROOTFS_DIR}/usr/local/bin/aeon-orb-heartbeat.py"

on_chroot << EOF
useradd -r -s /usr/sbin/nologin -G video,plugdev aeon || true
mkdir -p /run/aeon
chown aeon:aeon /run/aeon

systemctl enable aeon-firstboot.service
systemctl enable aeon-streamer.service
systemctl enable aeon-hid.service
systemctl enable aeon-supervisor.service
systemctl enable aeon-netwatch.timer
systemctl enable aeon-usb-net.service
systemctl enable aeon-net-services.service
systemctl enable aeon-undervolt-watchdog.timer
systemctl enable aeon-orb-glow.service
# Pi 5 HDMI-to-CSI bridge setup (oneshot; self-skips on Pi 4 / no X1301).
systemctl enable aeon-hdmi-csi.service
systemctl enable aeon-hdmi-csi-watch.service
# USB webcam feeder (self-skips when uvc.toml disabled / no camera / no feeder binary).
systemctl enable aeon-uvc.service
# UPS HAT (E) battery monitor + safe-shutdown (idles cleanly if no HAT).
systemctl enable aeon-ups.service
# Minute-resolution UPS/power trend (diagnostic ring for hard power-cuts).
systemctl enable aeon-ups-trend.timer
# On-device vision OCR/detection (idles cleanly if disabled / no streamer).
systemctl enable aeon-vision.service
# describe_screen VLM service (no-op until the VLM .hef is deployed; the unit's
# ConditionPathExists gates it, and the model loads lazily + idle-unloads).
systemctl enable aeon-vision-vlm.service
# BrainCraft HAT interface/viewfinder/voice (idles cleanly if disabled / no HAT).
systemctl enable aeon-braincraft.service
# WM8960 audio baseline so a fresh flash boots with an audible speaker (self-skips
# with no codec). Both tracks — the BrainCraft is not Pi-5-gated.
systemctl enable aeon-audio-init.service

# Pin system DNS to the local dnscrypt-proxy (127.0.2.1) via resolvconf's 'head'
# file. NetworkManager normally registers this, but /etc/resolv.conf was observed
# coming up EMPTY (regenerated before NM's record landed) -> box-wide DNS failure
# (the node could resolve nothing). 'head' is prepended to every generated
# resolv.conf, so resolution is guaranteed regardless of NM/resolvconf timing.
mkdir -p /etc/resolvconf/resolv.conf.d
if ! grep -q '127.0.2.1' /etc/resolvconf/resolv.conf.d/head 2>/dev/null; then echo 'nameserver 127.0.2.1' >> /etc/resolvconf/resolv.conf.d/head; fi

# aeon-wifi-unblock.service is created by 01-run.sh and enabled there.
EOF
