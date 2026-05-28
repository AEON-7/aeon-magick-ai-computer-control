#!/bin/bash
# aeon-net-services — apply DNSCrypt and VPN configuration based on
# /etc/aeon/network.toml. Runs at boot and on PUT /api/network/dnscrypt
# or /api/network/vpn.
#
# Reads:   dnscrypt.{enabled,provider,location}
#          vpn.{enabled,provider}
#          vpn.tailscale.{auth_key,hostname,exit_node,advertise_exit_node}
#          vpn.wireguard.config
#          vpn.openvpn.{config,auth_username,auth_password}
#
# Each function (dnscrypt / vpn) is idempotent and tolerates the others
# being absent — safe to invoke on every config change.

set -u
NETTOML=/etc/aeon/network.toml
LOG=/var/log/aeon-net-services.log
DNSCRYPT_CONF=/etc/dnscrypt-proxy/dnscrypt-proxy.toml
DNSCRYPT_BACKUP=/etc/dnscrypt-proxy/dnscrypt-proxy.toml.aeon-orig
WG_CONF=/etc/wireguard/aeon0.conf
OVPN_CONF=/etc/openvpn/client/aeon.conf
OVPN_AUTH=/etc/openvpn/client/aeon.auth

log() { echo "$(date -Iseconds) $*" | tee -a "$LOG"; }

toml_get() {
    # $1=section  $2=key  $3=default
    python3 -c "
import tomllib
try:
    with open('$NETTOML','rb') as f:
        d = tomllib.load(f)
    section = d
    for part in '$1'.split('.'):
        section = section.get(part, {})
    v = section.get('$2', '$3') if isinstance(section, dict) else '$3'
    if isinstance(v, bool):
        print('true' if v else 'false')
    else:
        print(v)
except Exception:
    print('$3')
"
}

# ──────────────────────────────────────────────────────────────────────
# DNSCrypt
# ──────────────────────────────────────────────────────────────────────
#
# Provider → resolver name(s) in dnscrypt-proxy's public resolvers list
# (https://github.com/DNSCrypt/dnscrypt-resolvers). v50: every name
# here MUST point to a true DNSCrypt v2 resolver (sdns://AQ... stamp).
# DoH-only providers (Cloudflare / NextDNS / Mullvad / etc.) were
# dropped because they leak the resolver's hostname via TLS SNI on
# every query. Users who still want them can paste their sdns://
# stamp into the Custom slot — the protocol is then labelled honestly
# in the UI.

dnscrypt_resolvers_for() {
    case "$1" in
        quad9)             echo "quad9-dnscrypt-ip4-filter-pri quad9-dnscrypt-ip4-filter-ecs-pri" ;;
        quad9-unfiltered)  echo "quad9-dnscrypt-ip4-nofilter-pri quad9-dnscrypt-ip4-nofilter-ecs-pri" ;;
        adguard)           echo "adguard-dns" ;;
        adguard-family)    echo "adguard-dns-family" ;;
        adguard-unfiltered) echo "adguard-dns-unfiltered" ;;
        opendns)           echo "cisco" ;;
        cleanbrowsing)     echo "cleanbrowsing-security" ;;
        *)                 echo "quad9-dnscrypt-ip4-filter-pri" ;;  # safe default
    esac
}

