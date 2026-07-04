#!/bin/bash
# Build the AEON Orb server container. Stages the cross-compiled supervisor
# binaries + the web build + helper scripts into the build context, then builds
# a multi-arch image (linux/arm64 for Mac + DGX Spark, linux/amd64 for x86).
#
#   ./build.sh              # build linux/arm64 + linux/amd64, load arm64 locally
#   ./build.sh --push TAG   # build both + push to a registry as TAG
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
TAG="${2:-aeon-magick/orb-server:latest}"

echo "== 1. cross-compile the supervisor (arm64 + amd64) =="
( cd "$REPO"
  rustup target add aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu >/dev/null 2>&1 || true
  cross build --release --target aarch64-unknown-linux-gnu -p aeon-supervisor
  cross build --release --target x86_64-unknown-linux-gnu  -p aeon-supervisor )
mkdir -p "$HERE/bin/arm64" "$HERE/bin/amd64"
cp "$REPO/target/aarch64-unknown-linux-gnu/release/aeon-supervisor" "$HERE/bin/arm64/aeon-supervisor"
cp "$REPO/target/x86_64-unknown-linux-gnu/release/aeon-supervisor"  "$HERE/bin/amd64/aeon-supervisor"
chmod 0755 "$HERE/bin/arm64/aeon-supervisor" "$HERE/bin/amd64/aeon-supervisor"

echo "== 2. build the web UI =="
( cd "$REPO/aeon-web" && [ -d node_modules ] || npm ci; npm run build )
rm -rf "$HERE/web"; cp -R "$REPO/aeon-web/build" "$HERE/web"

echo "== 3. stage the cross-platform helper scripts =="
SFILES="$REPO/image-builder/stage-aeon/02-services/files"
mkdir -p "$HERE/scripts"
cp "$SFILES/aeon-ipfs.sh"    "$HERE/scripts/aeon-ipfs"
cp "$SFILES/aeon-storage.sh" "$HERE/scripts/aeon-storage"
cp "$SFILES/aeon-nas.sh"     "$HERE/scripts/aeon-nas"
chmod 0755 "$HERE/scripts/aeon-ipfs" "$HERE/scripts/aeon-storage" "$HERE/scripts/aeon-nas"

echo "== 4. buildx multi-arch =="
docker buildx inspect aeon-builder >/dev/null 2>&1 || docker buildx create --name aeon-builder --use >/dev/null
if [ "${1:-}" = "--push" ]; then
  docker buildx build --builder aeon-builder --platform linux/arm64,linux/amd64 -t "$TAG" --push "$HERE"
  echo "pushed $TAG (linux/arm64 + linux/amd64)"
else
  # --load can only emit one arch; load the host arch for local testing.
  HOSTARCH="$(uname -m)"; [ "$HOSTARCH" = "x86_64" ] && PLAT=linux/amd64 || PLAT=linux/arm64
  docker buildx build --builder aeon-builder --platform "$PLAT" -t "$TAG" --load "$HERE"
  echo "built + loaded $TAG ($PLAT). Run: docker compose -f \"$HERE/docker-compose.yml\" up -d"
fi
