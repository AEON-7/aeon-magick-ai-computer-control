#!/bin/bash
# aeon-storage — external USB/SSD storage for the Orb.
#
#   list                 JSON of external (removable/hotplug) disks + status:
#                        "available" (Linux fs, usable now), "needs_prepare"
#                        (empty/foreign fs), or "in_use" (adopted by the Orb).
#   prepare <dev> FORMAT DESTRUCTIVE — wipe <dev>, GPT + one ext4 partition
#                        labelled AEON-DATA. Refuses the boot disk / any
#                        non-removable / system-mounted device. The literal
#                        second arg FORMAT is a required server-side safety gate.
#   use <dev>            Adopt a prepared drive as the Orb's data store: mount it
#                        (persisted by UUID, nofail), migrate + BIND-MOUNT the
#                        IPFS repo, model library, catalog + NAS share onto it so
#                        every hardcoded /var/lib/aeon path keeps working.
#   release              Revert to internal storage (unbind, drop fstab lines).
#   status               The adopted drive (if any) + bytes free/used.
#
# Privileged block ops live here (invoked as root by the supervisor), mirroring
# aeon-ipfs. Everything is guarded so a stray call can never touch the SD card.
set -uo pipefail

DATA=/var/lib/aeon
MNT=/mnt/aeon-data
LABEL=AEON-DATA
STATE=/etc/aeon/storage.state
# Dirs relocated onto the external drive (bind-mounted back to $DATA/<dir>).
BOUND_DIRS="ipfs model-library ipfs-models nas-share"

log() { echo "aeon-storage: $*" >&2; }

# The whole-disk device backing / — NEVER formattable.
boot_disk() {
  local src pk
  src=$(findmnt -n -o SOURCE / 2>/dev/null) || return 0
  pk=$(lsblk -n -o PKNAME "$src" 2>/dev/null | head -1)
  [ -n "$pk" ] && echo "/dev/$pk"
}

cmd_list() {
  local boot tmp; boot=$(boot_disk); tmp=$(mktemp)
  # NB: feed lsblk JSON via a temp FILE, not a pipe — `python3 -` already reads
  # its program from stdin (the heredoc), so a piped stdin would be swallowed.
  lsblk -J -b -o NAME,PATH,SIZE,TYPE,TRAN,FSTYPE,MOUNTPOINT,MODEL,VENDOR,HOTPLUG,RM,RO,LABEL,UUID 2>/dev/null > "$tmp"
  python3 - "${boot:-}" "$MNT" "$tmp" <<'PY'
import json, sys
boot, mnt, tmp = sys.argv[1], sys.argv[2], sys.argv[3]
data = json.load(open(tmp))
LINUX_FS = {"ext4", "ext3", "ext2", "xfs", "btrfs"}
out = []
for d in data.get("blockdevices", []):
    if d.get("type") != "disk":
        continue
    path = d.get("path", "")
    if boot and path == boot:
        continue
    # "External" = USB-attached (the reliable signal — many USB SSDs report
    # hotplug=rm=0 via their SATA/NVMe bridge) OR a removable/hotplug device.
    # The boot disk is already excluded above, so a USB-booted Pi is safe.
    if not (d.get("tran") == "usb" or d.get("hotplug") or d.get("rm")):
        continue
    parts = d.get("children") or [d]
    fss = [(c.get("fstype"), c.get("mountpoint"), c.get("path"), c.get("label")) for c in parts]
    linux = [f for f in fss if f[0] in LINUX_FS]
    is_aeon = any(f[3] == "AEON-DATA" for f in fss)
    mounted_here = any(f[1] == mnt for f in fss)
    if mounted_here:
        status = "in_use"
    elif linux:
        status = "available"
    else:
        status = "needs_prepare"
    out.append({
        "path": path,
        "size_bytes": int(d.get("size") or 0),
        "model": (d.get("model") or "").strip(),
        "vendor": (d.get("vendor") or "").strip(),
        "fs": [f[0] for f in fss if f[0]],
        "label": next((f[3] for f in fss if f[3]), None),
        "status": status,
        "is_aeon": is_aeon,
        "part": (linux[0][2] if linux else (parts[0].get("path") if parts else path)),
    })
print(json.dumps({"ok": True, "disks": out, "boot_disk": boot, "mount": mnt}))
PY
  rm -f "$tmp"
}

