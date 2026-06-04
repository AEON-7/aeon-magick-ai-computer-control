#!/bin/bash
# aeon-net-services — apply DNSCrypt and VPN configuration based on
# /etc/aeon/network.toml. Runs at boot and on PUT /api/network/dnscrypt
# or /api/network/vpn.
#
# Reads:   dnscrypt.{enabled,provider,location}
#          vpn.{enabled,provider}
#          tailscale.{enabled,auth_key,hostname,exit_node,advertise_exit_node}
#          vpn.wireguard.config
#          vpn.openvpn.{config,auth_username,auth_password}
#
# v80: Tailscale is an INDEPENDENT top-level toggle (like tor/i2p) — it
# runs alongside any vpn.provider. It is no longer torn down by the VPN
# apply path; apply_tailscale owns its own lifecycle via `--reset` /
# `tailscale down`.
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
        # v90: drop the tailscale0 exit-client DNS REDIRECT — the resolver
        # it points at is gone, so leaving it would black-hole exit DNS.
        iptables -t nat -D PREROUTING -i tailscale0 -p udp --dport 53 \
            -j REDIRECT --to-ports 53 -m comment --comment "$TS_EXIT_TAG" 2>/dev/null || true
        iptables -t nat -D PREROUTING -i tailscale0 -p tcp --dport 53 \
            -j REDIRECT --to-ports 53 -m comment --comment "$TS_EXIT_TAG" 2>/dev/null || true
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

    # v55: when server_mode = "auto", use the list the supervisor's
    # auto-picker already wrote to TOML — that's filtered against the
    # user's privacy/trust criteria across the full ~226-server
    # catalog. dnscrypt-proxy's lb_strategy="p2" picks per-query by
    # observed latency, so the user gets the lowest-latency match for
    # their criteria automatically. Falls back to the single-resolver
    # behaviour if server_mode is "specific" or the auto list is
    # somehow empty.
    local server_mode; server_mode="$(toml_get dnscrypt server_mode specific)"
    if [ "$server_mode" = "auto" ] && [ -z "$server_list" ]; then
        local auto_csv
        auto_csv=$(python3 -c "
import tomllib
try:
    cfg = tomllib.loads(open('$NETTOML').read())
    s = cfg.get('dnscrypt', {}).get('auto_picked_servers', [])
    print(', '.join(\"'\" + r + \"'\" for r in s))
except Exception:
    print('')
")
        if [ -n "$auto_csv" ]; then
            server_list="$auto_csv"
            log "dnscrypt: server_mode=auto — using $(echo "$auto_csv" | tr ',' '\n' | wc -l | tr -d ' ') resolvers from criteria"
        else
            log "WARN: dnscrypt server_mode=auto but auto_picked_servers is empty — falling back to provider"
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
    # DNSPort on localhost. Subsequent DNSCrypt traffic to the upstream
    # rides through Tor's TransPort via the iptables redirect chain —
    # BUT only if it's TCP. Tor TransPort can't forward UDP, and
    # DNSCrypt v2 talks UDP by default. So we ALSO have to flip
    # `force_tcp = true` in dnscrypt-proxy's config to keep it from
    # spraying UDP at Quad9:8443 that our anti-leak REJECT rule kills
    # (we saw exactly this: 814 packets blocked, all UDP/8443 to
    # 9.9.9.11 and 149.112.112.112). The upstream-resolver listens on
    # both TCP and UDP, so this is a transport-only change for us —
    # no flag needs to be flipped on the resolver side. End result
    # without Tor: dnscrypt-proxy's default behaviour (UDP first,
    # TCP fallback). With Tor: TCP-only, every query rides the
    # TransPort.
    local vpn_provider; vpn_provider="$(toml_get vpn provider none)"
    local vpn_enabled; vpn_enabled="$(toml_get vpn enabled false)"
    local bootstrap_line
    local force_tcp_line=""
    if [ "$vpn_enabled" = "true" ] && [ "$vpn_provider" = "tor" ]; then
        bootstrap_line="bootstrap_resolvers = ['127.0.0.1:5353']  # Tor DNSPort"
        force_tcp_line="force_tcp = true   # Tor TransPort only forwards TCP"
        log "dnscrypt: Tor is active — bootstrapping via Tor DNSPort (127.0.0.1:5353), force_tcp=true"
    else
        bootstrap_line="bootstrap_resolvers = ['9.9.9.11:53', '1.1.1.1:53', '8.8.8.8:53']"
        log "dnscrypt: bootstrap_resolvers = 9.9.9.11, 1.1.1.1, 8.8.8.8"
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

    # v51: Anonymized DNSCrypt. The supervisor's PUT handler writes a
    # comma-separated list of relay names to `picked_relays` in the
    # [dnscrypt.anonymized] section whenever the user enables this.
    # We translate that into dnscrypt-proxy's [anonymized_dns] routes
    # block here. Off by default; the section stays empty unless the
    # user opts in.
    local anon_enabled; anon_enabled="$(toml_get dnscrypt.anonymized enabled false)"
    local anonymized_section=""
    if [ "$anon_enabled" = "true" ]; then
        local picked; picked="$(toml_get dnscrypt.anonymized picked_relays '')"
        # picked_relays is stored as a TOML array like ["a", "b", "c"].
        # toml_get returns a Python repr; massage it into bare-comma-list.
        # Easier path: hit a small Python one-liner so we don't reinvent
        # TOML parsing in bash.
        local relay_csv
        relay_csv=$(python3 -c "
import tomllib
try:
    cfg = tomllib.loads(open('$NETTOML').read())
    relays = cfg.get('dnscrypt', {}).get('anonymized', {}).get('picked_relays', [])
    # Resolver name on the right side of the route — dnscrypt-proxy
    # uses route per server, but '*' wildcards apply to all servers.
    # We use the configured server here so the route is unambiguous.
    print(', '.join(\"'\" + r + \"'\" for r in relays))
except Exception as e:
    print('')
")
        if [ -n "$relay_csv" ]; then
            # We apply the relay list to ALL configured server_names
            # via '*' — that keeps the config concise even if the user
            # has multiple servers active.
            anonymized_section=$(cat <<EOF

[anonymized_dns]
routes = [
  { server_name = '*', via = [${relay_csv}] },
]
# Skip incompatible (relay) ↔ (server) pairs gracefully instead of
# refusing to start when one combination is currently offline.
skip_incompatible = true
EOF
)
            log "anonymized DNSCrypt active — relays: ${relay_csv}"
        else
            log "anonymized DNSCrypt enabled but no relays picked — falling back to direct DNSCrypt"
        fi
    fi

    # v90: when this Pi advertises a Tailscale exit node, ALSO bind the
    # resolver on the tailscale0 IP. Exit clients' DNS is REDIRECTed (in
    # the PREROUTING rule below) to the inbound iface's own IP:53 — and
    # REDIRECT only lands if dnscrypt-proxy is actually listening there
    # (the same reason apply_tor_service binds Tor on the usb0 IP). The
    # 127.0.2.1 + [::1] binds keep the Pi's own resolution unchanged.
    local ts_dns_listen=""
    local ts_dns_redirect=false
    local ts_dns_ip=""
    if [ "$(toml_get tailscale enabled false)" = "true" ] \
        && [ "$(toml_get tailscale advertise_exit_node false)" = "true" ]; then
        ts_dns_ip="$(/usr/bin/tailscale ip -4 2>/dev/null | head -1)"
        # Only redirect exit-client DNS to DNSCrypt when transparent Tor
        # isn't already grabbing all DNS (apply_tor_service points
        # tailscale0 :53 at Tor's DNSPort in that case — mirroring how the
        # Pi's own + usb0 DNS behave). Off/split Tor ⇒ DNSCrypt handles it.
        local _tor_en _tor_mode
        _tor_en="$(toml_get tor enabled false)"
        _tor_mode="$(toml_get tor mode split_tunnel)"
        if [ -n "$ts_dns_ip" ] && ! { [ "$_tor_en" = "true" ] && [ "$_tor_mode" = "transparent" ]; }; then
            ts_dns_listen=", '${ts_dns_ip}:53'"
            ts_dns_redirect=true
        fi
    fi

    install -d -m 0755 /etc/dnscrypt-proxy
    cat > "$DNSCRYPT_CONF" <<EOF
# Managed by aeon-net-services — do not edit by hand.
# Toggle/configure via PUT /api/network/dnscrypt.

server_names = [$server_list]
listen_addresses = ['127.0.2.1:53', '[::1]:53'${ts_dns_listen}]
max_clients = 250

# v55: "p2" = Weighted Power of Two. dnscrypt-proxy continuously
# probes the configured server_names and routes per-query to a
# weighted random pick among the two lowest-latency ones. With our
# auto-mode list of 30 criteria-matching resolvers, this is the
# "auto-pick best latency live" behaviour the user asked for. For
# specific-mode the server_names list has one entry, so the strategy
# is a no-op.
lb_strategy = 'p2'
lb_estimator = true

ipv4_servers = true
ipv6_servers = false
dnscrypt_servers = true
doh_servers = ${allow_doh}
odoh_servers = false

require_dnssec = true
require_nolog = true
require_nofilter = false

# v54: force TCP for all upstream queries when Tor is the active
# VPN. Tor's TransPort can't forward UDP, and our anti-leak iptables
# REJECT-s Pi-originating UDP outside of DHCP/NTP/mDNS — so without
# this, every DNSCrypt query gets killed and dnscrypt-proxy spins
# retrying. With force_tcp, every query rides Tor's TCP TransPort
# cleanly. (Empty string when Tor isn't active — UDP-first behaviour
# is faster on a normal connection.)
${force_tcp_line}

# Bootstrap: where to send the FIRST DNS query that resolves the
# upstream resolver's hostname. After bootstrap, that IP is cached
# and subsequent queries go straight to the upstream.
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
  # v51: also fetch the anonymized relay list so dnscrypt-proxy can
  # resolve any relay names in our [anonymized_dns] routes block.
  [sources.'relays']
    urls = [
      'https://raw.githubusercontent.com/DNSCrypt/dnscrypt-resolvers/master/v3/relays.md',
      'https://download.dnscrypt.info/resolvers-list/v3/relays.md',
    ]
    cache_file = '/var/cache/dnscrypt-proxy/relays.md'
    minisign_key = 'RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3'
    refresh_delay = 73
    prefix = ''
${custom_static_section}
${anonymized_section}
EOF
    chmod 0644 "$DNSCRYPT_CONF"

    install -d -m 0755 /var/cache/dnscrypt-proxy
    chown -R _dnscrypt-proxy:_dnscrypt-proxy /var/cache/dnscrypt-proxy 2>/dev/null || true
    chown -R _dnscrypt-proxy:_dnscrypt-proxy /etc/dnscrypt-proxy 2>/dev/null || true

    systemctl enable --now dnscrypt-proxy.service 2>/dev/null \
        || systemctl restart dnscrypt-proxy.service 2>/dev/null \
        || log "WARN: dnscrypt-proxy.service failed to start (check journalctl)"
    log "dnscrypt active — listening on 127.0.2.1:53${ts_dns_ip:+ + ${ts_dns_ip}:53 (tailscale0 exit clients)}"

    # ── v90: tailscale0 exit-client DNS → the encrypted resolver ──
    # Forwarded exit-node clients have no dnsmasq in front of them (unlike
    # usb0), so point their :53 straight at dnscrypt-proxy via REDIRECT.
    # REDIRECT rewrites the destination to tailscale0's own IP:53, which
    # the listen_addresses binding above now answers. Delete-before-add
    # keeps it idempotent across re-applies + reassert_policy_routing.
    # Tagged aeon-ts-exit so teardown_tailscale_exit_node sweeps it when
    # the feature (or DNSCrypt) goes away. Skipped when transparent Tor is
    # active (Tor owns tailscale0 DNS then).
    iptables -t nat -D PREROUTING -i tailscale0 -p udp --dport 53 \
        -j REDIRECT --to-ports 53 -m comment --comment "$TS_EXIT_TAG" 2>/dev/null || true
    iptables -t nat -D PREROUTING -i tailscale0 -p tcp --dport 53 \
        -j REDIRECT --to-ports 53 -m comment --comment "$TS_EXIT_TAG" 2>/dev/null || true
    if [ "$ts_dns_redirect" = "true" ]; then
        iptables -t nat -A PREROUTING -i tailscale0 -p udp --dport 53 \
            -j REDIRECT --to-ports 53 -m comment --comment "$TS_EXIT_TAG"
        iptables -t nat -A PREROUTING -i tailscale0 -p tcp --dport 53 \
            -j REDIRECT --to-ports 53 -m comment --comment "$TS_EXIT_TAG"
        log "dnscrypt: tailscale0 exit-client DNS REDIRECTed to ${ts_dns_ip}:53 (encrypted/filtered resolver)"
    fi

    # Point NetworkManager's shared-mode dnsmasq at 127.0.2.1 so DHCP
    # clients on usb0 get DNS via the encrypted upstream. The shared
    # method's embedded dnsmasq picks up these drop-ins.
    #
    # v57: ALSO carve out .onion + .exit suffixes to go DIRECTLY to
    # Tor's DNSPort on 127.0.0.1:5353 instead of through dnscrypt-
    # proxy. dnscrypt-proxy would forward .onion to the upstream
    # public resolver (Quad9 etc.), which doesn't know about onion
    # services and returns NXDOMAIN — so .onion sites in regular
    # browsers were silently broken even though Tor's TransPort
    # would have worked. With these two `server=/onion/...` lines,
    # Tor's AutomapHostsOnResolve catches the .onion lookup and
    # returns a virtual IP from 10.192.0.0/10, the iptables REDIRECT
    # chain catches the TCP and routes it through TransPort, and
    # any browser reaches the hidden service transparently.
    install -d -m 0755 /etc/NetworkManager/dnsmasq-shared.d
    # v77: Tor is an INDEPENDENT toggle now (tor.enabled), not a VPN provider.
    # The old gate (vpn.provider=="tor") never matched when Tor runs OVER a real
    # VPN (provider=airvpn + tor.enabled) — the exact Tor-over-VPN case — so
    # client .onion lookups fell through to DNSCrypt and NXDOMAIN'd. Gate on
    # tor.enabled so the .onion → Tor-DNSPort route is written whenever Tor is on.
    if [ "$(toml_get tor enabled false)" = "true" ]; then
        cat > /etc/NetworkManager/dnsmasq-shared.d/00-aeon-dnscrypt.conf <<'EOF'
# Forward all DNS through local dnscrypt-proxy by default.
no-resolv
server=127.0.2.1
# v57: route .onion / .exit straight to Tor's DNSPort. dnscrypt-proxy
# would NXDOMAIN them; Tor's AutomapHostsOnResolve returns a virtual
# IP that the iptables REDIRECT chain routes through TransPort.
server=/onion/127.0.0.1#5353
server=/exit/127.0.0.1#5353
EOF
    else
        cat > /etc/NetworkManager/dnsmasq-shared.d/00-aeon-dnscrypt.conf <<'EOF'
# Forward all DNS through local dnscrypt-proxy.
no-resolv
server=127.0.2.1
EOF
    fi
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
    # Match nmcli's connection TYPE strings: ethernet = "802-3-ethernet",
    # Wi-Fi = "802-11-wireless" (NOT "wifi"!). The old /ethernet|wifi/ silently
    # skipped every Wi-Fi connection, so on a Wi-Fi-connected Pi the DNS
    # override never applied and /etc/resolv.conf kept the router's DHCP DNS —
    # a plaintext DNS leak past DNSCrypt. Include "wireless" (+ aliases).
    local uuids; uuids=$(nmcli -t -f UUID,TYPE con show 2>/dev/null \
        | awk -F: '$2 ~ /ethernet|wireless|wifi/ {print $1}')
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

# Sweep iptables rules tagged "aeon-vpn" (a legacy tag carried over from
# when Tor was itself a VPN-provider option). Tor + I2P rules use the
# "aeon-tor" / "aeon-i2p" tags in their own apply paths, so this sweep
# never touches them. IMPORTANT: this does NOT stop any VPN service — it
# only removes stale iptables rules. Callers that also want the clearnet
# VPN torn down must do that separately (see stop_clearnet_vpns).
sweep_aeon_vpn_rules() {
    for table in filter nat mangle; do
        for chain in OUTPUT INPUT FORWARD PREROUTING POSTROUTING; do
            local lines
            lines=$(iptables -t "$table" -L "$chain" --line-numbers -n 2>/dev/null \
                | awk '/aeon-vpn/{print $1}' | sort -rn)
            for n in $lines; do
                iptables -t "$table" -D "$chain" "$n" 2>/dev/null || true
            done
        done
    done
}

stop_clearnet_vpns() {
    # v58: tighter version of stop_all_vpns that leaves tor + i2pd
    # alone. Called by apply_vpn so toggling a clearnet provider
    # doesn't bounce the independent Tor/I2P services.
    #
    # v80: also leaves Tailscale alone. Tailscale is an independent
    # top-level toggle now — `apply_tailscale` owns its lifecycle via
    # `tailscale up --reset` / `tailscale down`. Tearing the mesh down
    # here meant every VPN apply/switch dropped the (now-independent)
    # tailnet, severing remote management. The `tailscale down` line
    # was removed for exactly that reason.
    systemctl stop wg-quick@aeon0.service 2>/dev/null || true
    systemctl disable wg-quick@aeon0.service 2>/dev/null || true
    systemctl stop openvpn-client@aeon.service 2>/dev/null || true
    systemctl disable openvpn-client@aeon.service 2>/dev/null || true
    stop_airvpn_wrappers 2>/dev/null || true
    sweep_aeon_vpn_rules
}

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
    # v80: Tailscale is NOT torn down here anymore — it's an independent
    # top-level toggle (like tor/i2p above), owned by apply_tailscale via
    # `tailscale up --reset` / `tailscale down`. The old `tailscale down`
    # in this VPN-teardown path dropped the mesh on every VPN switch,
    # cutting remote management. apply_tailscale handles disable itself.
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

# v80: independent Tailscale toggle. Was apply_vpn_tailscale (dispatched
# from apply_vpn's provider case); now gated on its own top-level
# [tailscale] enabled flag and called directly from main(), so the mesh
# runs ALONGSIDE any clearnet VPN instead of being mutually exclusive
# with it. Reads section `tailscale` (not `vpn.tailscale`). Owns its full
# lifecycle: `tailscale up --reset` when enabled, `tailscale down` when
# disabled — nothing else tears the mesh down anymore.
# v92: Pin the tailnet CGNAT range to tailscale0 in the MAIN table so MESH
# traffic (100.64.0.0/10) ALWAYS routes direct over tailscale0 and is NEVER
# captured by a full-tunnel VPN (OpenVPN redirect-gateway's 0.0.0.0/1 routes, or
# a WireGuard default) — and is RESTORED after the DNSCrypt NM-reactivation
# flushes tailscaled's table 52 (the bug: with OpenVPN up, a tailnet IP fell
# through to `dev tun0` and timed out). /10 beats the VPN's /1-or-default by
# longest-prefix-match. Called from apply_tailscale AND as the last step of
# main() (after every VPN bounce + NM churn).
ensure_tailscale_mesh_route() {
    [ "$(toml_get tailscale enabled false)" = "true" ] || return 0
    ip link show tailscale0 >/dev/null 2>&1 || return 0
    ip route replace 100.64.0.0/10 dev tailscale0 2>/dev/null \
        && log "tailscale: pinned mesh route 100.64.0.0/10 -> tailscale0 (direct, bypasses VPN)"
    ip -6 route replace fd7a:115c:a1e0::/48 dev tailscale0 2>/dev/null || true
}

apply_tailscale() {
    local enabled="$(toml_get tailscale enabled false)"
    local auth_key="$(toml_get tailscale auth_key '')"
    local hostname="$(toml_get tailscale hostname '')"
    local exit_node="$(toml_get tailscale exit_node false)"
    local advertise_exit="$(toml_get tailscale advertise_exit_node false)"

    log "tailscale: enabled=$enabled advertise_exit=$advertise_exit exit_node=$exit_node"

    if [ ! -x /usr/bin/tailscale ]; then
        log "tailscale binary not present — skipping"
        return 0
    fi

    if [ "$enabled" != "true" ]; then
        # Bring the mesh down but leave tailscaled (the gateway daemon)
        # running — `tailscale down` just drops the tailnet IP. Phase-2
        # exit routing is torn down separately by apply_tailscale_exit_
        # routing (which no-ops its teardown when the toggle is off).
        /usr/bin/tailscale down 2>/dev/null || true
        log "tailscale disabled — tailscale down (daemon left running)"
        return 0
    fi

    systemctl enable --now tailscaled.service 2>/dev/null || true

    # Split-tunnel mesh ONLY. --accept-dns=false: do NOT overwrite
    # /etc/resolv.conf with MagicDNS (100.100.100.100) — that collides with
    # DNSCrypt / the normal resolver and breaks name resolution (looks like the
    # whole network died). --accept-routes=false: only tailnet addresses
    # (100.64.0.0/10) ride tailscale0; ALL other traffic stays on the existing
    # upstream (none / VPN / VPN+Tor). The host default route + DNS are never
    # hijacked — the mesh is purely additive.
    local args=("--reset" "--accept-dns=false" "--accept-routes=false")
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
    ensure_tailscale_mesh_route
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

# v59: provider-managed WireGuard. Reads the per-provider state file
# under /etc/aeon/vpn-secrets/ that the supervisor's setup wizard
# wrote, renders the wg-quick config, and brings up wg-quick@aeon0.
# Same downstream pipeline as apply_vpn_wireguard — just a different
# config source. The provider arg picks which state file to read.
apply_vpn_provider_wg() {
    local provider="$1"
    local secrets="/etc/aeon/vpn-secrets/${provider}.toml"
    if [ ! -f "$secrets" ]; then
        log "vpn provider '$provider' selected but no config yet — finish setup wizard at /network/vpn/providers/$provider"
        return 0
    fi
    local rendered
    rendered=$(python3 -c "
import tomllib, sys
try:
    with open('$secrets','rb') as f:
        s = tomllib.load(f)
    sel = s.get('selected_server','')
    if not sel:
        print('NO_SELECTED_SERVER', file=sys.stderr)
        sys.exit(2)
    server = next((sv for sv in s.get('servers', []) if sv.get('id') == sel), None)
    if not server:
        print(f'server {sel!r} not in cache', file=sys.stderr)
        sys.exit(3)

    # Per-provider MTU only. Mullvad uses 1380; IVPN and others use 1420.
    #
    # There is intentionally NO DNS line here. wg-quick applies a DNS
    # directive by shelling out to resolvconf, which is ABSENT on Pi OS
    # (no resolvconf/openresolv package, systemd-resolved inactive, and
    # NetworkManager owns a plain /etc/resolv.conf). With a DNS line
    # present, wg-quick aborts mid-bringup with a resolvconf-not-found
    # error (exit 127) and the tunnel never comes up. DNS privacy comes
    # instead from the device DNSCrypt layer at 127.0.2.1, whose encrypted
    # upstream queries ride through this tunnel (AllowedIPs 0.0.0.0/0), so
    # there is no DNS leak. Mirrors the supervisor ivpn.rs/mullvad.rs note.
    # NOTE: this entire block sits inside a bash double-quoted python3 -c
    # string, so it must never contain a double-quote, backtick, dollar or
    # backslash character (any of those would break out of the bash string).
    provider = '$provider'
    if provider == 'mullvad':
        mtu = '1380'
    elif provider == 'airvpn':
        # AirVPN recommends 1320; the value the user pasted is in the toml.
        mtu = str(s.get('mtu', 1320))
    else:
        mtu = '1420'

    # Strip any CIDR suffix the provider may have stored. Mullvad keeps
    # x.x.x.x/32 from register_device while IVPN already strips it; we re-add
    # /32 and /128 ourselves, so without this the address gets a doubled
    # suffix like x.x.x.x/32/32 and wg-quick aborts on the inet prefix.
    # NOTE: this block is inside a python3 -c bash double-quoted string, so it
    # must never contain a double-quote, backtick, dollar or backslash char.
    ipv4 = s.get('peer_ipv4','').split('/')[0]
    ipv6 = s.get('peer_ipv6','').split('/')[0]
    addr = f'{ipv4}/32'
    if ipv6:
        addr += f', {ipv6}/128'

    # Optional WireGuard PresharedKey (AirVPN Config-Generator files may
    # include one). Built with chr(10) for the trailing newline so this
    # stays free of the backslash-n escape the bash-string note forbids.
    psk = s.get('wg_preshared_key','')
    if psk:
        psk_line = 'PresharedKey = ' + psk + chr(10)
    else:
        psk_line = ''

    cfg = f'''# Managed by aeon-net-services (provider={provider}).
# No DNS= line on purpose — Pi OS has no resolvconf; DNSCrypt handles DNS.
[Interface]
PrivateKey = {s.get('wg_private_key','')}
Address    = {addr}
MTU        = {mtu}

[Peer]
PublicKey  = {server.get('public_key','')}
{psk_line}AllowedIPs = 0.0.0.0/0, ::/0
Endpoint   = {server.get('endpoint_ip','')}:{server.get('endpoint_port', 51820)}
PersistentKeepalive = 25
'''
    print(cfg, end='')
except SystemExit:
    raise
except Exception as e:
    print(f'render error: {e}', file=sys.stderr)
    sys.exit(1)
" 2>&1)
    local rc=$?
    if [ "$rc" -ne 0 ] || [ -z "$rendered" ]; then
        log "WARN: failed to render WG config for $provider: $rendered"
        return 0
    fi
    install -d -m 0700 /etc/wireguard
    printf '%s' "$rendered" > "$WG_CONF"
    chmod 0600 "$WG_CONF"
    # Bring the tunnel up with the freshly-rendered config. We must
    # `restart`, not `enable --now`: --now only *starts* a stopped unit, so
    # if wg-quick@aeon0 is already running (re-apply after picking a new
    # server) it would keep the STALE config, and if a prior attempt left
    # the unit in `failed` state (e.g. the old DNS-line resolvconf abort),
    # --now refuses to start it at all. reset-failed clears that latched
    # state, enable persists across boots, restart reloads the new config.
    systemctl reset-failed wg-quick@aeon0.service 2>/dev/null || true
    systemctl enable wg-quick@aeon0.service 2>&1 | tee -a "$LOG" || true
    systemctl restart wg-quick@aeon0.service 2>&1 | tee -a "$LOG" || true
    if systemctl is-active --quiet wg-quick@aeon0.service; then
        log "wireguard up via wg-quick@aeon0 (provider=$provider)"
    else
        log "WARN: wg-quick@aeon0 did not come up (provider=$provider) — check 'journalctl -u wg-quick@aeon0'"
    fi
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

# ── AirVPN OpenVPN family (plain / over-SSL / over-SSH) ──────────────
# AirVPN's API doesn't mint creds; the user pastes a .ovpn (Config
# Generator) which we store in airvpn.toml. Plain OpenVPN rides the same
# openvpn-client@aeon unit as apply_vpn_openvpn. The two stealth modes
# stand up a local obfuscation wrapper and rewrite the .ovpn so OpenVPN
# connects to 127.0.0.1:<wrapper port> instead of straight to the server:
#   openvpn_ssl — stunnel client wraps the OpenVPN/TCP stream in TLS to
#                 the AirVPN server's SSL entry port (443). On the wire it
#                 is indistinguishable from ordinary HTTPS web traffic.
#   openvpn_ssh — an SSH tunnel carries the OpenVPN/TCP stream to the
#                 server. On the wire it looks like a normal SSH session.
AIRVPN_SECRETS=/etc/aeon/vpn-secrets/airvpn.toml
AIRVPN_PKG_ROOT=/etc/aeon/vpn-secrets/airvpn   # per-mode generator packages
STUNNEL_CONF=/etc/stunnel/aeon-airvpn.conf
AIRVPN_SSH_KEY_RUN=/run/aeon-airvpn-ssh.key    # runtime copy of the package key

airvpn_mode() {
    python3 -c "
import tomllib
try:
    with open('$AIRVPN_SECRETS','rb') as f:
        print(tomllib.load(f).get('mode','wireguard'))
except Exception:
    print('wireguard')
"
}

# Entry IP of the currently-selected AirVPN server (wrapper connect host).
airvpn_selected_ip() {
    python3 -c "
import tomllib
try:
    with open('$AIRVPN_SECRETS','rb') as f:
        s = tomllib.load(f)
    sel = s.get('selected_server','')
    srv = next((x for x in s.get('servers',[]) if x.get('id') == sel), None)
    print(srv.get('endpoint_ip','') if srv else '')
except Exception:
    print('')
"
}

# v77: AirVPN WireGuard from a generator package. The .conf is complete
# (keys + endpoint); we only strip the DNS= line (Pi OS has no resolvconf, so
# wg-quick would abort on it) then hand it to wg-quick@aeon0. reassert_policy_
# routing handles the NM-churn route restore for WG.
apply_vpn_airvpn_wg() {
    local conf; conf="$(ls "$AIRVPN_PKG_ROOT/wireguard"/*.conf 2>/dev/null | head -1)"
    if [ -z "$conf" ]; then
        log "airvpn (wireguard): no generated config — generate one at /network/vpn-providers"
        return 0
    fi
    install -d -m 0700 /etc/wireguard
    grep -ivE '^[[:space:]]*DNS[[:space:]]*=' "$conf" > "$WG_CONF"
    chmod 0600 "$WG_CONF"
    systemctl reset-failed wg-quick@aeon0.service 2>/dev/null || true
    systemctl enable wg-quick@aeon0.service 2>&1 | tee -a "$LOG" || true
    systemctl restart wg-quick@aeon0.service 2>&1 | tee -a "$LOG" || true
    if systemctl is-active --quiet wg-quick@aeon0.service; then
        log "airvpn wireguard up via wg-quick@aeon0 (generated config)"
    else
        log "WARN: wg-quick@aeon0 did not come up (airvpn wireguard) — check 'journalctl -u wg-quick@aeon0'"
    fi
}

stop_airvpn_wrappers() {
    # We run both wrappers as our own transient systemd units (not Debian's
    # all-configs stunnel4.service, which defaults to ENABLED=0 and has no
    # per-instance template) so teardown is just stop + reset-failed.
    for u in aeon-airvpn-stunnel aeon-airvpn-ssh; do
        systemctl stop "${u}.service" 2>/dev/null || true
        systemctl reset-failed "${u}.service" 2>/dev/null || true
    done
    rm -f "$STUNNEL_CONF" "$AIRVPN_SSH_KEY_RUN" 2>/dev/null || true
}

apply_vpn_airvpn_openvpn() {
    local mode="$1"
    local dir="$AIRVPN_PKG_ROOT/$mode"
    local ovpn; ovpn="$(ls "$dir"/*.ovpn 2>/dev/null | head -1)"
    if [ -z "$ovpn" ]; then
        log "airvpn ($mode): no generated config — generate one at /network/vpn-providers"
        return 0
    fi
    stop_airvpn_wrappers
    install -d -m 0755 /etc/openvpn/client
    # AirVPN's generated .ovpn is self-contained and (for SSL/SSH) already
    # points at 127.0.0.1:<port> matching the wrapper below — run it verbatim.
    cp -f "$ovpn" "$OVPN_CONF"
    # DNS ownership: when DNSCrypt is active it owns /etc/resolv.conf
    # (127.0.2.1 — encrypted + tunneled), so strip OpenVPN's resolv.conf
    # management (AirVPN ships `up/down update-resolv-conf`) to stop it
    # clobbering DNSCrypt + leaking the pushed DNS. When DNSCrypt is OFF we
    # leave it, so the VPN's own pushed DNS is used (the sane fallback).
    if [ "$(toml_get dnscrypt enabled false)" = "true" ]; then
        sed -i -E '/^[[:space:]]*(up|down)[[:space:]].*update-resolv/d; /^[[:space:]]*script-security[[:space:]]/d' "$OVPN_CONF"
        printf '\n# aeon: DNSCrypt owns DNS — ignore any pushed resolver\npull-filter ignore "dhcp-option DNS"\n' >> "$OVPN_CONF"
    fi
    chmod 0600 "$OVPN_CONF"

    case "$mode" in
        openvpn_ssl)
            local stunnel_bin sslf crtf
            stunnel_bin="$(command -v stunnel4 || command -v stunnel || true)"
            sslf="$(ls "$dir"/*.ssl 2>/dev/null | head -1)"
            crtf="$(ls "$dir"/*.crt 2>/dev/null | head -1)"
            if [ -z "$stunnel_bin" ] || [ -z "$sslf" ]; then
                log "airvpn (ssl): missing stunnel binary or .ssl config — cannot start SSL stealth"; return 0
            fi
            install -d -m 0755 /etc/stunnel
            [ -n "$crtf" ] && cp -f "$crtf" /etc/stunnel/aeon-airvpn-ca.crt
            # Run AirVPN's own stunnel client config (real SSL endpoint +
            # verify=3) verbatim, but force foreground (systemd supervises) and
            # rewrite the relative CAfile to our absolute copy.
            {
                echo "foreground = yes"
                grep -ivE '^[[:space:]]*(CAfile|foreground|pid)[[:space:]]*=' "$sslf"
                echo "CAfile = /etc/stunnel/aeon-airvpn-ca.crt"
            } > "$STUNNEL_CONF"
            chmod 0644 "$STUNNEL_CONF"
            systemctl reset-failed aeon-airvpn-stunnel.service 2>/dev/null || true
            systemd-run --unit=aeon-airvpn-stunnel --collect \
                -p Restart=always -p RestartSec=5 \
                "$stunnel_bin" "$STUNNEL_CONF" 2>&1 | tee -a "$LOG" || true
            log "airvpn ssl: stunnel up from AirVPN config ($(basename "$sslf")) — looks like HTTPS"
            ;;
        openvpn_ssh)
            local keyf shf sshline lfwd userhost sshport
            keyf="$(ls "$dir"/*.key 2>/dev/null | head -1)"
            shf="$(ls "$dir"/*.sh 2>/dev/null | head -1)"
            if ! command -v autossh >/dev/null 2>&1 || [ -z "$keyf" ] || [ -z "$shf" ]; then
                log "airvpn (ssh): missing autossh / sshtunnel.key / launcher — cannot start SSH stealth"; return 0
            fi
            # Parse AirVPN's launcher for the exact ssh forward + endpoint:
            #   ssh -i sshtunnel.key -L <local>:127.0.0.1:<remote> sshtunnel@<ip> -p <port> -N -T
            sshline="$(grep -E '^[[:space:]]*ssh ' "$shf" | head -1)"
            lfwd="$(echo "$sshline" | grep -oE '\-L [0-9.:]+' | awk '{print $2}')"
            userhost="$(echo "$sshline" | grep -oE '[A-Za-z0-9._-]+@[0-9.]+' | head -1)"
            sshport="$(echo "$sshline" | grep -oE '\-p [0-9]+' | awk '{print $2}')"
            [ -z "$sshport" ] && sshport=22
            if [ -z "$lfwd" ] || [ -z "$userhost" ]; then
                log "airvpn (ssh): could not parse launcher ($(basename "$shf")) — leaving down"; return 0
            fi
            install -m 0600 "$keyf" "$AIRVPN_SSH_KEY_RUN"
            # autossh keeps the tunnel up; -M 0 (no monitor port, rely on
            # ServerAlive). The forward + endpoint come straight from AirVPN.
            systemctl reset-failed aeon-airvpn-ssh.service 2>/dev/null || true
            systemd-run --unit=aeon-airvpn-ssh --collect \
                -p Restart=always -p RestartSec=5 -E AUTOSSH_GATETIME=0 \
                /usr/bin/autossh -M 0 -N -T \
                -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
                -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
                -i "$AIRVPN_SSH_KEY_RUN" -p "$sshport" -L "$lfwd" "$userhost" 2>&1 | tee -a "$LOG" || true
            log "airvpn ssh: tunnel up ($userhost:$sshport, fwd $lfwd) — looks like SSH"
            ;;
        *)
            : # plain openvpn — AirVPN's .ovpn is self-contained (direct TCP 443)
            ;;
    esac

    systemctl reset-failed openvpn-client@aeon.service 2>/dev/null || true
    systemctl enable openvpn-client@aeon.service 2>&1 | tee -a "$LOG" || true
    systemctl restart openvpn-client@aeon.service 2>&1 | tee -a "$LOG" || true
    if systemctl is-active --quiet openvpn-client@aeon.service; then
        log "airvpn openvpn up via openvpn-client@aeon (mode=$mode)"
    else
        log "WARN: openvpn-client@aeon did not come up (mode=$mode) — check 'journalctl -u openvpn-client@aeon'"
    fi
}

apply_tor_service() {
    # Bridge preset selection — see apply_tor_bridges below.
    local preset; preset="$(toml_get tor preset direct)"
    # Custom bridges (only used if preset=custom): multi-line via Python read.
    local bridges="$(python3 -c "
import tomllib
try:
    with open('$NETTOML','rb') as f:
        d = tomllib.load(f)
    print(d.get('tor',{}).get('bridges',''))
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
# Map .onion into 10.192.0.0/10 so the virtual IP matches the iptables
# REDIRECT range below. Tor's DEFAULT is 127.192.0.0/10 — but 127.x is
# loopback (unroutable from USB clients) AND doesn't match our redirect, so
# .onion would resolve to a dead IP and never connect. This is the key line
# that makes transparent/split .onion routing actually work.
VirtualAddrNetworkIPv4 10.192.0.0/10
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
    # v90: same extra bindings on the tailscale0 IP when this Pi advertises
    # a Tailscale exit node — so the PREROUTING REDIRECT for forwarded
    # exit-client traffic (added in the usb0/tailscale0 iptables block
    # below) lands on a Tor listener. Exit traffic then inherits Tor just
    # like the usb0 LAN clients do. Same reasoning as the usb0 binding:
    # REDIRECT targets the inbound iface's own IP, so Tor must listen there.
    local ts_ip_for_tor=""
    if [ "$(toml_get tailscale enabled false)" = "true" ] \
        && [ "$(toml_get tailscale advertise_exit_node false)" = "true" ]; then
        ts_ip_for_tor="$(/usr/bin/tailscale ip -4 2>/dev/null | head -1)"
        if [ -n "$ts_ip_for_tor" ]; then
            cat >> /etc/tor/torrc.d/aeon.conf <<EOF
# v90: tailscale0 exit-client bindings (this Pi is a tailnet exit node).
TransPort ${ts_ip_for_tor}:9040 IsolateClientAddr IsolateDestPort IsolateDestAddr
DNSPort ${ts_ip_for_tor}:5353
EOF
        fi
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
    local exit_country; exit_country="$(toml_get tor exit_country '')"
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
    # 2b. v58.1: i2pd's outbound traffic exempt too — when both Tor
    # and I2P are on in this transparent mode, i2pd needs to reach
    # the I2P network directly (or via VPN per i2p.over_vpn) instead
    # of being looped through Tor. The user can still pipe I2P
    # over Tor explicitly by configuring tor as i2pd's outproxy, but
    # the default behaviour is independent routing for each overlay.
    if id -u i2pd >/dev/null 2>&1; then
        iptables -t nat -A OUTPUT -m owner --uid-owner i2pd \
            -j RETURN -m comment --comment "aeon-vpn"
    fi
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

        # v61: USB clients' UDP destined for the Pi gateway itself
        # (10.55.0.1) is a Tor-safe LAN-only flow — it never leaves
        # the device, so there's nothing to deanonymize. Allow it
        # before the catch-all REJECT below. This unblocks: target
        # apps using the Pi as a DNS/mDNS/NTP forwarder on non-standard
        # ports, IPMI/management broadcasts a target Windows host emits
        # on connection, and any future on-Pi UDP services.
        iptables -A FORWARD -i usb0 -d "$pi_addr" -p udp \
            -j ACCEPT -m comment --comment "aeon-vpn"

        # v61: subnet + global broadcasts (10.55.0.255, 255.255.255.255)
        # and link-local multicast (224.0.0.0/4) get *silently dropped*
        # before the LOG-and-REJECT pair fires. They're protocol noise
        # — Windows NetBIOS name service, SSDP, mDNS responder, etc.,
        # all expected to fail outside their L2 segment — and used to
        # spam the Security Console with 10-15 identical entries per
        # second. Same deanonymization story as the REJECT below
        # (they're not going to WAN), but no log noise.
        local usb_subnet; usb_subnet="$(toml_get usb_ethernet subnet 10.55.0.0/24)"
        local usb_bcast; usb_bcast="$(python3 -c "
import ipaddress
print(ipaddress.IPv4Network('${usb_subnet}').broadcast_address)
" 2>/dev/null || echo 10.55.0.255)"
        iptables -A FORWARD -i usb0 -p udp -d "$usb_bcast" \
            -j DROP -m comment --comment "aeon-vpn"
        iptables -A FORWARD -i usb0 -p udp -d 255.255.255.255 \
            -j DROP -m comment --comment "aeon-vpn"
        iptables -A FORWARD -i usb0 -p udp -d 224.0.0.0/4 \
            -j DROP -m comment --comment "aeon-vpn"

        # Block remaining forwarded UDP from usb0 clients when Tor
        # is active. REJECT with ICMP port-unreachable rather than DROP
        # — that tells the client *immediately* "this transport is
        # closed" so QUIC/HTTP-3 falls back to TCP HTTPS in
        # milliseconds, instead of waiting out the QUIC handshake
        # timeout (~1 sec per retry, several retries). End user impact:
        # first page load feels normal instead of laggy.
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

    # ── v90: tailscale0 exit-node clients ride Tor too (mirrors usb0) ──
    # When this Pi advertises a Tailscale exit node, forwarded exit-client
    # traffic on tailscale0 gets the SAME transparent-Tor treatment as the
    # usb0 LAN clients: non-DNS TCP → TransPort, DNS → DNSPort, UDP
    # rejected (Tor can't carry it). Emitted right after the usb0 block so
    # chain order stays consistent and the aeon-vpn tag sweeps them with
    # the rest of Tor's rules. ts_ip_for_tor was resolved in the torrc
    # section above (only set when the Pi is an advertised exit node AND
    # tailscale is up), and Tor is now bound on ${ts_ip_for_tor}:9040/:5353
    # so these REDIRECTs land on a live listener.
    #
    # UNLIKE usb0, tailscale0 has no dnsmasq DNAT in front of it, so we
    # REDIRECT client DNS (UDP + TCP /53) explicitly to Tor's DNSPort —
    # otherwise exit-client name resolution would escape Tor.
    if [ -n "$ts_ip_for_tor" ]; then
        # (a) Exempt the Pi's own tailscale0 IP for non-DNS TCP so the web
        # UI / SSH over the tailnet stays reachable (don't proxy it to Tor).
        iptables -t nat -A PREROUTING -i tailscale0 -d "$ts_ip_for_tor" -p tcp \
            ! --dport 53 \
            -j RETURN -m comment --comment "aeon-vpn"
        # (b) DNS → Tor's DNSPort (both transports). MUST precede the bare
        # default route; tailscale0 has no dnsmasq so this is the only DNS
        # interception for exit clients.
        iptables -t nat -A PREROUTING -i tailscale0 -p udp --dport 53 \
            -j REDIRECT --to-ports "$dns_port" -m comment --comment "aeon-vpn"
        iptables -t nat -A PREROUTING -i tailscale0 -p tcp --dport 53 \
            -j REDIRECT --to-ports "$dns_port" -m comment --comment "aeon-vpn"
        # (c) All other client TCP → Tor's TransPort on the tailscale0 IP.
        iptables -t nat -A PREROUTING -i tailscale0 -p tcp \
            ! --dport 53 \
            -j REDIRECT --to-ports "$trans_port" \
            -m comment --comment "aeon-vpn"
        # (d) FORWARD: allow the Tor-safe UDP (DHCP/NTP/mDNS + Pi-local),
        # then REJECT the rest — Tor can't carry UDP, and letting it leak
        # around Tor would deanonymize the exit client.
        iptables -A FORWARD -i tailscale0 -p udp --dport 123  -j ACCEPT -m comment --comment "aeon-vpn"
        iptables -A FORWARD -i tailscale0 -p udp --dport 5353 -j ACCEPT -m comment --comment "aeon-vpn"
        iptables -A FORWARD -i tailscale0 -d "$ts_ip_for_tor" -p udp \
            -j ACCEPT -m comment --comment "aeon-vpn"
        iptables -A FORWARD -i tailscale0 -p udp -d 224.0.0.0/4 \
            -j DROP -m comment --comment "aeon-vpn"
        aeon_block_pair FORWARD "vpn-udp-forward-ts" "aeon-vpn" reject-port \
            -i tailscale0 -p udp
        # (e) INPUT accept for the redirected TCP landing on the tailscale0 IP.
        iptables -A INPUT -i tailscale0 -d "$ts_ip_for_tor" -p tcp --dport "$trans_port" \
            -j ACCEPT -m comment --comment "aeon-vpn"
        iptables -A INPUT -i tailscale0 -d "$ts_ip_for_tor" -p udp --dport "$dns_port" \
            -j ACCEPT -m comment --comment "aeon-vpn"
        iptables -A INPUT -i tailscale0 -d "$ts_ip_for_tor" -p tcp --dport "$dns_port" \
            -j ACCEPT -m comment --comment "aeon-vpn"
        log "tor: tailscale0 exit-client TCP REDIRECTed to ${ts_ip_for_tor}:${trans_port}; DNS → ${ts_ip_for_tor}:${dns_port} (Tor DNSPort); UDP rejected"
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
    # v61: UDP destined for the Pi's own USB subnet (i.e. responses
    # to attached clients) is LAN-only — it can't reach WAN regardless
    # of whether Tor's up — so let it through without the REJECT noise.
    # Covers dnsmasq replies on UDP/53, mDNS responder hits, NTP
    # responses, and any custom on-Pi UDP service exposed to the target.
    if [ "$usb_enabled" = "true" ]; then
        local usb_subnet_out; usb_subnet_out="$(toml_get usb_ethernet subnet 10.55.0.0/24)"
        iptables -A OUTPUT -o usb0 -p udp -d "$usb_subnet_out" \
            -j ACCEPT -m comment --comment "aeon-vpn"
    fi
    # Same idea for Pi-local UDP — REJECT with ICMP port-unreachable
    # so local apps fall back fast instead of timing out.
    aeon_block_pair OUTPUT "vpn-udp-output" "aeon-vpn" reject-port -p udp
    log "tor active — TCP + DNS via tor; DHCP/NTP/mDNS UDP allowed; other UDP dropped"
}

# v58.1: split-tunnel iptables for Tor. Only TCP destined for the
# Tor automap range (10.192.0.0/10) gets REDIRECTed to TransPort.
# Clearnet TCP stays on the default route (or rides the VPN if a
# clearnet VPN is active). Assumes apply_tor_service already ran +
# Tor's TransPort is listening.
#
# DNS handling here is intentionally minimal: we DON'T blanket-
# REDIRECT UDP/53 → 5353 like transparent mode does. Instead the
# dnsmasq drop-in (00-aeon-dnscrypt.conf, written by apply_dnscrypt)
# already routes the `.onion` and `.exit` suffixes to Tor's DNSPort
# via per-domain `server=` lines, so .onion lookups land on 5353
# while clearnet DNS keeps going through dnscrypt-proxy → upstream.
apply_tor_split_tunnel_iptables() {
    log "tor: applying split-tunnel iptables (REDIRECT only TCP dst 10.192.0.0/10)"

    # ── OUTPUT chain: Pi-originating .onion traffic ──
    # 1. lo bypass (loopback always free)
    iptables -t nat -A OUTPUT -o lo -j RETURN -m comment --comment "aeon-vpn"
    # 2. debian-tor own traffic bypass (avoid Tor talking to itself)
    iptables -t nat -A OUTPUT -m owner --uid-owner debian-tor \
        -j RETURN -m comment --comment "aeon-vpn"
    # 3. i2pd traffic bypass — runs independently
    if id -u i2pd >/dev/null 2>&1; then
        iptables -t nat -A OUTPUT -m owner --uid-owner i2pd \
            -j RETURN -m comment --comment "aeon-vpn"
    fi
    # 4. Only TCP destined for Tor's virtual-IP range gets REDIRECTed.
    iptables -t nat -A OUTPUT -p tcp --syn -d 10.192.0.0/10 \
        -j REDIRECT --to-ports 9040 -m comment --comment "aeon-vpn"

    # ── PREROUTING chain: USB-client .onion traffic ──
    local usb_enabled; usb_enabled="$(toml_get usb_ethernet enabled false)"
    if [ "$usb_enabled" = "true" ]; then
        local pi_addr; pi_addr="$(toml_get usb_ethernet pi_addr 10.55.0.1)"
        # Catch TCP from USB clients destined to Tor's virtual-IP
        # range and route it through TransPort listening on usb0.
        iptables -t nat -A PREROUTING -i usb0 -p tcp -d 10.192.0.0/10 \
            -j REDIRECT --to-ports 9040 -m comment --comment "aeon-vpn"
        iptables -A INPUT -i usb0 -d "$pi_addr" -p tcp --dport 9040 \
            -j ACCEPT -m comment --comment "aeon-vpn"
        log "tor split: USB clients can reach .onion via $pi_addr:9040; clearnet stays on default route"
    fi
    log "tor split-tunnel active — .onion via Tor, everything else direct (or via clearnet VPN if up)"
}

# v58.1: detect the active clearnet VPN's interface name. Returns
# empty string if no clearnet VPN is up (in which case over_vpn
# nesting is a no-op and we log a warning).
detect_vpn_iface() {
    local provider="$(toml_get vpn provider none)"
    local enabled="$(toml_get vpn enabled false)"
    if [ "$enabled" != "true" ]; then
        echo ""
        return
    fi
    case "$provider" in
        # v80: 'tailscale' is no longer a VPN provider (it's an
        # independent mesh now), so it never appears here — and it must
        # NOT: the VPN "iface" is the CLEARNET exit that exit-routing
        # tunnels tailnet traffic through, never tailscale0 itself.
        # v74: mullvad + ivpn are the commercial wizard providers — they
        # ride the SAME wg-quick@aeon0 tunnel as a hand-rolled "wireguard"
        # provider. They were missing here, so detect_vpn_iface returned ""
        # for them, apply_tor_over_vpn logged "no clearnet VPN is up" and
        # skipped table 100, and Tor-over-VPN could never route through the
        # tunnel (Tor stalled at bootstrap → transparent mode then
        # blackholed the whole box). All three map to aeon0.
        wireguard|mullvad|ivpn) ip link show aeon0 >/dev/null 2>&1 && echo "aeon0" ;;
        airvpn)
            # WireGuard rides aeon0; the OpenVPN family (incl. SSL/SSH
            # stealth) rides a tun device.
            case "$(airvpn_mode)" in
                openvpn|openvpn_ssl|openvpn_ssh)
                    ip -o link show 2>/dev/null \
                        | awk -F': ' '/tun[0-9]+:/ {print $2}' | head -1 ;;
                *) ip link show aeon0 >/dev/null 2>&1 && echo "aeon0" ;;
            esac
            ;;
        openvpn)
            # OpenVPN's tun device name varies (tun0 / tun1 …).
            # Pick the first tun*  with an IP.
            ip -o link show 2>/dev/null \
                | awk -F': ' '/tun[0-9]+:/ {print $2}' \
                | head -1
            ;;
        *) echo "" ;;
    esac
}

