#!/bin/bash
# aeon-mysterium — run a Mysterium Network node on this Orb: share idle bandwidth
# to help decentralize internet access and earn MYST. Installs the official myst
# node (latest arm64 .deb from GitHub releases — the apt repo is unreliable) plus
# WireGuard, and runs the packaged `mysterium-node` systemd service. Live stats
# come from the node's local TequilAPI (127.0.0.1:4050), read by the supervisor.
#
# Account + wallet/payout are set up at mystnodes.co (non-custodial — your keys
# stay in your wallet; the node only ever holds a public beneficiary address).
#
# Subcommands: up | down | status | installed
set -uo pipefail

SERVICE=mysterium-node.service
GH_API=https://api.github.com/repos/mysteriumnetwork/node/releases/latest

log() { echo "aeon-mysterium: $*" >&2; }

ensure_install() {
  command -v myst >/dev/null 2>&1 && return 0
  log "installing myst node + wireguard…"
  DEBIAN_FRONTEND=noninteractive apt-get install -y wireguard >/dev/null 2>&1 || true
  local url tmp
  url=$(curl -fsSL --max-time 30 "$GH_API" 2>/dev/null | grep -oE 'https://[^"]*myst_linux_arm64\.deb' | head -1)
  [ -z "$url" ] && { log "could not resolve myst .deb url"; return 1; }
  tmp=$(mktemp -d)
  if ! curl -fsSL --max-time 180 "$url" -o "$tmp/myst.deb"; then log "download failed"; rm -rf "$tmp"; return 1; fi
  DEBIAN_FRONTEND=noninteractive apt-get install -y "$tmp/myst.deb" >/dev/null 2>&1 || { rm -rf "$tmp"; log "install failed"; return 1; }
  rm -rf "$tmp"
  command -v myst >/dev/null 2>&1
}

# The myst daemon defaults to --log-level=debug, which writes the MMN account API
# key (set when a user claims their node) to the node log in PLAINTEXT. Drop it to
# info so secrets never hit disk. Idempotent; restarts only if already running.
harden_logging() {
  local f=/etc/default/mysterium-node
  [ -f "$f" ] || return 0
  grep -q -- '--log-level' "$f" && return 0
  sed -i 's/^DAEMON_OPTS="\(.*\)"/DAEMON_OPTS="\1 --log-level=info"/' "$f"
  systemctl is-active --quiet "$SERVICE" && systemctl restart "$SERVICE" || true
}

cmd_up() {
  ensure_install || return 1
  harden_logging
  # Split-tunnel the node out the WAN BEFORE it starts — Mysterium can't serve
  # over the Orb's VPN/Tor (NAT-traversal + ToS), so its traffic egresses the
  # real circuit. No-op if no VPN is active (table 400 == the WAN default anyway).
  [ -x /usr/local/bin/aeon-myst-route ] && /usr/local/bin/aeon-myst-route apply || true
  systemctl enable --now "$SERVICE" 2>/dev/null || true
}

cmd_down() {
  [ -x /usr/local/bin/aeon-myst-route ] && /usr/local/bin/aeon-myst-route clear || true
  systemctl disable --now "$SERVICE" 2>/dev/null || true
}

cmd_status() {
  local installed daemon
  installed=$(command -v myst >/dev/null 2>&1 && echo true || echo false)
  daemon=$(systemctl is-active "$SERVICE" 2>/dev/null); daemon=${daemon:-inactive}
  printf '{"installed":%s,"daemon":"%s"}\n' "$installed" "$daemon"
}

case "${1:-}" in
  up)        cmd_up ;;
  down)      cmd_down ;;
  status)    cmd_status ;;
  installed) command -v myst >/dev/null 2>&1 && echo true || echo false ;;
  *) echo "usage: aeon-mysterium {up|down|status|installed}" >&2; exit 1 ;;
esac
