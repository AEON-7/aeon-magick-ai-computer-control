#!/bin/bash -e
# OS-update + image-version feature.
#
# 1. Install the aeon-update infra script (on-demand apt upgrade + auto toggle).
# 2. Stamp the image version + track so the running Orb can tell whether a newer
#    image has been published (compared against image-manifest.json in the repo).
#    The stamp lives at /etc/aeon-image-version — OUTSIDE /etc/aeon on purpose, so
#    the config backup/restore (which grabs all of /etc/aeon) can NEVER carry a
#    stale version onto a freshly-flashed newer image.
# 3. Turn ON automatic security patching by default (unattended-upgrades, no
#    auto-reboot) so a fresh Orb "remains patched" out of the box; the console
#    toggle can turn it off.

THIS_DIR="$(dirname "$0")"

# 1. aeon-update infra script.
install -m 0755 "${THIS_DIR}/files/aeon-update" "${ROOTFS_DIR}/usr/local/bin/aeon-update"

# 2. Image version + track + codename stamp.
#    Track comes from the build target (AEON_TARGET); version from
#    AEON_IMAGE_VERSION — BUMP it (or the per-track default below) whenever you
#    publish a new image, and keep image-manifest.json in sync. Codename is read
#    from the target rootfs's own os-release (bookworm for pi4, trixie for pi5).
TRACK="${AEON_TARGET:-pi5}"
case "$TRACK" in
    pi4) DEFV=115 ;;
    pi5) DEFV=115 ;;
    *)   DEFV=0 ;;
esac
VER="${AEON_IMAGE_VERSION:-$DEFV}"
CODENAME="$(. "${ROOTFS_DIR}/etc/os-release" 2>/dev/null; echo "${VERSION_CODENAME:-}")"
cat > "${ROOTFS_DIR}/etc/aeon-image-version" <<EOF
# Aeon Orb image stamp — set at build time, read by aeon-supervisor to compare
# against the latest published version. Do NOT edit on-device; reflash to update.
version=${VER}
track=${TRACK}
codename=${CODENAME}
built=$(date -u +%Y-%m-%d)
EOF
chmod 0644 "${ROOTFS_DIR}/etc/aeon-image-version"

# 3. Automatic security patching ON by default (no auto-reboot).
install -d -m 0755 "${ROOTFS_DIR}/etc/apt/apt.conf.d"
cat > "${ROOTFS_DIR}/etc/apt/apt.conf.d/20auto-upgrades" <<'EOF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
APT::Periodic::AutocleanInterval "7";
EOF
cat > "${ROOTFS_DIR}/etc/apt/apt.conf.d/52aeon-unattended" <<'EOF'
Unattended-Upgrade::Automatic-Reboot "false";
Unattended-Upgrade::Remove-Unused-Dependencies "true";
EOF
chmod 0644 "${ROOTFS_DIR}/etc/apt/apt.conf.d/20auto-upgrades" \
           "${ROOTFS_DIR}/etc/apt/apt.conf.d/52aeon-unattended"

echo "aeon: stamped image v${VER} (${TRACK}/${CODENAME}); auto security-updates on"
