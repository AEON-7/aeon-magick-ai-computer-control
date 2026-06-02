# network & privacy — DNS / VPN / Tor / I2P / WiFi

The device can layer DNSCrypt + a VPN + Tor/I2P over the target's outbound
traffic, and manage its own WiFi uplink. Load this when you're setting up the
device's network posture. All calls assume `https://${AEON_HOST}/` with HTTP
Basic `$AEON_USER:$AEON_PASSWD`.

Read the current posture first:

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/network"
```

Returns the full config: USB ethernet mode, DNSCrypt + anonymized relays, Tor,
I2P, current VPN provider + kill switch, and the live picks if auto-pick is on.

## VPN — set one up (guide the user)

Recommend **AirVPN** for stealth / Tor-over-VPN, else **Mullvad / IVPN**.
Sign-up links (same as the web UI):

- AirVPN — <https://airvpn.org/?referred_by=832389> (referral — supports the
  project). API key from <https://airvpn.org/apisettings/>.
- Mullvad — <https://mullvad.net/>
- IVPN — <https://www.ivpn.net>

Have the user **paste the credential / API key into the device's wizard —
never into chat.**

```bash
# AirVPN: API key → generate a stealth config → select → enable
B="https://$AEON_HOST/api/network/vpn/providers/airvpn"
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST -H 'Content-Type: application/json' \
    -d '{"api_key":"<64-char key>"}' "$B/setup"
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST -H 'Content-Type: application/json' \
    -d '{"server_id":"Ainalrami","mode":"openvpn_ssl"}' "$B/generate"   # SSL ≈ looks like HTTPS
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST -H 'Content-Type: application/json' \
    -d '{"server_id":"Ainalrami"}' "$B/select"
# Commercial WG (Mullvad / IVPN / AzireVPN): same shape —
#   /providers/{mullvad,ivpn,azirevpn}/setup {"credential":"…"} → /pick-fastest → /select
# Then enable the VPN (kill_switch recommended):
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X PUT -H 'Content-Type: application/json' \
    -d '{"vpn":{"provider":"airvpn","enabled":true,"kill_switch":true}}' \
    "https://$AEON_HOST/api/network"
```

AirVPN modes: `wireguard` (a WG tunnel), `openvpn` (a VPN), **`openvpn_ssl`**
(OpenVPN inside stunnel TLS → looks like plain HTTPS), **`openvpn_ssh`**
(OpenVPN inside an SSH tunnel → looks like an SSH session). Valid top-level
`provider` values: `none`, `tailscale`, `wireguard`, `openvpn`, `mullvad`,
`ivpn`, `azirevpn`, `airvpn`.

Confirm a tunnel came up with `GET /api/network/vpn/status` (bootstrap %, peer
list, public IP + country).

## Tor

`split_tunnel` routes only `.onion` via Tor; `transparent` routes all TCP.
`over_vpn:true` nests Tor inside the VPN — **only works through AirVPN
`openvpn_ssl` / `openvpn_ssh`**, not commercial WireGuard exits (their IPs are
tar-pitted by the Tor network).

```bash
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X PUT -H 'Content-Type: application/json' \
    -d '{"tor":{"enabled":true,"mode":"split_tunnel","over_vpn":true}}' \
    "https://$AEON_HOST/api/network"
# Change identity (new Tor circuit / SIGNAL NEWNYM):
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST "https://$AEON_HOST/api/network/vpn/rotate"
```

**`.onion` in a browser:** the Pi's Tor only serves apps that resolve `.onion`
through the **OS resolver** — `curl`, or Firefox with
`network.dns.blockDotOnion=false` + Secure DNS OFF. **Brave / Tor Browser
bundle their own Tor** and bypass the Pi — keep the Pi in `split_tunnel` (not
`transparent`) so their Tor isn't double-wrapped.

## I2P

Always proxy-based — browsers must point at the daemon's HTTP proxy
(`http://<aeon-ip>:4444`); there's no transparent mode.

```bash
# Read i2pd status (installed, running, bound addresses, browser-hint URLs)
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/i2p/status"

# Enable + optional outproxy + over-VPN
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X PUT -H 'Content-Type: application/json' \
    -d '{"i2p":{"enabled":true,"over_vpn":false,"outproxy":"exit.stormycloud.i2p"}}' \
    "https://$AEON_HOST/api/network"
```

## DNS (DNSCrypt)

DNSCrypt v2 wraps queries in an authenticated, encrypted tunnel and doesn't
leak SNI like DoH. **Auto** mode filters the resolver catalog by criteria you
set and load-balances the lowest-latency matches; **specific** pins one named
resolver. Optional **anonymized relays** route queries so the resolver never
sees the client IP.

```bash
# Auto + strict criteria + min trust 4
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X PUT -H 'Content-Type: application/json' \
    -d '{"dnscrypt":{"enabled":true,"server_mode":"auto",
         "auto_criteria":{"no_logs":true,"dnssec":true,"no_filter":true,
                          "outside_five_eyes":true,"min_trust_score":4}}}' \
    "https://$AEON_HOST/api/network"

# Anonymized relays on, auto-picked by criteria
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X PUT -H 'Content-Type: application/json' \
    -d '{"dnscrypt":{"anon_relays":{"enabled":true,"mode":"auto",
         "criteria":{"no_logs":true,"outside_five_eyes":true,"dnssec":true}}}}' \
    "https://$AEON_HOST/api/network"
```

(Flipping the VPN provider to `tor` auto-sets `force_tcp` and prefers
Tor-reachable resolver ports.)

## WiFi

The device's own uplink — scan, join, forget.

```bash
# Current state (connected SSID, radio on/off, AP-fallback flag)
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/wifi/state"

# Scan nearby networks
curl -sk -u "$AEON_USER:$AEON_PASSWD" "https://$AEON_HOST/api/wifi/scan"

# Connect (persisted across reboots; omit psk for open networks)
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X POST -H "Content-Type: application/json" \
    -d '{"ssid": "my-home", "psk": "secret"}' \
    "https://$AEON_HOST/api/wifi/connect"

# Forget a known network
curl -sk -u "$AEON_USER:$AEON_PASSWD" -X DELETE "https://$AEON_HOST/api/wifi/known"
```

## MCP

Over MCP: `network_status`, `vpn_state`, `set_vpn_provider`,
`vpn_providers_catalog`, `vpn_provider_state`, `vpn_provider_select`,
`vpn_provider_pick_fastest`, `dnscrypt_state`, `set_dnscrypt_criteria`,
`set_anonymized_relays`, `set_tor`, `set_i2p`, `i2p_status`, `wifi_state`,
`wifi_scan`, `wifi_connect`. See `references/mcp.md`. Two AirVPN/Tor actions are
**REST-only** (no MCP tool): the AirVPN config **generator**
(`POST /api/network/vpn/providers/airvpn/generate`) and Tor **change-identity**
(`POST /api/network/vpn/rotate`).
