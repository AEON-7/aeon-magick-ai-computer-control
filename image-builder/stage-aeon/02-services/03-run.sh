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

on_chroot << 'EOF'
id aeon-orbnet >/dev/null 2>&1 || \
    useradd -r -s /usr/sbin/nologin -d /var/lib/aeon/orbnet -M aeon-orbnet
install -d -m 0700 -o aeon-orbnet -g aeon-orbnet /var/lib/aeon/orbnet
EOF
