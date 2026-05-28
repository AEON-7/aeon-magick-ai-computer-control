#!/bin/bash
# aeon-usb-net — apply USB-ethernet passthrough configuration based on
# /etc/aeon/network.toml. Runs at boot and on POST /api/network/usb.
#
# Reads:   usb_ethernet.enabled, mode, subnet, pi_addr, dhcp_start, dhcp_end
# Effects:
#   1. NetworkManager profile for usb0 (method=shared = NM-runs-dnsmasq + NAT)
#   2. iptables FORWARD rules — block LAN destinations in isolation /
#      restricted modes, allow them in sharing mode.
#   3. iptables INPUT rules — in restricted mode, block ALL traffic from
#      usb0 to the Pi itself except DHCP + DNS.
#   4. ip_forward kernel sysctl
#
# This script is idempotent — safe to re-run on every config change.

set -u
NETTOML=/etc/aeon/network.toml
LOG=/var/log/aeon-usb-net.log

log() { echo "$(date -Iseconds) $*" | tee -a "$LOG"; }

# Robustly read TOML values via python3 (toml stdlib).
toml_get() {
    python3 -c "
import tomllib, sys
try:
    with open('$NETTOML', 'rb') as f:
        d = tomllib.load(f)
    section = d.get('usb_ethernet', {})
    print(section.get('$1', '$2'))
except Exception as e:
    print('$2')
"
}

ENABLED=$(toml_get enabled false)
MODE=$(toml_get mode isolation)
SUBNET=$(toml_get subnet 10.55.0.0/24)
PI_ADDR=$(toml_get pi_addr 10.55.0.1)
DHCP_START=$(toml_get dhcp_start 10.55.0.10)
DHCP_END=$(toml_get dhcp_end 10.55.0.50)

log "config: enabled=$ENABLED mode=$MODE subnet=$SUBNET pi=$PI_ADDR"

# ── Always: tear down any prior iptables rules we own ──
# We tag our rules with a comment "aeon-usb-net" so we can find + remove them.
# This sweeps both INPUT and FORWARD chains.
flush_our_rules() {
    # Use line-number-based deletion, same fix pattern as
    # aeon-net-services. The iptables-save | grep -v | iptables-restore
    # approach is brittle (one parse-rejected line and the whole batch
    # bails silently, leaving original rules — then the next apply
    # path stacks NEW rules on top of OLD ones until network breaks).
    for table in filter nat; do
        for chain in OUTPUT INPUT FORWARD PREROUTING POSTROUTING; do
            local lines
            lines=$(iptables -t "$table" -L "$chain" --line-numbers -n 2>/dev/null \
                | awk '/aeon-usb-net/{print $1}' | sort -rn)
            for n in $lines; do
                iptables -t "$table" -D "$chain" "$n" 2>/dev/null || true
            done
        done
    done
}
flush_our_rules

# Drop NM profile if we're disabling.
if [ "$ENABLED" != "True" ] && [ "$ENABLED" != "true" ]; then
    log "USB ethernet disabled — removing NM profile + leaving iptables clean"
    nmcli con delete aeon-usb0 2>/dev/null || true
    exit 0
fi

# ── Enabled path ──

# 1. NM profile for usb0. `method=shared` makes NM run dnsmasq for DHCP
#    on this iface AND apply NAT — does most of the work for us.
PI_PREFIX=$(echo "$SUBNET" | awk -F/ '{print $2}')
nmcli con delete aeon-usb0 2>/dev/null || true
nmcli con add type ethernet con-name aeon-usb0 ifname usb0 \
    ipv4.method shared \
    ipv4.addresses "${PI_ADDR}/${PI_PREFIX}" \
    ipv6.method disabled \
    connection.autoconnect yes \
    > /dev/null
log "NM profile aeon-usb0 created/refreshed"

# Bring it up (no-op if already up; usb0 may not exist yet if the gadget
# isn't bound — NM will pick it up when it appears).
nmcli con up aeon-usb0 >/dev/null 2>&1 || \
    log "(usb0 not present yet — NM will auto-bring-up when gadget enumerates)"

# 2. Ensure ip_forward is on (NM should do this already but be explicit).
sysctl -w net.ipv4.ip_forward=1 >/dev/null

# 2b. Force ALL DNS from USB clients to go through the Pi's dnsmasq
#     (which then forwards to DNSCrypt if enabled). Without this, the
#     connected host's OS uses whatever DNS it learned from its OTHER
#     interfaces (e.g. WiFi's gateway at 192.168.1.1), bypassing our
#     encrypted resolver entirely.
#
#     The DNAT redirect catches any DNS query — even ones the client
#     sends to e.g. 1.1.1.1 or 192.168.1.1 — and rewrites the
#     destination to the Pi's gateway IP (10.55.0.1:53), where NM's
#     shared-mode dnsmasq is listening. The kernel's conntrack
#     un-NATs the response so the client thinks it got an answer from
#     whatever DNS server it queried.
#
#     This is standard hotspot-router behavior (every captive portal
#     does this) and is what makes restricted mode actually meaningful
#     for DNS privacy too.
# Per-rule LOG-then-DROP helper. Each DROP point gets its own tag so
# the security console can attribute the block to a specific rule
# ("Blocked by: USB isolation RFC1918 deny" etc.) instead of a
# generic "something dropped this".
#
# Insert-at-top variant: emits DROP first then LOG so that LOG ends
# up *above* DROP in chain order (so packets log before getting
# dropped). Each pair tagged with the same comment so the cleanup
# sweep removes both atomically.
aeon_drop_pair_insert() {
    local chain="$1"
    local tag="$2"
    shift 2
    iptables -I "$chain" 1 "$@" \
        -j DROP \
        -m comment --comment "aeon-usb-net"
    iptables -I "$chain" 1 "$@" \
        -m limit --limit 5/sec --limit-burst 10 \
        -j LOG --log-prefix "AEON-DROP[${tag}]: " --log-level 4 \
        -m comment --comment "aeon-usb-net"
}

