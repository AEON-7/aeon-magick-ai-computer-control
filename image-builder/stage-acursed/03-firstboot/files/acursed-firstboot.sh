#!/bin/bash
# Aeon Cursed KVM — first-boot bootstrap.
#
#   1. Generate the admin password (printed to /boot/firmware/aeon-credentials.txt
#      where the user can see it after pulling the SD card).
#   2. If /boot/firmware/aeon-setup.toml is present, copy WiFi creds + Tailscale
#      auth key out of it and apply.
#   3. Mark firstboot done so we don't re-run.

set -e
exec > >(tee -a /var/log/acursed-firstboot.log) 2>&1
echo "=== acursed-firstboot $(date -Iseconds) ==="

mkdir -p /var/lib/acursed
mkdir -p /etc/acursed

# ── 1. Auth ──
if [ ! -f /etc/acursed/auth.toml ]; then
    PW=$(/usr/local/bin/acursed-supervisor --generate-auth /etc/acursed/auth.toml 2>/dev/null || true)
    if [ -n "$PW" ]; then
        cat > /boot/firmware/aeon-credentials.txt <<EOF
# Aeon Cursed KVM — generated credentials
# DELETE THIS FILE ONCE YOU'VE NOTED THE PASSWORD.

username: admin
password: $PW

Web UI: https://aeon-cursed.local/
SSH:    ssh admin@aeon-cursed.local
EOF
        chmod 600 /boot/firmware/aeon-credentials.txt
        echo "wrote /boot/firmware/aeon-credentials.txt"
    fi
fi

# ── 2. Setup TOML on the boot partition ──
if [ -f /boot/firmware/aeon-setup.toml ]; then
    echo "found /boot/firmware/aeon-setup.toml, applying..."
    # We use a tiny inline Python parser to avoid pulling in `tomlq`.
    eval "$(python3 -c '
import tomllib, sys
with open("/boot/firmware/aeon-setup.toml", "rb") as f:
    d = tomllib.load(f)
wifi = d.get("wifi", {})
print(f"WIFI_SSID={wifi.get(\"ssid\", \"\")!r}")
print(f"WIFI_PSK={wifi.get(\"password\", \"\")!r}")
ts = d.get("tailscale", {})
print(f"TS_KEY={ts.get(\"auth_key\", \"\")!r}")
print(f"TS_HOSTNAME={ts.get(\"hostname\", \"aeon-cursed\")!r}")
')"
    if [ -n "${WIFI_SSID:-}" ]; then
        nmcli con add type wifi con-name "$WIFI_SSID" ifname wlan0 ssid "$WIFI_SSID" 2>/dev/null || true
        nmcli con mod "$WIFI_SSID" wifi-sec.key-mgmt wpa-psk wifi-sec.psk "$WIFI_PSK"
        nmcli con mod "$WIFI_SSID" connection.autoconnect yes connection.autoconnect-priority 100
        echo "wifi $WIFI_SSID configured"
    fi
    if [ -n "${TS_KEY:-}" ]; then
        tailscale up --authkey="$TS_KEY" --hostname="$TS_HOSTNAME" --ssh || \
            echo "tailscale up failed (will retry on reboot)"
    fi
    # Don't leave a file with secrets on the public boot partition.
    rm -f /boot/firmware/aeon-setup.toml
fi

# Set hostname (idempotent)
HOSTNAME_FILE=/etc/hostname
if [ "$(cat $HOSTNAME_FILE 2>/dev/null)" != "aeon-cursed" ]; then
    hostnamectl set-hostname aeon-cursed || echo "aeon-cursed" > $HOSTNAME_FILE
fi

touch /var/lib/acursed/firstboot-done
echo "=== firstboot done $(date -Iseconds) ==="
