# Intergalactic Model Share — wire protocol

What two implementations must agree on to share one index and verify each
other's models. The Orb implements all of it (`aeon-supervisor/src/ipfs.rs`,
`publisher.rs`, and the `aeon-modelshare` gossip daemon); a bridge such as
FrostByte implements the parts it needs. Test vectors for every signature are
in [`model-share-vectors.json`](./model-share-vectors.json) and are checked
against the Orb's code by `cargo test -p aeon-supervisor published_vectors`.

## 1. Identities

| Key | What it is | Lives |
|---|---|---|
| **Orb key** | Ed25519 device key, generated on first use. Signs everything the Orb originates, so signing needs no unlock. | `/etc/aeon/orb-publisher.key` (0600) |
| **User identity** | Ed25519 key derived from a 24-word BIP39 phrase; portable between Orbs and apps. Reputation accrues to it. | encrypted in `/etc/aeon/publisher-accounts.json` |
| **Delegation** | The user key's signed statement that an Orb key may publish in its name. Issued on unlock, revoked on sign-out. | `/etc/aeon/orb-publisher-delegation.json` |

A signature is **credited** to `delegation.user_key` when a valid delegation
names the signer as `orb_key`; otherwise to the signer. Names (`pub_name`,
`user_name`, labels) are decorative — only keys are identities.

Keys are 32 bytes, lowercase hex. Signatures are Ed25519 over the exact UTF-8
bytes shown (no pre-hash), standard base64. `\n` is one line-feed byte.

## 2. Signed statements

**Publication** — binds a key to one catalog entry (gossiped beside it):

    aeon-publish/1\n<cid>\n<name>\n<sha256>\n<added_at_ms>

**Delegation** — signed by the user key:

    aeon-delegate/1\n<user_key>\n<orb_key>\n<issued_ms>

**Origin** — signed by the origin's Orb key and stored *inside* the shared
directory's `model-card.json`:

    aeon-origin/1\n<name>\n<source>\n<verified: 0|1>\n<signed_ms>\n
    <sha256> <size> <path>\n        ← one line per file, sorted by path as UTF-8 bytes

It lists every file in the directory except `model-card.json`. Because it
names plain file hashes it is **independent of transport**: it verifies the
same whether the bytes came over IPFS, BitTorrent or a copied folder. Because
it is part of the directory, every mirror carries it unchanged, and replacing
it produces a different CID — a different publication, credited to whoever
made it.

**Peer binding** — an Orb key vouching for its own libp2p peer id:

    aeon-peer/1\n<orb_key>\n<peer_id>

## 3. `model-card.json` (schema `aeon-model-card/2`)

```json
{
  "schema": "aeon-model-card/2",
  "name": "...", "file": "<primary weights path>",
  "size_bytes": 0, "sha256": "<primary file sha256>",
  "card": { "kind": "", "base_model": "", "params": "", "quant": "", "license": "",
            "description": "", "intended_use": "", "tags": [], "format": "",
            "image": "", "readme": false, "nsfw": false, "gallery": [] },
  "verified": false, "source": "<upstream URL or empty>",
  "files": [ { "path": "a/b.safetensors", "size": 0, "sha256": "<hex>" } ],
  "origin": { "key": "<orb key>", "name": "", "signed_ms": 0, "sig": "<base64>",
              "delegation": { "user_key": "", "user_name": "", "orb_key": "", "issued_ms": 0, "sig": "" } },
  "shared_by": { "id": "", "label": "" }, "created_ms": 0
}
```

`verified` means the publisher checked every file against the hash `source`
publishes. It is covered by the origin signature, so it is an *attributable
claim*, not a fact; a receiver keeps it only when the origin signature holds.

Schema `/1` cards (no `files`, no `origin`) are still accepted: the receiver
holds the primary file to the entry's `sha256` and shows the model as unsigned.

**Vendor mirrors.** A byte-identical copy of a vendor's artifact (the Hailo
model zoo) carries `"origin": {"upstream": "<vendor tag>", "signed_ms": 0}` with
no signature, no `shared_by`, and `created_ms: 0`. Nothing in the card is
specific to one node, and the directory is added with fixed parameters —
CIDv1, raw leaves, `size-1048576` chunks, sha2-256 — so **every node that
mirrors the same file produces the same CID** and they all seed one swarm.

