#!/bin/bash
# Aeon Magick AI Computer Control — WiFi watchdog.
#
# Goal: if NO known WiFi network is reachable for MAX_DOWN_SECONDS, spin up
# a setup AP named `aeon-setup` so the user can connect from a phone or
# laptop and configure WiFi via the web UI.
#
# When known WiFi comes back, drop the AP and rejoin client mode.

set -u
PRIMARY_CON_FILTER="!Eye-Setup,!aeon-setup"
AP_CON="aeon-setup"
AP_IFACE="wlan0"
AP_GATEWAY="192.168.50.1"
MAX_DOWN_SECONDS=90
AP_RETRY_SECONDS=300
STATE_FILE=/var/lib/aeon/netwatch.state
PING_TARGET="1.1.1.1"
DNSMASQ_CAPTIVE=/etc/NetworkManager/dnsmasq-shared.d/01-aeon-captive.conf

mkdir -p "$(dirname "$STATE_FILE")"
[[ -f "$STATE_FILE" ]] || echo "0" > "$STATE_FILE"

log() { logger -t aeon-netwatch -- "$*"; }

ap_active() {
    nmcli -t -f NAME con show --active 2>/dev/null | grep -qx "$AP_CON"
}

# ── Captive-portal hijack ──
#
# When the device is in AP-setup mode, any client connecting to it gets
# captured by these two mechanisms:
#
#   1. dnsmasq drop-in resolves every hostname to the Pi's gateway
#      (192.168.50.1) — `address=/#/...` is dnsmasq's wildcard.
#      Effect: OS probes for captive.apple.com, generate_204, ncsi.txt
#      get answered with the Pi's IP, triggering the captive sheet on
#      iOS/macOS/Android/Windows.
#
#   2. iptables PREROUTING redirects all HTTP/HTTPS from wlan0 (where
#      AP clients live) to the Pi's web UI on port 443. The TLS cert is
#      self-signed for the Pi's IP so the browser shows a warning, but
#      the OS captive sheet handles that gracefully by accepting it.
#
# Both pieces are tagged "aeon-captive" so apply_off can sweep them
# precisely.

apply_captive_portal_hijack() {
    log "AP-up: enabling captive portal hijack"
    # v67: signal the supervisor's port-80 captive listener that we ARE
    # in AP-setup mode. Without this flag the listener upgrades http→https
    # for the requested host (normal client behaviour); with it, the
    # listener runs the OS-probe captive flow + redirects to /setup/wifi.
    install -d -m 0755 /run/aeon
    : > /run/aeon/ap-mode
    # dnsmasq wildcard
    install -d -m 0755 /etc/NetworkManager/dnsmasq-shared.d
    cat > "$DNSMASQ_CAPTIVE" <<EOF
# Managed by aeon-netwatch — auto-removed when AP goes down.
# Resolve every hostname to the Pi's gateway IP, capturing OS
# captive-portal probes and triggering the popup on client devices.
address=/#/$AP_GATEWAY
EOF
    # Force the shared dnsmasq to re-read its drop-ins.
    nmcli con down "$AP_CON" >/dev/null 2>&1 || true
    nmcli con up "$AP_CON" >/dev/null 2>&1 || true

    # iptables: redirect TCP 80 + 443 from AP clients to the Pi web UI.
    # 80 is handled by a small HTTP listener inside the supervisor (it
    # responds to OS probe URLs with the right body to trigger the
    # captive popup, and redirects everything else to /setup/wifi).
    # 443 hits the supervisor's main HTTPS listener directly.
    iptables -t nat -A PREROUTING -i "$AP_IFACE" -p tcp --dport 80 \
        -j DNAT --to-destination "$AP_GATEWAY:80" \
        -m comment --comment "aeon-captive"
    iptables -t nat -A PREROUTING -i "$AP_IFACE" -p tcp --dport 443 \
        -j DNAT --to-destination "$AP_GATEWAY:443" \
        -m comment --comment "aeon-captive"
}

remove_captive_portal_hijack() {
    log "AP-down: removing captive portal hijack"
    # v67: clear the AP-mode flag so the port-80 listener reverts to
    # plain http→https upgrade (so a reconnected client hitting
    # http://<pi> reaches the console, not the setup page).
    rm -f /run/aeon/ap-mode
    rm -f "$DNSMASQ_CAPTIVE"
    # Sweep our captive iptables rules — line-number based for reliability.
    for chain in PREROUTING OUTPUT INPUT FORWARD; do
        local lines
        lines=$(iptables -t nat -L "$chain" --line-numbers -n 2>/dev/null \
            | awk '/aeon-captive/{print $1}' | sort -rn)
        for n in $lines; do
            iptables -t nat -D "$chain" "$n" 2>/dev/null || true
        done
    done
}