apply_dnscrypt() {
    local enabled="$(toml_get dnscrypt enabled false)"
    local provider="$(toml_get dnscrypt provider cloudflare)"
    local location="$(toml_get dnscrypt location auto)"

    log "dnscrypt: enabled=$enabled provider=$provider location=$location"

    if [ ! -x /usr/local/bin/dnscrypt-proxy ] \
        && [ ! -x /usr/sbin/dnscrypt-proxy ] \
        && [ ! -x /usr/bin/dnscrypt-proxy ]; then
        log "dnscrypt-proxy binary not installed — skipping"
        return 0
    fi

    if [ "$enabled" != "true" ]; then
        systemctl stop dnscrypt-proxy.service 2>/dev/null || true
        systemctl disable dnscrypt-proxy.service 2>/dev/null || true
        # Restore the package's default config if we had stomped on it.
        if [ -f "$DNSCRYPT_BACKUP" ]; then
            cp "$DNSCRYPT_BACKUP" "$DNSCRYPT_CONF"
        fi
        # Undo the NM per-connection override (restore DHCP-provided DNS)
        local uuids; uuids=$(nmcli -t -f UUID,TYPE con show 2>/dev/null \
            | awk -F: '$2 ~ /ethernet|wifi/ {print $1}')
        for uuid in $uuids; do
            nmcli con modify "$uuid" ipv4.ignore-auto-dns no 2>/dev/null || true
            nmcli con modify "$uuid" ipv4.dns "" 2>/dev/null || true
        done
        # Reactivate so /etc/resolv.conf gets rewritten with DHCP DNS
        for uuid in $(nmcli -t -f UUID con show --active 2>/dev/null); do
            nmcli con up "$uuid" 2>/dev/null || true
        done
        log "dnscrypt disabled — service stopped, NM DNS restored to DHCP defaults"
        return 0
    fi

    # Stash the original config once so toggling off can restore it.
    if [ ! -f "$DNSCRYPT_BACKUP" ] && [ -f "$DNSCRYPT_CONF" ]; then
        cp "$DNSCRYPT_CONF" "$DNSCRYPT_BACKUP"
    fi

    local custom_stamp; custom_stamp="$(toml_get dnscrypt custom_stamp '')"
    local custom_label; custom_label="$(toml_get dnscrypt custom_label custom)"
    # Sanitize label to alphanumeric-and-dashes so it slots into the
    # [static.LABEL] section header without TOML weirdness.
    custom_label=$(printf '%s' "$custom_label" | tr -c 'a-zA-Z0-9_-' '_' | head -c 40)
    [ -z "$custom_label" ] && custom_label="custom"

    local server_list=""
    local custom_static_section=""

    if [ "$provider" = "custom" ]; then
        if [ -z "$custom_stamp" ]; then
            log "WARN: dnscrypt provider=custom but custom_stamp is empty — falling back to cloudflare"
            provider="cloudflare"
        else
            # Single-server config via [static.LABEL]. dnscrypt-proxy
            # reads the sdns:// stamp out of this section instead of
            # pulling from the public-resolvers list.
            server_list="'$custom_label'"
            custom_static_section=$(cat <<EOF

[static]
  [static.'$custom_label']
    stamp = '$custom_stamp'
EOF
)
            log "dnscrypt: using custom provider '$custom_label'"
        fi
    fi

    if [ -z "$server_list" ]; then
        local resolvers; resolvers=$(dnscrypt_resolvers_for "$provider")
        for r in $resolvers; do
            if [ -z "$server_list" ]; then
                server_list="'$r'"
            else
                server_list="$server_list, '$r'"
            fi
        done
    fi

    # Geo preference. dnscrypt-proxy supports a `lb_strategy` for tie-
    # breaking and a `disabled_server_names` list but no positive geo
    # filter — we lean on the resolver short-list itself which is
    # already region-curated for Cloudflare/Quad9. Location maps to a
    # built-in latency probe hint dnscrypt-proxy uses to weight picks.
    log "selected resolvers: $server_list (location hint: $location)"

    # If Tor VPN is active, route DNSCrypt's bootstrap (the initial
    # DNS lookup to find the DoH/DoT server's IP) through Tor's own
    # DNSPort on localhost. Subsequent DoH/DoT traffic to the upstream
    # (TCP) then rides through Tor's TransPort via the iptables
    # redirect chain. End result: all DNS — bootstrap + queries —
    # leaves the device via Tor only. ISP sees Tor traffic, no DoH
    # fingerprint, no plaintext DNS.
    local vpn_provider; vpn_provider="$(toml_get vpn provider none)"
    local vpn_enabled; vpn_enabled="$(toml_get vpn enabled false)"
    local bootstrap_line
    if [ "$vpn_enabled" = "true" ] && [ "$vpn_provider" = "tor" ]; then
        bootstrap_line="bootstrap_resolvers = ['127.0.0.1:5353']  # Tor DNSPort"
        log "dnscrypt: Tor is active — bootstrapping via Tor DNSPort (127.0.0.1:5353)"
    else
        bootstrap_line="bootstrap_resolvers = ['9.9.9.11:53', '1.1.1.1:53', '8.8.8.8:53']"
    fi

    # v50: lock down protocol selection to DNSCrypt for all named
    # providers (their resolver names are guaranteed DNSCrypt v2 in
    # the public-resolvers list). For the Custom slot we leave DoH/DoT
    # enabled so a user-pasted stamp using those protocols still
    # works — but the UI calls that out honestly.
    local allow_doh="false"
    if [ "$provider" = "custom" ]; then
        allow_doh="true"
    fi

    install -d -m 0755 /etc/dnscrypt-proxy
    cat > "$DNSCRYPT_CONF" <<EOF
# Managed by aeon-net-services — do not edit by hand.
# Toggle/configure via PUT /api/network/dnscrypt.

server_names = [$server_list]
listen_addresses = ['127.0.2.1:53', '[::1]:53']
max_clients = 250

ipv4_servers = true
ipv6_servers = false
dnscrypt_servers = true
doh_servers = ${allow_doh}
odoh_servers = false

require_dnssec = true
require_nolog = true
require_nofilter = false

# Bootstrap: where to send the FIRST DNS query that resolves the
# upstream DoH/DoT server's hostname. After bootstrap, that IP is
# cached and all subsequent queries go straight to the upstream.
$bootstrap_line
ignore_system_dns = true

cache = true
cache_size = 4096
cache_min_ttl = 600
cache_max_ttl = 86400
cache_neg_min_ttl = 60
cache_neg_max_ttl = 600

[sources]
  [sources.'public-resolvers']
    urls = [
      'https://raw.githubusercontent.com/DNSCrypt/dnscrypt-resolvers/master/v3/public-resolvers.md',
      'https://download.dnscrypt.info/resolvers-list/v3/public-resolvers.md',
    ]
    cache_file = '/var/cache/dnscrypt-proxy/public-resolvers.md'
    minisign_key = 'RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3'
    refresh_delay = 73
    prefix = ''
${custom_static_section}
EOF
    chmod 0644 "$DNSCRYPT_CONF"

    install -d -m 0755 /var/cache/dnscrypt-proxy
    chown -R _dnscrypt-proxy:_dnscrypt-proxy /var/cache/dnscrypt-proxy 2>/dev/null || true
    chown -R _dnscrypt-proxy:_dnscrypt-proxy /etc/dnscrypt-proxy 2>/dev/null || true

    systemctl enable --now dnscrypt-proxy.service 2>/dev/null \
        || systemctl restart dnscrypt-proxy.service 2>/dev/null \
        || log "WARN: dnscrypt-proxy.service failed to start (check journalctl)"
    log "dnscrypt active — listening on 127.0.2.1:53"

    # Point NetworkManager's shared-mode dnsmasq at 127.0.2.1 so DHCP
    # clients on usb0 get DNS via the encrypted upstream. The shared
    # method's embedded dnsmasq picks up these drop-ins.
    install -d -m 0755 /etc/NetworkManager/dnsmasq-shared.d
    cat > /etc/NetworkManager/dnsmasq-shared.d/00-aeon-dnscrypt.conf <<'EOF'
# Forward all DNS through local dnscrypt-proxy.
no-resolv
server=127.0.2.1
EOF
    # Re-up usb0 connection so the shared dnsmasq re-reads its conf.
    nmcli con up aeon-usb0 >/dev/null 2>&1 || true

    # ── System-wide DNS override ──
    # Make the Pi's OWN resolution go through DNSCrypt too — not just
    # USB clients. Without this, /etc/resolv.conf still points at the
    # LAN router (DHCP-provided), so any tool running on the Pi
    # (curl, apt, the supervisor, MCP) bypasses the encrypted resolver.
    #
    # Mechanism: override each ethernet/wifi NetworkManager connection
    # to ignore DHCP-provided DNS and use 127.0.2.1 instead. NM then
    # rewrites /etc/resolv.conf with that nameserver. This survives
    # reboots (per-connection setting is persisted).
    local uuids; uuids=$(nmcli -t -f UUID,TYPE con show 2>/dev/null \
        | awk -F: '$2 ~ /ethernet|wifi/ {print $1}')
    local count=0
    for uuid in $uuids; do
        nmcli con modify "$uuid" ipv4.ignore-auto-dns yes 2>/dev/null || continue
        nmcli con modify "$uuid" ipv4.dns "127.0.2.1" 2>/dev/null || continue
        count=$((count + 1))
    done
    log "dnscrypt: overrode DNS on $count NetworkManager connection(s) to 127.0.2.1"

    # Reactivate active connections so /etc/resolv.conf gets rewritten
    # immediately (rather than on next reconnect). Brief network blip.
    for uuid in $(nmcli -t -f UUID con show --active 2>/dev/null); do
        nmcli con up "$uuid" >/dev/null 2>&1 || true
    done
}

