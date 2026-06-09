#!/bin/bash
# aeon-myst-route — split-tunnel the Mysterium node OUT THE WAN, bypassing any
# Orb-wide VPN/Tor tunnel. A Mysterium PROVIDER must reach the network + be
# reachable on the real circuit; nesting it inside a commercial VPN breaks NAT
# traversal and violates the VPN's ToS. Mirrors the existing tor/i2p over_vpn
# fwmark machinery in aeon-net-services.sh, but routes the OTHER way (to WAN).
#
#   mark mysterium-node (uid) OUTPUT packets 0x400 -> ip rule -> table 400
#   table 400 default = the physical WAN gateway (not tun0)
#   + kill-switch ACCEPT for the mark, + loose rp_filter for asymmetric replies
#
# Subcommands: apply | clear | show | test
set -u

MARK=0x400
TABLE=400
PRIO=5100
SVCUSER=mysterium-node
TAG=aeon-myst-wan
# Loopback/LAN/link-local/CGNAT must stay on the main table — critically the
# dnscrypt resolver lives on 127.0.2.1, so marked DNS must NOT be flung out the WAN.
LOCAL_NETS="127.0.0.0/8 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16 100.64.0.0/10"

# Physical WAN gateway+dev. OpenVPN's redirect uses a 0.0.0.0/1 + 128.0.0.0/1
# split (not a real default), so the DHCP-assigned `default` route survives and
# is the true circuit. Fall back to any default whose dev isn't a tunnel.
detect_wan() {
  local out
  out=$(ip -4 route show default proto dhcp 2>/dev/null | awk '/^default/{print $3, $5; exit}')
  [ -z "$out" ] && out=$(ip -4 route show default 2>/dev/null | awk '$5 !~ /^(tun|wg|tailscale|aeon|myst)/{print $3, $5; exit}')
  echo "$out"
}

apply() {
  local gw dev
  read -r gw dev <<<"$(detect_wan)"
  [ -z "${gw:-}" ] && { echo "aeon-myst-route: no physical WAN gateway found" >&2; return 1; }
  echo "aeon-myst-route: WAN egress via $gw dev $dev (mark $MARK table $TABLE)"
  ip route replace default via "$gw" dev "$dev" table "$TABLE"
  # Marked traffic to local destinations resolves via the main table (DNS = 127.0.2.1!),
  # consulted BEFORE the WAN catch-all below.
  local p=5090 net
  for net in $LOCAL_NETS; do
    ip rule del fwmark "$MARK/$MARK" to "$net" table main 2>/dev/null || true
    ip rule add fwmark "$MARK/$MARK" to "$net" table main priority "$p"
    p=$((p + 1))
  done
  # Everything else marked -> WAN.
  ip rule del fwmark "$MARK/$MARK" table "$TABLE" 2>/dev/null || true
  ip rule add fwmark "$MARK/$MARK" table "$TABLE" priority "$PRIO"
  iptables -t mangle -C OUTPUT -m owner --uid-owner "$SVCUSER" -j MARK --set-mark "$MARK" 2>/dev/null \
    || iptables -t mangle -A OUTPUT -m owner --uid-owner "$SVCUSER" -j MARK --set-mark "$MARK"
  iptables -C OUTPUT -m mark --mark "$MARK/$MARK" -j ACCEPT -m comment --comment "$TAG" 2>/dev/null \
    || iptables -I OUTPUT 1 -m mark --mark "$MARK/$MARK" -j ACCEPT -m comment --comment "$TAG"
  # Locally-generated sockets pick their source from the UNMARKED route (tun0's
  # 10.77.9.x) at connect() — before the packet mark lands — so a marked packet
  # would leave $dev with a tun0 source and get dropped upstream. MASQUERADE
  # rewrites it to the WAN interface's address as it egresses.
  iptables -t nat -C POSTROUTING -m mark --mark "$MARK/$MARK" -o "$dev" -j MASQUERADE 2>/dev/null \
    || iptables -t nat -A POSTROUTING -m mark --mark "$MARK/$MARK" -o "$dev" -j MASQUERADE
  # Replies arrive on the WAN dev while the main table's reverse path points at
  # tun0 (asymmetric) — strict rp_filter would drop them. Loosen it (max(all,dev)).
  sysctl -q -w "net.ipv4.conf.$dev.rp_filter=2" 2>/dev/null || true
  sysctl -q -w net.ipv4.conf.all.rp_filter=2 2>/dev/null || true
}

clear_all() {
  local net dev d
  read -r _ dev <<<"$(detect_wan)"
  for net in $LOCAL_NETS; do ip rule del fwmark "$MARK/$MARK" to "$net" table main 2>/dev/null || true; done
  ip rule del fwmark "$MARK/$MARK" table "$TABLE" 2>/dev/null || true
  ip route flush table "$TABLE" 2>/dev/null || true
  iptables -t mangle -D OUTPUT -m owner --uid-owner "$SVCUSER" -j MARK --set-mark "$MARK" 2>/dev/null || true
  while iptables -D OUTPUT -m mark --mark "$MARK/$MARK" -j ACCEPT -m comment --comment "$TAG" 2>/dev/null; do :; done
  for d in $dev wlan0 eth0; do
    while iptables -t nat -D POSTROUTING -m mark --mark "$MARK/$MARK" -o "$d" -j MASQUERADE 2>/dev/null; do :; done
  done
  echo "aeon-myst-route: cleared"
}

show() {
  echo "--- ip rule ---"; ip rule | grep -E "$TABLE|fwmark" || true
  echo "--- table $TABLE ---"; ip route show table "$TABLE" 2>/dev/null || true
  echo "--- mangle mark ---"; iptables -t mangle -S OUTPUT | grep "$SVCUSER" || true
}

ipinfo() {
  runuser -u "$SVCUSER" -- /usr/bin/curl -s --max-time 8 https://ipinfo.io/json 2>/dev/null \
    | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d.get("org"),"|",d.get("country"))' 2>/dev/null \
    || echo "(lookup failed)"
}

case "${1:-show}" in
  apply) apply ;;
  clear) clear_all ;;
  show)  show ;;
  test)  echo "BEFORE (mysterium-node egress): $(ipinfo)"; apply || exit 1; echo "AFTER  (mysterium-node egress): $(ipinfo)"; show ;;
  *) echo "usage: aeon-myst-route {apply|clear|show|test}" >&2; exit 1 ;;
esac