primary_active() {
    # NetworkManager reports types as "802-11-wireless" / "802-3-ethernet" /
    # "vpn" / "tun" / "loopback" / "bridge" / "ovs-*" / "tailscale" etc.,
    # NOT bare "wifi"/"ethernet". The original regex `:(wifi|ethernet)$`
    # never matched a real NM listing, so `primary_active` always returned
    # false and the AP-fallback kept firing even with eth0 connected. Fix:
    # match the actual NM type strings, and accept tailscale as a primary
    # (so a Pi reachable only over tailnet doesn't drop to AP mode).
    nmcli -t -f NAME,TYPE con show --active 2>/dev/null \
        | grep -E ":(802-11-wireless|802-3-ethernet|tun|wireguard|tailscale)$" \
        | grep -v "^${AP_CON}:" \
        | grep -q .
}

have_internet() {
    primary_active || return 1
    ping -c 1 -W 2 "$PING_TARGET" >/dev/null 2>&1
}

now=$(date +%s)
down_since=$(<"$STATE_FILE")

if have_internet; then
    if [[ "$down_since" != "0" ]]; then
        log "internet restored after $((now - down_since))s"
    fi
    echo "0" > "$STATE_FILE"
    if ap_active; then
        log "primary online, deactivating AP"
        nmcli con down "$AP_CON" >/dev/null 2>&1 || true
        remove_captive_portal_hijack
    else
        # Defensive: if AP is down for any reason, also sweep any
        # lingering captive rules (e.g. after a reboot mid-AP-session).
        [[ -f "$DNSMASQ_CAPTIVE" ]] && remove_captive_portal_hijack
    fi
    exit 0
fi

# ── offline ──

if ap_active; then
    if (( now - down_since >= AP_RETRY_SECONDS )); then
        log "AP up for $((now - down_since))s, retrying primary"
        nmcli con down "$AP_CON" >/dev/null 2>&1 || true
        sleep 5
        nmcli device wifi rescan >/dev/null 2>&1 || true
        sleep 8
        if have_internet; then
            log "back online"
            echo "0" > "$STATE_FILE"
        else
            log "still offline, resuming AP"
            nmcli con up "$AP_CON" >/dev/null 2>&1 || true
            echo "$now" > "$STATE_FILE"
        fi
    fi
    exit 0
fi

# Not in AP, not online — start countdown.
if [[ "$down_since" == "0" ]]; then
    echo "$now" > "$STATE_FILE"
    log "no internet, starting ${MAX_DOWN_SECONDS}s grace timer"
    exit 0
fi

if (( now - down_since >= MAX_DOWN_SECONDS )); then
    log "offline $((now - down_since))s, activating $AP_CON"
    # Ensure the AP profile exists with default SSID/PSK on first creation.
    if ! nmcli con show "$AP_CON" >/dev/null 2>&1; then
        nmcli con add type wifi con-name "$AP_CON" ifname wlan0 ssid "$AP_CON" \
            mode ap autoconnect no >/dev/null 2>&1
        nmcli con mod "$AP_CON" \
            wifi-sec.key-mgmt wpa-psk wifi-sec.psk "aeon-setup-pw" >/dev/null 2>&1
    fi
    # v67: ALWAYS enforce the AP network config before bringing it up.
    # The /wifi page's AP form (wifi.rs ap_set) also writes this profile,
    # and an earlier version created it on a different subnet (10.42.0.1)
    # which left the captive DNAT + the supervisor's setup redirect
    # (both hardcoded to AP_GATEWAY=192.168.50.1) pointing at an
    # unreachable address — the AP came up but the setup page was dead.
    # Re-asserting these fields every activation makes netwatch
    # authoritative for the fallback AP's L3 config regardless of who
    # created the profile, while PRESERVING any custom SSID/PSK the user
    # set via /wifi (we don't touch wifi-sec/ssid here).
    nmcli con mod "$AP_CON" \
        802-11-wireless.mode ap \
        ipv4.method shared "ipv4.addresses" "${AP_GATEWAY}/24" \
        ipv6.method disabled \
        wifi.band bg wifi.channel 6 >/dev/null 2>&1
    # Capture NM's actual error so we don't silently keep retrying — the
    # journal will now show *why* AP activation failed (regdomain, wpa
    # mode, iface busy, etc.).
    if ! AP_ERR=$(nmcli con up "$AP_CON" 2>&1); then
        log "AP activation failed: ${AP_ERR}"
    else
        # AP is up — enable the captive portal hijack so clients that
        # connect immediately see the setup page.
        apply_captive_portal_hijack
    fi
    echo "$now" > "$STATE_FILE"
fi
