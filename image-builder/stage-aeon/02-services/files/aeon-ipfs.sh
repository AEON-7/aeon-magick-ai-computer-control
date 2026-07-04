#!/bin/bash
# aeon-ipfs — stand up a local IPFS (kubo) node + HTTP gateway on this Orb, so
# you can host content on the decentralized web and reach the gateway from any
# device (LAN or Tailscale). Self-bootstrapping: downloads the kubo arm64 binary,
# inits the repo (lowpower profile — tuned for a Pi), binds the gateway to all
# interfaces (API stays localhost-only), sets a storage cap, and installs its
# systemd unit on first `up`. OFF by default.
#
# Subcommands:
#   up | down | status
#   storage <size>     e.g. 10GB   (sets Datastore.StorageMax; restarts daemon)
#   pin <cid> | unpin <cid> | pins
#   add <path>         add a file/dir to IPFS, prints the root CID
#   gateway            prints the gateway port
set -uo pipefail

DIR=/var/lib/aeon/ipfs
SVCUSER=aeon-ipfs
IPFSBIN=/usr/local/bin/ipfs
UNIT=/etc/systemd/system/aeon-ipfs.service
GATEWAY_PORT=8080
API_PORT=5001
# Absolute path to this script, so the systemd unit's ExecStartPre can call back
# into it (aeon-ipfs prestart) to clear a stale lock before the daemon starts.
SELF="$(readlink -f "$0" 2>/dev/null || echo /usr/local/bin/aeon-ipfs)"

log() { echo "aeon-ipfs: $*" >&2; }
ipfs_cmd() { runuser -u "$SVCUSER" -- env IPFS_PATH="$DIR" "$IPFSBIN" "$@"; }

ensure_user() {
  id "$SVCUSER" >/dev/null 2>&1 || \
    useradd -r -s /usr/sbin/nologin -d "$DIR" -M "$SVCUSER" 2>/dev/null || true
  install -d -m 0750 -o "$SVCUSER" -g "$SVCUSER" "$DIR"
}