# v58.1: Tor over VPN — fwmark debian-tor's outbound packets +
# policy-route them via the VPN's interface using a dedicated
# routing table. With this on, Tor's circuit handshakes with entry
# guards exit through the VPN tunnel instead of the bare upstream,
# so your ISP sees only VPN traffic + can't tell you're using Tor.
apply_tor_over_vpn() {
    local iface="$(detect_vpn_iface)"
    if [ -z "$iface" ]; then
        log "WARN: tor.over_vpn=true but no clearnet VPN is up — skipping policy routing"
        return 0
    fi
    log "tor: nesting through clearnet VPN ($iface) via fwmark 0x100 + table 100"

    # Mark Tor's outbound packets in the mangle table. Delete-before-add so
    # this is idempotent — reassert_policy_routing calls us a second time
    # after DNSCrypt's NM churn, and we must not stack duplicate MARK rules.
    iptables -t mangle -D OUTPUT -m owner --uid-owner debian-tor \
        -j MARK --set-mark 0x100 -m comment --comment "aeon-vpn" 2>/dev/null || true
    iptables -t mangle -A OUTPUT -m owner --uid-owner debian-tor \
        -j MARK --set-mark 0x100 -m comment --comment "aeon-vpn"

    # Custom routing table — default route via VPN.
    ip route flush table 100 2>/dev/null || true
    ip route add default dev "$iface" table 100 2>/dev/null || true

    # ip rule that hands marked packets to table 100.
    ip rule del fwmark 0x100 table 100 2>/dev/null || true
    ip rule add fwmark 0x100 table 100
}

