#!/bin/bash
# Aeon Cursed KVM — WiFi watchdog.
#
# Goal: if NO known WiFi network is reachable for MAX_DOWN_SECONDS, spin up
# a setup AP named `aeon-setup` so the user can connect from a phone or
# laptop and configure WiFi via the web UI.
#
# When known WiFi comes back, drop the AP and rejoin client mode.

set -u
PRIMARY_CON_FILTER="!Eye-Setup,!aeon-setup"
AP_CON="aeon-setup"
MAX_DOWN_SECONDS=90
AP_RETRY_SECONDS=300
STATE_FILE=/var/lib/acursed/netwatch.state
PING_TARGET="1.1.1.1"

mkdir -p "$(dirname "$STATE_FILE")"
[[ -f "$STATE_FILE" ]] || echo "0" > "$STATE_FILE"

log() { logger -t acursed-netwatch -- "$*"; }

ap_active() {
    nmcli -t -f NAME con show --active 2>/dev/null | grep -qx "$AP_CON"
}

primary_active() {
    nmcli -t -f NAME,TYPE con show --active 2>/dev/null \
        | grep -E ":(wifi|ethernet)$" \
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
    # Ensure the AP profile exists.
    if ! nmcli con show "$AP_CON" >/dev/null 2>&1; then
        nmcli con add type wifi con-name "$AP_CON" ifname wlan0 ssid "$AP_CON" \
            mode ap autoconnect no >/dev/null 2>&1
        nmcli con mod "$AP_CON" \
            wifi-sec.key-mgmt wpa-psk wifi-sec.psk "aeon-setup-pw" \
            ipv4.method shared "ipv4.addresses" "192.168.50.1/24" \
            ipv6.method disabled \
            wifi.band bg wifi.channel 6 >/dev/null 2>&1
    fi
    nmcli con up "$AP_CON" >/dev/null 2>&1 || log "AP activation failed"
    echo "$now" > "$STATE_FILE"
fi
