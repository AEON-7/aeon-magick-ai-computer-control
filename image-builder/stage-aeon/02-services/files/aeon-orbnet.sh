#!/bin/bash
# aeon-orbnet — provision + manage the OrbNet homeserver (Conduit) behind a
# dedicated Tor onion service.
#
# OrbNet is an opt-in, anonymous Matrix federation between Aeon Magick Orbs that
# rides Tor onion services: no port-forwarding, no DynDNS, no exposed home IP,
# works behind CGNAT. The .onion address cryptographically authenticates each
# peer and Tor encrypts the path, so the patched Conduit accepts self-signed
# certs — every Orb self-signs its own onion cert and trusts any onion. Fully
# decentralized: no shared CA, no shipped private key.
#
# This script is the INFRA layer (Tor onion + Conduit daemon + self-signed cert).
# Application logic — the owner account, auto-joining the community, personas,
# moderation — lives in the supervisor (orbnet.rs) via the Matrix C-S API.
#
# Subcommands:  up | down | status | keepalive | info | reg-token
#
# Idempotent + self-bootstrapping: creates its system user, systemd units,
# onion, cert + config on first `up`, so it runs on a live device and bakes the
# same way.
set -uo pipefail

DIR=/var/lib/aeon/orbnet
TORDIR="$DIR/tor"
HSDIR="$TORDIR/hs"
TLSDIR="$DIR/tls"
DBDIR="$DIR/db"
CONF="$DIR/conduit.toml"
REGTOKEN_FILE="$DIR/reg-token"
CONDUIT=/usr/local/bin/aeon-conduit
SVCUSER=aeon-orbnet
SOCKS=9072            # dedicated OrbNet Tor SOCKS (federation egress)
PORT=8448            # Matrix federation default; the onion maps here

log() { echo "aeon-orbnet: $*" >&2; }

ensure_user() {
  id "$SVCUSER" >/dev/null 2>&1 || \
    useradd -r -s /usr/sbin/nologin -d "$DIR" -M "$SVCUSER" 2>/dev/null || true
}

ensure_dirs() {
  for d in "$DIR" "$TORDIR" "$TORDIR/data" "$HSDIR" "$TLSDIR" "$DBDIR"; do
    install -d -m 700 "$d"
  done
  chown -R "$SVCUSER:$SVCUSER" "$DIR" 2>/dev/null || true
}

onion() { cat "$HSDIR/hostname" 2>/dev/null | tr -d '[:space:]'; }

write_torrc() {
  cat > "$TORDIR/torrc" <<EOF
# Managed by aeon-orbnet — dedicated OrbNet Tor instance (separate from the
# privacy-stack Tor so the two never interfere).
DataDirectory $TORDIR/data
SocksPort 127.0.0.1:$SOCKS
ControlPort 0
HiddenServiceDir $HSDIR
HiddenServiceVersion 3
HiddenServicePort $PORT 127.0.0.1:$PORT
EOF
  chown -R "$SVCUSER:$SVCUSER" "$TORDIR" 2>/dev/null || true
}

ensure_cert() {
  local on="$1"
  [ -s "$TLSDIR/cert.pem" ] && [ -s "$TLSDIR/key.pem" ] && return 0
  openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$TLSDIR/key.pem" -out "$TLSDIR/cert.pem" \
    -subj "/CN=$on" -addext "subjectAltName=DNS:$on" -days 3650 2>/dev/null
  chmod 600 "$TLSDIR/key.pem"
  chown -R "$SVCUSER:$SVCUSER" "$TLSDIR" 2>/dev/null || true
}

ensure_regtoken() {
  [ -s "$REGTOKEN_FILE" ] && return 0
  head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$REGTOKEN_FILE"
  chmod 600 "$REGTOKEN_FILE"
  chown "$SVCUSER:$SVCUSER" "$REGTOKEN_FILE" 2>/dev/null || true
}

write_conduit_conf() {
  local on="$1" tok
  ensure_regtoken
  tok=$(cat "$REGTOKEN_FILE")
  cat > "$CONF" <<EOF
# Managed by aeon-orbnet. server_name is this Orb's .onion; federation egresses
# through the dedicated OrbNet Tor SOCKS; TLS is a throwaway self-signed cert
# (peers authenticate via the onion address, not the cert).
[global]
server_name = "$on"
database_backend = "sqlite"
database_path = "$DBDIR"
port = $PORT
address = "127.0.0.1"
max_request_size = 20000000
allow_registration = true
registration_token = "$tok"
allow_federation = true
allow_check_for_updates = false
trusted_servers = []
log = "warn"

[global.proxy]
global = { url = "socks5h://127.0.0.1:$SOCKS" }

[global.tls]
certs = "$TLSDIR/cert.pem"
key = "$TLSDIR/key.pem"
EOF
  chown "$SVCUSER:$SVCUSER" "$CONF" 2>/dev/null || true
}

