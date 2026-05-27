#!/usr/bin/env python3
"""aeon-vpn-status — query the active VPN provider and emit JSON status.

Called by the supervisor's GET /api/network/vpn/status endpoint. Reads
/etc/aeon/network.toml to discover the active provider, then runs the
provider-specific introspection (Tor control port, `tailscale status
--json`, `wg show`, OpenVPN management socket, i2pd HTTP console) and
emits a normalized JSON shape the web UI renders.

Output schema (always valid JSON):
{
  "ok": true,
  "provider": "tor"|"tailscale"|"wireguard"|"openvpn"|"i2p"|"none",
  "enabled": bool,
  "state": "establishing"|"connected"|"reconnecting"|"failed"|"disabled",
  "bootstrap_percent": 0-100 | null,
  "summary": "<human-readable one-liner>",
  "public_ip": "1.2.3.4" | null,
  "public_country": "DE" | null,
  "detail": { provider-specific fields }
}
"""
import json
import os
import socket
import subprocess
import sys
import tomllib
import urllib.request

NET_TOML = "/etc/aeon/network.toml"


def load_net_config() -> dict:
    try:
        with open(NET_TOML, "rb") as f:
            return tomllib.load(f)
    except FileNotFoundError:
        return {}
    except tomllib.TOMLDecodeError:
        return {}


def public_ip_via_curl() -> tuple[str | None, str | None]:
    """Fetch our externally-visible IP + country code.

    Uses curl (rather than urllib) so it picks up the system's proxy
    chain, transparent iptables redirects, and /etc/resolv.conf.
    """
    try:
        out = subprocess.run(
            ["curl", "-s", "--max-time", "8", "https://ifconfig.co/json"],
            capture_output=True,
            timeout=12,
        )
        if out.returncode != 0:
            return None, None
        data = json.loads(out.stdout.decode("utf-8", "replace"))
        return data.get("ip"), data.get("country_iso")
    except (subprocess.TimeoutExpired, json.JSONDecodeError, OSError):
        return None, None


# ─────────────────────────────────────────────────────────────────────────
# Tor
# ─────────────────────────────────────────────────────────────────────────

def tor_control(command: str) -> str | None:
    """Send a single command to Tor's control port via the cookie-auth
    flow and return the response body (or None on any failure)."""
    try:
        with open("/var/run/tor/control.authcookie", "rb") as f:
            cookie_hex = f.read().hex()
    except OSError:
        return None
    try:
        with socket.create_connection(("127.0.0.1", 9051), timeout=4) as s:
            payload = (
                f"AUTHENTICATE {cookie_hex}\r\n"
                f"{command}\r\n"
                "QUIT\r\n"
            ).encode("ascii")
            s.sendall(payload)
            buf = b""
            while True:
                chunk = s.recv(8192)
                if not chunk:
                    break
                buf += chunk
        return buf.decode("ascii", "replace")
    except (OSError, socket.timeout):
        return None


