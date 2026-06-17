#!/bin/bash -e
# Ensure dwc2 is loaded for USB OTG, configfs is mounted at boot, USB
# autosuspend is disabled (so the Cam Link doesn't get suspended on
# idle), and NetworkManager owns wlan0.

# config.txt additions. The previous check (`grep -q "^dtoverlay=dwc2"`)
# was too loose — stock Pi OS config.txt has `[cm5] dtoverlay=dwc2,dr_mode=host`
# which matches and skips our append. Check for our exact line instead.
CONFIG_TXT="${ROOTFS_DIR}/boot/firmware/config.txt"
if [ -f "${CONFIG_TXT}" ]; then
    if ! grep -q "^dtoverlay=dwc2,dr_mode=peripheral$" "${CONFIG_TXT}"; then
        cat >> "${CONFIG_TXT}" <<'EOF'

# aeon-magick: enable USB OTG peripheral mode for HID gadget
[all]
dtoverlay=dwc2,dr_mode=peripheral
EOF
    fi

    # ── GPIO / HAT buses ──
    # Enable I2C + SPI on the 40-pin header so HATs + I2C/SPI breakouts work and
    # the hardware dashboard can scan (i2cdetect) + drive them. camera_auto_detect
    # is already on in stock Pi OS config.txt, so a CSI camera auto-detects.
    if ! grep -q "^# aeon-magick gpio buses" "${CONFIG_TXT}"; then
        cat >> "${CONFIG_TXT}" <<'EOF'

# aeon-magick gpio buses
[all]
dtparam=i2c_arm=on
dtparam=spi=on
EOF
    fi

    # ── Sense HAT LED matrix ──
    # The rpi-sense overlay registers the 8x8 RGB framebuffer (+ joystick) so the
    # "alive" heartbeat glow (aeon-orb-glow.service) can drive it. Harmless if no
    # Sense HAT is attached — the overlay simply doesn't probe.
    if ! grep -q "^dtoverlay=rpi-sense$" "${CONFIG_TXT}"; then
        cat >> "${CONFIG_TXT}" <<'EOF'

# aeon-magick: Sense HAT LED matrix (heartbeat glow)
[all]
dtoverlay=rpi-sense
EOF
    fi

    # ── Power-draw optimization ──
    # The device's job is: capture HDMI via Cam Link USB-3, serve frames
    # over the network, emulate USB HID. It NEVER needs Pi's own HDMI
    # output, on-board audio, or Bluetooth. Disabling these saves
    # ~100-150 mA — meaningful on borderline USB-C power deliveries
    # (e.g., from a Mac without a proper PD profile).
    if ! grep -q "^# aeon-magick power optimization" "${CONFIG_TXT}"; then
        cat >> "${CONFIG_TXT}" <<'EOF'

# aeon-magick power optimization + Pi 5 CSI vision
# Bluetooth radio is unused on every model (Cam Link / CSI capture +
# ethernet/WiFi, never BT).
[all]
dtoverlay=disable-bt

# ── Pi 4 (Cam Link USB capture path) ──
# The Pi never drives its own monitor or audio, so blank HDMI + disable audio
# to shave ~100-150 mA. (On Pi 5 we KEEP HDMI for console/debug, and the
# camera stack owns the CSI connectors instead.)
#
# CPU stays at the Pi 4 default 1.8 GHz — no software throttle. If you ever see
# throttle / under-voltage flags in `vcgencmd get_throttled` after a few
# minutes of normal use, the fix is hardware (USB-C cable, port pair, wall
# charger, Y-cable), NOT adding arm_freq back. The under-voltage watchdog timer
# logs any events to the journal regardless.
[pi4]
hdmi_blanking=2
dtparam=audio=off