# ──────────────────────────────────────────────────────────────────────
# VPN
# ──────────────────────────────────────────────────────────────────────

stop_all_vpns() {
    # Idempotent — silently tolerate "service not active".
    systemctl stop wg-quick@aeon0.service 2>/dev/null || true
    systemctl disable wg-quick@aeon0.service 2>/dev/null || true
    systemctl stop openvpn-client@aeon.service 2>/dev/null || true
    systemctl disable openvpn-client@aeon.service 2>/dev/null || true
    systemctl stop tor@default.service 2>/dev/null || true
    systemctl stop tor.service 2>/dev/null || true
    systemctl disable tor.service 2>/dev/null || true
    systemctl stop i2pd.service 2>/dev/null || true
    systemctl disable i2pd.service 2>/dev/null || true
    # Tailscale: we don't fully stop tailscaled (it's the gateway daemon
    # itself), just `tailscale down`. That removes the tailnet IP without
    # killing the daemon.
    /usr/bin/tailscale down 2>/dev/null || true
    # Sweep iptables rules we own. The v19 version used
    # `iptables-save | grep -v | iptables-restore` which is brittle:
    # one parse-rejected line and iptables-restore silently bails,
    # leaving the original rules intact — but the next apply path
    # then APPENDS a new set on top. Result: doubled / tripled rule
    # sets accumulating across toggles, eventually bricking the box.
    #
    # v20 fix: list line numbers of every rule whose comment contains
    # "aeon-vpn", then delete them in reverse order (so deletion of
    # rule N doesn't shift the numbers of rules >N we still want to
    # delete). iptables -D <chain> <n> by line number always works
    # and never fails partially.
    for table in filter nat; do
        for chain in OUTPUT INPUT FORWARD PREROUTING POSTROUTING; do
            # `iptables -L --line-numbers` includes the chain even if
            # it doesn't exist; suppress stderr in case the chain is
            # absent in this table.
            local lines
            lines=$(iptables -t "$table" -L "$chain" --line-numbers -n 2>/dev/null \
                | awk '/aeon-vpn/{print $1}' | sort -rn)
            for n in $lines; do
                iptables -t "$table" -D "$chain" "$n" 2>/dev/null || true
            done
        done
    done
}

# Per-rule LOG-then-{DROP,REJECT} helper. Each "kind" of system block
# carries its own tag in the LOG prefix so /security can attribute the
# block precisely ("VPN: UDP catchall — browsers will fall back to
# TCP automatically", etc.).
#
# `aeon_block_pair <chain> <tag> <comment> <mode> <predicate-args...>`
#
# <mode> picks how aggressively to fail the client:
#   drop          silently black-hole; client times out (slow).
#   reject-port   send ICMP port-unreachable; UDP/QUIC clients fall
#                 back to TCP almost immediately. Use for the VPN
#                 UDP catchall — that's what makes QUIC→TCP fast.
#   reject-host   send ICMP host-unreachable; signals "this network
#                 destination is dead" so all higher-level transports
#                 give up. Use for kill-switch + isolation LAN denies.
#   reject-tcp    send TCP RST; client immediately sees "connection
#                 refused" instead of SYN timeout. Use for TCP
#                 catchalls.
aeon_block_pair() {
    local chain="$1" tag="$2" comment="$3" mode="$4"
    shift 4
    iptables -A "$chain" "$@" \
        -m limit --limit 5/sec --limit-burst 10 \
        -j LOG --log-prefix "AEON-DROP[${tag}]: " --log-level 4 \
        -m comment --comment "$comment"
    case "$mode" in
        reject-port)
            iptables -A "$chain" "$@" \
                -j REJECT --reject-with icmp-port-unreachable \
                -m comment --comment "$comment"
            ;;
        reject-host)
            iptables -A "$chain" "$@" \
                -j REJECT --reject-with icmp-host-unreachable \
                -m comment --comment "$comment"
            ;;
        reject-tcp)
            iptables -A "$chain" "$@" \
                -j REJECT --reject-with tcp-reset \
                -m comment --comment "$comment"
            ;;
        drop|*)
            iptables -A "$chain" "$@" \
                -j DROP \
                -m comment --comment "$comment"
            ;;
    esac
}

