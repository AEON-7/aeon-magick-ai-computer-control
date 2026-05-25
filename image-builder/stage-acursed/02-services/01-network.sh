#!/bin/bash -e
# Ensure dwc2 is loaded for USB OTG, configfs is mounted at boot, and
# NetworkManager owns wlan0 (so our nmcli-based netwatch works).

# Pi 4 / Pi 5 config.txt additions: enable dwc2 in peripheral mode.
CONFIG_TXT="${ROOTFS_DIR}/boot/firmware/config.txt"
if [ -f "${CONFIG_TXT}" ]; then
    if ! grep -q "^dtoverlay=dwc2" "${CONFIG_TXT}"; then
        cat >> "${CONFIG_TXT}" <<'EOF'

# aeon-cursed-kvm: enable USB OTG peripheral mode for HID gadget
dtoverlay=dwc2,dr_mode=peripheral
EOF
    fi
fi

# Make sure modules-load contains dwc2 + libcomposite
install -d "${ROOTFS_DIR}/etc/modules-load.d"
cat > "${ROOTFS_DIR}/etc/modules-load.d/acursed.conf" <<'EOF'
dwc2
libcomposite
EOF

# configfs mount at boot
install -d "${ROOTFS_DIR}/etc/fstab.d" || true
if ! grep -q "configfs" "${ROOTFS_DIR}/etc/fstab" 2>/dev/null; then
    echo "configfs   /sys/kernel/config   configfs   defaults   0   0" >> "${ROOTFS_DIR}/etc/fstab"
fi

# Hand wlan0 to NetworkManager (default on Bookworm/Trixie, but be explicit).
install -d "${ROOTFS_DIR}/etc/NetworkManager/conf.d"
cat > "${ROOTFS_DIR}/etc/NetworkManager/conf.d/10-acursed.conf" <<'EOF'
[main]
plugins=keyfile

[device-wlan0]
managed=true
EOF
