#!/bin/bash
# Aeon Magick AI Computer Control — first-boot bootstrap.
#
#   1. Initialize auth.toml in OPEN state (session key, no password yet).
#      The web UI's setup wizard collects the password on first visit.
#   2. If /boot/firmware/aeon-setup.toml is present, copy WiFi creds + Tailscale
#      auth key out of it and apply.
#   3. Mark firstboot done so we don't re-run.

# NOT `set -e`. The supervisor is ordered after this unit, so any step that
# aborted the script (a Wi-Fi password NetworkManager rejects, nmcli racing
# NetworkManager's startup, a slow `tailscale up`) used to leave the device
# with no web UI at all — on every boot, since the done-marker was never
# reached. Every optional step below reports its own failure and carries on.
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

# Mint the per-device admin console+SSH password ONCE. If an earlier run of this
# script was interrupted, re-minting would change the password while the
# credentials file below (written once) still showed the old one.
PW_MARK=/var/lib/aeon/admin-pw-minted
if [ ! -f "$PW_MARK" ]; then
    ADMIN_PW="$(tr -dc 'A-Za-z0-9' </dev/urandom | head -c 20)"
    if [ -n "$ADMIN_PW" ] && echo "admin:${ADMIN_PW}" | chpasswd 2>/dev/null; then
        echo "admin password randomized (unique per device)"
        ( umask 077; printf '%s' "$ADMIN_PW" > "$PW_MARK" )
    else
        ADMIN_PW=""
        echo "WARN: could not set a per-device admin password — SSH keeps the image default until the setup wizard replaces it"
    fi
else
    ADMIN_PW="$(cat "$PW_MARK" 2>/dev/null)"
fi

# Setup code: what proves someone finishing the web setup wizard has the
# device in their hands. The wizard is open without it only for the first
# 30 minutes of the very first boot; after that (or after any reboot) it asks
# for this code, which lives here on the SD card. Without it, anyone who could
# reach an un-configured Orb on the network — at any later time — could set the
# admin password and with it gain root over SSH.
SETUP_CODE_FILE=/etc/aeon/setup-code
if [ ! -s "$SETUP_CODE_FILE" ]; then
    SETUP_CODE="$(tr -dc 'A-HJ-NP-Z2-9' </dev/urandom | head -c 8)"
    ( umask 077; printf '%s\n' "$SETUP_CODE" > "$SETUP_CODE_FILE" )
fi
SETUP_CODE="$(cat "$SETUP_CODE_FILE" 2>/dev/null)"
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
# Setup code: __SETUP_CODE__
#     The wizard asks for this once the device has been on for more than
#     30 minutes, or has been restarted, without setup being completed.
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
    sed -i "s|__ADMIN_PW__|${ADMIN_PW:-(unchanged image default)}|; s|__SETUP_CODE__|${SETUP_CODE:0:4}-${SETUP_CODE:4:4}|" /boot/firmware/aeon-credentials.txt
    chmod 600 /boot/firmware/aeon-credentials.txt
    echo "wrote /boot/firmware/aeon-credentials.txt (per-device credentials)"
fi

# ── 2. Setup TOML on the boot partition ──
if [ -f /boot/firmware/aeon-setup.toml ]; then
    echo "found /boot/firmware/aeon-setup.toml, applying..."
    # Applied from Python with argument lists — never through the shell. The
    # old version `eval`ed a Python repr of each value, so a Wi-Fi password
    # holding a quote plus `$` or a backtick was executed as root.
    python3 - <<'PY' || echo "WARN: aeon-setup.toml was not fully applied (details above)"
import json, os, re, subprocess, tomllib

def run(args, timeout=60):
    """Run a command; report failure, never raise."""
    try:
        r = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    except Exception as e:  # missing binary, timeout
        print(f"WARN: {args[0]} {args[1] if len(args) > 1 else ''}: {e}")
        return False
    if r.returncode != 0:
        print(f"WARN: {args[0]} {args[1] if len(args) > 1 else ''} failed: {(r.stderr or r.stdout).strip()[:200]}")
    return r.returncode == 0

try:
    with open("/boot/firmware/aeon-setup.toml", "rb") as f:
        d = tomllib.load(f)