# ── Pi 5 (Geekworm X1301 HDMI-to-CSI bridge + optional Pi camera) ──
# Both MIPI CSI connectors are hand-pinned, so turn camera auto-detect off.
[pi5]
camera_auto_detect=0
# X1301 / TC358743 HDMI-to-CSI bridge on CAM1 — lets a vision agent view the
# HDMI of a system it controls. The Geekworm X1301 wires 4 CSI lanes, and the
# TC358743 requests >2 lanes for a 1080p60 signal — WITHOUT `4lane` the rp1-cfe
# driver OOPSes ("Device has requested N data lanes, which is >2 configured in
# DT") on STREAMON and NO frames ever flow (capture wedges in D-state). 4lane is
# safe even on a 2-lane bridge (it just gives DT headroom). [Confirmed on real
# Pi 5 + X1301 hardware, 2026-06-11.] If `dmesg | grep tc358743` is EMPTY after
# boot, switch ,cam1 -> ,cam0 here and reboot.
dtoverlay=tc358743,cam1,4lane=1
# CAM0 live-camera vision source. Default: Sony IMX708 (Camera Module 3, incl.
# the Wide variant — same sensor, lens-only difference). Because the tc358743
# bridge above forces camera_auto_detect=0, the camera can't be auto-detected and
# must be named here. You do NOT need to edit this by hand to swap modules: drop a
# one-word file on the FAT /boot/firmware partition and reboot, e.g.
#   echo imx477 > /boot/firmware/aeon-camera   # HQ Camera
#   echo imx219 > /boot/firmware/aeon-camera   # Camera v2
#   echo off    > /boot/firmware/aeon-camera   # no camera fitted
# (handled by aeon-camera-select.service). Comment this out only if shipping
# headless with no camera and no flag. See docs/PI5_CAMERA.md.
dtoverlay=imx708,cam0
# Raspberry Pi AI HAT+ (Hailo accelerator) over PCIe — enable the Pi 5 PCIe lane
# at Gen3 for full bandwidth. A different bus from the CSI overlays above; purely
# additive. If `dmesg` shows PCIe link errors after boot, drop to pciex1_gen=2.
dtparam=pciex1
dtparam=pciex1_gen=3

# Restore the default filter so any later config lines apply to all models.
[all]
EOF
    fi
fi

# cmdline.txt: disable USB autosuspend so capture devices like Cam Link 4K
# don't get suspended after enumeration. Pi OS Bookworm defaults
# autosuspend=2 (suspend idle USB devices after 2s) which kills UVC streams.
CMDLINE_TXT="${ROOTFS_DIR}/boot/firmware/cmdline.txt"
if [ -f "${CMDLINE_TXT}" ]; then
    if ! grep -q "usbcore.autosuspend=-1" "${CMDLINE_TXT}"; then
        # cmdline.txt must be a SINGLE line — append in place, no newline.
        sed -i 's/$/ usbcore.autosuspend=-1/' "${CMDLINE_TXT}"
    fi
    # Enable the cgroup-v2 MEMORY controller. Pi OS ships it OFF by default —
    # cgroup.controllers lists only "cpuset cpu io pids", no "memory" — which
    # SILENTLY no-ops every systemd MemoryMax=/MemoryHigh= limit. Without this,
    # a runaway in any service (e.g. an oversized blacklist fetch) global-OOMs
    # the whole Pi instead of being cgroup-killed + restarted. This switches it
    # on so aeon-supervisor's MemoryMax=1G actually contains a leak.
    if ! grep -q "cgroup_enable=memory" "${CMDLINE_TXT}"; then
        sed -i 's/$/ cgroup_enable=memory cgroup_memory=1/' "${CMDLINE_TXT}"
    fi
fi

# Make sure required modules are loaded on every boot.
install -d "${ROOTFS_DIR}/etc/modules-load.d"
cat > "${ROOTFS_DIR}/etc/modules-load.d/aeon.conf" <<'EOF'
# USB gadget for HID emulation (target-facing)
dwc2
libcomposite
# UVC for HDMI capture devices like Elgato Cam Link 4K
uvcvideo
# I2C userspace char device (/dev/i2c-N) for HAT detection + i2cdetect.
# dtparam=i2c_arm=on (config.txt) enables the controller but does NOT create
# /dev/i2c-N — i2c-dev does. Without this the bus scan + HAT auto-detect (the
# EEPROM-less path used by /api/hardware/i2c) see nothing.
i2c-dev
EOF

