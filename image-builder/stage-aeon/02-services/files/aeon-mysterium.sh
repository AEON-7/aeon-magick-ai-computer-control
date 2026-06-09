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

# Harden the daemon flags (idempotent; restarts only if already running):
#  --log-level=info  — the default debug level writes the MMN account API key
#    (set when a user claims their node) to the node log in PLAINTEXT.
#  --ui.address=0.0.0.0 — myst's auto-bind picks "127.0.0.1 + local LAN IP",
#    and with the Orb's VPN up the "LAN IP" it picks is the VPN tun address —
#    i.e. the UI faces the VPN provider's client subnet and NOT the user's LAN.
#    Bind everywhere and let aeon-myst-route's ui_guard decide who gets in
#    (LAN + tailnet only; tun/usb/v6 dropped).
harden_opts() {
  local f=/etc/default/mysterium-node changed=0
  [ -f "$f" ] || return 0
  if ! grep -q -- '--log-level' "$f"; then
    sed -i 's/^DAEMON_OPTS="\(.*\)"/DAEMON_OPTS="\1 --log-level=info"/' "$f"; changed=1
  fi
  if ! grep -q -- '--ui.address' "$f"; then
    sed -i 's/^DAEMON_OPTS="\(.*\)"/DAEMON_OPTS="\1 --ui.address=0.0.0.0"/' "$f"; changed=1
  fi
  # Pin the provider's UDP session ports to a SMALL fixed range (default is the
  # huge 10000:60000) so the operator can forward just that range on their router
  # — the secure alternative to UPnP for nodes behind a NAT that won't hole-punch.
  if ! grep -q -- '--udp.ports' "$f"; then
    sed -i 's/^DAEMON_OPTS="\(.*\)"/DAEMON_OPTS="\1 --udp.ports=10000:10100"/' "$f"; changed=1
  fi
  [ "$changed" = 1 ] && systemctl is-active --quiet "$SERVICE" && systemctl restart "$SERVICE" || true
}

# The WAN split-tunnel + UI firewall are runtime ip-rule/iptables state — without
# this drop-in they'd vanish on reboot while the node auto-starts, silently
# pushing provider traffic back into the Orb's VPN and unguarding the UI.
ensure_dropin() {
  local d=/etc/systemd/system/mysterium-node.service.d
  [ -f "$d/aeon.conf" ] && return 0
  mkdir -p "$d"
  cat > "$d/aeon.conf" <<'EOF'
# Managed by aeon-mysterium: network plumbing must be in place before every
# node start (including boot) — WAN split-tunnel + NodeUI firewall.
[Unit]
After=network-online.target
Wants=network-online.target

[Service]
ExecStartPre=+/usr/local/bin/aeon-myst-route apply
EOF
  systemctl daemon-reload
}

PASSFILE=/etc/aeon/mysterium-ui.pass
TQ=http://127.0.0.1:4050

# Rotate the NodeUI/TequilAPI password off the well-known default (mystberry) to
# a per-device secret, stored root-only in $PASSFILE. The supervisor reads the
# file for its API calls and shows it (admin-gated) in the dashboard so the user
# can sign in to the NodeUI. If the node was reinstalled (password reset to
# default) the stored one is re-applied.
ensure_ui_password() {
  local pass code i
  for i in $(seq 1 30); do
    curl -s -o /dev/null --max-time 2 "$TQ/healthcheck" && break
    sleep 2
  done
  if [ -f "$PASSFILE" ]; then
    pass=$(cat "$PASSFILE")
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 -X POST -H 'Content-Type: application/json' \
      --data "{\"username\":\"myst\",\"password\":\"$pass\"}" "$TQ/auth/login")
    [ "$code" = "200" ] && return 0
    curl -s -o /dev/null --max-time 5 -u myst:mystberry -X PUT -H 'Content-Type: application/json' \
      --data "{\"username\":\"myst\",\"old_password\":\"mystberry\",\"new_password\":\"$pass\"}" "$TQ/auth/password" || true
    log "NodeUI password re-applied from $PASSFILE"
    return 0
  fi
  pass=$(head -c 32 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' | head -c 24)
  code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 -u myst:mystberry -X PUT -H 'Content-Type: application/json' \
    --data "{\"username\":\"myst\",\"old_password\":\"mystberry\",\"new_password\":\"$pass\"}" "$TQ/auth/password")
  if [ "$code" = "200" ]; then
    ( umask 077; printf '%s' "$pass" > "$PASSFILE" )
    log "NodeUI password rotated (stored in $PASSFILE)"
  else
    log "NodeUI password rotation skipped (http $code)"
  fi
}

cmd_up() {
  ensure_install || return 1
  harden_opts
  ensure_dropin
  # Split-tunnel the node out the WAN BEFORE it starts — Mysterium can't serve
  # over the Orb's VPN/Tor (NAT-traversal + ToS), so its traffic egresses the
  # real circuit. No-op if no VPN is active (table 400 == the WAN default anyway).
  [ -x /usr/local/bin/aeon-myst-route ] && /usr/local/bin/aeon-myst-route apply || true
  systemctl enable --now "$SERVICE" 2>/dev/null || true
  ensure_ui_password
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
