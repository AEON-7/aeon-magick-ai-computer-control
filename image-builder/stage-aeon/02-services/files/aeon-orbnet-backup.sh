#!/bin/bash
# aeon-orbnet-backup — back up + restore the OrbNet service IDENTITIES so they
# survive an image flash.
#
# WHY THIS EXISTS: a flash overwrites the whole rootfs (there is no separate data
# partition), and the bake DELIBERATELY scrubs OrbNet state + mints fresh keys on
# first boot (the v100 shared-key incident). So without this, a flash permanently
# loses: the REGISTERED Mysterium node identity (0x…; re-registers as a brand-new
# node, orphaning stake/earnings/beneficiary history), the OrbNet .onion + Matrix
# homeserver, every user hidden-service .onion, and the IPFS PeerID.
#
# This captures ONLY the small identity/key/config set (NOT re-fetchable bulk data
# like the IPFS blockstore or Tor consensus cache) into ONE AES-256-CBC/PBKDF2
# encrypted tarball — the same envelope as the supervisor's config_export, so the
# same password/tooling applies. Restore runs POST-FLASH on the live device:
# it stops services, ensures the service users exist, lays the keys back with the
# correct owners+perms, bounces services in dependency order, and verifies the
# public identifiers (node address / .onion / PeerID) are unchanged.
#
# NON-CUSTODIAL: keys never leave the device except inside the encrypted tarball
# that YOU move + store securely. This script never prints key contents.
#
# Usage:
#   aeon-orbnet-backup backup  [-o OUTFILE] [--stdout]
#   aeon-orbnet-backup restore [-i INFILE]  [--dry-run] [--force]
#   aeon-orbnet-backup list    [-i INFILE]            # captured paths + public IDs
# Password: env AEON_BK_PW, else prompted. NOT recoverable — store it safely.
set -uo pipefail

BOOT=/boot/firmware
[ -d "$BOOT" ] || BOOT=/boot
DEFAULT_OUT="$BOOT/aeon-orbnet-backup.aeonbackup"
ITER=200000
MAGIC="AEON-ORBNET-BACKUP-v1"

die(){ echo "aeon-orbnet-backup: $*" >&2; exit 1; }
log(){ echo "aeon-orbnet-backup: $*" >&2; }
need_root(){ [ "$(id -u)" = 0 ] || die "must run as root (sudo)"; }

# The validated IDENTITY set (paths relative to /). Specific subpaths only, so the
# bulk re-fetchable data (mysterium mainnet/db, tor/data caches, ipfs blocks/
# datastore) is naturally excluded. Non-existent paths are skipped at pack time.
PATHS=(
  # --- all aeon config + per-service secrets (small) ---
  etc/aeon
  # --- Mysterium: THE registered node identity (highest stakes) ---
  var/lib/mysterium-node/keystore
  var/lib/mysterium-node/nodeui-pass
  etc/mysterium-node
  # --- OrbNet: the .onion secret + TLS + Conduit homeserver DB + owner/persona creds ---
  var/lib/aeon/orbnet/tor/hs
  var/lib/aeon/orbnet/tls
  var/lib/aeon/orbnet/db
  var/lib/aeon/orbnet/owner.json
  var/lib/aeon/orbnet/personas.json
  var/lib/aeon/orbnet/persona-since
  var/lib/aeon/orbnet/reg-token
  var/lib/aeon/orbnet/conduit.toml
  # --- user hidden services: each .onion secret + port mappings ---
  var/lib/aeon/onions/hs
  var/lib/aeon/onions/services.d
  # --- IPFS: the PeerID private key (config) + named IPNS keys ---
  var/lib/aeon/ipfs/config
  var/lib/aeon/ipfs/keystore
  var/lib/aeon/ipfs/datastore_spec
)

# Service users whose files we restore; restore recreates any that are missing.
declare -A USERS=(
  [mysterium-node]="/var/lib/mysterium-node"
  [aeon-orbnet]="/var/lib/aeon/orbnet"
  [aeon-onions]="/var/lib/aeon/onions"
  [aeon-ipfs]="/var/lib/aeon/ipfs"
)