# Blacklist Bluetooth modules — we already disabled the hardware via
# disable-bt in config.txt, but the kernel modules try to load and probe
# anyway, briefly drawing power and slowing boot. Explicit blacklist.
install -d "${ROOTFS_DIR}/etc/modprobe.d"
cat > "${ROOTFS_DIR}/etc/modprobe.d/aeon-blacklist.conf" <<'EOF'
# aeon-magick: never load BT
blacklist btbcm
blacklist hci_uart
blacklist btintel
blacklist bluetooth
EOF

# configfs mount at boot
install -d "${ROOTFS_DIR}/etc/fstab.d" || true
if ! grep -q "configfs" "${ROOTFS_DIR}/etc/fstab" 2>/dev/null; then
    echo "configfs   /sys/kernel/config   configfs   defaults   0   0" >> "${ROOTFS_DIR}/etc/fstab"
fi

# Hand wlan0 to NetworkManager (default on Bookworm/Trixie, but be explicit).
install -d "${ROOTFS_DIR}/etc/NetworkManager/conf.d"
cat > "${ROOTFS_DIR}/etc/NetworkManager/conf.d/10-aeon.conf" <<'EOF'
[main]
plugins=keyfile

[device-wlan0]
managed=true
EOF

# ── WiFi regulatory domain ──
# Pi 4's brcmfmac chip is soft-blocked by rfkill until a country code is set
# (`iw reg get` returns "country 00: DFS-UNSET" otherwise). Without this,
# WiFi will NOT work — neither as client nor as the aeon-setup AP fallback.
# We bake `country=US` into wpa_supplicant.conf (raspi-config also writes
# there). NetworkManager picks it up via the regulatory subsystem on boot.
# Override at first-boot from aeon-setup.toml if a user prefers another
# regulatory domain.
install -d "${ROOTFS_DIR}/etc/wpa_supplicant"
if [ ! -f "${ROOTFS_DIR}/etc/wpa_supplicant/wpa_supplicant.conf" ]; then
    cat > "${ROOTFS_DIR}/etc/wpa_supplicant/wpa_supplicant.conf" <<'EOF'
ctrl_interface=DIR=/var/run/wpa_supplicant GROUP=netdev
update_config=1
country=US
EOF
    chmod 600 "${ROOTFS_DIR}/etc/wpa_supplicant/wpa_supplicant.conf"
fi

# Belt and suspenders: drop a oneshot that runs `rfkill unblock all` +
# `iw reg set US` at boot. Some Pi 4 + brcmfmac combos need this explicit
# kick even with `country=US` in wpa_supplicant.conf — and brcmfmac on Pi
# 4 loads asynchronously over SDIO, so `/sys/class/ieee80211` doesn't
# exist when this unit fires. We retry for up to 30s for the wireless
# subsystem to appear before issuing the unblock + reg-set.
install -d "${ROOTFS_DIR}/etc/systemd/system"
cat > "${ROOTFS_DIR}/etc/systemd/system/aeon-wifi-unblock.service" <<'EOF'
[Unit]
Description=Aeon Magick — unblock WiFi radio and set regulatory domain
DefaultDependencies=no
After=systemd-modules-load.service systemd-udev-settle.service
Before=NetworkManager.service wpa_supplicant.service
Wants=systemd-udev-settle.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/sh -c 'for i in $(seq 1 30); do [ -d /sys/class/ieee80211 ] && break; sleep 1; done; /usr/sbin/rfkill unblock all; /usr/sbin/iw reg set US'

[Install]
WantedBy=multi-user.target
EOF

# Belt-and-suspenders persistence for the regulatory domain in case
# something else stomps on it (e.g. a network-manager dispatcher script).
echo 'REGDOMAIN=US' > "${ROOTFS_DIR}/etc/default/crda"
echo 'US' > "${ROOTFS_DIR}/etc/regdomain"

# Enable the unit. 01-run.sh runs OUTSIDE the chroot, so we drop the
# install symlink directly into the rootfs instead of calling systemctl.
install -d "${ROOTFS_DIR}/etc/systemd/system/multi-user.target.wants"
ln -sf /etc/systemd/system/aeon-wifi-unblock.service \
    "${ROOTFS_DIR}/etc/systemd/system/multi-user.target.wants/aeon-wifi-unblock.service"