## 4. Catalog entry (what is gossiped)

`cid`, `name`, `file`, `size_bytes`, `sha256`, `card`, `added_at_ms`,
`origin_id`, `origin_label`, `verified`, `source`, `pub_key`, `pub_name`,
`pub_sig`, and optionally `delegation` and `btih` (BitTorrent v1 infohash of
the same files, hex40, when the announcer also seeds them there). Unknown
fields must be preserved when an entry is re-announced.

## 5. Discovery

- **Rendezvous.** Every node pins and provides the raw block
  `{"protocol":"aeon-model-share","topic":"aeon-model-share/v1","v":1}\n`
  (CID `bafkreidgutrbna4c363izsplxfzimihkz2fv3svoewwqcdqfmtczzfm46i`).
  `routing findprovs` of that CID is the directory of live nodes; dial the
  ones you are not connected to.
- **Gossip.** Floodsub topic `aeon-model-share/v1`. Every 60 s, and after any
  change, a node publishes:

  ```json
  { "v": 1, "peer_id": "", "label": "", "addrs": [], "seen": [], "updated_ms": 0,
    "models": [ <catalog entry> ], "starred": [ "<cid>" ],
    "orb_key": "", "orb_sig": "<aeon-peer/1 signature>" }
  ```

- **Acceptance.** Subscribe with the envelope (`pubsub sub --enc=json`) and
  accept an announcement **only if its `peer_id` equals the envelope's `from`**
  — the peer libp2p authenticated. Rebuild it field by field: at most 500
  models, 16 KiB per entry, CIDs `[A-Za-z0-9]{10,128}`, printable labels ≤ 64
  chars. Record it under your own receive time; drop a peer after 15 minutes
  of silence. Keep announcements under 900 KiB (pubsub drops messages over
  1 MiB).
- **Origin seeder.** A host is a model's origin seeder when its `orb_sig`
  verifies for `(orb_key, peer_id)` and that `orb_key` is the entry's
  `pub_key`.

## 6. Verifying a download

In this order, before the content is kept, pinned or re-announced:

1. No symbolic links, no hidden files, no special files.
2. Hash every file. With a manifest: the set of `(path, size, sha256)` must
   equal `files` exactly — nothing changed, missing or extra. Without one: the
   primary file must match the entry's `sha256`.
3. If `origin.sig` is present it must verify over that manifest; if it is
   present and does not, reject.
4. Format gate (no executables, scripts, pickle/zip weights, or fake
   extensions), then a malware scan where available.

A failure counts against the credited publisher on the verifying node.

## 7. Reputation

There is no global score. Each node computes 0–100 per credited identity,
weighting evidence by how hard it is to forge: the operator's own trust or
block decision (100 / 0) outranks everything; then downloads this node
verified itself (+15 each, up to 3); then other nodes mirroring the
publisher's models; then stars, capped lowest. One failed verification caps
the publisher at 5. Unsigned content scores 0. A node also remembers which
identity it *first* saw offering a given weights hash and flags a later,
different identity offering the same bytes as a re-publication.

## 8. Notes for a BitTorrent bridge (FrostByte)

- `frostbyte.release` already has the same `files: [{path, size, sha256}]`
  list. Sort it by path and it is the `aeon-origin/1` manifest; put the IPFS
  directory CID in `cid`. Store the origin statement beside the release.
- Verify `aeon-origin/1` after a download by **either** transport. That is
  what makes the first seeder checkable on both networks.
- A ready-to-port verifier (node:crypto only, in FrostByte's module and test
  style) is in [`bridge/frostbyte/`](./bridge/frostbyte/): `src/shared/aeon-origin.js`
  and `tests/aeon-origin.test.js`, passing against the shared vectors.
- To appear in the Orb index: pin + provide the rendezvous block, subscribe
  and publish on the topic, and include `btih` in entries you also seed over
  BitTorrent. An Orb shows `btih`; it does not fetch over BitTorrent.
- Dual seeding stays an explicit, per-collection choice on the bridge.
