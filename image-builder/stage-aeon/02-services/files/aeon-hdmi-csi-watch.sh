#!/bin/bash
# aeon-hdmi-csi-watch — keep the X1301 (TC358743) HDMI capture working when the
# input signal changes AFTER boot.
#
# WHY THIS EXISTS: aeon-hdmi-csi.service is a one-shot that runs at boot. The
# TC358743 does NOT auto-adapt once it's set up: it latches the incoming
# DV-timings and the rp1-cfe media pipeline for whatever signal exists at that
# instant. So if you plug a source in AFTER boot, change its resolution, or it
# sleeps/wakes, the bridge's latched timings no longer match the live signal and
# the already-running aeon-streamer keeps capturing nothing — the classic
# "hdmi-csi only works if the source was connected at exactly the right moment".
#
# This watcher polls the bridge's LIVE DV-timings (cheap, ~2 s) and, on any
# change (connect / disconnect / resolution change), re-runs the one-shot setup
# to re-latch the new timings + re-wire the pipeline, then bounces aeon-streamer
# IFF it's on the hdmi-csi source so it re-reads the new mode. Result: plug a
# source in at any time and the feed comes up within a couple of seconds.
set -u

SUBDEV="$(cat /run/aeon/hdmi-subdev 2>/dev/null)"
# Not a TC358743 rig (no X1301) → nothing to watch.
[ -n "${SUBDEV}" ] && [ -e "${SUBDEV}" ] || { echo "aeon-hdmi-csi-watch: no tc358743 subdev; idling"; exec sleep infinity; }

# Current active resolution of the LIVE signal ("none" when no signal locked).
live_mode() {
    v4l2-ctl -d "${SUBDEV}" --query-dv-timings 2>/dev/null | awk -F: '
        /Active width/  { gsub(/ /,"",$2); w=$2 }
        /Active height/ { gsub(/ /,"",$2); h=$2 }
        END { if (w+0 > 0 && h+0 > 0) print w"x"h; else print "none" }'
}

last=""
while true; do
    cur="$(live_mode)"
    if [ "${cur}" != "${last}" ]; then
        logger -t aeon-hdmi-csi-watch "HDMI signal change: ${last:-init} -> ${cur}"
        if [ "${cur}" != "none" ]; then
            # Re-latch timings + re-wire tc358743 -> csi2 -> CFE for the new mode.
            /usr/local/bin/aeon-hdmi-csi >/dev/null 2>&1 || true
        fi
        # Only disturb the stream when the console is actually watching HDMI.
        if grep -qE '^[[:space:]]*source[[:space:]]*=[[:space:]]*"hdmi-csi"' /etc/aeon/streamer.toml 2>/dev/null; then
            systemctl restart aeon-streamer 2>/dev/null || true
        fi
        last="${cur}"
    fi
    sleep 2
done
