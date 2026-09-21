# 🐳 Aeon Magick Orb — Docker

Headless Magick Orb for **any server** — x86, Apple Silicon, NVIDIA DGX Spark.
One public image: `linux/amd64` + `linux/arm64`.

It is the Orb console **without Pi hardware**: Intergalactic Model Share, Agent
Dashboard, terminals, tokens, MCP + REST. No USB-HID, no HDMI capture, no GPIO.

Package: **[ghcr.io/aeon-7/orb-server](https://github.com/users/AEON-7/packages/container/package/orb-server)**
· source: [`aeon-magick-ai-computer-control`](https://github.com/AEON-7/aeon-magick-ai-computer-control)
· Pi appliance: [main README](../README.md)

---

## Quick start

No clone. No build. Docker Engine 24+ (Compose v2 is optional).

```bash
docker run -d --name aeon-orb --restart unless-stopped \
  -p 8443:443 -p 8080:8080 \
  -p 4001:4001 -p 4001:4001/udp \
  -v aeon-etc:/etc/aeon \
  -v aeon-data:/var/lib/aeon \
  ghcr.io/aeon-7/orb-server:latest
```

1. Open **https://localhost:8443/** (self-signed cert — accept the warning).
2. Set an admin password in the setup wizard.
3. You're on the Agent Dashboard. IPFS + Model Share come up with the container.

**4001 tcp/udp** is the IPFS swarm. Publishing it lets other Magick Orbs (a Pi across the WAN, another container) hole-punch or dial you so Model Share catalogs and downloads work. Discovery itself uses the public Amino DHT (every Orb pins a well-known rendezvous CID) — no account, no fleet token, no need to know the other person's LAN IP. If 4001 is firewalled, catalogs may still arrive over a circuit relay; multi-GB model fetches usually need a direct path (this port, UPnP, IPv6, or hole punch).

Optional: pass `-e AEON_IPFS_ANNOUNCE=<this-machine-ipv4>` so the node advertises the host address instead of Docker's `172.x`.

Pin a version with `:v118` instead of `:latest` if you don't want surprise pulls.

### Compose (same image)

From this directory, or copy `docker-compose.yml` next to you:

```bash
docker compose up -d
```

| Port | What |
|------|------|
| `8443` → 443 | Web UI + REST + MCP (HTTPS) |
| `8080` → 8080 | IPFS gateway (once IPFS is up) |
| `4001` tcp+udp | IPFS swarm (WAN Model Share dial / hole-punch) |

State lives in two named volumes: `aeon-etc` (admin auth, TLS cert, API tokens)
and `aeon-data` (IPFS repo + model library). `docker rm` does not delete them.

Stop / start / wipe:

```bash
docker stop aeon-orb && docker rm aeon-orb          # keep volumes
docker volume rm aeon-etc aeon-data                 # factory reset
```

---

## What you get

- **IPFS + Intergalactic Model Share** — join the fleet-free model marketplace,
  host/share models, pull them, push them to connected systems.
- **AI Agent Dashboard** — DGX / gateways, persona pantheon, container
  orchestration, one-click model deploy, token telemetry.
- **Terminals** — concurrent SSH panes across your fleet.
- **Tokens + audit + MCP + REST** — any agent drives it. No SDK.

The Agent Dashboard, terminal, and model push reach other machines over
**outbound SSH**, so the container needs no extra host privileges.

## What it is not

Pi-only surface stays on the [flashable Pi 4 / Pi 5 images](../README.md):
USB-HID keyboard/mouse, video capture, GPIO, WiFi / firewall / VPN / Tor /
DNSCrypt. Those need a Raspberry Pi (or a privileged host).

---

## Reach your fleet by Tailscale name

Plain LAN / SSH addresses work with no extra setup.

To let the container use `100.x` tailnet names, join it to your tailnet —
e.g. a Tailscale sidecar with `network_mode: service:tailscale`, or Tailscale
on the host plus host networking.

---

## Build from source

Only if you are changing the Orb itself.

```bash
./build.sh                       # cross-compile + build; loads your host arch
./build.sh --push you/orb:tag    # linux/arm64 + linux/amd64, push a manifest
```

`linux/arm64` = Apple Silicon + DGX Spark. `linux/amd64` = x86.
