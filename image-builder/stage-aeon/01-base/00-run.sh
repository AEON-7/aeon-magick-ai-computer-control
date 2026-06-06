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
# Tailscale via their official repo (Debian Bookworm)
curl -fsSL https://pkgs.tailscale.com/stable/raspbian/bookworm.noarmor.gpg \
    | tee /usr/share/keyrings/tailscale-archive-keyring.gpg > /dev/null
curl -fsSL https://pkgs.tailscale.com/stable/raspbian/bookworm.tailscale-keyring.list \
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
