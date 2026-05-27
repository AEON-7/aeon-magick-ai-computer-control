#!/bin/bash
# aeon-undervolt-watchdog — sample vcgencmd get_throttled every minute.
# If under-voltage or throttling has occurred since boot (any sticky bit
# set), log loudly and (optionally, gated on a config flag) take
# self-healing action.
#
# Bits we care about:
#   0x00001  under-voltage right now
#   0x00002  ARM frequency capped right now
#   0x00004  throttling right now
#   0x10000  under-voltage occurred since boot
#   0x20000  ARM frequency cap occurred since boot
#   0x40000  throttling occurred since boot

set -u
STATE_FILE=/var/lib/aeon/undervolt.state
mkdir -p "$(dirname "$STATE_FILE")"
LOG_TAG="aeon-undervolt"

log() { logger -t "$LOG_TAG" -- "$*"; }

raw=$(/usr/bin/vcgencmd get_throttled 2>/dev/null || true)
val=${raw#throttled=}
# Strip 0x prefix and parse as hex
n=$(( ${val:-0} ))

prev=0
if [[ -f "$STATE_FILE" ]]; then
    prev=$(<"$STATE_FILE")
fi
echo "$n" > "$STATE_FILE"

# Bit checks
uv_now=$(( n & 0x1 ))
throttle_now=$(( n & 0x4 ))
uv_since_boot=$(( n & 0x10000 ))
throttle_since_boot=$(( n & 0x40000 ))

if (( uv_now )); then
    log "ALERT: UNDER-VOLTAGE RIGHT NOW (throttled=$val) — power source can't keep up with current load"
fi
if (( throttle_now )); then
    log "ALERT: throttling active now (throttled=$val)"
fi

# Edge-trigger on the sticky bits — only log the first time they go
# from 0 to set, so we don't spam every minute after a single event.
if (( uv_since_boot )) && ! (( prev & 0x10000 )); then
    log "first under-voltage event since boot (sticky bit set, throttled=$val)"
fi
if (( throttle_since_boot )) && ! (( prev & 0x40000 )); then
    log "first throttle event since boot (sticky bit set, throttled=$val)"
fi

exit 0