def tor_status(cfg: dict) -> dict:
    out: dict = {
        "provider": "tor",
        "state": "establishing",
        "bootstrap_percent": None,
        "summary": "querying Tor…",
        "detail": {"circuits": [], "exit_country": cfg.get("vpn", {}).get("tor", {}).get("exit_country", "")},
    }

    bootstrap_resp = tor_control("GETINFO status/bootstrap-phase")
    if bootstrap_resp:
        # Expected line: 250-status/bootstrap-phase=NOTICE BOOTSTRAP PROGRESS=100 TAG=done SUMMARY="Done"
        for line in bootstrap_resp.splitlines():
            if "bootstrap-phase=" in line:
                if "PROGRESS=" in line:
                    try:
                        pct = int(line.split("PROGRESS=")[1].split()[0])
                        out["bootstrap_percent"] = pct
                    except (IndexError, ValueError):
                        pass
                if "TAG=" in line:
                    tag = line.split("TAG=")[1].split()[0]
                    out["state"] = "connected" if tag == "done" else "establishing"
                if 'SUMMARY="' in line:
                    out["summary"] = line.split('SUMMARY="')[1].rstrip('"').rstrip("\r")

    circ_resp = tor_control("GETINFO circuit-status")
    if circ_resp:
        circuits = []
        for line in circ_resp.splitlines():
            # Lines: <CircID> <Status> <Path> <BUILD_FLAGS=...> <PURPOSE=...>
            # Path like: $FINGERPRINT~Nick,$FINGERPRINT~Nick,$FINGERPRINT~Nick
            line = line.strip()
            if not line or line.startswith("250") or line.startswith("."):
                continue
            parts = line.split(" ", 2)
            if len(parts) < 2:
                continue
            circ_id, status = parts[0], parts[1]
            if status not in ("BUILT", "EXTENDED"):
                continue
            path_field = parts[2] if len(parts) > 2 else ""
            # Take just the names from the path (skip fingerprints for UI)
            nicks = []
            for hop in path_field.split(",")[:5]:
                if "~" in hop:
                    nicks.append(hop.split("~", 1)[1].split(" ", 1)[0])
            if nicks:
                circuits.append({"id": circ_id, "hops": nicks})
        out["detail"]["circuits"] = circuits[:10]  # cap

    return out


# ─────────────────────────────────────────────────────────────────────────
# Tailscale
# ─────────────────────────────────────────────────────────────────────────

def tailscale_status(cfg: dict) -> dict:
    out: dict = {
        "provider": "tailscale",
        "state": "establishing",
        "bootstrap_percent": None,
        "summary": "querying Tailscale…",
        "detail": {},
    }
    try:
        result = subprocess.run(
            ["/usr/bin/tailscale", "status", "--json"],
            capture_output=True, timeout=6,
        )
        if result.returncode != 0:
            out["state"] = "failed"
            out["summary"] = "tailscale CLI returned non-zero"
            return out
        ts = json.loads(result.stdout)
    except (subprocess.TimeoutExpired, OSError, json.JSONDecodeError) as e:
        out["state"] = "failed"
        out["summary"] = f"tailscale status: {e}"
        return out

    self_ = ts.get("Self", {})
    online = self_.get("Online", False)
    out["state"] = "connected" if online else "failed"
    out["summary"] = (
        f"Online as {self_.get('HostName','?')} ({self_.get('TailscaleIPs',['?'])[0]})"
        if online else "Tailscale offline"
    )
    out["bootstrap_percent"] = 100 if online else 0

    peers = []
    for pkey, p in (ts.get("Peer") or {}).items():
        peers.append({
            "host": p.get("HostName"),
            "ips": p.get("TailscaleIPs", []),
            "online": p.get("Online", False),
            "exit_node": p.get("ExitNode", False),
            "os": p.get("OS"),
        })
    out["detail"]["peers"] = peers
    out["detail"]["magic_dns"] = ts.get("MagicDNSSuffix")
    return out


# ─────────────────────────────────────────────────────────────────────────
# WireGuard
# ─────────────────────────────────────────────────────────────────────────

def wireguard_status(cfg: dict) -> dict:
    out: dict = {
        "provider": "wireguard",
        "state": "failed",
        "bootstrap_percent": None,
        "summary": "wg show: not running",
        "detail": {"peers": []},
    }
    try:
        result = subprocess.run(
            ["/usr/bin/wg", "show", "aeon0", "dump"],
            capture_output=True, timeout=4,
        )
        if result.returncode != 0:
            return out
        lines = result.stdout.decode("ascii", "replace").splitlines()
    except (subprocess.TimeoutExpired, OSError):
        return out

    if not lines:
        return out

    # First line: interface info
    # Subsequent lines: peer info — fields: public_key preshared_key endpoint allowed_ips
    # latest_handshake transfer_rx transfer_tx persistent_keepalive
    import time
    now = int(time.time())
    peers = []
    for ln in lines[1:]:
        f = ln.split("\t")
        if len(f) < 8:
            continue
        try:
            handshake = int(f[4])
        except ValueError:
            handshake = 0
        peers.append({
            "endpoint": f[2] or None,
            "allowed_ips": f[3] or None,
            "handshake_age_s": (now - handshake) if handshake else None,
            "rx_bytes": int(f[5]) if f[5].isdigit() else 0,
            "tx_bytes": int(f[6]) if f[6].isdigit() else 0,
        })
    out["detail"]["peers"] = peers
    if peers:
        # State based on recent handshake
        recent = any((p.get("handshake_age_s") or 9999) < 180 for p in peers)
        out["state"] = "connected" if recent else "reconnecting"
        out["bootstrap_percent"] = 100 if recent else 50
        out["summary"] = (
            f"{len(peers)} peer(s), last handshake "
            f"{peers[0].get('handshake_age_s', '?')}s ago"
        )
    return out