# Same idea but for `iptables -I <chain> 1 ...` (insert-at-top).
# Inserts the block target first (lands at pos 1) then LOG (lands at
# pos 1, pushing block to pos 2) — so the packet hits LOG before
# being rejected.
aeon_block_pair_insert() {
    local chain="$1" tag="$2" comment="$3" mode="$4"
    shift 4
    case "$mode" in
        reject-port)
            iptables -I "$chain" 1 "$@" \
                -j REJECT --reject-with icmp-port-unreachable \
                -m comment --comment "$comment"
            ;;
        reject-host)
            iptables -I "$chain" 1 "$@" \
                -j REJECT --reject-with icmp-host-unreachable \
                -m comment --comment "$comment"
            ;;
        reject-tcp)
            iptables -I "$chain" 1 "$@" \
                -j REJECT --reject-with tcp-reset \
                -m comment --comment "$comment"
            ;;
        drop|*)
            iptables -I "$chain" 1 "$@" \
                -j DROP \
                -m comment --comment "$comment"
            ;;
    esac
    iptables -I "$chain" 1 "$@" \
        -m limit --limit 5/sec --limit-burst 10 \
        -j LOG --log-prefix "AEON-DROP[${tag}]: " --log-level 4 \
        -m comment --comment "$comment"
}

# Back-compat shims so existing call sites still work. They route to
# the new helpers with mode="drop".
aeon_drop_pair()        { aeon_block_pair        "$1" "$2" "$3" "drop" "${@:4}"; }
aeon_drop_pair_insert() { aeon_block_pair_insert "$1" "$2" "$3" "drop" "${@:4}"; }

apply_vpn_tailscale() {
    local auth_key="$(toml_get vpn.tailscale auth_key '')"
    local hostname="$(toml_get vpn.tailscale hostname '')"
    local exit_node="$(toml_get vpn.tailscale exit_node false)"
    local advertise_exit="$(toml_get vpn.tailscale advertise_exit_node false)"

    if [ ! -x /usr/bin/tailscale ]; then
        log "tailscale binary not present — skipping"
        return 0
    fi

    systemctl enable --now tailscaled.service 2>/dev/null || true

    local args=("--reset")
    if [ -n "$auth_key" ]; then
        args+=("--auth-key=$auth_key")
    fi
    if [ -n "$hostname" ]; then
        args+=("--hostname=$hostname")
    fi
    if [ "$advertise_exit" = "true" ]; then
        args+=("--advertise-exit-node")
    fi
    if [ "$exit_node" = "true" ]; then
        # Pick the first available exit node automatically.
        args+=("--exit-node-allow-lan-access=true")
        # `--exit-node` requires a specific node — we set it to a magic
        # "auto" sentinel that tailscale's exit-node-list-pick supports:
        # actually, the canonical approach is to use `--exit-node=<ip>`.
        # For "any" we leave it unset and the caller picks via the
        # tailscale-up CLI; on the Pi without a UI the best we can do
        # is enable LAN-bypass — full exit-node activation requires the
        # user to call `tailscale set --exit-node=<host>` separately.
        log "tailscale exit-node mode requested — set --exit-node=<host> with: tailscale set --exit-node=<host>"
    fi
    /usr/bin/tailscale up "${args[@]}" 2>&1 | tee -a "$LOG" || true
    log "tailscale up applied"
}

apply_vpn_wireguard() {
    local cfg="$(python3 -c "
import tomllib
try:
    with open('$NETTOML','rb') as f:
        d = tomllib.load(f)
    print(d.get('vpn',{}).get('wireguard',{}).get('config',''))
except Exception:
    print('')
")"

    if [ -z "$cfg" ]; then
        log "wireguard selected but config is empty — bailing"
        return 0
    fi

    install -d -m 0700 /etc/wireguard
    printf '%s\n' "$cfg" > "$WG_CONF"
    chmod 0600 "$WG_CONF"
    systemctl enable --now wg-quick@aeon0.service 2>&1 | tee -a "$LOG" || true
    log "wireguard up via wg-quick@aeon0"
}

apply_vpn_openvpn() {
    local cfg="$(python3 -c "
import tomllib
try:
    with open('$NETTOML','rb') as f:
        d = tomllib.load(f)
    print(d.get('vpn',{}).get('openvpn',{}).get('config',''))
except Exception:
    print('')
")"
    local user="$(toml_get vpn.openvpn auth_username '')"
    local pass="$(toml_get vpn.openvpn auth_password '')"

    if [ -z "$cfg" ]; then
        log "openvpn selected but config is empty — bailing"
        return 0
    fi

    install -d -m 0755 /etc/openvpn/client
    printf '%s\n' "$cfg" > "$OVPN_CONF"
    chmod 0600 "$OVPN_CONF"

    if [ -n "$user" ] && [ -n "$pass" ]; then
        printf '%s\n%s\n' "$user" "$pass" > "$OVPN_AUTH"
        chmod 0600 "$OVPN_AUTH"
        # Inject auth-user-pass directive if not present.
        if ! grep -q "^auth-user-pass" "$OVPN_CONF"; then
            echo "auth-user-pass $OVPN_AUTH" >> "$OVPN_CONF"
        else
            sed -i "s|^auth-user-pass.*|auth-user-pass $OVPN_AUTH|" "$OVPN_CONF"
        fi
    fi

    systemctl enable --now openvpn-client@aeon.service 2>&1 | tee -a "$LOG" || true
    log "openvpn up via openvpn-client@aeon"
}

