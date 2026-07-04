#!/bin/bash
# AEON Orb server entrypoint. The supervisor generates a self-signed TLS cert
# and enters "setup" (open) state on first run — open https://<host>:8443/ and
# set the admin password. Everything persists in the /etc/aeon + /var/lib/aeon
# volumes.
set -e

install -d /etc/aeon /var/lib/aeon /var/lib/aeon/model-library /run/aeon /var/log

export AEON_SERVER=1
# Config file is optional — the supervisor falls back to sane defaults
# (listen 0.0.0.0:443, web root /usr/share/aeon/web, state under /etc/aeon).
exec /usr/local/bin/aeon-supervisor "$@"
