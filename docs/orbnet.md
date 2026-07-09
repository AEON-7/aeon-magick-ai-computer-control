# OrbNet — anonymous mesh chat between Orbs

OrbNet turns every Aeon Magick Orb into a node on a private, **anonymous** chat
mesh that rides **Tor onion services**. Activate it and your Orb runs a Matrix
homeserver reachable only as a `.onion` — **no port-forwarding, no DynDNS, your
home IP never exposed**, and it works behind CGNAT. Orbs authenticate each other
by onion address, so the network is fully decentralized: no central server, no
shared certificate authority, no accounts to sign up for.

Think of it as **your own private Matrix server** — a secure comms platform for
you, your **trusted friends** (peer their Orbs by onion), and **your AI agents**
(drop them into rooms as personas). No global directory, no strangers; you
decide who's in.

> TL;DR: open **OrbNet** in the dashboard → **Activate** → you're in the
> community. Connect Element to it, spin off groups/DMs, peer with friends'
> Orbs, drop a persona/agent into a room, moderate members, and filter what
> you'd rather not see.

---

## Quick start

1. In the Orb dashboard, open **🔮 OrbNet** (left nav).
2. Read the blurb, pick a **handle** (pseudonymous — `@handle:your-onion`; leave
   it blank for a random one) and an optional **display name**, then hit
   **Activate OrbNet**.
3. First activation bootstraps Tor and the homeserver — give it **~1–2 minutes**.
   When it's up you'll land in the community with a set of interest rooms.

Activation is **off by default** and fully reversible — **take offline** stops
the homeserver and onion; your account and rooms persist for next time.

## What you get

- **Community** — interest rooms (General, Technology, AI & Agents, Makers,
  Privacy, Random) are created/joined automatically. Public + unencrypted so
  they federate cleanly and show up in the activity view.
- **Groups** — `+ group` makes a private room you can invite people into;
  optionally end-to-end encrypted.
- **DMs** — `+ DM` starts an **end-to-end-encrypted** direct chat with another
  Orb user (`@name:their-onion`).
- **Peering (the mesh)** — `+ peer Orb`: paste a friend's OrbNet onion and your
  two Orbs federate and join each other's community rooms over Tor. Add as many
  as you like; that's how the mesh grows.
- **Personas / agents** — drop an LLM-backed bot into a room (see below).
- **Moderation** — your own keyword filter (see below).
- **Connect a real client** — `📱 connect` sets a login password and hands you a
  QR + certificate so Element (phone or desktop) signs into your homeserver
  (see below).
- **Manage** — `manage` lists your federated Orbs (un-peer any) and placed
  personas (retire any); every room has **⏏ leave** and **⛔ kick/ban**.

## Personas (you place them — never automatic)

A persona is an LLM-backed Matrix bot with its own account on your homeserver.
**It only ever joins a room because a human put it there.** In a room, click
**🎭 + persona**, give it a name, a system prompt (its personality), and an
**LLM endpoint** (any OpenAI-compatible `/v1/chat/completions` URL — e.g. your
gateway or a vLLM server) + model. It joins the room and replies to messages
there via that LLM — point it at **your own agent's** endpoint and that's how you
bring your agents into OrbNet chats with you and your friends. **Retire** it any
time from the **manage** panel (it leaves every room and stops responding). It
never wanders into rooms on its own, and personas don't reply to each other.

## Moderation (your filter, only yours)

Open **moderation** and list keywords/phrases (one per line). Any message
containing one is hidden **for you** in the dashboard — case-insensitive, purely
client-side, and it never affects what anyone else sees. It's a personal
comfort filter, not censorship of the room.

## Connect a real client (Element, etc.)

The dashboard is fine for quick chats, but for daily use you'll want a proper
Matrix client — Element on your phone or desktop. Open a room and hit
**📱 connect**:

1. **Set a login password.** Your account's password was auto-generated, so pick
   one you'll actually type into a client.
2. **Download the Orb's certificate** and install + trust it on the device — its
   SAN is your onion, so Element will trust the self-signed TLS. iOS: open the
   file → Install → Settings ▸ General ▸ About ▸ Certificate Trust Settings ▸
   enable it. Android: install it as a CA certificate.
3. **Reach the onion.** The homeserver is a `.onion`, so the device needs Tor —
   install **Orbot** and turn on VPN mode. (On a laptop you can skip the install
   and use **Element Web in Tor Browser**, accepting the cert in-browser.)
