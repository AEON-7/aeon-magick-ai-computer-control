#!/bin/bash -e
# Install the first-boot helper.

THIS_DIR="$(dirname "$0")"
install -m 0755 "${THIS_DIR}/files/acursed-firstboot.sh" "${ROOTFS_DIR}/usr/local/bin/acursed-firstboot"
