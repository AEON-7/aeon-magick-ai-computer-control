#!/bin/bash
# Cross-build the three Rust daemons for aarch64 and stage them where
# pi-gen expects them.

set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="aarch64-unknown-linux-gnu"
OUT="${REPO}/image-builder/stage-acursed/01-base/files/bin"

mkdir -p "${OUT}"

# Make sure the target is installed
rustup target add "${TARGET}"

# cross or cargo+linker — leave the choice to the user's env. Default
# expects `cross` (containerized cross-compile, easiest on macOS).
if command -v cross >/dev/null 2>&1; then
    BUILDER="cross"
else
    BUILDER="cargo"
    echo "warning: 'cross' not found, using stock cargo (set CC_aarch64_unknown_linux_gnu)"
fi

cd "${REPO}"
${BUILDER} build --release --target "${TARGET}" \
    --manifest-path Cargo.toml \
    -p acursed-streamer -p acursed-hid -p acursed-supervisor

for b in acursed-streamer acursed-hid acursed-supervisor; do
    install -m 0755 "target/${TARGET}/release/${b}" "${OUT}/${b}"
done

echo "built:"
ls -lh "${OUT}/"
