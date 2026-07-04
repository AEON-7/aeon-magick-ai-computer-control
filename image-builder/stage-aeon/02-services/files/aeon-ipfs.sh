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
  # Pubsub (gossipsub) — the substrate for fleet-free Model Share discovery:
  # every Orb gossips its model catalog on a well-known topic and converges on
  # a global index with no shared token. Needs a daemon restart to take effect.
  ipfs_cmd config --json Pubsub.Enabled true >/dev/null 2>&1 || true
  # AcceleratedDHTClient makes provider lookups (finding who hosts a CID) far
  # faster on a wide network — worth the modest memory on a Pi 4/5.
  ipfs_cmd config --json Experimental.AcceleratedDHTClient true >/dev/null 2>&1 || true
  setup_root_landing
}

# kubo serves a bare 404 at the gateway root ("/") — it only resolves
# /ipfs/<cid> paths. So the console's "gateway" QR / link (which points at the
# base URL) opened a 404. Host a tiny branded landing page on IPFS and point
# Gateway.RootRedirect at it, so the root — for the QR and any client — shows a
# real "this gateway works" page instead. Idempotent: skip if already set.
setup_root_landing() {
  case "$(ipfs_cmd config Gateway.RootRedirect 2>/dev/null)" in
    /ipfs/*) return 0 ;;
  esac
  local cid
  cid=$(landing_html | ipfs_cmd add -Q 2>/dev/null)
  [ -n "$cid" ] || return 0
  ipfs_cmd pin add "$cid" >/dev/null 2>&1 || true
  ipfs_cmd config Gateway.RootRedirect "/ipfs/$cid" >/dev/null 2>&1 || true
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
[Service]
User=$SVCUSER
Environment=IPFS_PATH=$DIR
ExecStart=$IPFSBIN daemon --migrate=true --enable-gc
Restart=on-failure
RestartSec=10
[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload 2>/dev/null || true
}

cmd_up() {
  ensure_user || return 1
  ensure_bin || return 1
  ensure_init || return 1
  configure
  ensure_units
  systemctl enable --now aeon-ipfs.service 2>/dev/null || true
}

cmd_down() { systemctl disable --now aeon-ipfs.service 2>/dev/null || true; }

cmd_status() {
  local installed daemon ver pid peers repo smax dfree dtotal dfout
  installed=$([ -x "$IPFSBIN" ] && echo true || echo false)
  daemon=$(systemctl is-active aeon-ipfs.service 2>/dev/null); daemon=${daemon:-inactive}
  ver=""; pid=""; peers=0; repo=0; smax=""
  # Free/total bytes on the filesystem that backs the IPFS repo — lets the web
  # slider cap the allocation at what the disk can physically hold. df -PB1 →
  # POSIX columns in 1-byte blocks: field 2 = total, field 4 = available.
  dfree=0; dtotal=0
  dfout=$(df -PB1 "$DIR" 2>/dev/null | awk 'NR==2{print $2" "$4}')
  if [ -n "$dfout" ]; then dtotal=${dfout%% *}; dfree=${dfout##* }; fi
  [ -z "$dtotal" ] && dtotal=0; [ -z "$dfree" ] && dfree=0
  if [ "$installed" = true ] && [ -f "$DIR/config" ]; then
    ver=$(ipfs_cmd version --number 2>/dev/null)
    pid=$(ipfs_cmd config Identity.PeerID 2>/dev/null)
    smax=$(ipfs_cmd config Datastore.StorageMax 2>/dev/null)
    repo=$(ipfs_cmd repo stat 2>/dev/null | awk '/RepoSize/{print $2; exit}')
    [ "$daemon" = active ] && peers=$(ipfs_cmd swarm peers 2>/dev/null | wc -l | tr -d ' ')
  fi
  printf '{"installed":%s,"daemon":"%s","version":"%s","peer_id":"%s","peers":%d,"repo_bytes":%s,"storage_max":"%s","disk_free_bytes":%s,"disk_total_bytes":%s,"gateway_port":%d}\n' \
    "$installed" "$daemon" "${ver:-}" "${pid:-}" "${peers:-0}" "${repo:-0}" "${smax:-}" "${dfree:-0}" "${dtotal:-0}" "$GATEWAY_PORT"
}

cmd_storage() {
  local size="${1:-}"
  [ -z "$size" ] && { log "usage: storage <size e.g. 10GB>"; return 1; }
  case "$size" in *[!0-9GMKTBgmktb]*) log "bad size"; return 1;; esac
  ipfs_cmd config Datastore.StorageMax "$size" >/dev/null 2>&1 || { log "set storage failed"; return 1; }
  # Re-assert the unit so nodes provisioned before --enable-gc landed pick up
  # periodic GC on the next restart — without it, StorageMax is just a number
  # kubo reports but never enforces (pinned models are always kept; only
  # unpinned cached/shared blocks are reclaimed once the repo passes the cap).
  ensure_units
  systemctl restart aeon-ipfs.service 2>/dev/null || true
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
cmd_pub() { ipfs_cmd pubsub pub "${1:-}" 2>/dev/null; }
cmd_sub() { ipfs_cmd pubsub sub "${1:-}" 2>/dev/null; }
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
  mfs-mkdir) shift; cmd_mfs_mkdir "$@" ;;
  mfs-cp)    shift; cmd_mfs_cp "$@" ;;
  mfs-rm)    shift; cmd_mfs_rm "$@" ;;
  mfs-write) shift; cmd_mfs_write "$@" ;;
  mfs-hash)  shift; cmd_mfs_hash "$@" ;;
  get)     shift; cmd_get "$@" ;;
  gateway) echo "$GATEWAY_PORT" ;;
  *) echo "usage: aeon-ipfs {up|down|status|storage <size>|pin <cid>|unpin <cid>|pins|add <path>|cat <path>|get <cid> <dest>|connect <multiaddr>|id|pub <topic>|sub <topic>|mfs-{mkdir,cp,rm,write,hash} <path…>|gateway}" >&2; exit 1 ;;
esac
