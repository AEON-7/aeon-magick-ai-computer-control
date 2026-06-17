#!/bin/bash
# aeon-camera-select — pick the Pi 5 CSI camera sensor overlay from a boot-partition
# flag, so ONE image supports ANY single Pi camera on CAM0 (IMX708 Camera Module 3
# incl. Wide, IMX477 HQ, IMX219 v2, …) with no re-flash and no hand-editing config.txt.
#
# WHY THIS EXISTS: the Orb's HDMI-to-CSI bridge (tc358743 on CAM1) is NOT
# auto-detectable, which forces `camera_auto_detect=0` in config.txt — so the
# firmware can't auto-pick the camera, and the sensor on CAM0 must be named by an
# explicit `dtoverlay=<sensor>,cam0` line. This service lets you choose that sensor
# by dropping a one-word file on the FAT /boot/firmware partition (the same idiom
# as `aeon-force-ap`), instead of editing config.txt by hand.
#
# USAGE — write the sensor name into /boot/firmware/aeon-camera, then reboot once:
#     echo imx708 > /boot/firmware/aeon-camera   # Camera Module 3 (+ Wide)  [baked default]
#     echo imx477 > /boot/firmware/aeon-camera   # HQ Camera
#     echo imx219 > /boot/firmware/aeon-camera   # Camera v2
#     echo off    > /boot/firmware/aeon-camera   # no CSI camera fitted
# With NO flag file, the baked default (imx708) is used. The flag may stay in place;
# it's only acted on when it disagrees with the current config.
#
# SAFETY: loop-safe by construction. It parses the CURRENTLY-active CAM0 sensor each
# boot and reboots ONLY when that differs from the requested one; after the reboot
# the two match and it exits. Before rebooting it re-verifies the rewrite actually
# landed (restoring a backup and NOT rebooting if it didn't). It only ever touches
# the CAM0 camera overlay — never the tc358743 (cam1) HDMI-bridge line.
set -u

BOOT=/boot/firmware
CONFIG="$BOOT/config.txt"
FLAG="$BOOT/aeon-camera"
LOG() { echo "aeon-camera-select: $*" >&2; }

[ -f "$FLAG" ]   || { LOG "no $FLAG flag; using baked default overlay"; exit 0; }
[ -f "$CONFIG" ] || { LOG "no $CONFIG; nothing to do"; exit 0; }

# Supported CAM0 sensor overlays (+ 'off' = comment the camera overlay out).
ALLOW="imx708 imx477 imx219 imx519 imx296 imx290 imx327 imx462 imx500 ov5647 ov9281 ov64a40 off"

want="$(tr 'A-Z' 'a-z' < "$FLAG" | tr -d '[:space:]' | head -c 32)"
[ -n "$want" ] || { LOG "empty flag; ignoring"; exit 0; }
case " $ALLOW " in
    *" $want "*) : ;;
    *) LOG "unsupported camera '$want' (allowed: $ALLOW); ignoring"; exit 0 ;;
esac

# The CAM0 sensor currently ACTIVE (uncommented) in config.txt, or empty if none.
# Anchored to ',cam0' so the tc358743 ',cam1' bridge line can never match.
active_sensor() {
    grep -E '^[[:space:]]*dtoverlay=(imx|ov)[0-9a-z]+,cam0' "$CONFIG" 2>/dev/null \
      | head -1 | sed -E 's@^[[:space:]]*dtoverlay=([a-z0-9]+),cam0.*@\1@'
}
cur="$(active_sensor)"

# Loop guard: already in the requested state → done.
if [ "$want" = off ]; then
    [ -z "$cur" ] && { LOG "camera already disabled; no change"; exit 0; }
else
    [ "$cur" = "$want" ] && { LOG "camera already '$want'; no change"; exit 0; }
fi

LOG "request '$want' (current: '${cur:-none}') — rewriting $CONFIG"
cp -f "$CONFIG" "$CONFIG.aeon-camera.bak" 2>/dev/null || true

if [ "$want" = off ]; then
    # Comment out the active CAM0 camera overlay (leave the tc358743/cam1 line alone).
    sed -i -E 's@^([[:space:]]*)(dtoverlay=(imx|ov)[0-9a-z]+,cam0.*)$@\1#\2@' "$CONFIG"
else
    if grep -qE '^[[:space:]#]*dtoverlay=(imx|ov)[0-9a-z]+,cam0' "$CONFIG"; then
        # Replace the existing (active OR commented) CAM0 overlay in place.
        sed -i -E "s@^[[:space:]#]*dtoverlay=(imx|ov)[0-9a-z]+,cam0.*\$@dtoverlay=${want},cam0@" "$CONFIG"
    elif grep -qE '^dtoverlay=tc358743' "$CONFIG"; then
        # No camera line yet — add one right after the bridge line. awk (not sed
        # 'a') so it's sed-version-independent and inserts exactly once.
        awk -v line="dtoverlay=${want},cam0" \
            '{print} /^dtoverlay=tc358743/ && !done {print line; done=1}' \
            "$CONFIG" > "$CONFIG.tmp.$$" && cat "$CONFIG.tmp.$$" > "$CONFIG"
        rm -f "$CONFIG.tmp.$$"
    else
        printf '\ndtoverlay=%s,cam0\n' "$want" >> "$CONFIG"
    fi
fi

# Verify the rewrite landed before committing to a reboot.
new="$(active_sensor)"
if { [ "$want" = off ] && [ -z "$new" ]; } || [ "$new" = "$want" ]; then
    LOG "CAM0 camera set to '$want'; rebooting once to apply"
    sync
    systemctl reboot
else
    LOG "ERROR: rewrite to '$want' did not take (active now '${new:-none}'); restoring backup, NOT rebooting"
    [ -f "$CONFIG.aeon-camera.bak" ] && cp -f "$CONFIG.aeon-camera.bak" "$CONFIG"
    exit 1
fi
