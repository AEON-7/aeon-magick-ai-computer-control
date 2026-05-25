#!/bin/bash -e
# Install runtime packages.

on_chroot << EOF
apt-get update
apt-get install -y --no-install-recommends \
    ustreamer \
    v4l-utils \
    network-manager \
    avahi-daemon \
    iw \
    wireless-tools \
    wpasupplicant \
    usbutils \
    pciutils \
    ethtool \
    ca-certificates \
    curl \
    jq \
    iptables \
    dnsmasq \
    hostapd
# Tailscale via their official repo (Debian Bookworm)
curl -fsSL https://pkgs.tailscale.com/stable/raspbian/bookworm.noarmor.gpg \
    | tee /usr/share/keyrings/tailscale-archive-keyring.gpg > /dev/null
curl -fsSL https://pkgs.tailscale.com/stable/raspbian/bookworm.tailscale-keyring.list \
    | tee /etc/apt/sources.list.d/tailscale.list
apt-get update
apt-get install -y tailscale
EOF
