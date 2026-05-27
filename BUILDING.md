# Building Aeon Magick AI Computer Control on macOS

All three pieces can be built from an Apple Silicon MacBook. The pi-gen
image-bake step needs Linux semantics, but that's solved by running it
inside a container (Docker Desktop or OrbStack both work fine).

## 1. Rust daemons → aarch64-linux

You have two paths. Pick one.

### Option A — `cross` (recommended for first build)

`cross` runs the compile inside a Docker container, so you don't need any
Linux toolchains installed on your Mac. Just Docker.

```bash
# One-time
brew install --cask orbstack         # or docker desktop, your call
cargo install cross --git https://github.com/cross-rs/cross

# Build
cd ~/aeon-magick-ai-computer-control
./scripts/build-binaries.sh
# → image-builder/stage-aeon/01-base/files/bin/{aeon-streamer,aeon-hid,aeon-supervisor}
```

Pros: zero local toolchain. Easy to keep clean.
Cons: slower compiles (Docker filesystem overhead).

### Option B — native cross-compile with Homebrew toolchain

```bash
# One-time
brew tap messense/macos-cross-toolchains
brew install aarch64-unknown-linux-gnu
rustup target add aarch64-unknown-linux-gnu

# Tell cargo where the linker is. Add this to ~/.cargo/config.toml:
mkdir -p ~/.cargo
cat >> ~/.cargo/config.toml <<'EOF'

[target.aarch64-unknown-linux-gnu]
linker = "aarch64-unknown-linux-gnu-gcc"
EOF

# Build
cd ~/aeon-magick-ai-computer-control
cargo build --release --target aarch64-unknown-linux-gnu \
    -p aeon-streamer -p aeon-hid -p aeon-supervisor

# Stage where pi-gen wants them
mkdir -p image-builder/stage-aeon/01-base/files/bin
for b in aeon-streamer aeon-hid aeon-supervisor; do
    cp target/aarch64-unknown-linux-gnu/release/$b \
       image-builder/stage-aeon/01-base/files/bin/
done
```

Pros: fast incremental builds, no Docker.
Cons: more one-time setup; Homebrew tap is third-party.

Pure-Rust dependencies (no C bindings) make this work cleanly — we
intentionally shell out to `v4l2-ctl` for capture queries instead of
binding `libv4l2` so we don't need that dev library cross-compiled.

## 2. SvelteKit web UI → static `build/`

Trivial. macOS-native:

```bash
brew install node    # if you don't have it
cd ~/aeon-magick-ai-computer-control/aeon-web
npm install
./scripts/build-web.sh   # wraps `npm run build` + stages output
```

The output (`aeon-web/build/`) is plain static HTML/JS/CSS. No
server-side Node at runtime.

## 3. pi-gen image → `aeon-magick.img.xz`

Needs Linux, but pi-gen ships a Docker wrapper. Works fine on Mac.

```bash
# One-time
git clone https://github.com/RPi-Distro/pi-gen ~/pi-gen

# Each build
cp -R ~/aeon-magick-ai-computer-control/image-builder/stage-aeon ~/pi-gen/
cd ~/pi-gen
cat > config <<'EOF'
IMG_NAME=aeon-magick
RELEASE=bookworm
TARGET_HOSTNAME=aeon-magick
ENABLE_SSH=1
DISABLE_FIRST_BOOT_USER_RENAME=1
FIRST_USER_NAME=admin
FIRST_USER_PASS=aeon-default-change-me
DEPLOY_COMPRESSION=xz
# Explicit stage list. Without this, pi-gen globs `stage*` alphabetically
# and `stage-aeon` sorts BEFORE `stage0` (because '-' < '0' in ASCII), so
# our overlay would run with no rootfs underneath it and fail with
# "Previous stage rootfs not found".
STAGE_LIST="stage0 stage1 stage2 stage-aeon"
EOF
# Belt-and-suspenders: also drop SKIP markers on the heavy desktop stages
# so even if STAGE_LIST is removed they don't get pulled in.
touch stage3/SKIP stage4/SKIP stage5/SKIP \
      stage3/SKIP_IMAGES stage4/SKIP_IMAGES stage5/SKIP_IMAGES

# Build inside Docker (works on macOS)
sudo ./build-docker.sh
```

Output: `deploy/<date>-aeon-magick.img.xz`. Flash with Raspberry Pi
Imager, BalenaEtcher, or:

```bash
diskutil unmountDisk /dev/disk4
xz -d -k deploy/*-aeon-magick.img.xz
sudo dd if=deploy/*-aeon-magick.img of=/dev/rdisk4 bs=4m
```

## 4. End-to-end one-liner (once you have all the prereqs)

```bash
cd ~/aeon-magick-ai-computer-control
./scripts/build-binaries.sh && \
./scripts/build-web.sh && \
( cp -R image-builder/stage-aeon ~/pi-gen/ && cd ~/pi-gen && sudo ./build-docker.sh )
```

## Quick sanity check before cross-compiling

Want to confirm the code at least compiles for your *Mac* before going to
the trouble of cross-compile setup?

```bash
cd ~/aeon-magick-ai-computer-control
cargo check --workspace
```

This builds for your native target (aarch64-apple-darwin). It won't
produce a Pi-runnable binary, but it does verify the type-checking, which
is most of what shakes out 3rd-party API drift. We expect 1-3 errors from
crate API differences (axum-server, rcgen) that need 1-line tweaks.
