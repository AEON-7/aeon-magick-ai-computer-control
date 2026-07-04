# AEON Magick Orb — server container

A headless release of the Orb for **servers** — Apple Silicon, the NVIDIA DGX
Spark (GB10), and x86 — as a Docker image. It runs everything that isn't
Raspberry-Pi hardware:

- **IPFS + Model Share** — join the fleet-free model marketplace, host/share
  models, pull them down, and **push them to connected systems**.
- **AI Agent Dashboard** — connected systems (DGX / gateways), the persona
  pantheon, container orchestration, one-click model deploy, token telemetry.
- **Terminal management** — concurrent SSH panes across your fleet.
- **Tokens + audit + the MCP + REST API** — drive it all from an agent.

It deliberately leaves out the Pi-only surface: the USB-HID keyboard/mouse,
video capture/streaming, GPIO, and the host-network appliance (WiFi, firewall,
VPN/Tor/DNSCrypt). Those need a Raspberry Pi or a privileged host and don't
belong in a portable server container.

The Agent Dashboard, terminal, and model push all reach other machines over
**outbound SSH**, so the container needs no special host privileges.

## Build

```bash
./build.sh                       # cross-compile + build; loads your host arch locally
./build.sh --push you/orb:tag    # build linux/arm64 + linux/amd64 and push
```

`linux/arm64` covers both Apple Silicon and the DGX Spark; `linux/amd64` covers
x86 — one multi-arch manifest, no per-target images.

## Run

```bash
docker compose up -d
```

Then open **https://localhost:8443/** (self-signed cert — accept the warning),
set the admin password in the setup wizard, and you're in. The UI lands on the
Agent Dashboard (there's no KVM control page on a server).

| Port | What |
|------|------|
| `8443` → 443 | Web UI + REST + MCP (HTTPS) |
| `8080` → 8080 | IPFS gateway (once you enable IPFS) |

State persists in two named volumes: `aeon-etc` (admin auth, TLS cert, API
tokens) and `aeon-data` (IPFS repo + model library).

## Reaching your fleet by Tailscale name

To let the container address hosts by their `100.x` tailnet names, join it to
your tailnet — e.g. run `tailscale` as a sidecar and share its network
namespace (`network_mode: service:tailscale`), or run Tailscale on the host and
use host networking. Plain LAN/SSH addresses work with no extra setup.
