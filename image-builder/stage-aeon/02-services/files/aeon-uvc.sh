#!/bin/bash
# aeon-uvc — gate + launch the UVC webcam feeder.
#
# The kernel UVC gadget function (added by aeon-hid when uvc.toml enabled=true)
# only exposes a /dev/videoN SINK; this feeder pumps Cam0 (IMX477) frames into
# it, opening the camera only while the host is actively viewing the webcam.
#
# Clean-skip semantics for systemd (Type=simple, Restart=on-failure):
#   * webcam disabled / no camera  → exit 0 (done, no restart)
#   * enabled but gadget node not bound yet → exit 1 (retry until aeon-hid binds)
#   * ready → exec the feeder (runs until the gadget unbinds / it crashes)
set -u
CONF=/etc/aeon/uvc.toml

val() {  # read a `key = value` from uvc.toml, stripping comments/space
    grep -E "^[[:space:]]*$1[[:space:]]*=" "$CONF" 2>/dev/null | head -1 \
        | sed 's/.*=[[:space:]]*//; s/[[:space:]]*#.*//; s/["[:space:]]*$//; s/^"//'
}

[ "$(val enabled)" = "true" ] || { echo "aeon-uvc: disabled in uvc.toml; exiting"; exit 0; }

# Find the gadget node whose function_name is uvc.usb0 (NOT the IMX477 capture
# node — this is the USB-facing sink aeon-hid created).
node=""
for v in /sys/class/video4linux/video*; do
    [ "$(cat "$v/function_name" 2>/dev/null)" = "uvc.usb0" ] && node="/dev/$(basename "$v")" && break
done
if [ -z "$node" ]; then
    echo "aeon-uvc: no uvc.usb0 gadget node yet (aeon-hid not bound with UVC?) — retrying"
    exit 1   # transient: retry via Restart=on-failure until the node appears
fi

if ! command -v uvc-gadget >/dev/null 2>&1; then
    echo "aeon-uvc: /usr/local/bin/uvc-gadget not installed — build it on-device (see docs/UVC_WEBCAM.md); exiting"
    exit 0
fi

# Build the uvc-gadget source args for the selected source. camera-csi uses
# libcamera (-c); hdmi-csi / cam-link-usb are plain v4l2 devices (-d).
src="$(val source)"; src="${src:-camera-csi}"
case "$src" in
    camera-csi)
        if ! rpicam-hello --list-cameras 2>/dev/null | grep -qiE 'imx|ov'; then
            echo "aeon-uvc: source=camera-csi but no libcamera camera present; exiting"; exit 0
        fi
        cam_id="$(val camera_id)"; cam_id="${cam_id:-0}"
        SRC_ARGS=(-c "$cam_id")            # libcamera source
        srcdesc="libcamera cam $cam_id"
        ;;
    hdmi-csi)
        dev="/dev/aeon-hdmi"
        [ -e "$dev" ] || dev="$(cat /run/aeon/hdmi-video 2>/dev/null)"
        if [ -z "$dev" ] || [ ! -e "$dev" ]; then
            echo "aeon-uvc: source=hdmi-csi but no /dev/aeon-hdmi node; exiting"; exit 0
        fi
        SRC_ARGS=(-d "$dev")               # v4l2 source (UYVY)
        srcdesc="v4l2 $dev (HDMI-CSI)"
        ;;
    cam-link-usb)
        dev="/dev/kvmd-video"; [ -e "$dev" ] || dev="/dev/video0"
        if [ ! -e "$dev" ]; then
            echo "aeon-uvc: source=cam-link-usb but no $dev; exiting"; exit 0
        fi
        SRC_ARGS=(-d "$dev")
        srcdesc="v4l2 $dev (USB)"
        ;;
    *) echo "aeon-uvc: unknown webcam source '$src'; exiting"; exit 0;;
esac

# uvc-gadget (Ideas-on-Board): -c = libcamera camera, -d = V4L2 source. It
# needs the FULL configfs function specifier — its bare auto-detect assumes a
# "uvc.0"-style instance name and FAILS on ours ("uvc.usb0") with "Failed to
# identify function configuration". Derive <gadget>/functions/uvc.usb0.
# [Confirmed on real Pi 5 hardware, 2026-06-12.]
GADGET="$(ls -d /sys/kernel/config/usb_gadget/*/functions/uvc.usb0 2>/dev/null | head -1 \
    | sed 's#.*/usb_gadget/##; s#/functions/uvc.usb0##')"
SPEC="${GADGET:-aeon}/functions/uvc.usb0"

echo "aeon-uvc: launching feeder (gadget=$SPEC source=$srcdesc)"
# Resolves the function to its /dev/videoN, services the host's UVC PROBE/COMMIT,
# and feeds frames from the selected source, opening it on STREAMON and releasing
# on STREAMOFF.
exec uvc-gadget "${SRC_ARGS[@]}" "$SPEC"
