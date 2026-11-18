#!/bin/bash
# aeon-torrent — the BitTorrent side of Model Share.
#
# Runs transmission-daemon as its own unprivileged, sandboxed user so a model
# listed in a directory with a torrent hash can be pulled from, and then seeded
# back to, an ordinary BitTorrent swarm. The supervisor drives it over a
# loopback-only, password-protected RPC port; nothing else talks to it.
#
# OFF until the first pull from a directory. Self-bootstrapping like aeon-ipfs:
# an image without the package installs it on the first `up`.
#
# Subcommands:
#   up | down | status
set -uo pipefail

DIR=/var/lib/aeon/torrents
DATA="$DIR/data"            # one folder per torrent, named by its infohash
STATE="$DIR/state"          # transmission's own config + resume files
SVCUSER=aeon-torrent
BIN=/usr/bin/transmission-daemon
UNIT=/etc/systemd/system/aeon-torrent.service
SECRET=/etc/aeon/torrent-rpc.secret   # RPC password, root-only
RPC_PORT=9191
PEER_PORT=51413
PIDFILE=/run/aeon/torrent.pid

log() { echo "aeon-torrent: $*" >&2; }
have_systemd() { [ -d /run/systemd/system ]; }

ensure_bin() {
  [ -x "$BIN" ] && return 0
  log "installing transmission-daemon…"
  DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends transmission-daemon >/dev/null 2>&1 \
    || { DEBIAN_FRONTEND=noninteractive apt-get update >/dev/null 2>&1 \
         && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends transmission-daemon >/dev/null 2>&1; } \
    || { log "could not install transmission-daemon (no network?)"; return 1; }
  [ -x "$BIN" ]
}

# The Debian package starts its own always-on instance under a different user
# with its own settings. Only ours may run.
disable_stock_unit() {
  have_systemd || return 0
  systemctl disable --now transmission-daemon.service >/dev/null 2>&1 || true
  systemctl mask transmission-daemon.service >/dev/null 2>&1 || true
}

ensure_user() {
  id "$SVCUSER" >/dev/null 2>&1 || useradd -r -s /usr/sbin/nologin -d "$DIR" -M "$SVCUSER" 2>/dev/null || true
}

ensure_dirs() {
  install -d -m 0755 -o "$SVCUSER" -g "$SVCUSER" "$DIR" "$DATA"
  install -d -m 0700 -o "$SVCUSER" -g "$SVCUSER" "$STATE"
}

# 32 characters from the kernel generator, letters and digits only so it needs
# no escaping in JSON or an HTTP header.
ensure_secret() {
  [ -s "$SECRET" ] && return 0
  install -d -m 0755 /etc/aeon
  ( umask 077; LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c 32 >"$SECRET" )
  [ -s "$SECRET" ]
}

# transmission rewrites settings.json when it exits, so this is only ever
# written while the daemon is down. Every key is one both 3.x (Bookworm) and
# 4.x (Trixie) understand.
write_settings() {
  local pw; pw="$(cat "$SECRET")"
  local tmp="$STATE/settings.json.tmp"
  ( umask 077; cat >"$tmp" <<EOF
{
  "download-dir": "$DATA",
  "incomplete-dir-enabled": false,
  "rename-partial-files": true,
  "start-added-torrents": true,
  "trash-original-torrent-files": false,
  "watch-dir-enabled": false,
  "script-torrent-done-enabled": false,
  "blocklist-enabled": false,
  "dht-enabled": true,
  "pex-enabled": true,
  "lpd-enabled": true,
  "utp-enabled": true,
  "encryption": 1,
  "peer-port": $PEER_PORT,
  "peer-port-random-on-start": false,
  "port-forwarding-enabled": true,
  "peer-limit-global": 120,
  "peer-limit-per-torrent": 40,
  "download-queue-enabled": true,
  "download-queue-size": 3,
  "seed-queue-enabled": false,
  "ratio-limit-enabled": false,
  "idle-seeding-limit-enabled": false,
  "speed-limit-up-enabled": false,
  "speed-limit-down-enabled": false,
  "rpc-enabled": true,
  "rpc-bind-address": "127.0.0.1",
  "rpc-port": $RPC_PORT,
  "rpc-url": "/transmission/",
  "rpc-authentication-required": true,
  "rpc-username": "aeon",
  "rpc-password": "$pw",
  "rpc-whitelist-enabled": true,
  "rpc-whitelist": "127.0.0.1",
  "rpc-host-whitelist-enabled": true,
  "rpc-host-whitelist": "localhost"
}
EOF
  )
  chown "$SVCUSER:$SVCUSER" "$tmp" && mv -f "$tmp" "$STATE/settings.json"
}