# v58.1: same idea for i2pd's outbound traffic. fwmark 0x200 so
# the two policies don't collide if both are on.
apply_i2p_over_vpn() {
    local iface="$(detect_vpn_iface)"
    if [ -z "$iface" ]; then
        log "WARN: i2p.over_vpn=true but no clearnet VPN is up — skipping policy routing"
        return 0
    fi
    if ! id -u i2pd >/dev/null 2>&1; then
        log "WARN: i2p.over_vpn=true but i2pd user doesn't exist (i2pd installed?)"
        return 0
    fi
    log "i2p: nesting through clearnet VPN ($iface) via fwmark 0x200 + table 200"

    # Delete-before-add for idempotency (reassert_policy_routing re-runs us).
    iptables -t mangle -D OUTPUT -m owner --uid-owner i2pd \
        -j MARK --set-mark 0x200 -m comment --comment "aeon-vpn" 2>/dev/null || true
    iptables -t mangle -A OUTPUT -m owner --uid-owner i2pd \
        -j MARK --set-mark 0x200 -m comment --comment "aeon-vpn"

    ip route flush table 200 2>/dev/null || true
    ip route add default dev "$iface" table 200 2>/dev/null || true

    ip rule del fwmark 0x200 table 200 2>/dev/null || true
    ip rule add fwmark 0x200 table 200
}

