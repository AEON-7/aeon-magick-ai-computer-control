#!/bin/bash
# aeon-ipfs — stand up a local IPFS (kubo) node + HTTP gateway on this Orb, so
# you can host content on the decentralized web and reach the gateway from any
# device (LAN or Tailscale). Self-bootstrapping: downloads the kubo arm64 binary,
# inits the repo (lowpower profile — tuned for a Pi), binds the gateway to all
# interfaces (API stays localhost-only), sets a storage cap, and installs its
# systemd unit on first `up`. OFF by default.
#
# Subcommands:
#   up | down | status
#   storage <size>     e.g. 10GB   (sets Datastore.StorageMax; restarts daemon)
#   pin <cid> | unpin <cid> | pins
#   add <path>         add a file/dir to IPFS, prints the root CID
#   gateway            prints the gateway port
set -uo pipefail

DIR=/var/lib/aeon/ipfs
SVCUSER=aeon-ipfs
IPFSBIN=/usr/local/bin/ipfs
UNIT=/etc/systemd/system/aeon-ipfs.service
GATEWAY_PORT=8080
API_PORT=5001

log() { echo "aeon-ipfs: $*" >&2; }
ipfs_cmd() { runuser -u "$SVCUSER" -- env IPFS_PATH="$DIR" "$IPFSBIN" "$@"; }

ensure_user() {
  id "$SVCUSER" >/dev/null 2>&1 || \
    useradd -r -s /usr/sbin/nologin -d "$DIR" -M "$SVCUSER" 2>/dev/null || true
  install -d -m 0750 -o "$SVCUSER" -g "$SVCUSER" "$DIR"
}

ensure_bin() {
  [ -x "$IPFSBIN" ] && return 0
  log "downloading kubo…"
  local kver tmp
  kver=$(curl -fsSL --max-time 30 https://dist.ipfs.tech/kubo/versions 2>/dev/null | tail -1)
  [ -z "$kver" ] && { log "could not resolve latest kubo version"; return 1; }
  tmp=$(mktemp -d)
  if ! curl -fsSL --max-time 180 "https://dist.ipfs.tech/kubo/${kver}/kubo_${kver}_linux-arm64.tar.gz" -o "$tmp/kubo.tgz"; then
    log "kubo download failed"; rm -rf "$tmp"; return 1
  fi
  tar xzf "$tmp/kubo.tgz" -C "$tmp" 2>/dev/null || { rm -rf "$tmp"; return 1; }
  install -m 0755 "$tmp/kubo/ipfs" "$IPFSBIN"
  rm -rf "$tmp"
  log "installed kubo $kver"
}

ensure_init() {
  [ -f "$DIR/config" ] && return 0
  ipfs_cmd init --profile=lowpower >/dev/null 2>&1 || { log "ipfs init failed"; return 1; }
}

configure() {
  # Gateway reachable from any device; API stays localhost-only (it's powerful).
  ipfs_cmd config Addresses.Gateway "/ip4/0.0.0.0/tcp/${GATEWAY_PORT}" >/dev/null 2>&1 || true
  ipfs_cmd config Addresses.API "/ip4/127.0.0.1/tcp/${API_PORT}" >/dev/null 2>&1 || true
}

ensure_units() {
  cat > "$UNIT" <<EOF
[Unit]
Description=Aeon Magick — IPFS (kubo) node + gateway
After=network-online.target
Wants=network-online.target
[Service]
User=$SVCUSER
Environment=IPFS_PATH=$DIR
ExecStart=$IPFSBIN daemon --migrate=true
Restart=on-failure
RestartSec=10
[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload 2>/dev/null || true
}

cmd_up() {
  ensure_user || return 1
  ensure_bin || return 1
  ensure_init || return 1
  configure
  ensure_units
  systemctl enable --now aeon-ipfs.service 2>/dev/null || true
}

cmd_down() { systemctl disable --now aeon-ipfs.service 2>/dev/null || true; }

cmd_status() {
  local installed daemon ver pid peers repo smax
  installed=$([ -x "$IPFSBIN" ] && echo true || echo false)
  daemon=$(systemctl is-active aeon-ipfs.service 2>/dev/null); daemon=${daemon:-inactive}
  ver=""; pid=""; peers=0; repo=0; smax=""
  if [ "$installed" = true ] && [ -f "$DIR/config" ]; then
    ver=$(ipfs_cmd version --number 2>/dev/null)
    pid=$(ipfs_cmd config Identity.PeerID 2>/dev/null)
    smax=$(ipfs_cmd config Datastore.StorageMax 2>/dev/null)
    repo=$(ipfs_cmd repo stat 2>/dev/null | awk '/RepoSize/{print $2; exit}')
    [ "$daemon" = active ] && peers=$(ipfs_cmd swarm peers 2>/dev/null | wc -l | tr -d ' ')
  fi
  printf '{"installed":%s,"daemon":"%s","version":"%s","peer_id":"%s","peers":%d,"repo_bytes":%s,"storage_max":"%s","gateway_port":%d}\n' \
    "$installed" "$daemon" "${ver:-}" "${pid:-}" "${peers:-0}" "${repo:-0}" "${smax:-}" "$GATEWAY_PORT"
}

cmd_storage() {
  local size="${1:-}"
  [ -z "$size" ] && { log "usage: storage <size e.g. 10GB>"; return 1; }
  case "$size" in *[!0-9GMKTBgmktb]*) log "bad size"; return 1;; esac
  ipfs_cmd config Datastore.StorageMax "$size" >/dev/null 2>&1 || { log "set storage failed"; return 1; }
  systemctl restart aeon-ipfs.service 2>/dev/null || true
  echo "$size"
}

cmd_pin()   { ipfs_cmd pin add "${1:-}" 2>&1; }
cmd_unpin() { ipfs_cmd pin rm "${1:-}" 2>&1; }
cmd_pins()  { ipfs_cmd pin ls --type=recursive 2>/dev/null | awk '{print $1}'; }
cmd_add()   { ipfs_cmd add -rQ "${1:-}" 2>/dev/null; }
# Best-effort direct swarm connection (multiaddr), used before pinning a
# fleet peer's model so LAN/tailnet fetches don't wait on DHT routing.
cmd_connect() { ipfs_cmd swarm connect "${1:-}" 2>&1 || true; }

case "${1:-}" in
  up)      cmd_up ;;
  down)    cmd_down ;;
  status)  cmd_status ;;
  storage) shift; cmd_storage "$@" ;;
  pin)     shift; cmd_pin "$@" ;;
  unpin)   shift; cmd_unpin "$@" ;;
  pins)    cmd_pins ;;
  add)     shift; cmd_add "$@" ;;
  connect) shift; cmd_connect "$@" ;;
  gateway) echo "$GATEWAY_PORT" ;;
  *) echo "usage: aeon-ipfs {up|down|status|storage <size>|pin <cid>|unpin <cid>|pins|add <path>|connect <multiaddr>|gateway}" >&2; exit 1 ;;
esac
