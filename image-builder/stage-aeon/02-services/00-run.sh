#!/bin/bash -e
# Install systemd units + default configs.

THIS_DIR="$(dirname "$0")"

install -d "${ROOTFS_DIR}/etc/systemd/system"
install -d "${ROOTFS_DIR}/etc/aeon"

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
    dnscrypt-proxy.service; do
    install -m 0644 "${THIS_DIR}/files/${u}" "${ROOTFS_DIR}/etc/systemd/system/${u}"
done

# Default configs
install -m 0644 "${THIS_DIR}/files/streamer.toml"      "${ROOTFS_DIR}/etc/aeon/streamer.toml"
install -m 0644 "${THIS_DIR}/files/hid.toml"           "${ROOTFS_DIR}/etc/aeon/hid.toml"
install -m 0644 "${THIS_DIR}/files/supervisor.toml"    "${ROOTFS_DIR}/etc/aeon/supervisor.toml"
install -m 0644 "${THIS_DIR}/files/network.toml"       "${ROOTFS_DIR}/etc/aeon/network.toml"
install -m 0755 "${THIS_DIR}/files/aeon-usb-net.sh"    "${ROOTFS_DIR}/usr/local/bin/aeon-usb-net"
install -m 0755 "${THIS_DIR}/files/aeon-net-services.sh" "${ROOTFS_DIR}/usr/local/bin/aeon-net-services"

# /run/aeon is the unix-socket rendezvous. tmpfiles.d ensures it exists on
# every boot before any aeon-* service tries to bind there. Without this,
# aeon-streamer logs "No such file or directory" on its socket bind and
# half-starts.
install -d "${ROOTFS_DIR}/etc/tmpfiles.d"
cat > "${ROOTFS_DIR}/etc/tmpfiles.d/aeon.conf" <<'EOF'
d /run/aeon 0755 aeon aeon -
d /run/aeon/snapshots 0755 aeon aeon -
EOF

# Helper scripts
install -m 0755 "${THIS_DIR}/files/aeon-netwatch.sh"          "${ROOTFS_DIR}/usr/local/bin/aeon-netwatch"
install -m 0755 "${THIS_DIR}/files/aeon-undervolt-watchdog.sh" "${ROOTFS_DIR}/usr/local/bin/aeon-undervolt-watchdog"
install -m 0755 "${THIS_DIR}/files/aeon-vpn-status.py"        "${ROOTFS_DIR}/usr/local/bin/aeon-vpn-status"
install -m 0755 "${THIS_DIR}/files/aeon-vpn-rotate.py"        "${ROOTFS_DIR}/usr/local/bin/aeon-vpn-rotate"

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
# aeon-wifi-unblock.service is created by 01-run.sh and enabled there.
EOF