apply_i2p_service() {
    local outproxy="$(toml_get i2p outproxy '')"

    if [ ! -x /usr/sbin/i2pd ] && [ ! -x /usr/bin/i2pd ]; then
        log "i2pd binary not installed — skipping"
        return 0
    fi

    install -d -m 0755 /etc/i2pd

    # v57: bind i2pd's HTTP proxy, SOCKS proxy, and web console on
    # the usb0 IP (when USB networking is up) so USB-connected
    # target machines can actually reach them. The Debian package
    # ships localhost-only by default, which made AEON's I2P mode
    # invisible to anything off the Pi.
    #
    # The Debian /etc/i2pd/i2pd.conf has plain "address = 127.0.0.1"
    # lines under each [section]. We use a Python helper to update
    # the value under the right section header without touching
    # anything else. Pure sed gets confused by section boundaries.
    local pi_addr_for_i2p; pi_addr_for_i2p="$(toml_get usb_ethernet pi_addr 10.55.0.1)"
    local usb_up; usb_up="$(toml_get usb_ethernet enabled false)"
    local bind_addr="127.0.0.1"
    if [ "$usb_up" = "true" ]; then
        bind_addr="$pi_addr_for_i2p"
    fi
    python3 - "$bind_addr" "$outproxy" <<'PYEOF'
import sys, re, configparser
bind_addr = sys.argv[1]
outproxy = sys.argv[2]
path = "/etc/i2pd/i2pd.conf"
try:
    text = open(path).read()
except FileNotFoundError:
    text = ""
# i2pd's config is INI-shaped but uses '=' with spaces and inline
# comments. Use configparser with relaxed settings.
cp = configparser.ConfigParser(interpolation=None, strict=False)
cp.read_string(text)
# Ensure each section exists, then set the address/port. ConfigParser
# preserves whatever else is in there.
for section in ("httpproxy", "socksproxy", "http"):
    if section not in cp:
        cp[section] = {}
    cp[section]["enabled"] = "true"
    cp[section]["address"] = bind_addr
# Apply the user outproxy override if present (it lives in [httpproxy]).
if outproxy:
    cp["httpproxy"]["outproxy"] = outproxy
elif "outproxy" in cp.get("httpproxy", {}):
    # Cleared via empty input — remove the stale value.
    cp["httpproxy"].pop("outproxy", None)
with open(path, "w") as f:
    cp.write(f, space_around_delimiters=True)
PYEOF

    systemctl enable --now i2pd.service 2>&1 | tee -a "$LOG" || true
    log "i2p (i2pd) active — HTTP ${bind_addr}:4444, SOCKS ${bind_addr}:4447, console http://${bind_addr}:7070/"
    if [ "$usb_up" != "true" ]; then
        log "    (USB networking is off — proxies only reachable from the Pi itself)"
    fi
    log "    (apps must opt in by configuring those proxies — not transparently routed)"
}

