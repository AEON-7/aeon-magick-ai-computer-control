#!/usr/bin/env python3
"""aeon-vpn-rotate — refresh the active VPN's identity / circuit / session.

For Tor: SIGNAL NEWNYM via the control port (new circuits for next
attached streams). For Tailscale: `tailscale up --reset` (renegotiates
node key). For WireGuard: down + up wg-quick@aeon0. For OpenVPN: restart
openvpn-client@aeon. For I2P: trigger fresh outbound tunnels via i2pd's
HTTP API.

Output: JSON {"ok": bool, "provider": str, "action": str, "detail": str}.
"""
import json
import socket
import subprocess
import sys
import tomllib
import urllib.request

NET_TOML = "/etc/aeon/network.toml"


def load() -> dict:
    try:
        with open(NET_TOML, "rb") as f:
            return tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError):
        return {}


def rotate_tor() -> dict:
    try:
        with open("/var/run/tor/control.authcookie", "rb") as f:
            cookie_hex = f.read().hex()
    except OSError as e:
        return {"ok": False, "provider": "tor", "action": "NEWNYM",
                "detail": f"cookie unreadable: {e}"}
    try:
        with socket.create_connection(("127.0.0.1", 9051), timeout=4) as s:
            s.sendall(f"AUTHENTICATE {cookie_hex}\r\nSIGNAL NEWNYM\r\nQUIT\r\n".encode())
            buf = b""
            while True:
                chunk = s.recv(4096)
                if not chunk:
                    break
                buf += chunk
        # We expect "250 OK" lines for AUTHENTICATE and SIGNAL.
        ok = buf.count(b"250 OK") >= 2 or b"250 OK" in buf
        return {"ok": ok, "provider": "tor", "action": "NEWNYM",
                "detail": buf.decode("ascii", "replace").strip()}
    except OSError as e:
        return {"ok": False, "provider": "tor", "action": "NEWNYM",
                "detail": str(e)}


def rotate_tailscale() -> dict:
    try:
        result = subprocess.run(
            ["/usr/bin/tailscale", "up", "--reset"],
            capture_output=True, timeout=20,
        )
        return {"ok": result.returncode == 0, "provider": "tailscale",
                "action": "up --reset",
                "detail": (result.stdout or result.stderr).decode("utf-8", "replace").strip()}
    except (subprocess.TimeoutExpired, OSError) as e:
        return {"ok": False, "provider": "tailscale", "action": "up --reset",
                "detail": str(e)}


def rotate_wireguard() -> dict:
    # down + up
    detail = []
    ok = True
    for cmd in (["wg-quick", "down", "aeon0"], ["wg-quick", "up", "aeon0"]):
        try:
            r = subprocess.run(cmd, capture_output=True, timeout=15)
            if r.returncode != 0:
                ok = False
            detail.append((r.stderr or r.stdout).decode("utf-8", "replace").strip())
        except (subprocess.TimeoutExpired, OSError) as e:
            ok = False
            detail.append(str(e))
    return {"ok": ok, "provider": "wireguard",
            "action": "wg-quick down + up", "detail": " | ".join(detail)}


def rotate_openvpn() -> dict:
    try:
        r = subprocess.run(
            ["systemctl", "restart", "openvpn-client@aeon.service"],
            capture_output=True, timeout=20,
        )
        return {"ok": r.returncode == 0, "provider": "openvpn",
                "action": "systemctl restart",
                "detail": (r.stderr or r.stdout).decode("utf-8", "replace").strip()}
    except (subprocess.TimeoutExpired, OSError) as e:
        return {"ok": False, "provider": "openvpn",
                "action": "systemctl restart", "detail": str(e)}


def rotate_i2p() -> dict:
    # i2pd's HTTP API: /?cmd=reload_tunnels_config triggers a tunnel rebuild.
    try:
        req = urllib.request.Request("http://127.0.0.1:7070/?cmd=reload_tunnels_config")
        with urllib.request.urlopen(req, timeout=5) as resp:
            return {"ok": resp.status == 200, "provider": "i2p",
                    "action": "reload_tunnels_config",
                    "detail": f"http {resp.status}"}
    except (urllib.error.URLError, OSError) as e:
        return {"ok": False, "provider": "i2p",
                "action": "reload_tunnels_config", "detail": str(e)}


HANDLERS = {
    "tor": rotate_tor,
    "tailscale": rotate_tailscale,
    "wireguard": rotate_wireguard,
    "openvpn": rotate_openvpn,
    "i2p": rotate_i2p,
}


def main() -> int:
    cfg = load()
    provider = cfg.get("vpn", {}).get("provider", "none")
    if provider not in HANDLERS:
        print(json.dumps({"ok": False, "provider": provider,
                          "action": "rotate", "detail": "no rotate for this provider"}))
        return 0
    print(json.dumps(HANDLERS[provider]()))
    return 0


if __name__ == "__main__":
    sys.exit(main())
