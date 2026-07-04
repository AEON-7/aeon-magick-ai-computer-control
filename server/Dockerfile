# syntax=docker/dockerfile:1
#
# AEON Magick Orb — headless SERVER release. Everything that isn't Raspberry Pi
# hardware: IPFS + Model Share, the AI Agent Dashboard (fleet / personas /
# container orchestration / model deploy + push), terminal management, tokens,
# and the MCP + REST API. No HID, no video capture, no GPIO, no host-network
# appliance. Runs on Apple Silicon, the DGX Spark (GB10), and x86 — a single
# multi-arch manifest (linux/arm64 covers Mac + DGX; linux/amd64 covers x86).
#
# Binaries are cross-compiled on the host per arch; build via ./build.sh.
FROM debian:bookworm-slim

ARG TARGETARCH
# Keep in lockstep with the Pi image bake (image-builder .../02-services/03-run.sh)
# so every release track runs the same IPFS version.
ARG KUBO_VERSION=v0.42.0

# Runtime deps for the cross-platform surface: openssh-client + rsync (Agent
# Dashboard, terminal, model push over SSH), python3 (storage/nas helpers),
# iproute2 (route lookups for pushes), curl/ca-certs (kubo + Tailscale),
# procps (pkill for the container IPFS daemon), tini (PID 1 reaping).
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl bash python3 openssh-client rsync \
      iproute2 procps tar xz-utils tini \
 && rm -rf /var/lib/apt/lists/*

# Pre-install kubo (IPFS) for the target arch so the first "enable IPFS" is
# instant and arch-correct.
RUN set -eux; \
    curl -fsSL "https://dist.ipfs.tech/kubo/${KUBO_VERSION}/kubo_${KUBO_VERSION}_linux-${TARGETARCH}.tar.gz" -o /tmp/kubo.tgz; \
    tar xzf /tmp/kubo.tgz -C /tmp; \
    install -m0755 /tmp/kubo/ipfs /usr/local/bin/ipfs; \
    rm -rf /tmp/kubo /tmp/kubo.tgz

# The supervisor binary (cross-compiled on the host: ./build.sh stages it here).
COPY bin/${TARGETARCH}/aeon-supervisor /usr/local/bin/aeon-supervisor
# The SvelteKit static UI (arch-independent).
COPY web/ /usr/share/aeon/web/
# Cross-platform helper scripts (systemd-free paths kick in inside the container).
COPY scripts/aeon-ipfs    /usr/local/bin/aeon-ipfs
COPY scripts/aeon-storage /usr/local/bin/aeon-storage
COPY scripts/aeon-nas     /usr/local/bin/aeon-nas
COPY entrypoint.sh        /entrypoint.sh
RUN chmod 0755 /usr/local/bin/aeon-supervisor /usr/local/bin/aeon-ipfs \
      /usr/local/bin/aeon-storage /usr/local/bin/aeon-nas /entrypoint.sh

# Persist admin auth + TLS cert + tokens, and the IPFS repo + model library.
VOLUME ["/etc/aeon", "/var/lib/aeon"]

# 443 = web UI + REST + MCP (HTTPS, self-signed). 8080 = IPFS gateway.
EXPOSE 443 8080

ENV AEON_SERVER=1
ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]