# ── v90: Tailscale exit-node egress — inherit the NORMAL WAN stack ────
#
# Gated on:
#   tailscale.enabled          = true
#   tailscale.advertise_exit_node = true   (this Pi is an exit node)
#
# CORRECTED MODEL (the old Phase-2 table-400 design was wrong-shaped):
# exit-node traffic FORWARDED in from the tailnet on tailscale0 is NOT
# special-cased onto a dedicated VPN-only routing table. It rides the
# Pi's STANDARD egress and inherits EVERYTHING the box already has —
# VPN, DNSCrypt, DNS filtering, Tor — exactly like the LAN-side usb0
# clients do. No separate routing table, no fwmark, no toggle. The mesh
# itself stays split-tunnel (only 100.64.0.0/10 over tailscale0, via
# --accept-routes=false).
#
# Why this is automatic, not routed:
#   * VPN: exit traffic already follows the Pi's DEFAULT route. When a
#     wg-quick VPN is up, its `not from all fwmark 0xca6c` catch-all rule
#     pulls every non-VPN-sourced packet (incl. forwarded exit traffic)
#     into the tunnel table — so it egresses the VPN with zero extra
#     routing from us. We only add a MASQUERADE safety-net so the
#     tunnelled packets leave with the tunnel's source IP (tailscaled's
#     own NAT may not cover the VPN iface). With no VPN, tailscaled's
#     normal masquerade handles the bare egress and we add nothing.
#   * DNSCrypt: handled in apply_dnscrypt — exit clients' :53 is
#     REDIRECTed to the Pi's encrypted resolver (gated on the exit-node
#     condition, and only when transparent Tor isn't already grabbing
#     DNS). See the "tailscale0 exit-client DNS" block there.
#   * Tor: handled in apply_tor_service alongside the usb0 transparent
#     rules — exit clients' TCP → TransPort, DNS → DNSPort, UDP rejected.
#     Co-located so chain ordering + the aeon-vpn sweep stay consistent.
#   * Kill-switch: handled in apply_kill_switch — the exit FORWARD path
#     is kept open so the kill-switch doesn't black-hole it.
#
# This function owns the VPN MASQUERADE safety-net + the single teardown
# for every aeon-ts-exit-tagged rule (MASQUERADE here, the DNSCrypt
# REDIRECT, and the kill-switch FORWARD accepts). Tor's tailscale0 rules
# carry the aeon-vpn tag and live/die with the rest of Tor's rule-set.
# Everything is idempotent and re-asserted from reassert_policy_routing.
TS_EXIT_TAG=aeon-ts-exit

