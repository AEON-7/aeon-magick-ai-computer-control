#!/bin/bash -e
# Install runtime packages.

on_chroot << EOF
apt-get update
apt-get install -y --no-install-recommends \
    ustreamer \
    ffmpeg \
    v4l-utils \
    network-manager \
    avahi-daemon \
    iw \
    rfkill \
    wireless-regdb \
    wireless-tools \
    wpasupplicant \
    usbutils \
    pciutils \
    i2c-tools \
    python3-smbus \
    ethtool \
    ca-certificates \
    curl \
    jq \
    iptables \
    dnsmasq-base \
    hostapd \
    wireguard-tools \
    openvpn \
    stunnel4 \
    autossh \
    sshpass \
    tor \
    obfs4proxy \
    i2pd
# Tailscale via their official repo. Derive the suite from THIS image's
# /etc/os-release (in-chroot) so the same stage works for a Bookworm (pi4) or a
# Trixie (pi5) bake — don't hardcode the codename (it would break the other track).
TSCODENAME="\$(. /etc/os-release && echo \${VERSION_CODENAME:-bookworm})"
curl -fsSL "https://pkgs.tailscale.com/stable/raspbian/\${TSCODENAME}.noarmor.gpg" \
    | tee /usr/share/keyrings/tailscale-archive-keyring.gpg > /dev/null
curl -fsSL "https://pkgs.tailscale.com/stable/raspbian/\${TSCODENAME}.tailscale-keyring.list" \
    | tee /etc/apt/sources.list.d/tailscale.list
apt-get update
apt-get install -y tailscale
# DNSCrypt-proxy is not in Pi OS Bookworm's apt sources. Install the
# upstream signed Linux arm64 binary from the official GitHub release.
# Trust comes from GitHub's TLS cert (same model we use for Tailscale).
DNSCRYPT_VERSION=2.1.16
curl -fsSL --retry 3 \
    "https://github.com/DNSCrypt/dnscrypt-proxy/releases/download/\${DNSCRYPT_VERSION}/dnscrypt-proxy-linux_arm64-\${DNSCRYPT_VERSION}.tar.gz" \
    -o /tmp/dnscrypt.tgz
tar -xzf /tmp/dnscrypt.tgz -C /tmp/
install -m 0755 /tmp/linux-arm64/dnscrypt-proxy /usr/local/bin/dnscrypt-proxy
rm -rf /tmp/dnscrypt.tgz /tmp/linux-arm64
# Standard system user + directories. aeon-net-services rewrites the
# config file when [dnscrypt].enabled is toggled.
useradd -r -s /usr/sbin/nologin -d /var/cache/dnscrypt-proxy _dnscrypt-proxy 2>/dev/null || true
install -d -m 0755 -o _dnscrypt-proxy -g _dnscrypt-proxy /etc/dnscrypt-proxy /var/cache/dnscrypt-proxy
EOF

# libcamera / rpicam stack for the Pi 5 "camera-csi" vision source (rpicam-vid).
# Bookworm renamed libcamera-apps → rpicam-apps; try the new name first, fall
# back to the old one, and never abort the bake if neither resolves (the
# camera path is optional + the user can apt-install on-device). v4l-utils
# (already installed above) provides v4l2-ctl + media-ctl for the HDMI-CSI
# bridge — no extra package needed there.
on_chroot << 'EOF'
apt-get install -y --no-install-recommends rpicam-apps \
  || apt-get install -y --no-install-recommends libcamera-apps \
  || echo "WARN: rpicam-apps/libcamera-apps unavailable — camera-csi source will need an on-device apt install"
EOF

# On-device vision AI: CPU OCR (tesseract — works on any Pi; the vision daemon's
# default backend). The Hailo runtime is deliberately NOT baked — it's installed
# chip-aware + button-driven by the Hailo AI super-app (POST /api/hailo/install →
# hailo-all for a Hailo-8/8L, hailo-h10-all for the AI HAT+ 2 / Hailo-10H), so a
# fresh image never forces the wrong runtime. See docs/HAILO.md.
on_chroot << 'EOF'
apt-get install -y --no-install-recommends tesseract-ocr \
  || echo "WARN: tesseract-ocr unavailable — vision OCR backend needs an on-device apt install"
EOF