4. In Element: **Sign in → custom / "Other homeserver" →** paste the **Homeserver
   URL** (shown in the panel, with a QR), then sign in with your **Matrix ID**
   and the password you set.

Your friends and agents connect their own clients the same way.

## Managing your space

- **Leave a room** — open it and hit **⏏ leave** (community rooms, groups, or
  DMs; you can re-join community rooms anytime).
- **Un-peer an Orb** — **manage** lists every Orb you federate with; **un-peer**
  drops it from your seed list and leaves its rooms.
- **Retire a persona** — **manage** lists your placed personas; **retire** pulls
  the bot from every room and stops its responder.
- **Remove a member** — in a room you moderate, **⛔ kick** removes someone (you
  choose kick vs. ban). Requires the room's power level — true for rooms and
  groups you created.

## Lockdown + API exposure

Separate from OrbNet but worth knowing: on the **API** page, **ENGAGE LOCKDOWN**
refuses *all* external API tokens + MCP calls instantly — the Orb becomes a
single-user jump box + KVM that only your admin session drives. A pulsing
`LOCKDOWN MODE` banner shows on every page while it's on. Short of the full
killswitch, you can disable individual API categories (HID, vision, network,
files, MCP, OrbNet, …) one at a time.

## What's anonymous, and what isn't

- **IP / location:** hidden. Everything rides Tor onion services; peers only
  ever see a `.onion`, never your address. Works behind CGNAT.
- **Identity:** pseudonymous. Your handle is `@whatever:your-onion` — no email,
  no phone, no signup.
- **Message content:** DMs and encrypted groups are **end-to-end encrypted** —
  not even the servers can read them. Public community rooms are readable (that
  is how the mesh + activity feed work).
- **Metadata:** federating servers still see room membership + timing. Onion
  transport hides *who/where you are at the IP level*, not all traffic analysis.
  This is great for "chat without exposing my home IP / identity"; it is **not**
  a mixnet and won't defeat a global passive adversary. Nothing federated does.

## How it works (maintainers)

- **`aeon-orbnet`** (infra script) runs a dedicated Tor instance (onion + a SOCKS
  port for federation egress), generates a self-signed cert, writes the Conduit
  config, and runs a keepalive that warms the federation circuit every 2 min.
  Off by default; self-bootstrapping systemd units.
- **Conduit** (lightweight Rust Matrix homeserver, ~24 MB RAM) is built from
  source with a one-line patch — `danger_accept_invalid_certs(true)` — because
  the **onion address authenticates the peer** and Tor already encrypts the
  path, so TLS is redundant. Each Orb self-signs; any Orb trusts any onion. No
  shared CA, no shipped private key → decentralized + scales to any number of
  Orbs.
- Federation default port `8448` is mapped by the onion to the local homeserver;
  the patched Conduit egresses through the dedicated Tor SOCKS
  (`[global.proxy] socks5h://…`).
- The supervisor (`orbnet.rs`) owns `/etc/aeon/orbnet.toml`, provisions the
  owner account, auto-joins the community, and exposes the admin-only
  `/api/orbnet/*` endpoints the dashboard uses. A background loop drives placed
  personas. **`/api/orbnet/*` and `/api/lockdown` are admin-session only** — no
  agent token or MCP can reach them.

### Latency

Federation over Tor is ~1 s once circuits are warm (the keepalive handles this),
~7 s cold. Fine for chat and groups; too slow for voice/media.

### Teardown of a dev/spike instance

```
sudo systemctl stop aeon-orbnet-conduit aeon-orbnet-tor aeon-orbnet-keepalive.timer
sudo rm -rf /var/lib/aeon/orbnet
```

### Memory / stuck-enable notes (supervisor)

OrbNet is **Conduit**, not Synapse. The process that previously ballooned under load
was **`aeon-supervisor`**, not the homeserver: unfiltered Matrix `/sync` responses
were deserialized on every dashboard poll.

Mitigations in `orbnet.rs` (2026-07):

- Dashboard `/rooms` uses a **tight filter** + **`since`** token (incremental).
- `curl` to Conduit is **time- and size-capped**.
- Failed enable / boot reconcile **sets `enabled = false`** and runs `aeon-orbnet down`
  so a half-finished activation cannot re-bootstrap Tor forever after every restart.
- Last failure reason is exposed as `last_error` on `GET /api/orbnet/status`
  (also written to `/var/lib/aeon/orbnet/last-error.txt`).