iptables -t nat -A PREROUTING -i usb0 -p udp --dport 53 \
    -j DNAT --to-destination "${PI_ADDR}:53" \
    -m comment --comment "aeon-usb-net"
iptables -t nat -A PREROUTING -i usb0 -p tcp --dport 53 \
    -j DNAT --to-destination "${PI_ADDR}:53" \
    -m comment --comment "aeon-usb-net"

# 3. Apply mode-specific FORWARD + INPUT rules. NAT (MASQUERADE) for
#    usb0→others is handled by NM's shared method already; we add the
#    isolation / restricted overlays on top.
apply_isolation_forward() {
    # Order matters: deny RFC1918 destinations FIRST. Insert at front of
    # FORWARD so they evaluate before any NM-installed ACCEPTs.
    aeon_drop_pair_insert FORWARD "usbnet-iso-rfc1918" -i usb0 -d 192.168.0.0/16
    aeon_drop_pair_insert FORWARD "usbnet-iso-rfc1918" -i usb0 -d 172.16.0.0/12
    # 10.x is tricky — block all of 10/8 EXCEPT our own usb-net subnet
    # (which contains the Pi and DHCP clients).
    aeon_drop_pair_insert FORWARD "usbnet-iso-rfc1918" -i usb0 -d 10.0.0.0/8 ! -d "$SUBNET"
}

case "$MODE" in
    restricted)
        log "applying restricted rules (host has WAN only; Pi services hidden)"
        # Same LAN-isolation as isolation mode:
        apply_isolation_forward
        # Plus: cut ALL traffic from usb0 → the Pi itself except DHCP/DNS.
        # Insert DROP first so it ends up BELOW the subsequent ACCEPTs
        # (iptables -I always inserts at position 1, pushing earlier rules
        # down). Final ordering: ACCEPT tcp/53, ACCEPT udp/53, ACCEPT udp/67,
        # DROP all.
        aeon_drop_pair_insert INPUT "usbnet-restricted-input" -i usb0
        iptables -I INPUT 1 -i usb0 -p udp --dport 67 -j ACCEPT -m comment --comment "aeon-usb-net"
        iptables -I INPUT 1 -i usb0 -p udp --dport 53 -j ACCEPT -m comment --comment "aeon-usb-net"
        iptables -I INPUT 1 -i usb0 -p tcp --dport 53 -j ACCEPT -m comment --comment "aeon-usb-net"
        log "restricted active — only DHCP (udp/67) + DNS (53) reach the Pi"
        ;;
    isolation)
        log "applying isolation FORWARD + INPUT rules"
        apply_isolation_forward
        # CRITICAL: block usb clients from reaching the Pi via any
        # non-usb0 interface IP. Without this, a client connected to
        # the usb-net gadget can simply ARP/connect to the Pi's
        # eth0/wlan0/tailscale0 address and manage the device that
        # way, completely defeating isolation. The Pi's web UI, SSH,
        # and supervisor API bind to 0.0.0.0 → they accept connections
        # to the LAN address from a usb client routing through.
        #
        # Implementation uses RFC1918 deny ranges with a single
        # exemption for the Pi's usb0 gateway IP. Insert order matters:
        # the LAST `-I` lands at position 1, so we insert DROPs first
        # then the ACCEPT, leaving the ACCEPT at the top of the chain.
        #
        # Why RFC1918 instead of enumerating Pi interface IPs at
        # apply-time? Two reasons: (a) it survives interface changes
        # without a re-apply (eth0 swaps, wifi reconnects to a new
        # AP), and (b) it covers ANY future Pi interface address that
        # falls in the private space — including container bridges,
        # docker0, etc. — without enumeration drift.
        aeon_drop_pair_insert INPUT "usbnet-iso-pi-rfc1918" -i usb0 -d 10.0.0.0/8
        aeon_drop_pair_insert INPUT "usbnet-iso-pi-rfc1918" -i usb0 -d 172.16.0.0/12
        aeon_drop_pair_insert INPUT "usbnet-iso-pi-rfc1918" -i usb0 -d 192.168.0.0/16
        # ACCEPT must be inserted LAST so it ends up at position 1
        # — first match wins, so this gets matched before any DROP.
        iptables -I INPUT -i usb0 -d "$PI_ADDR"     -j ACCEPT -m comment --comment "aeon-usb-net"
        log "isolation active — usb clients reach Pi only via ${PI_ADDR}; all other RFC1918 paths blocked, WAN routed via FORWARD"
        ;;
    sharing)
        log "sharing mode — no FORWARD restrictions (host has full LAN access)"
        ;;
    *)
        log "WARN: unknown mode '$MODE' — defaulting to isolation"
        apply_isolation_forward
        ;;
esac

log "aeon-usb-net done"
