#!/bin/bash -e
# Drop pre-built daemons + web UI into the rootfs.
#
# Expects, relative to the image-builder dir:
#   bin/aarch64/aeon-streamer
#   bin/aarch64/aeon-hid
#   bin/aarch64/aeon-supervisor
#   web/build/   (the SvelteKit `npm run build` output)

THIS_DIR="$(dirname "$0")"
BIN_DIR="${THIS_DIR}/files/bin"
WEB_DIR="${THIS_DIR}/files/web"
SHARE_DIR="${THIS_DIR}/files/share/aeon"

mkdir -p "${ROOTFS_DIR}/usr/local/bin"
mkdir -p "${ROOTFS_DIR}/usr/share/aeon/web"
mkdir -p "${ROOTFS_DIR}/usr/share/aeon/macros"
mkdir -p "${ROOTFS_DIR}/usr/share/aeon/prompts"
mkdir -p "${ROOTFS_DIR}/etc/aeon"
mkdir -p "${ROOTFS_DIR}/etc/aeon/macros"
mkdir -p "${ROOTFS_DIR}/etc/aeon/prompts"
mkdir -p "${ROOTFS_DIR}/etc/aeon/scripts"

for b in aeon-streamer aeon-hid aeon-supervisor; do
    install -m 0755 "${BIN_DIR}/${b}" "${ROOTFS_DIR}/usr/local/bin/${b}"
done

cp -R "${WEB_DIR}/." "${ROOTFS_DIR}/usr/share/aeon/web/"

# Shipped, read-only macros + prompts. aeon-supervisor uses /etc/aeon/<dir>/
# (user-editable) first and falls back here.
if [ -d "${SHARE_DIR}/macros" ]; then
    cp -R "${SHARE_DIR}/macros/." "${ROOTFS_DIR}/usr/share/aeon/macros/"
fi
if [ -d "${SHARE_DIR}/prompts" ]; then
    cp -R "${SHARE_DIR}/prompts/." "${ROOTFS_DIR}/usr/share/aeon/prompts/"
fi
