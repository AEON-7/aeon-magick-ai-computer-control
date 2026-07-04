#!/bin/bash
# AEON Orb server entrypoint. The supervisor generates a self-signed TLS cert
# and enters "setup" (open) state on first run — open https://<host>:8443/ and
# set the admin password. Everything persists in the /etc/aeon + /var/lib/aeon
# volumes.
#
# There's no systemd in the container, so this stands in for the Pi's boot
# oneshots: it starts the ClamAV signature updater, brings IPFS up, and launches
# the Model Share gossip daemon — so a server participates in the network the
# same way an Orb does — then hands off to the supervisor as PID 1 (via tini).
set -e

install -d /etc/aeon /var/lib/aeon /var/lib/aeon/model-library \
          /var/lib/aeon/ipfs-models /run/aeon /var/log
export AEON_SERVER=1

# ClamAV signatures in the background (best-effort; the download sandbox's scan
# degrades gracefully until the DB is present, then refreshes every 6h).
if command -v freshclam >/dev/null 2>&1; then
  ( freshclam --quiet 2>/dev/null || true
    while :; do sleep 21600; freshclam --quiet 2>/dev/null || true; done ) &
fi

# IPFS + Model Share are core + default-on. Opt out with AEON_NO_IPFS=1 or
# `[ipfs] enabled = false` in /etc/aeon/ipfs.toml (same contract as the Pi).
ipfs_disabled=0
[ -n "${AEON_NO_IPFS:-}" ] && [ "${AEON_NO_IPFS}" != "0" ] && ipfs_disabled=1
if [ -f /etc/aeon/ipfs.toml ] && grep -qiE '^[[:space:]]*enabled[[:space:]]*=[[:space:]]*false' /etc/aeon/ipfs.toml; then
  ipfs_disabled=1
fi
if [ "$ipfs_disabled" = 0 ] && command -v aeon-ipfs >/dev/null 2>&1; then
  ( aeon-ipfs up >/dev/null 2>&1 || true ) &                  # container path = kubo via setsid
  ( aeon-modelshare >/dev/null 2>&1 || true ) &               # gossip catalog + hear peers
fi

# Config file is optional — the supervisor falls back to sane defaults
# (listen 0.0.0.0:443, web root /usr/share/aeon/web, state under /etc/aeon).
exec /usr/local/bin/aeon-supervisor "$@"
