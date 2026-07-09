#!/bin/bash
# aeon-ups-trend — once-a-minute power/UPS snapshot for post-mortem.
# Appends one line to /var/log/aeon-ups-trend.log. Kept small; rotate by hand
# or via logrotate if it grows past a few MB.
set -u
LOG=/var/log/aeon-ups-trend.log
UPS=$(cat /run/aeon/ups.json 2>/dev/null || echo '{}')
THR=$(/usr/bin/vcgencmd get_throttled 2>/dev/null | cut -d= -f2)
LOAD=$(cut -d' ' -f1 /proc/loadavg)
printf '%s thr=%s load=%s ups=%s\n' "$(date -Is)" "${THR:-?}" "${LOAD:-?}" "$UPS" >> "$LOG"
# Cap at ~2 MB so a long-running Orb doesn't fill the rootfs.
if [[ -f "$LOG" ]] && [[ $(stat -c%s "$LOG" 2>/dev/null || echo 0) -gt 2000000 ]]; then
  tail -c 1000000 "$LOG" > "${LOG}.tmp" && mv "${LOG}.tmp" "$LOG"
fi