# Idempotent removal of every aeon-ts-exit-tagged rule. Safe to call when
# nothing is installed. Used by the gate-off path AND as the pre-step of
# a clean re-add. Matches purely on the comment tag, so it sweeps the
# MASQUERADE (apply_tailscale_exit_node), the DNSCrypt REDIRECT
# (apply_dnscrypt), and the kill-switch FORWARD accepts wherever they sit.
teardown_tailscale_exit_node() {
    local table chain lines n
    for table in nat filter mangle; do
        for chain in PREROUTING POSTROUTING FORWARD INPUT OUTPUT; do
            lines=$(iptables -t "$table" -L "$chain" --line-numbers -n 2>/dev/null \
                | awk -v t="$TS_EXIT_TAG" '$0 ~ t {print $1}' | sort -rn)
            for n in $lines; do
                iptables -t "$table" -D "$chain" "$n" 2>/dev/null || true
            done
        done
    done
}

apply_tailscale_exit_node() {
    local enabled adv
    enabled="$(toml_get tailscale enabled false)"
    adv="$(toml_get tailscale advertise_exit_node false)"

    # Gate: only when this Pi is an advertised exit node. Any miss → tear
    # our rules down (so disabling the feature cleans up) and bail. The
    # DNSCrypt/Tor redirects are self-gated in their own functions on the
    # same condition, so this teardown + their re-apply keep everything
    # consistent.
    if [ "$enabled" != "true" ] || [ "$adv" != "true" ]; then
        teardown_tailscale_exit_node
        log "tailscale exit-node: off (enabled=$enabled advertise=$adv) — egress rules removed"
        return 0
    fi

    log "tailscale exit-node: ON — forwarded exit traffic rides the normal WAN egress (inherits VPN/DNSCrypt/Tor); mesh stays split-tunnel"

    # ── VPN MASQUERADE safety-net ──
    # Exit traffic already routes through the VPN via the wg-quick
    # catch-all (no routing table needed). But tailscaled's own SNAT may
    # not rewrite packets that leave on the VPN iface, so the peer would
    # see a 100.64/10 source and drop them. Add a MASQUERADE for the
    # tailnet source on the VPN iface. When NO VPN is up, detect_vpn_iface
    # is empty and tailscaled's normal masquerade covers the bare egress —
    # we add nothing.
    #
    # First sweep any prior aeon-ts-exit POSTROUTING MASQUERADE (it may
    # name a now-stale iface after a VPN switch / teardown), then re-add
    # the fresh one. This keeps re-applies + reassert_policy_routing from
    # stacking duplicates or leaving a rule bound to a dead iface.
    local pn
    for pn in $(iptables -t nat -L POSTROUTING --line-numbers -n 2>/dev/null \
        | awk -v t="$TS_EXIT_TAG" '$0 ~ t {print $1}' | sort -rn); do
        iptables -t nat -D POSTROUTING "$pn" 2>/dev/null || true
    done
    local vpniface; vpniface="$(detect_vpn_iface)"
    if [ -n "$vpniface" ]; then
        iptables -t nat -A POSTROUTING -s 100.64.0.0/10 -o "$vpniface" -j MASQUERADE \
            -m comment --comment "$TS_EXIT_TAG"
        log "tailscale exit-node: MASQUERADE 100.64.0.0/10 -> $vpniface (VPN egress source-NAT safety-net)"
    else
        log "tailscale exit-node: no VPN iface — bare egress, tailscaled handles masquerade"
    fi
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
        wireguard|mullvad|ivpn)
            # v74: mullvad/ivpn ride wg-quick@aeon0 just like "wireguard".
            # They were missing here, so enabling the kill-switch on a
            # commercial provider blocked ALL outbound (including the tunnel
            # itself) — a self-inflicted lockout. All three exit via aeon0.
            iptables -A OUTPUT -o aeon0 -j ACCEPT -m comment --comment "aeon-vpn"
            ;;
        openvpn)
            iptables -A OUTPUT -o tun0 -j ACCEPT -m comment --comment "aeon-vpn"
            iptables -A OUTPUT -o tun1 -j ACCEPT -m comment --comment "aeon-vpn"
            ;;
        airvpn)
            case "$(airvpn_mode)" in
                openvpn|openvpn_ssl|openvpn_ssh)
                    iptables -A OUTPUT -o tun0 -j ACCEPT -m comment --comment "aeon-vpn"
                    iptables -A OUTPUT -o tun1 -j ACCEPT -m comment --comment "aeon-vpn"
                    # The SSL/SSH wrapper dials the AirVPN server over the
                    # bare uplink BEFORE the tunnel exists — without a hole
                    # for that one server IP the kill-switch would block the
                    # very handshake that establishes the tunnel.
                    local av_ip; av_ip="$(airvpn_selected_ip)"
                    if [ -n "$av_ip" ]; then
                        iptables -A OUTPUT -d "$av_ip" -j ACCEPT -m comment --comment "aeon-vpn"
                    fi
                    ;;
                *) iptables -A OUTPUT -o aeon0 -j ACCEPT -m comment --comment "aeon-vpn" ;;
            esac
            ;;
        tor|i2p)
            # For Tor + I2P the transparent-redirect rules already
            # enforce "everything goes through them". Allow connections
            # originated by the proxy daemons themselves.
            iptables -A OUTPUT -m owner --uid-owner debian-tor -j ACCEPT -m comment --comment "aeon-vpn" 2>/dev/null || true
            iptables -A OUTPUT -m owner --uid-owner i2pd -j ACCEPT -m comment --comment "aeon-vpn" 2>/dev/null || true
            ;;
    esac

    # ── Decoupled Tailscale mesh survives the kill-switch ──
    # Tailscale is no longer a VPN provider; it can run ALONGSIDE a commercial
    # VPN, and its mesh is meant to stay DIRECT (not via the VPN). When the mesh
    # is enabled, let its OWN overlay out even under the kill-switch — otherwise
    # enabling the kill-switch severs remote management over Tailscale. This
    # allows only packets into tailscale0 + tailscaled's 0x80000-marked
    # WireGuard underlay, never arbitrary clearnet. (Verify the 0x80000 mark on
    # real hardware — it's Tailscale's documented bypass mark.)
    if [ "$(toml_get tailscale enabled false)" = "true" ]; then
        iptables -A OUTPUT -o tailscale0 -j ACCEPT -m comment --comment "aeon-vpn" 2>/dev/null || true
        iptables -A OUTPUT -m mark --mark 0x80000/0x80000 -j ACCEPT -m comment --comment "aeon-vpn" 2>/dev/null || true
        log "kill-switch: Tailscale mesh enabled — underlay (0x80000) + tailscale0 kept open (mesh stays direct)"
    fi

    # ── v90: keep the Tailscale exit-node FORWARD path open ──
    # When this Pi advertises an exit node, tailnet exit clients reach the
    # WAN via the FORWARD chain (not OUTPUT), and under the corrected model
    # that forwarded flow rides the Pi's normal egress — which, with a VPN
    # kill-switch active, is the VPN iface. The kill-switch's job is "no
    # clearnet leaks while the tunnel owns egress"; it must NOT black-hole
    # that forwarded exit path. So when the Pi is an advertised exit node
    # AND a VPN iface is up, insert a FORWARD accept (+ conntrack return)
    # at the TOP of FORWARD so it beats any stricter FORWARD policy.
    # Tagged aeon-ts-exit so teardown_tailscale_exit_node sweeps it.
    # Idempotent via -C guard. (No VPN iface ⇒ the kill-switch can't be
    # engaged anyway — apply_kill_switch refuses without a provider.)
    local ks_enabled ks_adv ks_iface
    ks_enabled="$(toml_get tailscale enabled false)"
    ks_adv="$(toml_get tailscale advertise_exit_node false)"
    ks_iface="$(detect_vpn_iface)"
    if [ "$ks_enabled" = "true" ] && [ "$ks_adv" = "true" ] && [ -n "$ks_iface" ]; then
        iptables -C FORWARD -i tailscale0 -o "$ks_iface" -j ACCEPT \
            -m comment --comment "$TS_EXIT_TAG" 2>/dev/null \
            || iptables -I FORWARD 1 -i tailscale0 -o "$ks_iface" -j ACCEPT \
                -m comment --comment "$TS_EXIT_TAG"
        iptables -C FORWARD -i "$ks_iface" -o tailscale0 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT \
            -m comment --comment "$TS_EXIT_TAG" 2>/dev/null \
            || iptables -I FORWARD 1 -i "$ks_iface" -o tailscale0 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT \
                -m comment --comment "$TS_EXIT_TAG"
        log "kill-switch: Tailscale exit-node active — FORWARD path tailscale0 -> $ks_iface kept open (exit traffic rides the VPN)"
    fi

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

    # v58: Tor + I2P are no longer "VPN providers" — they're
    # independent toggles handled by apply_tor() / apply_i2p().
    # provider=tor/i2p from a legacy config gets migrated by the
    # supervisor at read time, so we shouldn't see them here. But
    # defend against direct edits anyway.
    if [ "$provider" = "tor" ] || [ "$provider" = "i2p" ]; then
        log "WARN: vpn.provider='$provider' is legacy — treating as 'none'. Use [tor]/[i2p] enabled instead."
        provider="none"
    fi

    # Always start by stopping VPN tunnels (not Tor / I2P — those are
    # independent now and have their own sweep functions).
    stop_clearnet_vpns 2>/dev/null || stop_all_vpns 2>/dev/null || true

    if [ "$enabled" != "true" ] || [ "$provider" = "none" ] || [ -z "$provider" ]; then
        log "vpn (clearnet) disabled — no tunnel"
        return 0
    fi

    case "$provider" in
        # v80: 'tailscale' is gone from this dispatch — it's an
        # independent toggle handled by apply_tailscale() (called from
        # main, gated on [tailscale] enabled). A legacy provider=tailscale
        # is migrated to "none" by the supervisor at read time, so we
        # should never see it here; if a direct TOML edit slips one
        # through it falls to the unknown-provider warning below.
        wireguard)            apply_vpn_wireguard ;;
        openvpn)              apply_vpn_openvpn ;;
        mullvad|ivpn|azirevpn) apply_vpn_provider_wg "$provider" ;;
        airvpn)
            # v77: all modes run from auto-pulled generator packages under
            # /etc/aeon/vpn-secrets/airvpn/<mode>/. WireGuard → wg-quick@aeon0;
            # the OpenVPN family (plain/SSL/SSH) → openvpn-client@aeon (+ stunnel
            # or autossh wrapper). The selected mode lives in airvpn.toml.
            local av_m; av_m="$(airvpn_mode)"
            case "$av_m" in
                openvpn|openvpn_ssl|openvpn_ssh) apply_vpn_airvpn_openvpn "$av_m" ;;
                *)                               apply_vpn_airvpn_wg ;;
            esac
            ;;
        *)                    log "WARN: unknown vpn provider '$provider' — leaving tunnel down" ;;
    esac

    # Kill-switch is layered on TOP of the chosen provider so the rules
    # see the VPN's interface already up.
    apply_kill_switch
}

