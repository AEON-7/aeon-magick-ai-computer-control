#!/bin/bash
# aeon-myst-route — give the Mysterium node clean WAN egress while the rest of the
# Orb stays on its VPN, WITHOUT the double-NAT that makes a provider unreachable.
#
# Approach: policy-route the mysterium-node UID via a dedicated table at CONNECT
# time (`ip rule uidrange`) — NOT by marking packets after the socket has already
# chosen a source. The kernel therefore selects the WAN interface's address as the
# source from the start, so NO MASQUERADE is needed -> single NAT (the home router
# only) -> the node can be reached (hole-punching / port-forward work). The earlier
# mark+MASQUERADE design made the node look symmetric-NAT'd ("Monitoring failed").
# Local nets stay on the main table (dnscrypt resolver is 127.0.2.1; LAN replies;
# UPnP multicast 239.255.255.250).
#
# Also firewalls the NodeUI (:4449) to LAN/tailnet only; TequilAPI (:4050) loopback.
#
# Subcommands: apply | clear | show | test
set -u

TABLE=400
PRIO=5100
SVCUSER=mysterium-node
SVCUID=$(id -u "$SVCUSER" 2>/dev/null || echo 989)
TAG=aeon-myst-wan
MARK=0x400   # legacy — referenced only to clean up the pre-uidrange mark/MASQUERADE design
UI_PORT=4449
TQ_PORT=4050
UI_ALLOWED_IFACES="lo wlan0 eth0 tailscale0"
# Loopback/LAN/link-local/CGNAT/multicast stay on the main table (see header).
LOCAL_NETS="127.0.0.0/8 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16 100.64.0.0/10 224.0.0.0/4"

# Physical WAN gateway+dev. OpenVPN's redirect uses a 0.0.0.0/1 + 128.0.0.0/1
# split (not a real default), so the DHCP-assigned `default` route survives and
# is the true circuit. Fall back to any default whose dev isn't a tunnel.
detect_wan() {
  local out
  out=$(ip -4 route show default proto dhcp 2>/dev/null | awk '/^default/{print $3, $5; exit}')
  [ -z "$out" ] && out=$(ip -4 route show default 2>/dev/null | awk '$5 !~ /^(tun|wg|tailscale|aeon|myst)/{print $3, $5; exit}')
  echo "$out"
}

# Remove the node's policy-routing + kill-switch rules (current uidrange design AND
# the legacy fwmark/mangle/MASQUERADE design, for clean in-place migration).
clear_routing() {
  local net d dev
  for net in $LOCAL_NETS; do ip rule del uidrange "$SVCUID-$SVCUID" to "$net" table main 2>/dev/null || true; done
  ip rule del uidrange "$SVCUID-$SVCUID" table "$TABLE" 2>/dev/null || true
  while iptables -D OUTPUT -m owner --uid-owner "$SVCUSER" -j ACCEPT -m comment --comment "$TAG" 2>/dev/null; do :; done
  # --- legacy mark-based design cleanup ---
  for net in $LOCAL_NETS; do ip rule del fwmark "$MARK/$MARK" to "$net" table main 2>/dev/null || true; done
  ip rule del fwmark "$MARK/$MARK" table "$TABLE" 2>/dev/null || true
  iptables -t mangle -D OUTPUT -m owner --uid-owner "$SVCUSER" -j MARK --set-mark "$MARK" 2>/dev/null || true
  for net in $LOCAL_NETS; do iptables -t mangle -D OUTPUT -m owner --uid-owner "$SVCUSER" -d "$net" -j RETURN 2>/dev/null || true; done
  while iptables -D OUTPUT -m mark --mark "$MARK/$MARK" -j ACCEPT -m comment --comment "$TAG" 2>/dev/null; do :; done
  read -r _ dev <<<"$(detect_wan)"
  for d in $dev wlan0 eth0; do
    while iptables -t nat -D POSTROUTING -m mark --mark "$MARK/$MARK" -o "$d" -j MASQUERADE 2>/dev/null; do :; done
  done
}

apply() {
  local gw dev net p
  read -r gw dev <<<"$(detect_wan)"
  [ -z "${gw:-}" ] && { echo "aeon-myst-route: no physical WAN gateway found" >&2; return 1; }
  echo "aeon-myst-route: WAN egress for uid $SVCUID ($SVCUSER) via $gw dev $dev (table $TABLE, no NAT)"
  ip route replace default via "$gw" dev "$dev" table "$TABLE"
  clear_routing
  # Route the node's UID via the WAN table at connect() time — local nets first
  # (DNS=127.0.2.1, LAN replies, UPnP multicast), then the WAN catch-all. Because
  # this routes BEFORE source selection, the source is the WAN iface IP and no NAT
  # is required.
  p=5090
  for net in $LOCAL_NETS; do
    ip rule add uidrange "$SVCUID-$SVCUID" to "$net" table main priority "$p"
    p=$((p + 1))
  done
  ip rule add uidrange "$SVCUID-$SVCUID" table "$TABLE" priority "$PRIO"
  # Kill-switch exception: the node egresses the WAN by design.
  iptables -I OUTPUT 1 -m owner --uid-owner "$SVCUSER" -j ACCEPT -m comment --comment "$TAG"
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
  clear_routing
  ip route flush table "$TABLE" 2>/dev/null || true
  clear_ui_guard
  echo "aeon-myst-route: cleared"
}

show() {
  echo "--- ip rule (uid $SVCUID) ---"; ip rule | grep -E "$TABLE|uidrange" || true
  echo "--- table $TABLE ---"; ip route show table "$TABLE" 2>/dev/null || true
  echo "--- kill-switch accept ---"; iptables -S OUTPUT | grep -- "$TAG\$" || true
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
  test)  apply || exit 1; echo "mysterium-node egress: $(ipinfo)"; show ;;
  *) echo "usage: aeon-myst-route {apply|clear|show|test}" >&2; exit 1 ;;
esac