apply_vpn_tor() {
    # Bridge preset selection — see apply_tor_bridges below.
    local preset; preset="$(toml_get vpn.tor preset direct)"
    # Custom bridges (only used if preset=custom): multi-line via Python read.
    local bridges="$(python3 -c "
import tomllib
try:
    with open('$NETTOML','rb') as f:
        d = tomllib.load(f)
    print(d.get('vpn',{}).get('tor',{}).get('bridges',''))
except Exception:
    print('')
")"

    if [ ! -x /usr/bin/tor ] && [ ! -x /usr/sbin/tor ]; then
        log "tor binary not installed — skipping"
        return 0
    fi

    install -d -m 0755 /etc/tor
    # Write a minimal torrc that adds transparent-proxy + DNS-port lines
    # alongside the Debian default. We append into a drop-in so the
    # package's default torrc keeps shipping the SOCKS port too (useful
    # for in-Pi apps that prefer SOCKS).
    install -d -m 0755 /etc/tor/torrc.d
    # Bind TransPort + DNSPort to BOTH loopback AND usb0 IP (if USB
    # ethernet is enabled). The usb0 binding is what lets USB-connected
    # clients reach Tor via PREROUTING REDIRECT — DNAT'ing to 127.0.0.1
    # from a non-loopback interface needs route_localnet=1 AND wins
    # against rp_filter on some kernels and not others. REDIRECT to the
    # input interface's own IP is reliable everywhere.
    #
    # We deliberately DON'T bind to 0.0.0.0 — that would expose Tor's
    # unauthenticated TransPort to eth0/wlan0 too, turning the Pi into
    # an open Tor proxy for anyone on the LAN.
    local usb_enabled_for_tor; usb_enabled_for_tor="$(toml_get usb_ethernet enabled false)"
    local pi_addr_for_tor; pi_addr_for_tor="$(toml_get usb_ethernet pi_addr 10.55.0.1)"
    cat > /etc/tor/torrc.d/aeon.conf <<EOF
# Managed by aeon-net-services.
# Transparent proxy port for iptables REDIRECT (Pi's own traffic).
TransPort 127.0.0.1:9040 IsolateClientAddr IsolateDestPort IsolateDestAddr
# DNS resolver — iptables redirects UDP/53 here.
DNSPort 127.0.0.1:5353
# Automap onion addresses so apps resolving .onion get a real IP.
AutomapHostsOnResolve 1
AutomapHostsSuffixes .onion,.exit
# Don't run a SOCKS port on a privileged interface.
SOCKSPort 127.0.0.1:9050
EOF
    if [ "$usb_enabled_for_tor" = "true" ]; then
        cat >> /etc/tor/torrc.d/aeon.conf <<EOF
# Extra bindings for USB-connected client traffic. iptables PREROUTING
# REDIRECT in the usb0 chain rewrites client destinations to these.
TransPort ${pi_addr_for_tor}:9040 IsolateClientAddr IsolateDestPort IsolateDestAddr
DNSPort ${pi_addr_for_tor}:5353
EOF
    fi
    # Apply bridge configuration based on preset. Each preset emits its
    # own ClientTransportPlugin + Bridge lines to torrc.d/aeon.conf.
    case "$preset" in
        direct)
            log "tor: preset=direct, no bridges"
            ;;
        obfs4)
            log "tor: preset=obfs4, using built-in obfs4 bridges"
            cat >> /etc/tor/torrc.d/aeon.conf <<'EOF'
UseBridges 1
ClientTransportPlugin obfs4 exec /usr/bin/obfs4proxy managed
# Built-in obfs4 bridges from Tor Browser. These rotate with releases;
# if all are unreachable, request fresh ones from bridges.torproject.org
# and use preset="custom".
Bridge obfs4 192.95.36.142:443 CDF2E852BF539B82BD10E27E9115A31734E378C2 cert=qUVQ0srL1JI/vO6V6m/24anYXiJD3QP2HgzUKQtQ7GRqqUvs7P+tG43RtAqdhLOALP7DJQ iat-mode=1
Bridge obfs4 37.218.245.14:38224 D9A82D2F9C2F65A18407B1D2B764F130847F8B5D cert=bjRaMrr1BRiAW8IE9U5z27fQaYgOhX1UCmOpg2pFpoMvo6ZgQMzLsaTzzQNTlm7hNcb+Sg iat-mode=0
Bridge obfs4 85.31.186.98:443 011F2599C0E9B27EE74B353155E244813763C3E5 cert=ayq0XzCwhpdysn5o0EyDUbmSOx3X/oTEbzDMvczHOdBJKlvIdHHLJGkZARtT4dcBFArPPg iat-mode=0
Bridge obfs4 85.31.186.26:443 91A6354697E6B02A386312F68D82CF86824D3606 cert=PBwr+S8JTVZo6MPdHnkTwXJPILWADLqfMGoVvhZClMq/Urndyd42BwX9YFJHZnBB3H0XCw iat-mode=0
EOF
            ;;
        meek-azure)
            log "tor: preset=meek-azure, using Microsoft Azure CDN fronting"
            cat >> /etc/tor/torrc.d/aeon.conf <<'EOF'
UseBridges 1
ClientTransportPlugin meek_lite exec /usr/bin/obfs4proxy managed
# meek_lite via Azure CDN — looks like HTTPS to Microsoft to any DPI
# observer. Slower than obfs4 but harder to block.
Bridge meek_lite 192.0.2.18:80 BE776A53492E1E044A26F17306E1BC46A55A1625 url=https://meek.azureedge.net/ front=ajax.aspnetcdn.com
EOF
            ;;
        snowflake)
            log "tor: preset=snowflake"
            if [ ! -x /usr/bin/snowflake-client ]; then
                log "WARN: snowflake-client binary not installed (try: apt install snowflake-client); falling back to direct"
            else
                cat >> /etc/tor/torrc.d/aeon.conf <<'EOF'
UseBridges 1
ClientTransportPlugin snowflake exec /usr/bin/snowflake-client -url https://snowflake-broker.torproject.net.global.prod.fastly.net/ -front cdn.sstatic.net -ice stun:stun.l.google.com:19302,stun:stun.antisip.com:3478,stun:stun.bluesip.net:3478,stun:stun.dus.net:3478,stun:stun.epygi.com:3478,stun:stun.sonetel.com:3478,stun:stun.uls.co.za:3478,stun:stun.voipgate.com:3478,stun:stun.voys.nl:3478
Bridge snowflake 192.0.2.3:80 2B280B23E1107BB62ABFC40DDCC8824814F80A72
EOF
            fi
            ;;
        custom)
            if [ -n "$bridges" ]; then
                log "tor: preset=custom, $(printf '%s\n' "$bridges" | wc -l) bridge(s) configured"
                cat >> /etc/tor/torrc.d/aeon.conf <<'EOF'
