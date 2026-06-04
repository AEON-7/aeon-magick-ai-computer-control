#!/bin/bash
# Aeon Magick AI Computer Control — first-boot bootstrap.
#
#   1. Initialize auth.toml in OPEN state (session key, no password yet).
#      The web UI's setup wizard collects the password on first visit.
#   2. If /boot/firmware/aeon-setup.toml is present, copy WiFi creds + Tailscale
#      auth key out of it and apply.
#   3. Mark firstboot done so we don't re-run.

set -e
exec > >(tee -a /var/log/aeon-firstboot.log) 2>&1
echo "=== aeon-firstboot $(date -Iseconds) ==="

mkdir -p /var/lib/aeon
mkdir -p /etc/aeon

# ── 1. Auth bootstrap ──
# Initialize the auth file in OPEN state: random session key gets baked in,
# but NO password — the user creates that via the web UI's setup wizard at
# first visit. This means no static creds ever sit on the public boot
# partition. The admin console+SSH password is randomized PER-DEVICE just below
# (replacing pi-gen's build-time default) and recorded on this device's own boot
# partition — so no two devices flashed from the same image share a login.

# Mint a per-device admin console+SSH password (replacing pi-gen's build-time
# default) so no two devices flashed from the same image share an SSH login.
# Nothing secret is baked into the image; the web setup wizard can still replace
# it later. (The setup-AP Wi-Fi PSK stays a STATIC default — proximity-locked,
# setup-mode-only, and joining it doesn't grant admin; see aeon-netwatch.)
ADMIN_PW="$(tr -dc 'A-Za-z0-9' </dev/urandom | head -c 20)"
if echo "admin:${ADMIN_PW}" | chpasswd 2>/dev/null; then
    echo "admin password randomized (unique per device)"
else
    ADMIN_PW="aeon-default-change-me"
    echo "WARN: chpasswd failed — admin keeps the build default; change with passwd"
fi
if [ ! -f /etc/aeon/auth.toml ]; then
    /usr/local/bin/aeon-supervisor --generate-auth /etc/aeon/auth.toml >/dev/null 2>&1 || true
    cat > /boot/firmware/aeon-credentials.txt <<'EOF'
# Aeon Magick AI Computer Control — first-boot setup
#
# Your device is in SETUP mode. There are two ways to reach the web UI:
#
# A) Over your LAN (if you pre-seeded WiFi via aeon-setup.toml, or plugged
#    in Ethernet):
#
#       https://aeon-magick.local/
#       (or use the device's LAN IP if mDNS isn't resolving)
#
# B) Over the built-in setup Wi-Fi access point (when no known network is
#    reachable — e.g. a brand-new device with only power connected). The
#    device broadcasts its own network shortly after boot:
#
#       Wi-Fi network: aeon-setup
#       password:      aeon-setup-pw
#       then open:     https://192.168.50.1/   (a setup page should also
#                      pop up automatically as a captive portal)
#
# The first page is the setup wizard. After you set a password, that's
# what you'll use for the web UI, the REST API, and MCP clients.
#
# ── If the `aeon-setup` Wi-Fi never appears ──
#   * Give it ~60-90 seconds after power-on.
#   * Pull this SD card and read `aeon-ap-status.txt` on this same
#     partition — it records exactly why the AP did/didn't start.
#   * To FORCE setup-AP mode (even on a configured device), create an
#     empty file named `aeon-force-ap` on this partition, reinsert, and
#     boot. Delete it later to return to normal Wi-Fi-client behaviour.
#
# SSH login (separate from the web UI password):
#     ssh admin@aeon-magick.local
#     password: __ADMIN_PW__
#     (the web setup wizard replaces this with your chosen password.)
#
# Delete this file once you've completed setup.
EOF
    sed -i "s|__ADMIN_PW__|${ADMIN_PW}|" /boot/firmware/aeon-credentials.txt
    chmod 600 /boot/firmware/aeon-credentials.txt
    echo "wrote /boot/firmware/aeon-credentials.txt (per-device credentials)"
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
print(f"TS_HOSTNAME={ts.get(\"hostname\", \"aeon-magick\")!r}")
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
if [ "$(cat $HOSTNAME_FILE 2>/dev/null)" != "aeon-magick" ]; then
    hostnamectl set-hostname aeon-magick || echo "aeon-magick" > $HOSTNAME_FILE
fi

touch /var/lib/aeon/firstboot-done
echo "=== firstboot done $(date -Iseconds) ==="