get_pw(){
  if [ -n "${AEON_BK_PW:-}" ]; then printf '%s' "$AEON_BK_PW"; return; fi
  local p1 p2
  read -r -s -p "Backup password: " p1 </dev/tty >/dev/tty; echo >/dev/tty
  if [ "${1:-}" = confirm ]; then
    read -r -s -p "Confirm password: " p2 </dev/tty >/dev/tty; echo >/dev/tty
    [ "$p1" = "$p2" ] || die "passwords do not match"
  fi
  [ -n "$p1" ] || die "empty password"
  printf '%s' "$p1"
}

enc(){ openssl enc -aes-256-cbc -salt -pbkdf2 -iter "$ITER" -pass env:_AEON_PW; }
dec(){ openssl enc -d -aes-256-cbc -pbkdf2 -iter "$ITER" -pass env:_AEON_PW; }

# Public identifiers only — safe to read/print (no secret key material).
public_ids(){
  local f
  echo "# $MAGIC"
  echo "# created: $(date -u '+%Y-%m-%dT%H:%M:%SZ') host: $(hostname)"
  for f in /var/lib/mysterium-node/keystore/UTC--*; do
    [ -e "$f" ] && echo "mysterium_identity: 0x${f##*--}"
  done
  [ -f /var/lib/aeon/orbnet/tor/hs/hostname ] && \
    echo "orbnet_onion: $(cat /var/lib/aeon/orbnet/tor/hs/hostname 2>/dev/null)"
  if [ -f /var/lib/aeon/ipfs/config ]; then
    echo "ipfs_peerid: $(python3 -c 'import json,sys;print(json.load(open("/var/lib/aeon/ipfs/config")).get("Identity",{}).get("PeerID",""))' 2>/dev/null)"
  fi
  local d
  for d in /var/lib/aeon/onions/hs/*/; do
    [ -f "$d/hostname" ] && echo "user_onion: $(basename "$d")=$(cat "$d/hostname" 2>/dev/null)"
  done
}

cmd_backup(){
  need_root
  local out="$DEFAULT_OUT" to_stdout=0
  while [ $# -gt 0 ]; do case "$1" in
    -o) out="$2"; shift 2;; --stdout) to_stdout=1; shift;; *) die "bad arg: $1";; esac; done
  local present=() p
  for p in "${PATHS[@]}"; do [ -e "/$p" ] && present+=("$p"); done
  [ ${#present[@]} -gt 0 ] || die "nothing to back up (no OrbNet identity files found)"
  tmp=$(mktemp -d); trap 'rm -rf "${tmp:-}" 2>/dev/null' EXIT
  public_ids > "$tmp/MANIFEST.txt"
  export _AEON_PW; _AEON_PW=$(get_pw confirm)
  log "packing ${#present[@]} identity paths…"
  # --numeric-owner captures UIDs numerically (restore re-chowns by name). MANIFEST
  # is added from $tmp (stored as MANIFEST.txt), then the identity paths from /.
  tar --numeric-owner --ignore-failed-read -czpf "$tmp/raw.tgz" \
      -C "$tmp" MANIFEST.txt -C / "${present[@]}" 2>/dev/null || true
  [ -s "$tmp/raw.tgz" ] || die "tar produced nothing"
  enc < "$tmp/raw.tgz" > "$tmp/blob"; rm -f "$tmp/raw.tgz"
  local size; size=$(stat -c %s "$tmp/blob" 2>/dev/null)
  if [ "$to_stdout" = 1 ]; then
    cat "$tmp/blob"
  else
    install -m 600 /dev/null "$out"; cat "$tmp/blob" > "$out"
    log "wrote $out ($size bytes, root 0600)"
  fi
  unset _AEON_PW
  echo "" >&2
  log "=== captured identities (public, safe) ==="
  grep -vE '^#' "$tmp/MANIFEST.txt" >&2
  cat >&2 <<EOF

  ⚠  This file contains the Mysterium PRIVATE key, the .onion secret key(s), the
     IPFS private key, and admin/Matrix/VPN credentials — all AES-256 encrypted.
     Anyone with the file AND the password controls your registered node.
     • Store it offline + encrypted. The password is NOT recoverable.
     • Pull it OFF the device before you flash:
         scp -i ~/.ssh/aeon_magick_ed25519 admin@$(hostname -I 2>/dev/null | awk '{print $1}'):$out ./
     • After flashing the new image + first boot, restore with:
         sudo AEON_BK_PW=… aeon-orbnet-backup restore -i $out
EOF
}

# Decrypt INFILE to a temp dir, echo the dir. Caller cleans up.
unpack(){
  local in="$1" tmp; tmp=$(mktemp -d)
  dec < "$in" | tar -C "$tmp" -xzpf - 2>/dev/null \
    || { rm -rf "$tmp"; die "decrypt/extract failed (wrong password or not an aeon backup?)"; }
  grep -q "$MAGIC" "$tmp/MANIFEST.txt" 2>/dev/null || { rm -rf "$tmp"; die "not an OrbNet backup (manifest missing)"; }
  echo "$tmp"
}

cmd_list(){
  local in="$DEFAULT_OUT"
  while [ $# -gt 0 ]; do case "$1" in -i) in="$2"; shift 2;; *) die "bad arg: $1";; esac; done
  [ -f "$in" ] || die "no backup at $in"
  export _AEON_PW; _AEON_PW=$(get_pw)
  tmp=$(unpack "$in"); trap 'rm -rf "${tmp:-}" 2>/dev/null' EXIT
  echo "=== public identifiers ==="; cat "$tmp/MANIFEST.txt"
  echo "=== captured paths ==="; dec < "$in" | tar -tzf - 2>/dev/null | grep -v '^MANIFEST.txt$'
  unset _AEON_PW
}

cmd_restore(){
  need_root
  local in="$DEFAULT_OUT" dry=0 force=0
  while [ $# -gt 0 ]; do case "$1" in
    -i) in="$2"; shift 2;; --dry-run) dry=1; shift;; --force) force=1; shift;; *) die "bad arg: $1";; esac; done
  [ -f "$in" ] || die "no backup at $in (use -i FILE)"
  # Guard: never run inside the image build (chroot) — restore is a live-device op.
  [ -f /etc/aeon/.in-bake ] && die "refusing to restore inside the image bake"
  export _AEON_PW; _AEON_PW=$(get_pw)
  tmp=$(unpack "$in"); trap 'rm -rf "${tmp:-}" 2>/dev/null' EXIT
  echo "=== restoring identities from $in ==="; grep -vE '^#' "$tmp/MANIFEST.txt"
  if [ "$dry" = 1 ]; then
    echo "--- DRY RUN: would restore these paths ---"
    (cd "$tmp" && find . -path ./MANIFEST.txt -prune -o -print | sed 's|^\./|/|' | sort)
    echo "--- would: stop services -> ensure users (${!USERS[*]}) -> cp -aT -> chown/chmod -> start services -> verify ---"
    unset _AEON_PW; return 0
  fi
  [ "$force" = 1 ] || { read -r -p "Overwrite this device's OrbNet identities with the backup? [y/N] " a </dev/tty; [ "$a" = y ] || [ "$a" = Y ] || die "aborted"; }

  log "stopping services…"
  for s in aeon-supervisor mysterium-node aeon-orbnet aeon-onions aeon-ipfs; do systemctl stop "$s" 2>/dev/null; done
  [ -x /usr/local/bin/aeon-orbnet ] && /usr/local/bin/aeon-orbnet down 2>/dev/null || true
  [ -x /usr/local/bin/aeon-onions ] && /usr/local/bin/aeon-onions down 2>/dev/null || true

  log "ensuring service users exist…"
  local u home
  for u in "${!USERS[@]}"; do
    home="${USERS[$u]}"
    id "$u" >/dev/null 2>&1 || useradd -r -s /usr/sbin/nologin -d "$home" -M "$u" 2>/dev/null || true
  done

  log "laying back identity files (cp -aT, perms preserved)…"
  local rel src dst
  while IFS= read -r src; do
    rel="${src#$tmp/}"; [ "$rel" = "MANIFEST.txt" ] && continue
    dst="/$rel"
    install -d "$(dirname "$dst")"
    cp -aT "$src" "$dst"
  done < <(find "$tmp" -mindepth 1 -maxdepth 1)

  log "re-applying owners + perms (belt-and-suspenders against UID drift)…"
  if id mysterium-node >/dev/null 2>&1; then
    chown -R mysterium-node:mysterium-node /var/lib/mysterium-node/keystore /var/lib/mysterium-node/nodeui-pass 2>/dev/null || true
    chmod 700 /var/lib/mysterium-node/keystore 2>/dev/null || true
    chmod 600 /var/lib/mysterium-node/keystore/UTC--* /var/lib/mysterium-node/nodeui-pass 2>/dev/null || true
  fi
  if id aeon-orbnet >/dev/null 2>&1; then
    chown -R aeon-orbnet:aeon-orbnet /var/lib/aeon/orbnet 2>/dev/null || true
    chmod 700 /var/lib/aeon/orbnet/tor/hs 2>/dev/null || true
    chmod 600 /var/lib/aeon/orbnet/tor/hs/hs_ed25519_secret_key /var/lib/aeon/orbnet/tls/key.pem \
              /var/lib/aeon/orbnet/owner.json /var/lib/aeon/orbnet/reg-token 2>/dev/null || true
  fi
  if id aeon-onions >/dev/null 2>&1; then
    chown -R aeon-onions:aeon-onions /var/lib/aeon/onions 2>/dev/null || true
    chmod 700 /var/lib/aeon/onions/hs 2>/dev/null || true
    chmod 700 /var/lib/aeon/onions/hs/*/ 2>/dev/null || true
  fi
  if id aeon-ipfs >/dev/null 2>&1; then
    chown -R aeon-ipfs:aeon-ipfs /var/lib/aeon/ipfs 2>/dev/null || true
    chmod 600 /var/lib/aeon/ipfs/config 2>/dev/null || true
  fi
  chmod 600 /etc/aeon/auth.toml /etc/aeon/tokens.toml /etc/aeon/key.pem /etc/aeon/mysterium-ui.pass 2>/dev/null || true
  [ -d /etc/aeon/vpn-secrets ] && chmod 700 /etc/aeon/vpn-secrets 2>/dev/null || true

  log "starting services in dependency order…"
  systemctl start mysterium-node 2>/dev/null || true
  [ -x /usr/local/bin/aeon-ipfs ] && /usr/local/bin/aeon-ipfs up 2>/dev/null || systemctl start aeon-ipfs 2>/dev/null || true
  [ -x /usr/local/bin/aeon-orbnet ] && /usr/local/bin/aeon-orbnet up 2>/dev/null || systemctl start aeon-orbnet 2>/dev/null || true
  [ -x /usr/local/bin/aeon-onions ] && /usr/local/bin/aeon-onions up 2>/dev/null || systemctl start aeon-onions 2>/dev/null || true
  systemctl start aeon-supervisor 2>/dev/null || true
  unset _AEON_PW

  echo ""
  log "=== verification (compare to the backup's manifest) ==="
  for f in /var/lib/mysterium-node/keystore/UTC--*; do [ -e "$f" ] && echo "  mysterium_identity: 0x${f##*--}"; done
  [ -f /var/lib/aeon/orbnet/tor/hs/hostname ] && echo "  orbnet_onion: $(cat /var/lib/aeon/orbnet/tor/hs/hostname 2>/dev/null)"
  [ -f /var/lib/aeon/ipfs/config ] && echo "  ipfs_peerid: $(python3 -c 'import json;print(json.load(open("/var/lib/aeon/ipfs/config")).get("Identity",{}).get("PeerID",""))' 2>/dev/null)"
  echo "  (these should match the manifest values printed above)"
  log "restore complete. Give it a minute, then check the node + dashboard."
}

case "${1:-}" in
  backup)  shift; cmd_backup "$@";;
  restore) shift; cmd_restore "$@";;
  list)    shift; cmd_list "$@";;
  *) echo "usage: aeon-orbnet-backup {backup [-o FILE] [--stdout] | restore [-i FILE] [--dry-run] [--force] | list [-i FILE]}" >&2; exit 1;;
esac