UseBridges 1
ClientTransportPlugin obfs4 exec /usr/bin/obfs4proxy managed
EOF
                printf '%s\n' "$bridges" | while IFS= read -r line; do
                    [ -z "$line" ] && continue
                    echo "Bridge $line" >> /etc/tor/torrc.d/aeon.conf
                done
            else
                log "tor: preset=custom but no bridges field — defaulting to direct"
            fi
            ;;
        *)
            log "tor: unknown preset '$preset' — defaulting to direct"
            ;;
    esac

    # Make sure /etc/tor/torrc includes drop-ins. Debian's default does
    # (`%include /etc/tor/torrc.d/*.conf`) but be defensive.
    if ! grep -q "torrc.d" /etc/tor/torrc 2>/dev/null; then
        echo "%include /etc/tor/torrc.d/*.conf" >> /etc/tor/torrc
    fi

    # Open the control port + cookie auth so the supervisor can query
    # status (bootstrap %, circuits, NEWNYM identity refresh).
    if ! grep -q "^ControlPort" /etc/tor/torrc.d/aeon.conf 2>/dev/null; then
        cat >> /etc/tor/torrc.d/aeon.conf <<EOF
ControlPort 9051
CookieAuthentication 1
CookieAuthFileGroupReadable 1
EOF
    fi

    # Optional: exit-node country pin. v19's status panel writes this.
    local exit_country; exit_country="$(toml_get vpn.tor exit_country '')"
    if [ -n "$exit_country" ]; then
        # Strip any existing ExitNodes line + add the new one
        sed -i '/^ExitNodes/d' /etc/tor/torrc.d/aeon.conf
        echo "ExitNodes {$exit_country}" >> /etc/tor/torrc.d/aeon.conf
        echo "StrictNodes 1" >> /etc/tor/torrc.d/aeon.conf
    fi

    # enable + (re)start. enable --now only starts if stopped, so we
    # follow with an explicit restart to make sure a Tor that was
    # already running with old config picks up the new torrc.d/aeon.conf
    # (TransPort bindings change when usb_ethernet is toggled).
    systemctl enable tor.service 2>&1 | tee -a "$LOG" || true
    systemctl restart tor.service 2>&1 | tee -a "$LOG" || true

    # Wait for Tor's TransPort to actually start listening before
    # applying the iptables REDIRECTs. If we redirect to a port that
    # isn't accepting yet, every TCP connection on the box drops until
    # tor finishes bootstrap (30-90s, longer with bridges). Better to
    # let traffic continue uncovered for a few seconds than to black-
    # hole everything.
    local trans_port=9040
    local dns_port=5353
    local wait_max=60
    local wait_n=0
    log "waiting for tor TransPort:$trans_port to listen (up to ${wait_max}s)…"
    while ! ss -tln 2>/dev/null | grep -q ":$trans_port "; do
        wait_n=$((wait_n + 1))
        if [ "$wait_n" -ge "$wait_max" ]; then
            log "WARN: tor never opened TransPort:$trans_port after ${wait_max}s — NOT installing iptables redirects (traffic stays untunneled)"
            return 1
        fi
        sleep 1
    done
    log "tor TransPort up after ${wait_n}s — installing iptables redirects"

    # iptables: transparent redirect of all TCP + DNS to Tor. Tag with
    # comment "aeon-vpn" so stop_all_vpns can sweep them out (across
    # both filter AND nat tables).
    #
    # RULE ORDER IS CRITICAL. The default /etc/resolv.conf on Pi OS
    # with NetworkManager points at the LAN router (e.g. 192.168.1.1).
    # If the LAN-bypass rule (RETURN -d 192.168.0.0/16) is evaluated
    # before the DNS-redirect rule, all DNS queries go to the LAN
    # router *outside Tor* — apps see NXDOMAIN (LAN router doesn't
    # resolve external hosts when Tor's iptables intercepts the
    # response path) and connectivity breaks despite Tor being
    # bootstrapped and TransPort working. Fix is to insert DNS
    # redirects BEFORE LAN bypass.
    local lan_bypass; lan_bypass="$(toml_get vpn lan_bypass '192.168.0.0/16')"

    # 1. Loopback always exempt.
    iptables -t nat -A OUTPUT -o lo -j RETURN -m comment --comment "aeon-vpn"
    # 2. Tor's own traffic exempt (otherwise it'd redirect itself).
    iptables -t nat -A OUTPUT -m owner --uid-owner debian-tor \
        -j RETURN -m comment --comment "aeon-vpn"
    # 3. NOTE: we deliberately do NOT exempt DNSCrypt-proxy's UID.
    #    When DNSCrypt is ALSO active alongside Tor, we want its
    #    DoH/DoT traffic (TCP) to ride through Tor's TransPort too —
    #    that way the ISP sees only Tor traffic (not encrypted-but-
    #    distinctively-DoH packets). This is the privacy-strict
    #    behavior. The chicken-and-egg deadlock that the earlier
    #    exemption fixed is now solved a different way: when both Tor
    #    and DNSCrypt are on, apply_dnscrypt sets dnscrypt-proxy's
    #    bootstrap_resolvers to 127.0.0.1:5353 (Tor's own DNSPort) +
    #    ignore_system_dns=true. So DNSCrypt's bootstrap stays local
    #    (loopback exempts it from redirect), while its DoH/DoT
    #    upstream traffic goes through Tor.
    # 4. ALL DNS — including queries destined for the LAN router —
    #    goes to Tor's DNSPort. MUST be before LAN bypass.
    iptables -t nat -A OUTPUT -p udp --dport 53 \
        -j REDIRECT --to-ports "$dns_port" -m comment --comment "aeon-vpn"
    iptables -t nat -A OUTPUT -p tcp --dport 53 \
        -j REDIRECT --to-ports "$dns_port" -m comment --comment "aeon-vpn"
    # 5. LAN bypass for non-DNS (web UI / SSH / other LAN services).
    if [ -n "$lan_bypass" ]; then
        iptables -t nat -A OUTPUT -d "$lan_bypass" \
            -j RETURN -m comment --comment "aeon-vpn"
    fi
    # 6. All other TCP → Tor TransPort.
    iptables -t nat -A OUTPUT -p tcp --syn \
        -j REDIRECT --to-ports "$trans_port" -m comment --comment "aeon-vpn"

    # ── Forwarded-traffic hijack: route USB ethernet clients through Tor ──
    #
    # The OUTPUT chain only sees packets originating ON the Pi. USB
    # clients' traffic enters via usb0, hits PREROUTING → FORWARD →
    # POSTROUTING → eth0 egress — OUTPUT is never touched. Without
    # PREROUTING redirect rules, client TCP escapes Tor entirely and
    # goes straight out the Pi's WAN connection.
    #
    # v24-v27: tried DNAT --to-destination 127.0.0.1:9040 + route_localnet=1.
    #          Works on some kernels, silently drops on others (rp_filter,
    #          martian filtering, distro-specific sysctls).
    # v28+:    use REDIRECT, which rewrites dst to the INBOUND INTERFACE's
    #          own IP. We bind extra TransPort/DNSPort lines on the usb0
    #          IP (see apply_vpn_tor torrc setup above) so Tor accepts
    #          the redirected traffic. No route_localnet, no rp_filter
    #          juggling — just standard NAT.
    local usb_enabled; usb_enabled="$(toml_get usb_ethernet enabled false)"
    if [ "$usb_enabled" = "true" ]; then
        local pi_addr; pi_addr="$(toml_get usb_ethernet pi_addr 10.55.0.1)"
        # (a) Exempt Pi's own usb0 IP for non-DNS ports — keep web UI
        # reachable from clients (we don't want to proxy HTTPS to the
        # web UI through Tor; that would loop).
        iptables -t nat -A PREROUTING -i usb0 -d "$pi_addr" -p tcp \
            ! --dport 53 \
            -j RETURN -m comment --comment "aeon-vpn"
        # (b) Redirect all OTHER client TCP (except port 53; dnsmasq
        # handles that and forwards to DNSCrypt or Tor DNSPort) to
        # Tor's TransPort listening on the usb0 IP.
        iptables -t nat -A PREROUTING -i usb0 -p tcp \
            ! --dport 53 \
            -j REDIRECT --to-ports "$trans_port" \
            -m comment --comment "aeon-vpn"
        # (c) DNS handling: aeon-usb-net.sh already DNATs usb0 port-53
        # traffic to dnsmasq on PI_ADDR:53 (rule installed at boot).
        # dnsmasq forwards to its upstream (system resolver); those
        # upstream queries pass through the OUTPUT chain which we
        # already REDIRECT to 5353 (Tor DNSPort). So client DNS rides
        # through Tor automatically — no extra PREROUTING rule needed
        # here, and adding one would be unreachable dead code anyway
        # (the aeon-usb-net DNAT comes first in the chain).
        #
        # (d) FORWARD: drop client UDP except DHCP/NTP/mDNS. Tor can't
        # carry UDP; letting it leak around Tor would deanonymize.
        # (TCP is intercepted by PREROUTING above so it never reaches
        # FORWARD in the first place.)
        iptables -A FORWARD -i usb0 -p udp --dport 67   -j ACCEPT -m comment --comment "aeon-vpn"
        iptables -A FORWARD -i usb0 -p udp --dport 68   -j ACCEPT -m comment --comment "aeon-vpn"
        iptables -A FORWARD -i usb0 -p udp --dport 123  -j ACCEPT -m comment --comment "aeon-vpn"
        iptables -A FORWARD -i usb0 -p udp --dport 5353 -j ACCEPT -m comment --comment "aeon-vpn"
        # Block forwarded UDP from usb0 clients when Tor is active.
        # REJECT with ICMP port-unreachable rather than DROP — that
        # tells the client *immediately* "this transport is closed"
        # so QUIC/HTTP-3 falls back to TCP HTTPS in milliseconds,
        # instead of waiting out the QUIC handshake timeout (~1 sec
        # per retry, several retries). End user impact: first page
        # load feels normal instead of laggy.
        aeon_block_pair FORWARD "vpn-udp-forward" "aeon-vpn" reject-port \
            -i usb0 -p udp
        # (e) Make sure the INPUT chain accepts the redirected TCP on
        # the usb0 IP. Default Debian INPUT is ACCEPT but NetworkManager
        # / hardening profiles sometimes flip it. Add an explicit rule
        # tagged with our comment so a stricter base policy doesn't
        # silently drop the proxied traffic.
        iptables -A INPUT -i usb0 -d "$pi_addr" -p tcp --dport "$trans_port" \
            -j ACCEPT -m comment --comment "aeon-vpn"
        log "tor: USB client TCP REDIRECTed to ${pi_addr}:${trans_port}; DNS via dnsmasq → Tor; UDP dropped except DHCP/NTP/mDNS"
    fi
    # UDP can't traverse Tor — drop it BUT exempt:
    #  - Loopback (allows local UDP to DNSCrypt 127.0.2.1:53, Tor's
    #    own DNSPort, and any future on-host UDP services). Without
    #    this, the catch-all DROP below kills legitimate localhost
    #    UDP — including the Pi's own DNS queries to DNSCrypt —
    #    making the whole privacy-strict DNS chain unusable.
    #  - DHCP (67/68): the host gets an address from upstream
    #  - NTP (123): clock sync (NTP-over-Tor exists but is fragile)
    #  - mDNS (5353): .local hostnames on the LAN
    iptables -A OUTPUT -o lo -p udp -j ACCEPT -m comment --comment "aeon-vpn"
    iptables -A OUTPUT -p udp --dport 67 -j ACCEPT -m comment --comment "aeon-vpn"
    iptables -A OUTPUT -p udp --dport 68 -j ACCEPT -m comment --comment "aeon-vpn"
    iptables -A OUTPUT -p udp --dport 123 -j ACCEPT -m comment --comment "aeon-vpn"
    iptables -A OUTPUT -p udp --dport 5353 -j ACCEPT -m comment --comment "aeon-vpn"
    # Same idea for Pi-local UDP — REJECT with ICMP port-unreachable
    # so local apps fall back fast instead of timing out.
    aeon_block_pair OUTPUT "vpn-udp-output" "aeon-vpn" reject-port -p udp
    log "tor active — TCP + DNS via tor; DHCP/NTP/mDNS UDP allowed; other UDP dropped"
}

