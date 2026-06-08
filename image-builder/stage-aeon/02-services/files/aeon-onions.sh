#!/bin/bash
# aeon-onions — host Tor v3 hidden services on this Orb. Each service maps an
# .onion virtual port to a local app port; mint as many as you like, one per
# thing you host (a site, an app, an API). Runs a DEDICATED Tor instance
# (separate from the privacy-stack Tor and the OrbNet Tor) so user-hosted
# onions never interfere with those.
#
# Self-bootstrapping: creates its system user, data dir, torrc + systemd unit
# on first `up`, so it behaves identically on a live device and when baked.
#
# Subcommands:
#   up | down | status
#   add <id> <local_port> [virt_port=80]   -> mints the onion, prints the address
#   remove <id>
#   list                                    -> "<id>\t<onion>\t<virt>\t<local>" per line
#   onion <id>                              -> prints the .onion for <id>
set -uo pipefail

DIR=/var/lib/aeon/onions
TORDIR="$DIR/tor"
DATADIR="$TORDIR/data"
HSROOT="$DIR/hs"
CONFD="$DIR/services.d"
TORRC="$TORDIR/torrc"
SVCUSER=aeon-onions
UNIT=/etc/systemd/system/aeon-onions-tor.service

log() { echo "aeon-onions: $*" >&2; }

ensure_user() {
  id "$SVCUSER" >/dev/null 2>&1 || \
    useradd -r -s /usr/sbin/nologin -d "$DIR" -M "$SVCUSER" 2>/dev/null || true
}

ensure_dirs() {
  for d in "$DIR" "$TORDIR" "$DATADIR" "$HSROOT" "$CONFD"; do
    install -d -m 700 "$d"
  done
  chown -R "$SVCUSER:$SVCUSER" "$DIR" 2>/dev/null || true
}

write_torrc() {
  # Pure hosting: no outbound SOCKS needed. Per-service HiddenService blocks
  # live in $CONFD/*.conf and are pulled in by the %include below (Tor includes
  # every file in the directory), so add/remove is just write/delete + reload.
  cat > "$TORRC" <<EOF
# Managed by aeon-onions — dedicated Tor for user-hosted hidden services.
DataDirectory $DATADIR
SocksPort 0
%include $CONFD
EOF
  chown "$SVCUSER:$SVCUSER" "$TORRC" 2>/dev/null || true
}

ensure_units() {
  cat > "$UNIT" <<EOF
[Unit]
Description=Aeon Magick — user hidden-services Tor
After=network-online.target
Wants=network-online.target
[Service]
User=$SVCUSER
ExecStart=/usr/bin/tor -f $TORRC
ExecReload=/bin/kill -HUP \$MAINPID
Restart=on-failure
RestartSec=5
[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload 2>/dev/null || true
}

reload_tor() {
  systemctl reload aeon-onions-tor.service 2>/dev/null || \
    systemctl restart aeon-onions-tor.service 2>/dev/null || true
}

cmd_up() {
  ensure_user; ensure_dirs; write_torrc; ensure_units
  systemctl enable --now aeon-onions-tor.service 2>/dev/null || true
}

cmd_down() {
  systemctl disable --now aeon-onions-tor.service 2>/dev/null || true
}

cmd_status() {
  local tor n
  tor=$(systemctl is-active aeon-onions-tor.service 2>/dev/null || echo inactive)
  n=$(ls -1 "$CONFD"/*.conf 2>/dev/null | wc -l | tr -d ' ')
  printf '{"tor":"%s","count":%d}\n' "$tor" "${n:-0}"
}

# add <id> <local_port> [virt_port=80]
cmd_add() {
  local id="${1:-}" lport="${2:-}" vport="${3:-80}"
  [ -z "$id" ] || [ -z "$lport" ] && { log "usage: add <id> <local_port> [virt_port]"; return 1; }
  case "$id" in *[!a-zA-Z0-9_-]*) log "bad id (alnum/_/- only)"; return 1;; esac
  case "$lport$vport" in *[!0-9]*) log "ports must be numeric"; return 1;; esac
  cmd_up
  install -d -m 700 "$HSROOT/$id"
  chown "$SVCUSER:$SVCUSER" "$HSROOT/$id"
  cat > "$CONFD/$id.conf" <<EOF
HiddenServiceDir $HSROOT/$id
HiddenServiceVersion 3
HiddenServicePort $vport 127.0.0.1:$lport
EOF
  chown "$SVCUSER:$SVCUSER" "$CONFD/$id.conf"
  reload_tor
  # Tor publishes the onion hostname a moment after picking up the new block.
  local on=""
  for _ in $(seq 1 30); do
    on=$(tr -d '[:space:]' < "$HSROOT/$id/hostname" 2>/dev/null)
    [ -n "$on" ] && break
    sleep 1
  done
  [ -z "$on" ] && { log "onion not provisioned for $id"; return 1; }
  echo "$on"
}

cmd_remove() {
  local id="${1:-}"
  [ -z "$id" ] && { log "usage: remove <id>"; return 1; }
  case "$id" in *[!a-zA-Z0-9_-]*) log "bad id"; return 1;; esac
  rm -f "$CONFD/$id.conf"
  rm -rf "${HSROOT:?}/$id"
  reload_tor
}

cmd_list() {
  local f id on vport lport
  for f in "$CONFD"/*.conf; do
    [ -e "$f" ] || continue
    id=$(basename "$f" .conf)
    on=$(tr -d '[:space:]' < "$HSROOT/$id/hostname" 2>/dev/null)
    read -r vport lport < <(awk '/^HiddenServicePort/{split($3,a,":"); print $2" "a[2]; exit}' "$f")
    printf '%s\t%s\t%s\t%s\n' "$id" "${on:-pending}" "${vport:-80}" "${lport:-}"
  done
}

cmd_onion() { tr -d '[:space:]' < "$HSROOT/${1:-}/hostname" 2>/dev/null; }

case "${1:-}" in
  up)     cmd_up ;;
  down)   cmd_down ;;
  status) cmd_status ;;
  add)    shift; cmd_add "$@" ;;
  remove) shift; cmd_remove "$@" ;;
  list)   cmd_list ;;
  onion)  shift; cmd_onion "$@" ;;
  *) echo "usage: aeon-onions {up|down|status|add <id> <local_port> [virt_port]|remove <id>|list|onion <id>}" >&2; exit 1 ;;
esac