except Exception as e:
    print("WARN: aeon-setup.toml is not valid TOML:", e)
    d = {}

wifi = d.get("wifi") if isinstance(d.get("wifi"), dict) else {}
ssid, psk = str(wifi.get("ssid") or ""), str(wifi.get("password") or "")
if ssid:
    # NetworkManager may still be starting this early in boot.
    for _ in range(30):
        if subprocess.run(["nmcli", "-t", "general", "status"], capture_output=True).returncode == 0:
            break
        subprocess.run(["sleep", "1"])
    run(["nmcli", "con", "add", "type", "wifi", "con-name", ssid, "ifname", "wlan0", "ssid", ssid])
    if psk:
        if len(psk) < 8:
            print("WARN: the Wi-Fi password is shorter than 8 characters, which WPA rejects — join the aeon-setup hotspot to enter it again")
        run(["nmcli", "con", "mod", ssid, "wifi-sec.key-mgmt", "wpa-psk", "wifi-sec.psk", psk])
    run(["nmcli", "con", "mod", ssid, "connection.autoconnect", "yes", "connection.autoconnect-priority", "100"])
    print("wifi", ssid, "configured")

ts = d.get("tailscale") if isinstance(d.get("tailscale"), dict) else {}
key = str(ts.get("auth_key") or "")
if key:
    host = str(ts.get("hostname") or "aeon-magick")
    # Split-tunnel mesh only: don't let Tailscale hijack DNS (MagicDNS) or
    # accept subnet routes — only tailnet addresses go over the mesh.
    joined = run(["tailscale", "up", "--authkey=" + key, "--hostname=" + host, "--ssh",
                  "--accept-dns=false", "--accept-routes=false", "--timeout=45s"], timeout=75)
    # Record the mesh as ENABLED in network.toml. aeon-net-services runs
    # `tailscale down` on every boot unless it is, which silently undid a
    # pre-seeded Tailscale after the first restart. If joining failed (no
    # network yet) the key is kept there, root-only, so that service retries —
    # the boot-partition copy is deleted either way.
    path = "/etc/aeon/network.toml"
    try:
        text = open(path).read() if os.path.exists(path) else ""
        want = {"enabled": "true", "hostname": json.dumps(host)}
        if not joined:
            want["auth_key"] = json.dumps(key)
        m = re.search(r"(?ms)^\[tailscale\][ \t]*\n(.*?)(?=^\[|\Z)", text)
        body = m.group(1) if m else ""
        for k, v in want.items():
            line = f"{k} = {v}"
            if re.search(rf"(?m)^{k}[ \t]*=.*$", body):
                body = re.sub(rf"(?m)^{k}[ \t]*=.*$", lambda _m, line=line: line, body)
            else:
                body = line + "\n" + body
        if m:
            text = text[:m.start(1)] + body + text[m.end(1):]
        else:
            text = text.rstrip("\n") + ("\n\n" if text.strip() else "") + "[tailscale]\n" + body
        tmp = path + ".tmp"
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            f.write(text if text.endswith("\n") else text + "\n")
        os.replace(tmp, path)
        print("tailscale", "joined" if joined else "not joined yet — will retry at next boot", "(recorded as enabled)")
    except Exception as e:
        print("WARN: could not record Tailscale in network.toml:", e)
PY
    # Don't leave a file with secrets on the public boot partition.
    rm -f /boot/firmware/aeon-setup.toml
fi

# Set hostname (idempotent)
HOSTNAME_FILE=/etc/hostname
if [ "$(cat $HOSTNAME_FILE 2>/dev/null)" != "aeon-magick" ]; then
    hostnamectl set-hostname aeon-magick || echo "aeon-magick" > $HOSTNAME_FILE
fi

# The marker records WHICH boot first-boot completed in. The supervisor compares
# it with the running boot id to decide whether the setup wizard is still inside
# its no-code window (see the setup code above).
cat /proc/sys/kernel/random/boot_id > /var/lib/aeon/firstboot-done 2>/dev/null || touch /var/lib/aeon/firstboot-done
echo "=== firstboot done $(date -Iseconds) ==="