apply_vpn_i2p() {
    local outproxy="$(toml_get vpn.i2p outproxy '')"

    if [ ! -x /usr/sbin/i2pd ] && [ ! -x /usr/bin/i2pd ]; then
        log "i2pd binary not installed — skipping"
        return 0
    fi

    install -d -m 0755 /etc/i2pd
    # The Debian package ships a default /etc/i2pd/i2pd.conf with HTTP
    # proxy on 4444 and SOCKS on 4447 already enabled. We leave that
    # alone and only manage the optional outproxy override.
    if [ -n "$outproxy" ]; then
        # Append outproxy directive to the [httpproxy] section if not
        # already present; replace it in-place otherwise.
        if grep -q "^outproxy" /etc/i2pd/i2pd.conf 2>/dev/null; then
            sed -i "s|^outproxy.*|outproxy = $outproxy|" /etc/i2pd/i2pd.conf
        else
            cat >> /etc/i2pd/i2pd.conf <<EOF

# aeon outproxy override
[httpproxy]
outproxy = $outproxy
EOF
        fi
        log "i2p: outproxy set to $outproxy"
    fi

    systemctl enable --now i2pd.service 2>&1 | tee -a "$LOG" || true
    log "i2p (i2pd) active — HTTP proxy on 127.0.0.1:4444, SOCKS on 127.0.0.1:4447"
    log "    (apps must opt in by configuring those proxies — not transparently routed)"
}

