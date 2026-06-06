#!/bin/bash
# Cross-build the OrbNet homeserver (Conduit, a lightweight Rust Matrix server)
# from source with the OrbNet patch, and stage it where pi-gen installs it as
# /usr/local/bin/aeon-conduit.
#
# The patch (scripts/conduit-orbnet.patch) adds danger_accept_invalid_certs to
# Conduit's federation HTTP client: OrbNet federates over Tor onion services, so
# the .onion address authenticates the peer and Tor already encrypts the path —
# the TLS cert is redundant. Every Orb self-signs its own onion cert; no shared
# CA, no shipped private key, decentralized + scales to any number of Orbs.
#
# Run on the build host (needs `cross` + Docker), like scripts/build-binaries.sh.

set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="aarch64-unknown-linux-gnu"
OUT="${REPO}/image-builder/stage-aeon/01-base/files/bin"
SRC="${TMPDIR:-/tmp}/orbnet-conduit"
CONDUIT_GIT="https://gitlab.com/famedly/conduit.git"
# Pinned so the patch always applies cleanly + the build is reproducible.
CONDUIT_REF="8def22bfb8b9b23bbd47e17772f2bd80500eacf6"   # conduit v0.11.0-alpha

mkdir -p "${OUT}"
rm -rf "${SRC}"
git clone "${CONDUIT_GIT}" "${SRC}"
git -C "${SRC}" checkout --quiet "${CONDUIT_REF}"
git -C "${SRC}" apply "${REPO}/scripts/conduit-orbnet.patch"

( cd "${SRC}" && cross build --release --target "${TARGET}" \
    --no-default-features --features backend_sqlite,conduit_bin --bin conduit )

install -m 0755 "${SRC}/target/${TARGET}/release/conduit" "${OUT}/aeon-conduit"
echo "staged: ${OUT}/aeon-conduit"
ls -lh "${OUT}/aeon-conduit"
