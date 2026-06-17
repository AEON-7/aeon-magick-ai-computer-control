#!/bin/bash
# Configure the Geekworm X1301 (Toshiba TC358743) HDMI-to-CSI-2 capture
# pipeline on a Raspberry Pi 5.
#
# Why this is needed: on the Pi 5 the camera frontend is `rp1-cfe` (not the
# Pi 4's unicam), and the TC358743 is a Media-Controller device — opening its
# /dev/video node cold yields 0x0 / no frames until (1) an EDID is loaded so
# the HDMI source negotiates a mode, (2) the incoming DV timings are latched,
# and (3) the MC graph is wired. RPi engineer 6by9: "Pi5 always requires Media
# Controller to configure the pipeline."
#
# Contract: this script RELIABLY loads the EDID, discovers the post-renumber
# device nodes, and publishes them (/run/aeon/hdmi-{video,subdev,media} + a
# stable /dev/aeon-hdmi symlink the streamer points at). The media-ctl format
# wiring is best-effort (|| true) — it's the documented on-device tuning point
# if capture comes up black (see the post-flash checklist).
#
# Exit codes: 0 = configured OR definitively nothing to do (not a Pi 5).
#             3 = Pi 5 but the TC358743 isn't visible yet (caller retries).
# No-op on Pi 4 and on a Pi 5 with no X1301 attached (e.g. camera-only).
set -u

log() { echo "aeon-hdmi-csi: $*"; }

grep -qi "Raspberry Pi 5" /proc/device-tree/model 2>/dev/null || { log "not a Pi 5; skipping"; exit 0; }

# 1. Find the /dev/mediaN that owns the tc358743 bridge.
MEDIA=""
for m in /dev/media*; do
    [ -e "$m" ] || continue
    if media-ctl -d "$m" -p 2>/dev/null | grep -q 'tc358743'; then MEDIA="$m"; break; fi
done
if [ -z "$MEDIA" ]; then
    log "no tc358743 on any /dev/media* yet (X1301 not attached / node late)"
    exit 3
fi

TOPO="$(media-ctl -d "$MEDIA" -p 2>/dev/null)"
# The capture video node hangs off the rp1-cfe CSI channel (scoped to THIS
# media device, so it's the tc358743's channel, not the camera's).
VIDEO="$(printf '%s\n' "$TOPO"  | awk '/rp1-cfe-csi2_ch0/{f=1} f&&/\/dev\/video/{print $NF; exit}')"
# The tc358743 CONTROL subdev (EDID + DV-timings) MUST be found by NAME. Its
# node is NOT adjacent to the tc358743 entity line in media-ctl -p output, so
# picking by topology adjacency grabs the wrong subdev (e.g. pisp-fe) and every
# S_EDID / DV-timings ioctl fails with "Inappropriate ioctl for device".
# /sys/class/video4linux/*/name is authoritative (the node is named e.g.
# "tc358743 11-000f"). [Confirmed on real Pi 5 hardware, 2026-06-11.]
SUBDEV=""
for sd in /sys/class/video4linux/v4l-subdev*; do
    case "$(cat "$sd/name" 2>/dev/null)" in
        *tc358743*) SUBDEV="/dev/$(basename "$sd")"; break;;
    esac
done
# Full entity name (e.g. "tc358743 11-000f") for media-ctl -V.
TC_ENTITY="$(printf '%s\n' "$TOPO" | sed -n 's/^- entity [0-9]*: \(tc358743[^(]*\)(.*/\1/p' | head -1 | sed 's/[[:space:]]*$//')"

if [ -z "$VIDEO" ] || [ -z "$SUBDEV" ]; then
    log "could not resolve VIDEO ($VIDEO) / SUBDEV ($SUBDEV) from media topology"
    exit 3
fi
log "media=$MEDIA video=$VIDEO subdev=$SUBDEV entity='$TC_ENTITY'"