apply_kill_switch() {
    local enabled="$(toml_get vpn kill_switch false)"
    local provider="$(toml_get vpn provider none)"

    if [ "$enabled" != "true" ]; then
        log "kill-switch: disabled"
        return 0
    fi

    # If no VPN is even active, refuse — the kill-switch would orphan
    # the device entirely.
    if [ "$provider" = "none" ] || [ -z "$provider" ]; then
        log "kill-switch: refusing to apply because no VPN provider is selected"
        return 0
    fi

    local lan_bypass; lan_bypass="$(toml_get vpn lan_bypass '192.168.0.0/16')"
    log "kill-switch: ENABLED — VPN-only outbound; lan_bypass=$lan_bypass"

    # Allow loopback.
    iptables -A OUTPUT -o lo -j ACCEPT -m comment --comment "aeon-vpn"
    # Allow established/related (return traffic for inbound conns).
    iptables -A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT -m comment --comment "aeon-vpn"
    # Allow LAN bypass.
    if [ -n "$lan_bypass" ]; then
        iptables -A OUTPUT -d "$lan_bypass" -j ACCEPT -m comment --comment "aeon-vpn"
    fi

    case "$provider" in
        tailscale)
            iptables -A OUTPUT -o tailscale0 -j ACCEPT -m comment --comment "aeon-vpn"
            ;;
        wireguard)
            iptables -A OUTPUT -o aeon0 -j ACCEPT -m comment --comment "aeon-vpn"
            ;;
        openvpn)
            iptables -A OUTPUT -o tun0 -j ACCEPT -m comment --comment "aeon-vpn"
            iptables -A OUTPUT -o tun1 -j ACCEPT -m comment --comment "aeon-vpn"
            ;;
        tor|i2p)
            # For Tor + I2P the transparent-redirect rules already
            # enforce "everything goes through them". Allow connections
            # originated by the proxy daemons themselves.
            iptables -A OUTPUT -m owner --uid-owner debian-tor -j ACCEPT -m comment --comment "aeon-vpn" 2>/dev/null || true
            iptables -A OUTPUT -m owner --uid-owner i2pd -j ACCEPT -m comment --comment "aeon-vpn" 2>/dev/null || true
            ;;
    esac

    # Drop everything else outbound.
    # Kill-switch catchall: tunnel is down, block everything else.
    # ICMP host-unreachable is the right signal — tells apps the
    # network destination itself is dead, no need to retry.
    aeon_block_pair OUTPUT "vpn-killswitch" "aeon-vpn" reject-host
    log "kill-switch applied — non-VPN outbound traffic is now dropped"
}

apply_vpn() {
    local enabled="$(toml_get vpn enabled false)"
    local provider="$(toml_get vpn provider none)"

    log "vpn: enabled=$enabled provider=$provider"

    # v39+: dropped the central AEON_DROP chain in favor of inline
    # LOG-then-DROP pairs per call site (aeon_drop_pair helper above).
    # No setup needed here — pairs are emitted on the fly.

    # Always start by stopping all VPNs — this gives us a clean slate
    # (also sweeps any prior iptables rules tagged "aeon-vpn").
    stop_all_vpns

    if [ "$enabled" != "true" ] || [ "$provider" = "none" ] || [ -z "$provider" ]; then
        log "vpn disabled — all tunnels down"
        return 0
    fi

    case "$provider" in
        tailscale) apply_vpn_tailscale ;;
        wireguard) apply_vpn_wireguard ;;
        openvpn)   apply_vpn_openvpn ;;
        tor)       apply_vpn_tor ;;
        i2p)       apply_vpn_i2p ;;
        *)         log "WARN: unknown vpn provider '$provider' — leaving all tunnels down" ;;
    esac

    # Kill-switch is layered on TOP of the chosen provider so the rules
    # see the VPN's interface already up.
    apply_kill_switch
}

# ──────────────────────────────────────────────────────────────────────
# Main
# ──────────────────────────────────────────────────────────────────────

# Order matters: VPN first so the tunnel + iptables redirects are
# established BEFORE DNSCrypt tries to bootstrap. When Tor is the active
# VPN, DNSCrypt's bootstrap_resolvers point at Tor's DNSPort
# (127.0.0.1:5353) — that port must exist before DNSCrypt's first
# query, so we set up Tor first.
apply_vpn
apply_dnscrypt
log "aeon-net-services done"
