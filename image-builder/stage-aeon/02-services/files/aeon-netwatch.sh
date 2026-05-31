#!/bin/bash
# Aeon Magick AI Computer Control — WiFi watchdog.
#
# Goal: keep the device reachable for setup.
#   * A never-configured device (no saved WiFi) brings up the setup AP
#     `aeon-setup` on the first tick (~60s after boot) so first-time setup
#     is immediate — not after a 2.5-minute grace timer.
#   * A configured device that loses its known WiFi for MAX_DOWN_SECONDS
#     falls back to the same AP (the grace period keeps brief outages /
#     roams from flapping it).
# Connect from a phone/laptop and configure WiFi via the web UI. When known
# WiFi comes back, the AP drops and client mode resumes.
#
# AP activation is robust (radio unblock + regdomain + 2.4GHz channel
# fallback + last-resort profile recreate) and writes its status to
# /boot/firmware/aeon-ap-status.txt — so "the setup AP never appeared" is
# diagnosable by pulling the SD card into any computer, no network needed.

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
# Out-of-band AP status. AP_STATUS_BOOT lives on the FAT /boot/firmware
# partition so "why didn't the setup AP appear?" is answerable by pulling
# the SD card into any computer — no network, SSH, or console needed.
AP_STATUS_BOOT=/boot/firmware/aeon-ap-status.txt
AP_STATUS_LOG=/var/lib/aeon/ap-status.log
# Drop this empty file on the FAT boot partition to force setup-AP mode.
FORCE_AP_FLAG=/boot/firmware/aeon-force-ap

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

# ── AP activation helpers (v70) ──

write_status() {
    # $1 = state (OK|RETRY|FAIL), $2 = human detail. Mirrors to syslog, a
    # rolling log under /var, and a single-file snapshot on the FAT boot
    # partition (the out-of-band diagnostic channel — readable by pulling
    # the SD card, no network/console/SSH required).
    local ts line
    ts=$(date -Iseconds)
    line="${ts} [$1] $2"
    log "$line"
    echo "$line" >> "$AP_STATUS_LOG" 2>/dev/null || true
    {
        echo "Aeon Magick — setup AP (SSID: ${AP_CON}) status"
        echo "Updated: ${ts}"
        echo "State:   $1"
        echo "Detail:  $2"
        echo
        echo "OK   = setup AP is broadcasting at ${AP_GATEWAY}."
        echo "FAIL = NetworkManager could not start the AP; Detail carries the"
        echo "       exact error + radio/rfkill/regdomain state. Safe to delete."
    } > "$AP_STATUS_BOOT" 2>/dev/null || true
}

radio_kick() {
    # Defeat the common reasons AP activation silently fails on a fresh
    # Pi 4 (brcmfmac): radio soft-blocked by rfkill, NM's wireless toggle
    # off, or the regulatory domain not yet applied so the driver rejects
    # the AP channel. Also wait briefly for wlan0 — brcmfmac loads over
    # SDIO asynchronously and can lag NetworkManager on a cold boot. All
    # idempotent + cheap.
    rfkill unblock wifi >/dev/null 2>&1 || true
    rfkill unblock all  >/dev/null 2>&1 || true
    nmcli radio wifi on >/dev/null 2>&1 || true
    local reg; reg=$(cat /etc/regdomain 2>/dev/null || true)
    iw reg set "${reg:-US}" >/dev/null 2>&1 || true
    local i
    for i in $(seq 1 10); do
        nmcli -t -f DEVICE device 2>/dev/null | grep -qx "$AP_IFACE" && break
        sleep 1
    done
}

known_client_wifi_count() {
    # Count saved *client* WiFi profiles (excludes the AP itself). 0 means
    # a never-configured device → bring the setup AP up immediately rather
    # than waiting out the full grace timer.
    nmcli -t -f NAME,TYPE con show 2>/dev/null \
        | grep ':802-11-wireless$' \
        | grep -vc "^${AP_CON}:"
}

ensure_ap_profile() {
    # (Re)create the AP connection profile with default SSID/PSK if absent.
    if ! nmcli con show "$AP_CON" >/dev/null 2>&1; then
        nmcli con add type wifi con-name "$AP_CON" ifname "$AP_IFACE" ssid "$AP_CON" \
            mode ap autoconnect no >/dev/null 2>&1
        nmcli con mod "$AP_CON" \
            wifi-sec.key-mgmt wpa-psk wifi-sec.psk "aeon-setup-pw" >/dev/null 2>&1
    fi
}

