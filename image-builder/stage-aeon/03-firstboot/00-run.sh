#!/bin/bash -e
# Install the first-boot helper.

THIS_DIR="$(dirname "$0")"
install -m 0755 "${THIS_DIR}/files/aeon-firstboot.sh" "${ROOTFS_DIR}/usr/local/bin/aeon-firstboot"

# systemd saves a random seed to carry entropy across boots. If one were left
# in the image, every Orb flashed from it would start from the SAME seed. The
# kernel does not count that seed as entropy by default, so this is belt and
# braces — but there is no reason to ship it. systemd recreates it on first boot.
rm -f "${ROOTFS_DIR}/var/lib/systemd/random-seed"
