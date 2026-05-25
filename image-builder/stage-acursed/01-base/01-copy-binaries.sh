#!/bin/bash -e
# Drop pre-built daemons + web UI into the rootfs.
#
# Expects, relative to the image-builder dir:
#   bin/aarch64/acursed-streamer
#   bin/aarch64/acursed-hid
#   bin/aarch64/acursed-supervisor
#   web/build/   (the SvelteKit `npm run build` output)

THIS_DIR="$(dirname "$0")"
BIN_DIR="${THIS_DIR}/files/bin"
WEB_DIR="${THIS_DIR}/files/web"

mkdir -p "${ROOTFS_DIR}/usr/local/bin"
mkdir -p "${ROOTFS_DIR}/usr/share/acursed/web"
mkdir -p "${ROOTFS_DIR}/etc/acursed"

for b in acursed-streamer acursed-hid acursed-supervisor; do
    install -m 0755 "${BIN_DIR}/${b}" "${ROOTFS_DIR}/usr/local/bin/${b}"
done

cp -R "${WEB_DIR}/." "${ROOTFS_DIR}/usr/share/acursed/web/"
