#!/bin/bash -e
# OrbNet — install the homeserver control script + its system user.
#
# OrbNet (anonymous Matrix federation over Tor) is OFF by default. This only
# stages the control script + a low-privilege user; nothing runs until the admin
# enables OrbNet from the dashboard, which provisions the onion + homeserver via
# `aeon-orbnet up`. The patched Conduit binary is staged separately as
# /usr/local/bin/aeon-conduit (01-base/01-run.sh, built by scripts/build-conduit.sh).

THIS_DIR="$(dirname "$0")"

install -m 0755 "${THIS_DIR}/files/aeon-orbnet.sh" "${ROOTFS_DIR}/usr/local/bin/aeon-orbnet"
install -m 0755 "${THIS_DIR}/files/aeon-onions.sh" "${ROOTFS_DIR}/usr/local/bin/aeon-onions"
install -m 0755 "${THIS_DIR}/files/aeon-ipfs.sh"   "${ROOTFS_DIR}/usr/local/bin/aeon-ipfs"
install -m 0755 "${THIS_DIR}/files/aeon-mysterium.sh" "${ROOTFS_DIR}/usr/local/bin/aeon-mysterium"
install -m 0755 "${THIS_DIR}/files/aeon-myst-route.sh" "${ROOTFS_DIR}/usr/local/bin/aeon-myst-route"

on_chroot << 'EOF'
id aeon-orbnet >/dev/null 2>&1 || \
    useradd -r -s /usr/sbin/nologin -d /var/lib/aeon/orbnet -M aeon-orbnet
install -d -m 0700 -o aeon-orbnet -g aeon-orbnet /var/lib/aeon/orbnet
# Belt-and-suspenders: OrbNet ships OFF, and every device MUST mint its own onion
# keys / Conduit DB / reg-token on first `up` (no two flashes may share keys).
# Scrub any OrbNet state that leaked into the build rootfs from a prior
# incremental build — an early bake-wiring once left /etc/aeon/orbnet.toml
# (enabled=true) + provisioned keys here, which shipped in v100 and auto-started
# Tor on every flash with SHARED keys + OOM-looped the supervisor.
rm -f /etc/aeon/orbnet.toml
rm -rf /var/lib/aeon/orbnet/db /var/lib/aeon/orbnet/tls /var/lib/aeon/orbnet/tor \
       /var/lib/aeon/orbnet/reg-token /var/lib/aeon/orbnet/conduit.toml \
       /var/lib/aeon/orbnet/owner.json /var/lib/aeon/orbnet/personas.json
EOF
