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

# OrbNet homeserver (patched Conduit) — staged by scripts/build-conduit.sh. OrbNet
# is off by default, so this just places the binary for when the admin enables it.
if [ -f "${BIN_DIR}/aeon-conduit" ]; then
    install -m 0755 "${BIN_DIR}/aeon-conduit" "${ROOTFS_DIR}/usr/local/bin/aeon-conduit"
fi

# USB-webcam feeder (uvc-gadget, Ideas-on-Board) + its shared lib. Vendored
# prebuilt (built on a Pi 5 against libcamera 0.5.x; its other runtime deps —
# libcamera / libjpeg — come from rpicam-apps). Only used when the webcam
# (uvc.toml) is enabled; harmless otherwise.
if [ -f "${BIN_DIR}/uvc-gadget" ]; then
    install -m 0755 "${BIN_DIR}/uvc-gadget" "${ROOTFS_DIR}/usr/local/bin/uvc-gadget"
    install -d "${ROOTFS_DIR}/usr/local/lib/aarch64-linux-gnu"
    install -m 0755 "${THIS_DIR}/files/lib/libuvcgadget.so.0.4.0" \
        "${ROOTFS_DIR}/usr/local/lib/aarch64-linux-gnu/libuvcgadget.so.0.4.0"
    ln -sf libuvcgadget.so.0.4.0 "${ROOTFS_DIR}/usr/local/lib/aarch64-linux-gnu/libuvcgadget.so.0"
    ln -sf libuvcgadget.so.0 "${ROOTFS_DIR}/usr/local/lib/aarch64-linux-gnu/libuvcgadget.so"
    # Make sure the runtime linker searches the multiarch /usr/local/lib dir.
    echo "/usr/local/lib/aarch64-linux-gnu" > "${ROOTFS_DIR}/etc/ld.so.conf.d/aeon-uvc.conf"
    on_chroot << 'LDCONFIG'
ldconfig
LDCONFIG
fi

cp -R "${WEB_DIR}/." "${ROOTFS_DIR}/usr/share/aeon/web/"

# Shipped, read-only macros + prompts. aeon-supervisor uses /etc/aeon/<dir>/
# (user-editable) first and falls back here.
if [ -d "${SHARE_DIR}/macros" ]; then
    cp -R "${SHARE_DIR}/macros/." "${ROOTFS_DIR}/usr/share/aeon/macros/"
fi
if [ -d "${SHARE_DIR}/prompts" ]; then
    cp -R "${SHARE_DIR}/prompts/." "${ROOTFS_DIR}/usr/share/aeon/prompts/"
fi

# Bundled HAT knowledge base (distilled pinout.xyz data, CC BY-SA 4.0) — the
# supervisor loads this to identify a detected HAT, map its Pi pin usage, flag
# pin collisions, and hand the AI each board's chip/control facts.
if [ -f "${SHARE_DIR}/hat-library.json" ]; then
    cp "${SHARE_DIR}/hat-library.json" "${ROOTFS_DIR}/usr/share/aeon/hat-library.json"
fi

# Curated Hailo-10H model library — the menu of LLM/VLM/STT/vision/OCR models the
# Hailo tab offers for on-device deploy. aeon-hailo + the supervisor's hailo.rs
# read this; sizes drive the ~5500 MB HAT-RAM budget. (aeon-hailo also creates
# /usr/share/aeon/hailo/hef at runtime for downloaded .hef markers.)
# Pi-5-only (the AI HAT+ is PCIe / Pi 5) — gated on AEON_TARGET like the runtime.
if [ "${AEON_TARGET:-pi5}" = "pi5" ] && [ -f "${SHARE_DIR}/hailo/library.json" ]; then
    install -d "${ROOTFS_DIR}/usr/share/aeon/hailo"
    install -m 0644 "${SHARE_DIR}/hailo/library.json" "${ROOTFS_DIR}/usr/share/aeon/hailo/library.json"
fi