# v58: independent Tor toggle. Replaces the old apply_vpn_tor() flow
# (which assumed Tor was THE active VPN). Two new behaviours:
#   - mode=split_tunnel — only REDIRECT TCP destined for Tor's
#     virtual-IP range (10.192.0.0/10) to TransPort. Clearnet TCP
#     keeps its normal default-route / VPN path.
#   - over_vpn=true — fwmark debian-tor UID packets, policy-route
#     them via the active VPN tunnel (Tor's entry guards exit
#     through the VPN, hiding "uses Tor" from your ISP).
apply_tor() {
    local enabled="$(toml_get tor enabled false)"
    local mode="$(toml_get tor mode split_tunnel)"
    local over_vpn="$(toml_get tor over_vpn false)"

    log "tor: enabled=$enabled mode=$mode over_vpn=$over_vpn"

    # v61: Sweep prior aeon-vpn-tagged iptables rules unconditionally
    # so we never pile up duplicates. apply_tor_service installs rules
    # tagged "aeon-vpn" (legacy carry-over from when Tor was a VPN
    # provider option). Re-enabling Tor or switching split⇄transparent
    # used to leave the previous rule-set in place, producing 10+ LOG
    # entries per dropped packet (visible in the Security Console as
    # repeated identical drops at the same timestamp). The earlier
    # `grep -v 'aeon-tor'` was a no-op — wrong tag.
    #
    # v74 FIX: this used to call stop_clearnet_vpns here, which ALSO runs
    # `systemctl stop wg-quick@aeon0`. apply_tor runs right after apply_vpn
    # in main(), so it tore down the clearnet VPN that apply_vpn had JUST
    # brought up — even when Tor is disabled. The tunnel flapped up then
    # immediately down and egress fell back to the bare ISP. We only ever
    # wanted the iptables-rule sweep here, never to stop the VPN (Tor-over-
    # VPN nesting in fact REQUIRES the clearnet tunnel to stay up). So
    # sweep the stale aeon-vpn rules only — leave every VPN service alone.
    sweep_aeon_vpn_rules
    iptables -t nat -F AEON_TOR_OUT 2>/dev/null && iptables -t nat -X AEON_TOR_OUT 2>/dev/null || true

    if [ "$enabled" != "true" ]; then
        systemctl stop tor.service tor@default.service 2>/dev/null || true
        systemctl disable tor.service tor@default.service 2>/dev/null || true
        # Clean up policy-routing leftovers from a previous over_vpn run.
        ip rule del fwmark 0x100 table 100 2>/dev/null || true
        ip route flush table 100 2>/dev/null || true
        log "tor disabled — service stopped, ip rules cleared"
        return 0
    fi

    if [ "$mode" = "transparent" ]; then
        # Full transparent redirect — all outbound TCP goes via Tor.
        # apply_tor_service does both torrc + transparent iptables.
        # (Includes i2pd UID exemption so I2P can still reach the
        # I2P network when both are on.)
        apply_tor_service
    else
        # Split tunnel: torrc + minimal iptables for .onion only.
        # Run apply_tor_service to get the torrc and bring tor up,
        # but then SWEEP the transparent iptables it just installed
        # and replace them with the narrow split-tunnel set.
        apply_tor_service
        # Sweep aeon-vpn-tagged rules just installed and reapply only
        # the split-tunnel subset. Wasteful (we do the work twice) but
        # safer than reorganizing apply_tor_service mid-PR — the
        # second pass leaves us in the correct state.
        for table in filter nat mangle; do
            for chain in OUTPUT INPUT FORWARD PREROUTING POSTROUTING; do
                local lines
                lines=$(iptables -t "$table" -L "$chain" --line-numbers -n 2>/dev/null \
                    | awk '/aeon-vpn/{print $1}' | sort -rn)
                for n in $lines; do
                    iptables -t "$table" -D "$chain" "$n" 2>/dev/null || true
                done
            done
        done
        apply_tor_split_tunnel_iptables
    fi

    if [ "$over_vpn" = "true" ]; then
        apply_tor_over_vpn
    fi
}

