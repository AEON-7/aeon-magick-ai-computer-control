#!/bin/bash
# aeon-nas — optional LAN file sharing (Samba/SMB) for the Orb. OFF by default.
#
#   status          JSON: installed? running? share paths + host.
#   enable <pw>     Install Samba on first use, expose two READ/WRITE shares —
#                   the model library and a general "Aeon Share" folder —
#                   protected by the `admin` account with password <pw>.
#   disable         Stop + disable smbd (config + files are left in place).
#   set-pass <pw>   Change the SMB password without touching the config.
#
# SMB works from macOS (Finder → Go → Connect to Server → smb://<orb>),
# Windows (\\<orb>\aeon-share) and Linux. Nothing is exposed unauthenticated:
# both shares require the admin username + the password set here.
set -uo pipefail

SHARE_DIR=/var/lib/aeon/nas-share
LIB_DIR=/var/lib/aeon/model-library
SMB_USER=admin
INCLUDE=/etc/samba/aeon-shares.conf
SMBCONF=/etc/samba/smb.conf

log() { echo "aeon-nas: $*" >&2; }

installed() { command -v smbd >/dev/null 2>&1 && echo true || echo false; }

ensure_installed() {
  [ "$(installed)" = true ] && return 0
  log "installing samba…"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -y >/dev/null 2>&1 || true
  apt-get install -y --no-install-recommends samba >/dev/null 2>&1 || { log "samba install failed (no network?)"; return 1; }
}

write_shares() {
  install -d -m 0775 "$SHARE_DIR" "$LIB_DIR"
  cat > "$INCLUDE" <<EOF
# Managed by aeon-nas — do not edit by hand.
[aeon-share]
   comment = Aeon Orb shared files
   path = $SHARE_DIR
   browseable = yes
   read only = no
   valid users = $SMB_USER
   create mask = 0664
   directory mask = 0775

[models]
   comment = Aeon model library
   path = $LIB_DIR
   browseable = yes
   read only = no
   valid users = $SMB_USER
   create mask = 0664
   directory mask = 0775
EOF
  # Pull our shares into the main config exactly once.
  grep -q "include = $INCLUDE" "$SMBCONF" 2>/dev/null || printf '\ninclude = %s\n' "$INCLUDE" >> "$SMBCONF"
}

set_pass() {
  local pw="${1:-}"
  [ -z "$pw" ] && { log "password required"; return 1; }
  id "$SMB_USER" >/dev/null 2>&1 || { log "system user $SMB_USER missing"; return 1; }
  printf '%s\n%s\n' "$pw" "$pw" | smbpasswd -s -a "$SMB_USER" >/dev/null 2>&1 \
    || { log "smbpasswd failed"; return 1; }
  smbpasswd -e "$SMB_USER" >/dev/null 2>&1 || true
}

cmd_enable() {
  local pw="${1:-}"
  [ -z "$pw" ] && { log "usage: enable <password>"; return 1; }
  ensure_installed || return 1
  write_shares
  set_pass "$pw" || return 1
  # smbd is the file server; nmbd (NetBIOS name) is optional and often masked.
  systemctl enable --now smbd >/dev/null 2>&1 || { log "could not start smbd"; return 1; }
  systemctl reload smbd >/dev/null 2>&1 || systemctl restart smbd >/dev/null 2>&1 || true
  echo "enabled"
}

cmd_disable() {
  systemctl disable --now smbd >/dev/null 2>&1 || true
  echo "disabled"
}

cmd_status() {
  local inst run host
  inst=$(installed)
  run=$([ "$inst" = true ] && systemctl is-active smbd 2>/dev/null || echo inactive); run=${run:-inactive}
  host=$(hostname -I 2>/dev/null | awk '{print $1}')
  printf '{"ok":true,"installed":%s,"running":"%s","host":"%s","user":"%s","shares":["aeon-share","models"],"share_dir":"%s","library_dir":"%s"}\n' \
    "$inst" "$run" "${host:-}" "$SMB_USER" "$SHARE_DIR" "$LIB_DIR"
}

case "${1:-}" in
  status)   cmd_status ;;
  enable)   shift; cmd_enable "$@" ;;
  disable)  cmd_disable ;;
  set-pass) shift; set_pass "$@" && echo ok ;;
  *) echo "usage: aeon-nas {status|enable <password>|disable|set-pass <password>}" >&2; exit 1 ;;
esac
