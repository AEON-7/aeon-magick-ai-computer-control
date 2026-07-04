#!/bin/bash
# aeon-ipfs-boot — auto-enroll this Orb in the IPFS / Model Share network at
# boot. Runs `aeon-ipfs up` (self-bootstrapping: downloads kubo on first run,
# inits the repo, enables pubsub, installs + starts the node's systemd unit)
# UNLESS the operator has opted out — either [ipfs] enabled=false in
# /etc/aeon/ipfs.toml, or a `aeon-no-ipfs` flag file on the FAT boot partition
# (readable from any computer, like `aeon-force-ap`). Idempotent + non-fatal:
# on a network-less first boot the kubo download fails and the node comes up on
# a later boot; the daemon/gossip services self-idle until then.
set -u

BOOT_FLAG=/boot/firmware/aeon-no-ipfs
CONFIG=/etc/aeon/ipfs.toml
log() { echo "aeon-ipfs-boot: $*" >&2; }

if [ -f "$BOOT_FLAG" ]; then
    log "opt-out flag $BOOT_FLAG present; not enrolling"
    exit 0
fi
# Honor an explicit enabled=false (the console's Disable writes this).
if [ -f "$CONFIG" ] && grep -qiE '^[[:space:]]*enabled[[:space:]]*=[[:space:]]*false' "$CONFIG"; then
    log "ipfs disabled in $CONFIG; not enrolling"
    exit 0
fi

log "auto-enrolling in IPFS / Model Share…"
/usr/local/bin/aeon-ipfs up || log "aeon-ipfs up returned non-zero (will retry next boot / when online)"
exit 0
