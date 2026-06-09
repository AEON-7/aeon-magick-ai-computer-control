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
# NodeUI / TequilAPI exposure (see ui_guard): the UI binds 0.0.0.0 (set via
# DAEMON_OPTS by aeon-mysterium) and these rules decide who may reach it.
UI_PORT=4449
TQ_PORT=4050
UI_ALLOWED_IFACES="lo wlan0 eth0 tailscale0"
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
  # Rebuild the mangle OUTPUT marking in order: RETURN (skip) for loopback/LAN/
  # CGNAT so those route normally — the DNS resolver is 127.0.2.1, and myst runs
  # as this uid so its NodeUI replies to LAN clients must NOT be marked+masqueraded
  # (that rewrites the reply's source port and kills inbound :4449 connections).
  # Then MARK everything else for WAN egress.
  iptables -t mangle -D OUTPUT -m owner --uid-owner "$SVCUSER" -j MARK --set-mark "$MARK" 2>/dev/null || true
  for net in $LOCAL_NETS; do iptables -t mangle -D OUTPUT -m owner --uid-owner "$SVCUSER" -d "$net" -j RETURN 2>/dev/null || true; done
  for net in $LOCAL_NETS; do iptables -t mangle -A OUTPUT -m owner --uid-owner "$SVCUSER" -d "$net" -j RETURN; done
  iptables -t mangle -A OUTPUT -m owner --uid-owner "$SVCUSER" -j MARK --set-mark "$MARK"
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
  ui_guard
}

# Gate the NodeUI to operator-side interfaces ONLY (LAN + tailnet + loopback).
# Everything else — the VPN tun (myst's auto-bind once picked tun0's IP, exposing
# the UI toward the VPN provider's client subnet), the usb0 target side, any
# future iface — gets an explicit DROP. TequilAPI (4050) never leaves loopback;
# the UI reverse-proxies it at :4449/tequilapi. v6 is dropped outright (myst
# binds v4; the DROP makes that a guarantee instead of an observation).
ui_guard() {
  local i
  for i in $UI_ALLOWED_IFACES; do
    iptables -C INPUT -i "$i" -p tcp --dport "$UI_PORT" -j ACCEPT -m comment --comment "$TAG-ui" 2>/dev/null \
      || iptables -I INPUT -i "$i" -p tcp --dport "$UI_PORT" -j ACCEPT -m comment --comment "$TAG-ui"
  done
  iptables -C INPUT -p tcp --dport "$UI_PORT" -j DROP -m comment --comment "$TAG-ui" 2>/dev/null \
    || iptables -A INPUT -p tcp --dport "$UI_PORT" -j DROP -m comment --comment "$TAG-ui"
  iptables -C INPUT ! -i lo -p tcp --dport "$TQ_PORT" -j DROP -m comment --comment "$TAG-ui" 2>/dev/null \
    || iptables -A INPUT ! -i lo -p tcp --dport "$TQ_PORT" -j DROP -m comment --comment "$TAG-ui"
  if command -v ip6tables >/dev/null 2>&1; then
    ip6tables -C INPUT -p tcp --dport "$UI_PORT" -j DROP -m comment --comment "$TAG-ui" 2>/dev/null \
      || ip6tables -A INPUT -p tcp --dport "$UI_PORT" -j DROP -m comment --comment "$TAG-ui"
    ip6tables -C INPUT ! -i lo -p tcp --dport "$TQ_PORT" -j DROP -m comment --comment "$TAG-ui" 2>/dev/null \
      || ip6tables -A INPUT ! -i lo -p tcp --dport "$TQ_PORT" -j DROP -m comment --comment "$TAG-ui"
  fi
}

clear_ui_guard() {
  local i
  for i in $UI_ALLOWED_IFACES; do
    while iptables -D INPUT -i "$i" -p tcp --dport "$UI_PORT" -j ACCEPT -m comment --comment "$TAG-ui" 2>/dev/null; do :; done
  done
  while iptables -D INPUT -p tcp --dport "$UI_PORT" -j DROP -m comment --comment "$TAG-ui" 2>/dev/null; do :; done
  while iptables -D INPUT ! -i lo -p tcp --dport "$TQ_PORT" -j DROP -m comment --comment "$TAG-ui" 2>/dev/null; do :; done
  if command -v ip6tables >/dev/null 2>&1; then
    while ip6tables -D INPUT -p tcp --dport "$UI_PORT" -j DROP -m comment --comment "$TAG-ui" 2>/dev/null; do :; done
    while ip6tables -D INPUT ! -i lo -p tcp --dport "$TQ_PORT" -j DROP -m comment --comment "$TAG-ui" 2>/dev/null; do :; done
  fi
}

clear_all() {
  clear_ui_guard
  local net dev d
  read -r _ dev <<<"$(detect_wan)"
  for net in $LOCAL_NETS; do ip rule del fwmark "$MARK/$MARK" to "$net" table main 2>/dev/null || true; done
  ip rule del fwmark "$MARK/$MARK" table "$TABLE" 2>/dev/null || true
  ip route flush table "$TABLE" 2>/dev/null || true
  iptables -t mangle -D OUTPUT -m owner --uid-owner "$SVCUSER" -j MARK --set-mark "$MARK" 2>/dev/null || true
  for net in $LOCAL_NETS; do iptables -t mangle -D OUTPUT -m owner --uid-owner "$SVCUSER" -d "$net" -j RETURN 2>/dev/null || true; done
  while iptables -D OUTPUT -m mark --mark "$MARK/$MARK" -j ACCEPT -m comment --comment "$TAG" 2>/dev/null; do :; done
  for d in $dev wlan0 eth0; do
    while iptables -t nat -D POSTROUTING -m mark --mark "$MARK/$MARK" -o "$d" -j MASQUERADE 2>/dev/null; do :; done
  done
  echo "aeon-myst-route: cleared"
}

show() {
  echo "--- ip rule ---"; ip rule | grep -E "$TABLE|fwmark" || true
  echo "--- table $TABLE ---"; ip route show table "$TABLE" 2>/dev/null || true
  echo "--- mangle mark ---"; iptables -t mangle -S OUTPUT | grep -E "$SVCUSER|0x400" || true
  echo "--- ui guard ---"; iptables -S INPUT | grep -- "$TAG-ui" || echo "(none)"
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