# 2. Load EDID so the HDMI source sends a valid mode (native 1080p60). TOGGLE
#    HPD (clear, then set) so a source that's ALREADY connected re-reads the
#    new EDID and (re)starts output — a plain --set-edid often does NOT trigger
#    a re-read on an already-live link, so the source stays dark. [Confirmed
#    required on real Pi 5 hardware, 2026-06-11.]
v4l2-ctl -d "$SUBDEV" --clear-edid >/dev/null 2>&1 || true
sleep 1
# `--fix-edid-checksums` was DROPPED from the v4l-utils shipped on Pi OS Trixie:
# passing it makes v4l2-ctl abort the WHOLE command ("unrecognized option") so
# NO EDID loads and the source stays dark (the bug that made hdmi-csi show only
# a stale frame). Try it first for older v4l-utils, then fall back to a plain
# --set-edid — the shipped /etc/aeon/hdmi-edid.txt already carries valid
# checksums. [Confirmed on Pi 5 + Trixie, 2026-06-17.]
if v4l2-ctl -d "$SUBDEV" --set-edid=file=/etc/aeon/hdmi-edid.txt --fix-edid-checksums >/dev/null 2>&1 \
   || v4l2-ctl -d "$SUBDEV" --set-edid=file=/etc/aeon/hdmi-edid.txt >/dev/null 2>&1; then
    log "EDID loaded (HPD toggled)"
else
    log "EDID load failed (continuing — check /etc/aeon/hdmi-edid.txt)"
fi
sleep 2  # give the source a moment to re-read EDID + lock a mode

# 3. Latch whatever timing the source is currently sending. Harmless / no-op
#    when no signal is connected yet.
v4l2-ctl -d "$SUBDEV" --set-dv-bt-timings query >/dev/null 2>&1 || true

# 4. Wire the Media-Controller graph for the tc358743 → csi2 → rp1-cfe path.
#    Set UYVY 1920x1080 across the pads AND — critically — ENABLE the csi2
#    source → rp1-cfe-csi2_ch0 capture link, which is DISABLED by default.
#    Without that link, VIDIOC_STREAMON on the video node fails "Invalid
#    argument" (the format is right but there's no data path). [The load-bearing
#    fix, confirmed on real Pi 5 hardware, 2026-06-11.]
if [ -n "$TC_ENTITY" ]; then
    media-ctl -d "$MEDIA" -V "'${TC_ENTITY}':0 [fmt:UYVY8_1X16/1920x1080 field:none]" >/dev/null 2>&1 \
        && log "tc358743 source pad set UYVY 1920x1080" || log "tc358743 pad format set skipped"
fi
# csi2 receiver sink (from tc358743) + source (to the CFE) pad formats:
media-ctl -d "$MEDIA" -V "'csi2':0 [fmt:UYVY8_1X16/1920x1080 field:none]" >/dev/null 2>&1 || true
media-ctl -d "$MEDIA" -V "'csi2':4 [fmt:UYVY8_1X16/1920x1080 field:none]" >/dev/null 2>&1 || true
# THE capture data-path link (disabled by default on the rp1-cfe):
if media-ctl -d "$MEDIA" -l "'csi2':4 -> 'rp1-cfe-csi2_ch0':0 [1]" >/dev/null 2>&1; then
    log "enabled csi2 → rp1-cfe-csi2_ch0 capture link"
else
    log "could not enable csi2 → CFE link (tuning point — inspect media-ctl -p)"
fi
v4l2-ctl -d "$VIDEO" --set-fmt-video=width=1920,height=1080,pixelformat=UYVY >/dev/null 2>&1 || true

# 5. Publish resolved paths + the stable symlink the streamer points at.
mkdir -p /run/aeon
printf '%s\n' "$VIDEO"  > /run/aeon/hdmi-video
printf '%s\n' "$SUBDEV" > /run/aeon/hdmi-subdev
printf '%s\n' "$MEDIA"  > /run/aeon/hdmi-media
ln -sf "$VIDEO" /dev/aeon-hdmi
log "ready: /dev/aeon-hdmi -> $VIDEO"
exit 0