ensure_bin() {
  [ -x "$IPFSBIN" ] && return 0
  log "downloading kubo…"
  local kver tmp
  kver=$(curl -fsSL --max-time 30 https://dist.ipfs.tech/kubo/versions 2>/dev/null | tail -1)
  [ -z "$kver" ] && { log "could not resolve latest kubo version"; return 1; }
  tmp=$(mktemp -d)
  if ! curl -fsSL --max-time 180 "https://dist.ipfs.tech/kubo/${kver}/kubo_${kver}_linux-arm64.tar.gz" -o "$tmp/kubo.tgz"; then
    log "kubo download failed"; rm -rf "$tmp"; return 1
  fi
  tar xzf "$tmp/kubo.tgz" -C "$tmp" 2>/dev/null || { rm -rf "$tmp"; return 1; }
  install -m 0755 "$tmp/kubo/ipfs" "$IPFSBIN"
  rm -rf "$tmp"
  log "installed kubo $kver"
}

ensure_init() {
  [ -f "$DIR/config" ] && return 0
  ipfs_cmd init --profile=lowpower >/dev/null 2>&1 || { log "ipfs init failed"; return 1; }
}

configure() {
  # Gateway reachable from any device; API stays localhost-only (it's powerful).
  ipfs_cmd config Addresses.Gateway "/ip4/0.0.0.0/tcp/${GATEWAY_PORT}" >/dev/null 2>&1 || true
  ipfs_cmd config Addresses.API "/ip4/127.0.0.1/tcp/${API_PORT}" >/dev/null 2>&1 || true
  # Pubsub — the substrate for fleet-free Model Share discovery: every Orb
  # gossips its model catalog on a well-known topic and converges on a global
  # index with no shared token. Needs a daemon restart to take effect. We use
  # FLOODSUB (not gossipsub): on a sparse topic like ours — a handful of Orbs,
  # not thousands — gossipsub's mesh is slow/unreliable to form and drops
  # messages, whereas floodsub delivers every announcement to every connected
  # subscriber. Small, infrequent messages make the O(N) fan-out a non-issue.
  ipfs_cmd config --json Pubsub.Enabled true >/dev/null 2>&1 || true
  ipfs_cmd config Pubsub.Router floodsub >/dev/null 2>&1 || true
  # Keep peers CONNECTED. The lowpower init profile sets a tiny ConnMgr
  # HighWater, so once an Orb has a few dozen peers it aggressively prunes
  # connections — including the very links Model Share pubsub rides on, making
  # delivery intermittent. Raise the watermarks so inter-Orb links survive.
  ipfs_cmd config --json Swarm.ConnMgr.LowWater 200 >/dev/null 2>&1 || true
  ipfs_cmd config --json Swarm.ConnMgr.HighWater 500 >/dev/null 2>&1 || true
  # AcceleratedDHTClient makes provider lookups (finding who hosts a CID) far
  # faster on a wide network — worth the modest memory on a Pi 4/5.
  ipfs_cmd config --json Experimental.AcceleratedDHTClient true >/dev/null 2>&1 || true
  # NB: the branded gateway landing page is NOT set here — it needs to `ipfs add`
  # content, which is unreliable offline (before the daemon is up) on newer kubo.
  # It's handled by ensure_landing() AFTER daemon_up instead.
}

# kubo serves a bare 404 at the gateway root ("/") — it only resolves
# /ipfs/<cid> paths. So the console's "gateway" QR / link (which points at the
# base URL) opened a 404. Host a tiny branded landing page on IPFS and point
# Gateway.RootRedirect at it, so the root — for the QR and any client — shows a
# real "this gateway works" page instead.
#
# Must run with the daemon ONLINE: on kubo ≥0.42 an offline `ipfs add` during
# configure (before the daemon starts) silently fails, so the CID never gets set
# and the QR 404s on a fresh boot. So we wait for the API, add the page online,
# then restart ONCE so the gateway picks up the new RootRedirect. Idempotent:
# once RootRedirect points at an /ipfs/ path we return immediately (no restart),
# so only the very first boot pays the extra restart.
ensure_landing() {
  case "$(ipfs_cmd config Gateway.RootRedirect 2>/dev/null)" in
    /ipfs/*) return 0 ;;
  esac
  # Wait (≤30s) for the daemon API before adding content.
  local i=0; while [ $i -lt 30 ] && ! ipfs_cmd id >/dev/null 2>&1; do sleep 1; i=$((i + 1)); done
  local cid
  cid=$(landing_html | ipfs_cmd add -Q 2>/dev/null)
  [ -n "$cid" ] || return 0
  ipfs_cmd pin add "$cid" >/dev/null 2>&1 || true
  ipfs_cmd config Gateway.RootRedirect "/ipfs/$cid" >/dev/null 2>&1 || true
  # RootRedirect is read at gateway startup — restart once so it takes effect.
  daemon_restart
}

landing_html() {
  cat <<'HTML'
<!doctype html><html lang=en><head><meta charset=utf-8>
<title>Aeon Orb · IPFS gateway</title>
<meta name=viewport content="width=device-width,initial-scale=1">
<style>
  :root{color-scheme:dark}
  body{background:#0c0d14;color:#c4b5fd;font:16px/1.65 system-ui,-apple-system,sans-serif;
       max-width:34rem;margin:0 auto;min-height:100vh;display:flex;flex-direction:column;
       justify-content:center;padding:2rem 1.5rem}
  h1{font-weight:600;font-size:1.4rem;margin:0 0 .25rem;color:#a78bfa}
  p{color:#9ca3af;margin:.5rem 0}
  code{background:#1a1c28;padding:.15em .45em;border-radius:.35em;color:#a78bfa;font-size:.95em}
  .dot{display:inline-block;width:.55em;height:.55em;border-radius:50%;background:#34d399;margin-right:.4em;vertical-align:middle}
</style></head><body>
<h1>🔮 Aeon Orb — IPFS gateway</h1>
<p><span class=dot></span>This gateway is live.</p>
<p>Fetch any content by its CID at <code>/ipfs/&lt;CID&gt;</code>, or set this as your
gateway in IPFS Companion. Models shared from the Orb's console open here.</p>
</body></html>
HTML
}

ensure_units() {
  cat > "$UNIT" <<EOF
[Unit]
Description=Aeon Magick — IPFS (kubo) node + gateway
After=network-online.target
Wants=network-online.target
# Bound the crash-loop: if the daemon fails 5x in 10 min (e.g. a stale lock we
# somehow can't clear, a corrupt repo, a port collision) give up and land in
# 'failed' — a state the status endpoint surfaces — instead of retrying forever
# behind an eternal amber "starting". (StartLimit* live in [Unit] on systemd ≥229.)
StartLimitIntervalSec=600
StartLimitBurst=5
[Service]
User=$SVCUSER
Environment=IPFS_PATH=$DIR
# Clear a repo.lock left by an unclean shutdown before kubo runs; '-' = ignore
# failure so a first boot with no lock is fine.
ExecStartPre=-$SELF prestart
ExecStart=$IPFSBIN daemon --migrate=true --enable-gc
Restart=on-failure
RestartSec=10
# Allow a real fs-repo migration / large-repo open on a slow SD card, but don't
# let a wedged start hang indefinitely.
TimeoutStartSec=900
[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload 2>/dev/null || true
}

# ── Daemon lifecycle: systemd on a Pi, direct process in a container ─────────
# In the headless server container there's no systemd, so run kubo directly
# (detached, tracked by a PID file) instead of `systemctl`.
PIDFILE=/run/aeon/ipfs.pid
have_systemd() { [ -d /run/systemd/system ]; }

# Richer than a bare boolean: active | starting | failed | inactive. On a Pi we
# read systemd's sub-state so a crash-loop (auto-restart) or a hard failure is
# distinguishable from a genuine slow startup — the web UI keys off this so a
# dead daemon no longer shows an eternal amber "starting".
daemon_state() {
  if have_systemd; then
    case " $(systemctl show -p ActiveState -p SubState --value aeon-ipfs.service 2>/dev/null | tr '\n' ' ') " in
      *" active "*)              echo active ;;
      *failed*|*auto-restart*)   echo failed ;;
      *deactivating*)            echo inactive ;;
      *activating*)              echo starting ;;
      *)                         echo inactive ;;
    esac
    return
  fi
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null; then echo active; else echo inactive; fi
}

daemon_active() { [ "$(daemon_state)" = active ]; }

# Remove a repo lock left by an unclean shutdown/crash — but ONLY when no ipfs
# daemon is actually alive against this repo, so we never yank the lock from a
# running (or legitimately starting) daemon. Called on the `up` path and by the
# unit's ExecStartPre (`prestart`).
clear_stale_lock() {
  if pgrep -f "$IPFSBIN daemon" >/dev/null 2>&1; then return 0; fi
  if have_systemd && systemctl is-active --quiet aeon-ipfs.service; then return 0; fi
  if [ -e "$DIR/repo.lock" ]; then
    log "clearing stale repo.lock (no live daemon)"
    rm -f "$DIR/repo.lock" "$DIR/api" 2>/dev/null || true
  fi
}

# Time-boxed ipfs call for status reads: a crash-looping or lock-blocked daemon
# must never hang GET /api/ipfs/status (which has no timeout upstream).
ipfs_cmd_to() { timeout "$1" runuser -u "$SVCUSER" -- env IPFS_PATH="$DIR" "$IPFSBIN" "${@:2}"; }

daemon_up() {
  if have_systemd; then
    ensure_units
    systemctl enable --now aeon-ipfs.service 2>/dev/null || true
    return 0
  fi
  daemon_active && return 0
  clear_stale_lock
  install -d /run/aeon 2>/dev/null || true
  setsid runuser -u "$SVCUSER" -- env IPFS_PATH="$DIR" "$IPFSBIN" daemon --migrate=true --enable-gc \
    </dev/null >/var/log/aeon-ipfs.log 2>&1 &
  echo $! > "$PIDFILE"
  # Wait (≤10s) for the API socket so a following command doesn't race the boot.
  local i=0; while [ $i -lt 20 ] && ! ipfs_cmd id >/dev/null 2>&1; do sleep 0.5; i=$((i + 1)); done
}

daemon_down() {
  if have_systemd; then systemctl disable --now aeon-ipfs.service 2>/dev/null || true; return; fi
  [ -f "$PIDFILE" ] && kill "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null || true
  rm -f "$PIDFILE"
  pkill -f "$IPFSBIN daemon" 2>/dev/null || true
}

daemon_restart() {
  if have_systemd; then systemctl restart aeon-ipfs.service 2>/dev/null || true; return; fi
  daemon_down; sleep 1; daemon_up
}

cmd_up() {
  ensure_user || return 1
  ensure_bin || return 1
  ensure_init || return 1
  configure
  clear_stale_lock
  daemon_up
  ensure_landing
}

cmd_down() { daemon_down; }

cmd_status() {
  local installed daemon ver pid peers repo smax dfree dtotal dfout
  installed=$([ -x "$IPFSBIN" ] && echo true || echo false)
  daemon=$(daemon_state)
  ver=""; pid=""; peers=0; repo=0; smax=""
  # Free/total bytes on the filesystem that backs the IPFS repo — lets the web
  # slider cap the allocation at what the disk can physically hold. df -PB1 →
  # POSIX columns in 1-byte blocks: field 2 = total, field 4 = available.
  dfree=0; dtotal=0
  dfout=$(df -PB1 "$DIR" 2>/dev/null | awk 'NR==2{print $2" "$4}')
  if [ -n "$dfout" ]; then dtotal=${dfout%% *}; dfree=${dfout##* }; fi
  [ -z "$dtotal" ] && dtotal=0; [ -z "$dfree" ] && dfree=0
  if [ "$installed" = true ] && [ -f "$DIR/config" ]; then
    # These are local config reads (fast), but time-box them anyway so a wedged
    # repo can't stall the status endpoint.
    ver=$(ipfs_cmd_to 5 version --number 2>/dev/null)
    pid=$(ipfs_cmd_to 5 config Identity.PeerID 2>/dev/null)
    smax=$(ipfs_cmd_to 5 config Datastore.StorageMax 2>/dev/null)
    # `repo stat` walks the datastore and can be slow / lock-blocked, so only run
    # it (and swarm peers, which needs the API) when the daemon is actually up.
    if [ "$daemon" = active ]; then
      repo=$(ipfs_cmd_to 5 repo stat 2>/dev/null | awk '/RepoSize/{print $2; exit}')
      peers=$(ipfs_cmd_to 5 swarm peers 2>/dev/null | wc -l | tr -d ' ')
    fi
  fi
  printf '{"installed":%s,"daemon":"%s","version":"%s","peer_id":"%s","peers":%d,"repo_bytes":%s,"storage_max":"%s","disk_free_bytes":%s,"disk_total_bytes":%s,"gateway_port":%d}\n' \
    "$installed" "$daemon" "${ver:-}" "${pid:-}" "${peers:-0}" "${repo:-0}" "${smax:-}" "${dfree:-0}" "${dtotal:-0}" "$GATEWAY_PORT"
}

cmd_storage() {
  local size="${1:-}"
  [ -z "$size" ] && { log "usage: storage <size e.g. 10GB>"; return 1; }
  case "$size" in *[!0-9GMKTBgmktb]*) log "bad size"; return 1;; esac
  ipfs_cmd config Datastore.StorageMax "$size" >/dev/null 2>&1 || { log "set storage failed"; return 1; }
  # Restart to pick up the new cap (and, on systemd hosts, re-assert the unit so
  # nodes provisioned before --enable-gc landed get periodic GC — without it
  # StorageMax is just a number kubo reports but never enforces: pinned models
  # are always kept; only unpinned cached/shared blocks are reclaimed).
  daemon_restart
  echo "$size"
}

cmd_pin()   { ipfs_cmd pin add "${1:-}" 2>&1; }
cmd_unpin() { ipfs_cmd pin rm "${1:-}" 2>&1; }
cmd_pins()  { ipfs_cmd pin ls --type=recursive 2>/dev/null | awk '{print $1}'; }
cmd_add()   { ipfs_cmd add -rQ "${1:-}" 2>/dev/null; }
cmd_cat()   { ipfs_cmd cat "${1:-}" 2>/dev/null; }
# Best-effort direct swarm connection (multiaddr), used before pinning a
# peer's model so LAN/tailnet fetches don't wait on DHT routing.
cmd_connect() { ipfs_cmd swarm connect "${1:-}" 2>&1 || true; }
cmd_id()      { ipfs_cmd id -f='<id>' 2>/dev/null; }
# Model Share gossip primitives (used by aeon-modelshare):
#   pub <topic>    publish stdin to a pubsub topic
#   sub <topic>    stream messages on a pubsub topic (one per line, blocks)
#   peer <maddr>   add a persistent peer (kept connected across ConnMgr pruning)
cmd_pub() { ipfs_cmd pubsub pub "${1:-}" 2>/dev/null; }
# EXEC (not a child): replace this bash with ipfs so its stdout is the caller's
# fd directly. Going through an extra bash+runuser layer buffers the stream so
# small, infrequent announcements never flush to a reader's pipe — the reason
# subscribers silently received nothing. `exec` fixes that.
cmd_sub() { exec runuser -u "$SVCUSER" -- env IPFS_PATH="$DIR" "$IPFSBIN" pubsub sub "${1:-}"; }
# Keep discovered Orbs persistently connected: kubo's Peering survives the
# lowpower ConnMgr's aggressive pruning, which otherwise drops the inter-Orb
# link and makes pubsub delivery intermittent.
cmd_peer() { ipfs_cmd swarm peering add "${1:-}" >/dev/null 2>&1 || true; }
# MFS (mutable filesystem) primitives — used to build/EDIT a model directory
# while reusing the (multi-GB) weights by CID reference, so editing a model's
# card/readme/image never re-uploads or re-downloads the weights. All paths are
# MFS paths (e.g. /aeon-build/<id>) except cp's /ipfs/<cid> source.
cmd_mfs_mkdir() { ipfs_cmd files mkdir -p "${1:-}" 2>&1; }
cmd_mfs_cp()    { ipfs_cmd files cp "${1:-}" "${2:-}" 2>&1; }
cmd_mfs_rm()    { ipfs_cmd files rm -r "${1:-}" 2>/dev/null || true; }
cmd_mfs_write() { ipfs_cmd files write --create --truncate "${1:-}" 2>&1; }  # stdin → file
cmd_mfs_hash()  { ipfs_cmd files stat --hash "${1:-}" 2>/dev/null; }

# get <cid> <dest> — materialize a CID (a model directory) to PLAIN files on
# local disk, so the Orb holds a re-pushable copy (rsync to connected systems).
# Runs as root (the supervisor invokes this script as root) reading the
# aeon-ipfs-owned repo, then hands ownership of the output to root so rsync can
# read it. Fetches from the network on demand if the blocks aren't local yet.
cmd_get() {
  local cid="${1:-}" dest="${2:-}"
  [ -z "$cid" ] || [ -z "$dest" ] && { log "usage: get <cid> <dest>"; return 1; }
  case "$cid" in *[!A-Za-z0-9]*) log "bad cid"; return 1;; esac
  install -d "$(dirname "$dest")"
  rm -rf "$dest"
  # Root can read the 0750 aeon-ipfs repo; write the materialized tree to dest.
  env IPFS_PATH="$DIR" "$IPFSBIN" get "$cid" -o "$dest" >/dev/null 2>&1 || { log "ipfs get failed"; return 1; }
  du -sb "$dest" 2>/dev/null | awk '{print $1}'
}

case "${1:-}" in
  up)      cmd_up ;;
  down)    cmd_down ;;
  prestart) clear_stale_lock ;;
  status)  cmd_status ;;
  storage) shift; cmd_storage "$@" ;;
  pin)     shift; cmd_pin "$@" ;;
  unpin)   shift; cmd_unpin "$@" ;;
  pins)    cmd_pins ;;
  add)     shift; cmd_add "$@" ;;
  cat)     shift; cmd_cat "$@" ;;
  connect) shift; cmd_connect "$@" ;;
  id)      cmd_id ;;
  pub)     shift; cmd_pub "$@" ;;
  sub)     shift; cmd_sub "$@" ;;
  peer)    shift; cmd_peer "$@" ;;
  mfs-mkdir) shift; cmd_mfs_mkdir "$@" ;;
  mfs-cp)    shift; cmd_mfs_cp "$@" ;;
  mfs-rm)    shift; cmd_mfs_rm "$@" ;;
  mfs-write) shift; cmd_mfs_write "$@" ;;
  mfs-hash)  shift; cmd_mfs_hash "$@" ;;
  get)     shift; cmd_get "$@" ;;
  gateway) echo "$GATEWAY_PORT" ;;
  *) echo "usage: aeon-ipfs {up|down|status|storage <size>|pin <cid>|unpin <cid>|pins|add <path>|cat <path>|get <cid> <dest>|connect <multiaddr>|id|pub <topic>|sub <topic>|mfs-{mkdir,cp,rm,write,hash} <path…>|gateway}" >&2; exit 1 ;;
esac
