#!/bin/bash -e
# Symlink common HDMI USB capture devices to /dev/kvmd-video.

install -d "${ROOTFS_DIR}/etc/udev/rules.d"
cat > "${ROOTFS_DIR}/etc/udev/rules.d/99-aeon-capture.rules" <<'EOF'
# Elgato Cam Link 4K
SUBSYSTEM=="video4linux", ATTRS{idVendor}=="0fd9", ATTRS{idProduct}=="0066", ATTR{index}=="0", GROUP="video", SYMLINK+="kvmd-video"

# Generic MS2109-based dongles (USB Video Class, several VID/PIDs)
SUBSYSTEM=="video4linux", ATTRS{idVendor}=="534d", ATTRS{idProduct}=="2109", ATTR{index}=="0", GROUP="video", SYMLINK+="kvmd-video"
SUBSYSTEM=="video4linux", ATTRS{idVendor}=="1bcf", ATTRS{idProduct}=="2c99", ATTR{index}=="0", GROUP="video", SYMLINK+="kvmd-video"
EOF