ap_set_l3() {
    # Enforce mode/IP/band on every activation (self-heals a profile a
    # different code path wrote on the wrong subnet). $1 = channel number,
    # or "" for driver auto-pick. PRESERVES any custom SSID/PSK.
    nmcli con mod "$AP_CON" \
        802-11-wireless.mode ap \
        802-11-wireless.powersave 2 \
        ipv4.method shared ipv4.addresses "${AP_GATEWAY}/24" \
        ipv6.method disabled \
        wifi.band bg wifi.channel "$1" >/dev/null 2>&1
}

activate_ap() {
    # Robust AP bring-up. Returns 0 on success (AP broadcasting), 1 on
    # failure. Tries several 2.4GHz channels, then a from-scratch profile
    # recreate, capturing NetworkManager's real error to the out-of-band
    # status file so a persistent failure is always diagnosable.
    radio_kick
    ensure_ap_profile

    local ch err
    for ch in 6 1 11 ""; do
        ap_set_l3 "$ch"
        if err=$(nmcli con up "$AP_CON" 2>&1); then
            write_status OK "AP up (channel ${ch:-auto}) at ${AP_GATEWAY}"
            apply_captive_portal_hijack
            return 0
        fi
        write_status RETRY "channel ${ch:-auto} failed: ${err}"
    done

    # Last resort: the profile may be wedged — delete + recreate, try once more.
    nmcli con delete "$AP_CON" >/dev/null 2>&1 || true
    ensure_ap_profile
    ap_set_l3 6
    if err=$(nmcli con up "$AP_CON" 2>&1); then
        write_status OK "AP up after profile recreate at ${AP_GATEWAY}"
        apply_captive_portal_hijack
        return 0
    fi

    local radio rk reg
    radio=$(nmcli -t radio wifi 2>/dev/null)
    rk=$(rfkill list wifi 2>/dev/null | tr '\n' ' ')
    reg=$(iw reg get 2>/dev/null | awk -F'country |:' '/country/{print $2; exit}')
    write_status FAIL "nmcli con up failed: ${err} || radio=${radio} rfkill=[${rk}] regdomain=${reg}"
    return 1
}

now=$(date +%s)
down_since=$(<"$STATE_FILE")

# ── Forced setup-AP escape hatch ──
# Drop an empty file `aeon-force-ap` on the FAT /boot/firmware partition
# (trivial from any computer holding the SD card) to force the setup AP up
# unconditionally — regardless of WiFi/Ethernet/online state. This is the
# deterministic way into setup mode when the offline-detection heuristics
# don't fire (e.g. a configured device you want to reconfigure). Remove the
# file to return to normal client/online behaviour on the next tick.
if [[ -f "$FORCE_AP_FLAG" ]]; then
    if ap_active; then
        exit 0
    fi
    log "force-AP flag present (${FORCE_AP_FLAG}) — bringing up setup AP"
    activate_ap || true
    exit 0
fi

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
            if activate_ap; then
                echo "$now" > "$STATE_FILE"
            fi
        fi
    fi
    exit 0
fi

# Not in AP, not online.

KNOWN_WIFI=$(known_client_wifi_count)

# Grace: a device with KNOWN client networks waits MAX_DOWN_SECONDS before
# falling back (rides out brief outages / roams without flapping the AP).
# A device with NO known networks is in first-time setup — bring the setup
# AP up on the next tick instead of making the user wait ~2.5 minutes.
grace=$MAX_DOWN_SECONDS
(( KNOWN_WIFI == 0 )) && grace=0

# Start / continue the countdown.
if [[ "$down_since" == "0" ]]; then
    echo "$now" > "$STATE_FILE"
    down_since=$now
    if (( grace > 0 )); then
        log "no internet, starting ${grace}s grace timer (known WiFi: ${KNOWN_WIFI})"
        exit 0
    fi
    log "no known WiFi configured — bringing up setup AP now"
fi

if (( now - down_since >= grace )); then
    log "offline $((now - down_since))s — activating $AP_CON (known WiFi: ${KNOWN_WIFI})"
    if activate_ap; then
        # AP up — stamp NOW so the ap_active branch's AP_RETRY_SECONDS
        # 'retry primary' clock starts from when the AP actually came up.
        echo "$now" > "$STATE_FILE"
    fi
    # On failure: deliberately DON'T touch the state file. Leaving
    # down_since at the offline-start time means the next 20s tick still
    # satisfies the threshold and retries — instead of resetting the clock
    # and waiting another full grace period between attempts (the prior bug
    # that turned a transient activation hiccup into a ~90s stall).
fi