# v58: independent I2P toggle. Always proxy-based (no transparent
# routing possible — that's by design). Wraps the same i2pd config
# rewriting we had in apply_vpn_i2p, gated on the new top-level
# i2p.enabled flag.
apply_i2p() {
    local enabled="$(toml_get i2p enabled false)"
    local over_vpn="$(toml_get i2p over_vpn false)"
    log "i2p: enabled=$enabled over_vpn=$over_vpn"

    if [ "$enabled" != "true" ]; then
        systemctl stop i2pd.service 2>/dev/null || true
        systemctl disable i2pd.service 2>/dev/null || true
        # Clear any over_vpn policy routing left over.
        ip rule del fwmark 0x200 table 200 2>/dev/null || true
        ip route flush table 200 2>/dev/null || true
        log "i2p disabled — service stopped, ip rules cleared"
        return 0
    fi
    # Reuses the i2pd config rewriter we built in v57.
    apply_i2p_service
    if [ "$over_vpn" = "true" ]; then
        apply_i2p_over_vpn
    fi
}

# v74: Re-assert ALL policy routing after apply_dnscrypt's NM churn.
#
# apply_dnscrypt reactivates every active NetworkManager connection
# (`nmcli con up <uuid>`) so the 127.0.2.1 resolver takes effect on each
# link. That reactivation FLUSHES the custom routing tables we depend on:
#   * wg-quick's tunnel default route (table == its fwmark, e.g. 51820) —
#     the `default dev aeon0` for an AllowedIPs=0.0.0.0/0 tunnel. The ip
#     RULES (suppress_prefixlength / not-fwmark) survive, but with the
#     table's default route gone, traffic falls through to main and leaks
#     out the bare ISP (tunnel still UP + handshaking, yet every packet
#     bypasses it).
#   * Tor's over_vpn table 100 and I2P's over_vpn table 200 (`default dev
#     aeon0`) — without these, Tor/I2P's fwmarked packets have no route
#     through the tunnel, so Tor can never reach its guards and stalls at
#     ~5% (the "All traffic via Tor" lockout: transparent mode then
#     blackholes every TCP connection, including the management plane).
#
# Since apply_dnscrypt is the LAST apply step, we restore everything here,
# AFTER all NM churn. Crucially we re-add routes IN PLACE (ip route
# replace) rather than bouncing wg-quick: an interface bounce would delete
# + recreate aeon0 and invalidate the Tor/I2P tables that reference it.
reassert_policy_routing() {
    local vpn_enabled vpn_provider
    vpn_enabled="$(toml_get vpn enabled false)"
    vpn_provider="$(toml_get vpn provider none)"
    [ "$vpn_enabled" = "true" ] || return 0

    # Pick the transport so we restore the right routing. WireGuard (aeon0 +
    # fwmark table) bounces wg-quick; the OpenVPN family (tun0, routes in the
    # MAIN table) restarts openvpn-client@aeon. AirVPN is either, per its mode.
    local transport=""
    case "$vpn_provider" in
        wireguard|mullvad|ivpn) transport="wg" ;;
        openvpn)                transport="ovpn" ;;
        airvpn)
            case "$(airvpn_mode)" in
                openvpn|openvpn_ssl|openvpn_ssh) transport="ovpn" ;;
                *)                               transport="wg" ;;
            esac
            ;;
        *) return 0 ;;
    esac

    # ── OpenVPN family: re-add the redirect route NM's reactivation flushed ──
    # OpenVPN installs its redirect-gateway routes into the MAIN table; the
    # DNSCrypt step's NM reactivation (earlier in main) wipes them, and unlike
    # WireGuard there is no fwmark table to bounce. Proven on hardware: routing
    # only sticks when openvpn is (re)started AFTER the last NM churn. So if the
    # default-override is gone, restart the client here (reassert runs last).
    if [ "$transport" = "ovpn" ]; then
        systemctl is-active --quiet openvpn-client@aeon.service || return 0
        local tun; tun="$(ip -o link show 2>/dev/null | awk -F': ' '/tun[0-9]+:/{print $2}' | head -1)"
        if [ -z "$tun" ] || ! ip route show 2>/dev/null | grep -qE "^0\.0\.0\.0/1 .* dev ${tun}( |\$)"; then
            log "vpn: OpenVPN redirect route missing (NM reactivation flushed it) — restarting openvpn-client@aeon to reinstate it"
            systemctl restart openvpn-client@aeon.service 2>&1 | tee -a "$LOG" || true
        fi
        return 0
    fi

    # ── WireGuard path ──
    systemctl is-active --quiet wg-quick@aeon0.service || return 0

    # wg-quick numbers its table after the fwmark it set (0xca6c == 51820).
    # Check whether NM's reactivation flushed the tunnel default route from
    # THAT specific table (not "any table" — Tor's table 100 also carries a
    # `default dev aeon0`, which would mask a missing VPN route).
    local fwmark_hex table=0 need_bounce=0
    fwmark_hex="$(wg show aeon0 fwmark 2>/dev/null)"
    case "$fwmark_hex" in 0x*) table=$(( fwmark_hex )) ;; esac
    if [ "$table" -gt 0 ]; then
        ip route show table "$table" 2>/dev/null | grep -q "^default" || need_bounce=1
    else
        ip route show table all 2>/dev/null | grep -q "default dev aeon0" || need_bounce=1
    fi

    if [ "$need_bounce" = "1" ]; then
        # Bounce wg-quick — the PROVEN, reliable restore (re-adds the route,
        # the fwmark, and the not-fwmark / suppress_prefixlength rules in one
        # shot). We deliberately bounce rather than re-add the route alone: a
        # lone `ip route` can lose a race with NM's still-settling
        # reactivation, whereas a full wg-quick restart lands cleanly (this
        # is what worked reliably across reboots before v74). The bounce
        # drops Tor/I2P's table 100/200, but we rebuild those just below.
        log "vpn: tunnel route missing after NM reactivation — bouncing wg-quick@aeon0 to reinstate it"
        systemctl restart wg-quick@aeon0.service 2>&1 | tee -a "$LOG" || true
    fi

    # Re-assert Tor / I2P over_vpn policy routing AFTER any bounce (the
    # bounce recreates aeon0 and drops their tables; NM churn can flush them
    # independently too). Both helpers are idempotent.
    if [ "$(toml_get tor enabled false)" = "true" ] && [ "$(toml_get tor over_vpn false)" = "true" ]; then
        apply_tor_over_vpn
        log "vpn: re-asserted Tor over_vpn policy routing (fwmark 0x100 -> table 100)"
    fi
    if [ "$(toml_get i2p enabled false)" = "true" ] && [ "$(toml_get i2p over_vpn false)" = "true" ]; then
        apply_i2p_over_vpn
        log "vpn: re-asserted I2P over_vpn policy routing (fwmark 0x200 -> table 200)"
    fi
    # v90: re-assert the Tailscale exit-node VPN MASQUERADE safety-net.
    # The wg-quick bounce above recreates aeon0 (a new iface incarnation)
    # and the DNSCrypt NM churn flushes NAT rules, so the
    # 100.64.0.0/10 -> VPN MASQUERADE can vanish. apply_tailscale_exit_node
    # is fully idempotent AND self-gating — it re-adds the MASQUERADE when
    # the Pi is an advertised exit node + a VPN is up, and is a no-op
    # teardown otherwise, so calling it unconditionally here is safe.
    # (The DNSCrypt/Tor tailscale0 redirects are re-asserted by their own
    # functions; apply_dnscrypt runs in the same main() pass and the Tor
    # rules survive the NM churn since they're not in a flushed table.)
    apply_tailscale_exit_node
}

# ──────────────────────────────────────────────────────────────────────
# Main
# ──────────────────────────────────────────────────────────────────────

# v58 order (v80 added Tailscale; v90 reshaped exit-node egress):
#   1. clearnet VPN (wireguard/openvpn/commercial) — establishes
#      tun0/aeon0 default route if active.
#   1b. (v80) Tailscale — independent mesh, brought up alongside any
#      VPN. Must run after apply_vpn so the VPN iface exists, and BEFORE
#      apply_tor/apply_dnscrypt so `tailscale ip -4` is available when
#      they bind extra listeners + emit the tailscale0 exit-client
#      redirects.
#   2. Tor — its iptables rules need the VPN's tunnel interface to
#      exist before tor.over_vpn=true can fwmark traffic through it.
#      apply_tor_service ALSO emits the tailscale0 exit-client transparent
#      redirects (mirrors usb0) when this Pi advertises an exit node.
#   3. I2P — independent; HTTP proxy bind needs usb0 + nothing else.
#   4. DNSCrypt — bootstrap_resolvers may point at Tor's DNSPort
#      when Tor is on, so set up Tor first. ALSO binds on the tailscale0
#      IP + REDIRECTs exit-client DNS to the encrypted resolver.
#   4b. (v90) Tailscale exit-node egress — the VPN MASQUERADE safety-net
#      for forwarded exit traffic. Runs after apply_vpn (needs the VPN
#      iface) and after the DNSCrypt/Tor redirects are already in place.
#      No routing table; exit traffic just inherits the normal egress.
#   5. (v74) re-assert ALL policy routing — DNSCrypt's NM reactivation
#      flushes wg-quick's table AND Tor/I2P over_vpn tables (100/200) +
#      the exit-node MASQUERADE; restore them once all NM churn is done.
apply_vpn
apply_tailscale
apply_tor
apply_i2p
apply_dnscrypt
apply_tailscale_exit_node
reassert_policy_routing
# v92: re-pin the tailnet mesh route LAST — after every VPN bounce + DNSCrypt
# NM-flush — so mesh traffic (100.64/10) always stays direct over tailscale0.
ensure_tailscale_mesh_route
log "aeon-net-services done"