# ─────────────────────────────────────────────────────────────────────────
# OpenVPN
# ─────────────────────────────────────────────────────────────────────────

def openvpn_status(cfg: dict) -> dict:
    out: dict = {
        "provider": "openvpn",
        "state": "failed",
        "bootstrap_percent": None,
        "summary": "openvpn-client@aeon not running",
        "detail": {},
    }
    try:
        result = subprocess.run(
            ["systemctl", "is-active", "openvpn-client@aeon.service"],
            capture_output=True, timeout=3,
        )
        active = result.stdout.decode().strip() == "active"
    except (subprocess.TimeoutExpired, OSError):
        active = False
    out["state"] = "connected" if active else "failed"
    out["bootstrap_percent"] = 100 if active else 0
    out["summary"] = "OpenVPN tunnel up" if active else "OpenVPN tunnel down"
    return out


# ─────────────────────────────────────────────────────────────────────────
# I2P (i2pd)
# ─────────────────────────────────────────────────────────────────────────

def i2p_status(cfg: dict) -> dict:
    out: dict = {
        "provider": "i2p",
        "state": "failed",
        "bootstrap_percent": None,
        "summary": "i2pd console unreachable",
        "detail": {},
    }
    try:
        req = urllib.request.Request("http://127.0.0.1:7070/")
        with urllib.request.urlopen(req, timeout=4) as resp:
            html = resp.read().decode("utf-8", "replace")
            # Very rough scrape — i2pd's console is HTML, not JSON
            out["state"] = "connected"
            out["bootstrap_percent"] = 100
            out["summary"] = "i2pd console online"
            # Try to grab a peer count from the status table
            import re
            m = re.search(r"Active peers[^<]*<[^>]*>\s*(\d+)", html)
            if m:
                out["detail"]["active_peers"] = int(m.group(1))
                out["summary"] = f"i2pd online, {m.group(1)} active peers"
    except (urllib.error.URLError, OSError, ValueError):
        pass
    return out


# ─────────────────────────────────────────────────────────────────────────
# Dispatch
# ─────────────────────────────────────────────────────────────────────────

PROVIDERS = {
    "tor": tor_status,
    "tailscale": tailscale_status,
    "wireguard": wireguard_status,
    "openvpn": openvpn_status,
    "i2p": i2p_status,
}


def main() -> int:
    cfg = load_net_config()
    vpn = cfg.get("vpn", {})
    enabled = bool(vpn.get("enabled", False))
    provider = vpn.get("provider", "none")

    response = {
        "ok": True,
        "provider": provider,
        "enabled": enabled,
        "state": "disabled",
        "bootstrap_percent": None,
        "summary": "VPN disabled",
        "public_ip": None,
        "public_country": None,
        "detail": {},
    }

    if not enabled or provider == "none" or provider not in PROVIDERS:
        print(json.dumps(response))
        return 0

    detail = PROVIDERS[provider](cfg)
    response.update(detail)

    # Only bother fetching public IP if we're connected — saves an
    # 8-second curl timeout while bootstrapping.
    if response.get("state") == "connected":
        ip, country = public_ip_via_curl()
        response["public_ip"] = ip
        response["public_country"] = country

    print(json.dumps(response))
    return 0


if __name__ == "__main__":
    sys.exit(main())