ensure_units() {
  cat > /etc/systemd/system/aeon-orbnet-tor.service <<EOF
[Unit]
Description=Aeon Magick OrbNet — Tor onion service
After=network-online.target
Wants=network-online.target
[Service]
User=$SVCUSER
ExecStart=/usr/bin/tor -f $TORDIR/torrc
Restart=on-failure
RestartSec=5
[Install]
WantedBy=multi-user.target
EOF
  cat > /etc/systemd/system/aeon-orbnet-conduit.service <<EOF
[Unit]
Description=Aeon Magick OrbNet — Matrix homeserver (Conduit)
After=aeon-orbnet-tor.service
Wants=aeon-orbnet-tor.service
[Service]
User=$SVCUSER
Environment=CONDUIT_CONFIG=$CONF
ExecStart=$CONDUIT
Restart=on-failure
RestartSec=5
[Install]
WantedBy=multi-user.target
EOF
  # Keepalive: warm the Tor circuit every 2 min so federation stays snappy
  # (cold onion circuits add ~7s to the first hop; warm ~1s).
  cat > /etc/systemd/system/aeon-orbnet-keepalive.service <<EOF
[Unit]
Description=Aeon Magick OrbNet — warm the federation Tor circuit
[Service]
Type=oneshot
ExecStart=/usr/local/bin/aeon-orbnet keepalive
EOF
  cat > /etc/systemd/system/aeon-orbnet-keepalive.timer <<EOF
[Unit]
Description=Aeon Magick OrbNet — keepalive timer
[Timer]
OnBootSec=90
OnUnitActiveSec=120
[Install]
WantedBy=timers.target
EOF
  systemctl daemon-reload
}

cmd_up() {
  ensure_user
  ensure_dirs
  ensure_units
  write_torrc
  systemctl enable --now aeon-orbnet-tor.service
  local on=""
  for _ in $(seq 1 30); do on=$(onion); [ -n "$on" ] && break; sleep 1; done
  [ -z "$on" ] && { log "onion not provisioned"; return 1; }
  ensure_cert "$on"
  write_conduit_conf "$on"
  chown -R "$SVCUSER:$SVCUSER" "$DBDIR" 2>/dev/null || true
  systemctl enable --now aeon-orbnet-conduit.service
  for _ in $(seq 1 30); do
    curl -sk -o /dev/null --max-time 5 "https://127.0.0.1:$PORT/_matrix/key/v2/server" && break
    sleep 1
  done
  systemctl enable --now aeon-orbnet-keepalive.timer 2>/dev/null || true
  ( cmd_keepalive >/dev/null 2>&1 & )
  echo "$on"
}

cmd_down() {
  systemctl disable --now aeon-orbnet-keepalive.timer 2>/dev/null || true
  systemctl disable --now aeon-orbnet-conduit.service 2>/dev/null || true
  systemctl disable --now aeon-orbnet-tor.service 2>/dev/null || true
}

cmd_keepalive() {
  local on; on=$(onion); [ -z "$on" ] && return 0
  curl -sk --socks5-hostname "127.0.0.1:$SOCKS" --max-time 60 -o /dev/null \
    "https://$on:$PORT/_matrix/key/v2/server" 2>/dev/null || true
}

cmd_status() {
  local on tor con local_up=false
  on=$(onion)
  tor=$(systemctl is-active aeon-orbnet-tor.service 2>/dev/null || echo inactive)
  con=$(systemctl is-active aeon-orbnet-conduit.service 2>/dev/null || echo inactive)
  curl -sk -o /dev/null --max-time 5 "https://127.0.0.1:$PORT/_matrix/key/v2/server" 2>/dev/null && local_up=true
  printf '{"onion":"%s","tor":"%s","conduit":"%s","homeserver_up":%s,"socks":%d,"port":%d}\n' \
    "${on:-}" "$tor" "$con" "$local_up" "$SOCKS" "$PORT"
}

case "${1:-}" in
  up)        cmd_up ;;
  down)      cmd_down ;;
  status)    cmd_status ;;
  keepalive) cmd_keepalive ;;
  info)      onion ;;
  reg-token) cat "$REGTOKEN_FILE" 2>/dev/null ;;
  *) echo "usage: aeon-orbnet {up|down|status|keepalive|info|reg-token}" >&2; exit 1 ;;
esac