# Reject anything that isn't a safe, external, non-system target.
_guard_dev() {
  local dev="$1" boot hot rm tran
  [ -b "$dev" ] || { log "not a block device: $dev"; return 1; }
  boot=$(boot_disk)
  [ -n "$boot" ] && [ "$dev" = "$boot" ] && { log "refusing to touch the boot disk"; return 1; }
  tran=$(lsblk -dn -o TRAN "$dev" 2>/dev/null | tr -d ' ')
  read -r hot rm < <(lsblk -dn -o HOTPLUG,RM "$dev" 2>/dev/null)
  [ "$tran" = usb ] || [ "${hot:-0}" = 1 ] || [ "${rm:-0}" = 1 ] \
    || { log "refusing: $dev is not a USB/removable device"; return 1; }
  # No child (or the device itself) mounted at a system path.
  if lsblk -nr -o MOUNTPOINT "$dev" 2>/dev/null | grep -qE '^/($|boot)'; then
    log "refusing: $dev is mounted at a system path"; return 1
  fi
  return 0
}

cmd_prepare() {
  local dev="${1:-}" confirm="${2:-}"
  [ -n "$dev" ] || { log "usage: prepare <dev> FORMAT"; return 1; }
  [ "$confirm" = FORMAT ] || { log "refusing: pass the literal FORMAT confirmation"; return 1; }
  _guard_dev "$dev" || return 1
  # Unmount anything currently mounted from the device.
  local m
  while read -r m; do [ -n "$m" ] && umount "$m" 2>/dev/null || true; done \
    < <(lsblk -nr -o MOUNTPOINT "$dev" 2>/dev/null)
  wipefs -a "$dev" >/dev/null 2>&1 || true
  parted -s "$dev" mklabel gpt mkpart primary ext4 0% 100% >/dev/null 2>&1 \
    || { log "partitioning failed"; return 1; }
  sync; partprobe "$dev" 2>/dev/null || true; sleep 1
  local part
  part=$(lsblk -nr -o PATH,TYPE "$dev" 2>/dev/null | awk '$2=="part"{print $1; exit}')
  [ -b "$part" ] || { log "new partition not found"; return 1; }
  mkfs.ext4 -F -q -L "$LABEL" "$part" >/dev/null 2>&1 || { log "mkfs.ext4 failed"; return 1; }
  echo "$part"
}

# Pick the partition to adopt: the arg itself if it's a partition, else the
# AEON-DATA-labelled partition on the disk, else its first Linux-fs partition.
# Adopting an existing ext4 drive is non-destructive — we only add subdirs.
_usable_part() {
  local dev="$1" part
  if [ "$(lsblk -dn -o TYPE "$dev" 2>/dev/null)" = part ]; then echo "$dev"; return; fi
  part=$(lsblk -rn -o PATH,LABEL "$dev" 2>/dev/null | awk '$2=="AEON-DATA"{print $1; exit}')
  [ -z "$part" ] && part=$(lsblk -rn -o PATH,FSTYPE,TYPE "$dev" 2>/dev/null \
    | awk '$3=="part" && $2 ~ /^(ext4|ext3|ext2|xfs|btrfs)$/{print $1; exit}')
  echo "$part"
}

