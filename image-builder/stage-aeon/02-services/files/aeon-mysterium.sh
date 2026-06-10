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

# A SECOND dnscrypt instance dedicated to the NODE'S DNS only (listens on
# 127.0.2.2), running as the mysterium-node user — whose uid aeon-myst-route
# policy-routes out the physical WAN. So the node resolves Mysterium infra
# (quality/hermes3/broker) over a fast, reliable, still-encrypted path that does
# NOT depend on the box's VPN tunnel, while the rest of the box keeps using the
# VPN-routed resolver on 127.0.2.1. Why this matters: the VPN here is AirVPN in
# openvpn-ssl stealth mode (OpenVPN-over-TCP); tunnelling DoH (also TCP) through
# it is TCP-over-TCP, which stalls under latency and made the node intermittently
# fail to reach quality.mysterium.network (monitoring/quality never completed).
ensure_wan_dns() {
  local bin main=/etc/dnscrypt-proxy/dnscrypt-proxy.toml drop=/etc/systemd/system/mysterium-node.service.d/dns.conf changed=0
  bin=$(command -v dnscrypt-proxy 2>/dev/null) || return 0
  [ -f "$main" ] || return 0            # no box resolver to base ours on
  install -d -m755 /etc/dnscrypt-proxy-wan /etc/aeon /var/cache/dnscrypt-proxy-wan
  chown mysterium-node:mysterium-node /var/cache/dnscrypt-proxy-wan 2>/dev/null || true
  # WAN config = the box's resolver config, but bound only to 127.0.2.2 + own cache.
  sed -e "s|^listen_addresses =.*|listen_addresses = ['127.0.2.2:53']|" \
      -e "s|/var/cache/dnscrypt-proxy/|/var/cache/dnscrypt-proxy-wan/|g" \
      "$main" > /etc/dnscrypt-proxy-wan/dnscrypt-proxy.toml
  cat > /etc/systemd/system/dnscrypt-proxy-wan.service <<UNIT
[Unit]
Description=dnscrypt-proxy (WAN egress) for Mysterium node DNS
After=network-online.target ${SERVICE}
Wants=network-online.target
[Service]
User=mysterium-node
Group=mysterium-node
AmbientCapabilities=CAP_NET_BIND_SERVICE
CacheDirectory=dnscrypt-proxy-wan
RuntimeDirectory=dnscrypt-proxy-wan
ExecStart=${bin} -config /etc/dnscrypt-proxy-wan/dnscrypt-proxy.toml
Restart=on-failure
RestartSec=5
[Install]
WantedBy=multi-user.target
UNIT
  cat > /etc/aeon/mysterium-resolv.conf <<'RES'
# Mysterium node's private resolver: dedicated WAN-egress dnscrypt (127.0.2.2),
# running as uid mysterium-node (WAN-routed). SINGLE nameserver ON PURPOSE — a
# 127.0.2.1 fallback is VPN-routed and stalls (TCP-over-TCP through AirVPN); Go's
# resolver falls through to it on any hiccup, the stalled lookup is canceled, and
# that aborts the node's signed quality-metrics POST -> monitoring-status=failed.
# dnscrypt-proxy-wan has Restart=on-failure, so WAN-only is the safe failure mode.
nameserver 127.0.2.2
options edns0 trust-ad
RES
  mkdir -p /etc/systemd/system/mysterium-node.service.d
  if ! grep -qs 'mysterium-resolv.conf' "$drop" 2>/dev/null; then
    printf '[Service]\nBindReadOnlyPaths=/etc/aeon/mysterium-resolv.conf:/etc/resolv.conf\n' > "$drop"
    changed=1
  fi
  systemctl daemon-reload
  systemctl enable --now dnscrypt-proxy-wan.service 2>/dev/null || true
  # If the resolver bind-mount is newly added and the node is already running,
  # restart it so the private resolv.conf takes effect.
  [ "$changed" = 1 ] && systemctl is-active --quiet "$SERVICE" && systemctl restart "$SERVICE" || true
}

