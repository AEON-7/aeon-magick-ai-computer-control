#!/bin/bash
# Build the SvelteKit web UI and stage it where pi-gen expects it.

set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
WEB_OUT="${REPO}/image-builder/stage-aeon/01-base/files/web"

cd "${REPO}/aeon-web"
[ -d node_modules ] || npm install
npm run build

rm -rf "${WEB_OUT}"
mkdir -p "${WEB_OUT}"
cp -R build/. "${WEB_OUT}/"

echo "staged web at ${WEB_OUT}"