cmd_use() {
  local dev="${1:-}" part uuid d fst
  [ -n "$dev" ] || { log "usage: use <dev>"; return 1; }
  part=$(_usable_part "$dev")
  [ -b "$part" ] || { log "no usable Linux partition on $dev — prepare it (ext4) first"; return 1; }
  # The IPFS repo needs POSIX ownership/perms, so the fs must be Linux-native.
  fst=$(lsblk -dn -o FSTYPE "$part" 2>/dev/null | tr -d ' ')
  case "$fst" in ext4|ext3|ext2|xfs|btrfs) ;; *) log "partition fs '${fst:-none}' can't host the data store — prepare it (ext4) first"; return 1;; esac
  uuid=$(blkid -s UUID -o value "$part" 2>/dev/null)
  [ -n "$uuid" ] || { log "could not read filesystem UUID"; return 1; }
  systemctl stop aeon-ipfs.service 2>/dev/null || true
  install -d "$MNT"
  grep -q " $MNT " /etc/fstab || echo "UUID=$uuid $MNT $fst defaults,nofail,noatime 0 2" >> /etc/fstab
  mountpoint -q "$MNT" || mount "$MNT" 2>/dev/null || mount "UUID=$uuid" "$MNT" \
    || { log "mount failed"; return 1; }
  # Transient scratch (download quarantine + import drafts) must NOT migrate — it
  # can be tens of GB of orphaned junk that would slow the copy for no reason.
  # Prune it from the source first (aeon-ipfs is stopped; it's recreated on demand).
  rm -rf "$DATA/ipfs-models/staging" 2>/dev/null || true
  for d in $BOUND_DIRS; do
    install -d "$MNT/$d" "$DATA/$d"
    # First adoption: copy existing internal data onto the drive (once).
    if [ -z "$(ls -A "$MNT/$d" 2>/dev/null)" ] && [ -n "$(ls -A "$DATA/$d" 2>/dev/null)" ]; then
      cp -a "$DATA/$d/." "$MNT/$d/" 2>/dev/null || true
    fi
    # `x-systemd.requires-mounts-for=$MNT` makes the bind WAIT for the (slow USB)
    # drive at boot instead of being silently skipped by `nofail` — otherwise a
    # reboot could expose the empty SD dir underneath and look like data loss.
    grep -q " $DATA/$d " /etc/fstab || echo "$MNT/$d $DATA/$d none bind,nofail,x-systemd.requires-mounts-for=$MNT 0 0" >> /etc/fstab
    mountpoint -q "$DATA/$d" || mount --bind "$MNT/$d" "$DATA/$d"
  done
  id aeon-ipfs >/dev/null 2>&1 && chown -R aeon-ipfs:aeon-ipfs "$DATA/ipfs" 2>/dev/null || true
  printf '{"uuid":"%s","dev":"%s"}\n' "$uuid" "$part" > "$STATE"
  systemctl start aeon-ipfs.service 2>/dev/null || true
  echo "$MNT"
}

cmd_release() {
  local d
  systemctl stop aeon-ipfs.service 2>/dev/null || true
  for d in $BOUND_DIRS; do
    mountpoint -q "$DATA/$d" && umount "$DATA/$d" 2>/dev/null || true
    sed -i "\| $DATA/$d |d" /etc/fstab 2>/dev/null || true
  done
  mountpoint -q "$MNT" && umount "$MNT" 2>/dev/null || true
  sed -i "\| $MNT |d" /etc/fstab 2>/dev/null || true
  rm -f "$STATE"
  systemctl start aeon-ipfs.service 2>/dev/null || true
  echo "released"
}

cmd_status() {
  local uuid="" dev="" free=0 total=0 adopted=false
  if [ -f "$STATE" ]; then
    uuid=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("uuid",""))' "$STATE" 2>/dev/null)
    dev=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("dev",""))' "$STATE" 2>/dev/null)
  fi
  if mountpoint -q "$MNT"; then
    adopted=true
    read -r total free < <(df -PB1 "$MNT" 2>/dev/null | awk 'NR==2{print $2, $4}')
  fi
  printf '{"ok":true,"adopted":%s,"mount":"%s","uuid":"%s","dev":"%s","free_bytes":%s,"total_bytes":%s}\n' \
    "$adopted" "$MNT" "${uuid:-}" "${dev:-}" "${free:-0}" "${total:-0}"
}

case "${1:-}" in
  list)    cmd_list ;;
  prepare) shift; cmd_prepare "$@" ;;
  use)     shift; cmd_use "$@" ;;
  release) cmd_release ;;
  status)  cmd_status ;;
  *) echo "usage: aeon-storage {list|prepare <dev> FORMAT|use <dev>|release|status}" >&2; exit 1 ;;
esac