# Self-healing guard. A reboot OR the box's periodic VPN/dnscrypt reactivation
# (NetworkManager bounces) FLUSHES routing table 400 — dropping the node off the
# WAN carve-out onto the VPN tunnel (unreachable + wrong location) — and can leave
# the node's /etc/resolv.conf showing the HOST resolver (127.0.2.1, VPN-routed ->
# stalled quality lookups -> monitoring=failed) instead of the bind-mounted WAN
# one. A 60s systemd timer re-asserts BOTH, but only when actually broken (so it's
# near-zero churn in steady state). This is what makes the fix survive reboots.
ensure_guard() {
  cat > /usr/local/bin/aeon-myst-guard <<'GUARD'
#!/bin/bash
# aeon-myst-guard — re-assert the Mysterium node's WAN carve-out + private WAN-DNS
# after boot or the box's network churn. Acts ONLY when broken. (Managed by
# aeon-mysterium; runs from aeon-myst-guard.timer.)
set -u
U=$(id -u mysterium-node 2>/dev/null || echo 989)
log(){ logger -t aeon-myst-guard -- "$*"; }
# 1) Carve-out: uid must NOT egress a tunnel, and table 400 must have its default.
eg=$(ip route get 1.1.1.1 uid "$U" 2>/dev/null)
case "$eg" in
  *dev\ tun*|*dev\ wg*|*dev\ aeon*)
    log "uid $U egress via tunnel ($eg) — re-applying aeon-myst-route"
    /usr/local/bin/aeon-myst-route apply >/dev/null 2>&1 ;;
esac
if ! ip -4 route show table 400 2>/dev/null | grep -q '^default'; then
  log "table 400 lost its default — re-applying aeon-myst-route"
  /usr/local/bin/aeon-myst-route apply >/dev/null 2>&1
fi
# 2) DNS: the node's resolv must point ONLY at the WAN resolver 127.0.2.2, never
#    the VPN-routed 127.0.2.1. Heal in-place via the mount namespace (no restart).
pid=$(pgrep -x myst | head -1)
if [ -n "${pid:-}" ] && [ -f /etc/aeon/mysterium-resolv.conf ]; then
  rc=$(nsenter -t "$pid" -m -- cat /etc/resolv.conf 2>/dev/null)
  if printf '%s' "$rc" | grep -q '127\.0\.2\.1' || ! printf '%s' "$rc" | grep -q '127\.0\.2\.2'; then
    log "node resolv wrong (127.0.2.1 present or 127.0.2.2 missing) — re-binding"
    if nsenter -t "$pid" -m -- mount --bind /etc/aeon/mysterium-resolv.conf /etc/resolv.conf 2>/dev/null; then
      log "re-bound node /etc/resolv.conf -> 127.0.2.2"
    else
      log "re-bind failed — restarting node"; systemctl restart mysterium-node.service
    fi
  fi
fi
GUARD
  chmod 755 /usr/local/bin/aeon-myst-guard
  cat > /etc/systemd/system/aeon-myst-guard.service <<'SVC'
[Unit]
Description=Self-heal Mysterium node WAN carve-out + private DNS
After=mysterium-node.service
[Service]
Type=oneshot
ExecStart=/usr/local/bin/aeon-myst-guard
SVC
  cat > /etc/systemd/system/aeon-myst-guard.timer <<'TMR'
[Unit]
Description=Periodically re-assert Mysterium node networking (self-heal)
[Timer]
OnBootSec=45
OnUnitActiveSec=60
AccuracySec=10
[Install]
WantedBy=timers.target
TMR
  systemctl daemon-reload
  systemctl enable --now aeon-myst-guard.timer 2>/dev/null || true
}

cmd_up() {
  ensure_install || return 1
  harden_opts
  ensure_dropin
  # Split-tunnel the node out the WAN BEFORE it starts — Mysterium can't serve
  # over the Orb's VPN/Tor (NAT-traversal + ToS), so its traffic egresses the
  # real circuit. No-op if no VPN is active (table 400 == the WAN default anyway).
  [ -x /usr/local/bin/aeon-myst-route ] && /usr/local/bin/aeon-myst-route apply || true
  ensure_wan_dns   # dedicated WAN-egress resolver for the node (after the route is up)
  ensure_guard     # 60s self-heal timer: keeps the carve-out + DNS up across reboots/churn
  systemctl enable --now "$SERVICE" 2>/dev/null || true
  ensure_ui_password
}

cmd_down() {
  systemctl disable --now aeon-myst-guard.timer 2>/dev/null || true
  [ -x /usr/local/bin/aeon-myst-route ] && /usr/local/bin/aeon-myst-route clear || true
  systemctl disable --now dnscrypt-proxy-wan.service 2>/dev/null || true
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
