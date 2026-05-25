#!/bin/bash -e
# Install systemd units + default configs.

THIS_DIR="$(dirname "$0")"

install -d "${ROOTFS_DIR}/etc/systemd/system"
install -d "${ROOTFS_DIR}/etc/acursed"

# Systemd units
for u in \
    acursed-streamer.service \
    acursed-hid.service \
    acursed-supervisor.service \
    acursed-firstboot.service \
    acursed-netwatch.service \
    acursed-netwatch.timer; do
    install -m 0644 "${THIS_DIR}/files/${u}" "${ROOTFS_DIR}/etc/systemd/system/${u}"
done

# Default configs
install -m 0644 "${THIS_DIR}/files/streamer.toml"    "${ROOTFS_DIR}/etc/acursed/streamer.toml"
install -m 0644 "${THIS_DIR}/files/hid.toml"         "${ROOTFS_DIR}/etc/acursed/hid.toml"
install -m 0644 "${THIS_DIR}/files/supervisor.toml"  "${ROOTFS_DIR}/etc/acursed/supervisor.toml"

# Helper scripts
install -m 0755 "${THIS_DIR}/files/acursed-netwatch.sh" "${ROOTFS_DIR}/usr/local/bin/acursed-netwatch"

on_chroot << EOF
useradd -r -s /usr/sbin/nologin -G video,plugdev acursed || true
mkdir -p /run/acursed
chown acursed:acursed /run/acursed

systemctl enable acursed-firstboot.service
systemctl enable acursed-streamer.service
systemctl enable acursed-hid.service
systemctl enable acursed-supervisor.service
systemctl enable acursed-netwatch.timer
EOF