# Warnings and errors only: at the default level the daemon writes a line to the
# journal (and so to the SD card) every few seconds while it looks for a router
# that will forward its port. 3.x and 4.x spell the option differently.
log_arg() {
  if "$BIN" --help 2>&1 | grep -q -- '--log-level'; then echo "--log-level=warn"; else echo "--log-error"; fi
}

ensure_unit() {
  cat >"$UNIT" <<EOF
[Unit]
Description=Aeon Magick — BitTorrent service for Model Share
After=network-online.target
Wants=network-online.target
RequiresMountsFor=$DIR
StartLimitIntervalSec=600
StartLimitBurst=5
[Service]
User=$SVCUSER
ExecStart=$BIN -f -g $STATE $(log_arg)
Restart=on-failure
RestartSec=10
UMask=0022
# It parses data from strangers. It gets its own folder and nothing else.
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths=$DIR
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK
RestrictNamespaces=yes
LockPersonality=yes
SystemCallArchitectures=native
CapabilityBoundingSet=
AmbientCapabilities=
[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload 2>/dev/null || true
}

daemon_state() {
  if have_systemd; then
    case " $(systemctl show -p ActiveState -p SubState --value aeon-torrent.service 2>/dev/null | tr '\n' ' ') " in
      *" active "*)            echo active ;;
      *failed*|*auto-restart*) echo failed ;;
      *activating*)            echo starting ;;
      *)                       echo inactive ;;
    esac
    return
  fi
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null; then echo active; else echo inactive; fi
}

cmd_up() {
  ensure_bin || return 1
  disable_stock_unit
  ensure_user
  ensure_dirs
  ensure_secret || { log "could not create the RPC secret"; return 1; }
  if [ "$(daemon_state)" = active ]; then echo ok; return 0; fi
  write_settings || { log "could not write settings"; return 1; }
  if have_systemd; then
    ensure_unit
    systemctl reset-failed aeon-torrent.service 2>/dev/null || true
    systemctl enable --now aeon-torrent.service 2>/dev/null || { log "service did not start"; return 1; }
  else
    install -d /run/aeon 2>/dev/null || true
    setsid setpriv --reuid "$SVCUSER" --regid "$SVCUSER" --init-groups -- "$BIN" -f -g "$STATE" $(log_arg) \
      </dev/null >/var/log/aeon-torrent.log 2>&1 &
    echo $! >"$PIDFILE"
  fi
  echo ok
}

cmd_down() {
  if have_systemd; then
    systemctl disable --now aeon-torrent.service 2>/dev/null || true
  else
    [ -f "$PIDFILE" ] && kill "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null || true
    rm -f "$PIDFILE"
  fi
  echo ok
}

cmd_status() {
  local installed=false version=""
  if [ -x "$BIN" ]; then
    installed=true
    version="$("$BIN" --version 2>&1 | head -1 | awk '{print $2}')"
  fi
  printf '{"installed":%s,"state":"%s","version":"%s","rpc_port":%s,"peer_port":%s}\n' \
    "$installed" "$(daemon_state)" "$version" "$RPC_PORT" "$PEER_PORT"
}

case "${1:-}" in
  up)     cmd_up ;;
  down)   cmd_down ;;
  status) cmd_status ;;
  *) echo "usage: aeon-torrent {up|down|status}" >&2; exit 2 ;;
esac
