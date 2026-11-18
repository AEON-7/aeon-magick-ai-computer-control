# Intergalactic Model Share — wire protocol

What two implementations must agree on to share one index and verify each
other's models. The Orb implements all of it (`aeon-supervisor/src/ipfs.rs`,
`ipfs/directory.rs`, `ipfs/upstream.rs`, `publisher.rs`, and the
`aeon-modelshare` gossip daemon);
a bridge to another network implements the parts it needs. Test vectors for every signature are
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

**Witness** — signed by an Orb key: "I saw `provider` publish these hashes for
`repo` at `revision`, at `observed_ms`" (§10):

    aeon-witness/1\n<provider>\n<repo>\n<revision>\n<observed_ms>\n
    <sha256> <size> <path>\n        ← one line per file whose SHA-256 the provider
                                      itself published, sorted by path as UTF-8 bytes

A file whose hash the importer computed, because the provider published none,
is never in a witness. A statement needs at least one line. The signature
proves who made the statement; `observed_ms` is that signer's claim, not proof
of when.

**Peer binding** — an Orb key vouching for its own libp2p peer id:

    aeon-peer/1\n<orb_key>\n<peer_id>

**Identity proof** — signed by the *user* key itself (never an Orb key), to
tie the identity to an account on another platform:

    aeon-identity-proof/1\n<user_key>\n<name>\n<claim>\n<issued_ms>

`claim` is one line naming the account, e.g. `huggingface.co/your-name`. It is
published as a text block the owner posts **on that account**:

    -----BEGIN AEON IDENTITY PROOF-----
    aeon-identity-proof/1
    key: <user_key>
    name: <name>
    claim: <claim>
    issued: <issued_ms>
    sig: <base64>
    -----END AEON IDENTITY PROOF-----

The signature shows the key holder wrote it; where it is posted shows the
account holder did. A verifier checks the first and must look at the second.

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
  "shared_by": { "id": "", "label": "" }, "created_ms": 0,
  "upstream_record": { "provider": "", "repo": "", "revision": "",
                       "files": [ { "path": "", "size": 0, "sha256": "<hex>", "hash_from": "provider" } ] },
  "witnesses": [ { "key": "<orb key>", "observed_ms": 0, "sig": "<base64>", "delegation": { } } ]
}
```

`upstream_record` and `witnesses` are present only on a model imported from a
provider (§10). They are not covered by the origin signature: the witness
carries its own.

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
`pub_sig`, and optionally `delegation`, `btih` (BitTorrent v1 infohash of
the same files, hex40, when the announcer also seeds them there),
`upstream_record` and `witnesses` (§10). Unknown fields must be preserved when
an entry is re-announced.

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
- **Witnesses.** Rebuild an entry's `upstream_record` field by field; if it is
  not well-formed, remove it and the entry's `witnesses`, and keep the entry.
  Verify every witness signature against that record and drop each one that
  fails. Drop a delegation that does not verify and keep its witness, credited
  to the signing key. At most 16 witnesses per entry. The 16 KiB entry limit
  includes them: a sender that would exceed it leaves out other publishers'
  witnesses first, then its own, then the record.
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

## 8. BitTorrent alongside IPFS

The same model can be seeded as a torrent. Nothing about verification changes:
the origin statement and the file manifest name plain per-file hashes, so they
hold whichever network delivered the bytes.

- An entry may carry `btih`, the BitTorrent v1 infohash of a torrent holding
  exactly the model's files. An Orb carries the field through its catalog and
  gossip, and seeds that torrent itself when it pulled the model from one (§9).
- A torrent's piece hashes prove the bytes match *that torrent*. They say
  nothing about who published it or whether it is the model it claims to be;
  only the manifest check in §6 and, where present, the origin signature do.
- A bridge that speaks both networks verifies `aeon-origin/1` after a download
  by **either** transport. A verifier using only `node:crypto`, with tests that
  run against the shared vectors, is in [`bridge/frostbyte/`](./bridge/frostbyte/).
- To appear in the Orb index, a bridge pins and provides the rendezvous block,
  subscribes and publishes on the topic, and includes `btih` in entries it also
  seeds as torrents.

## 9. Directories (`aeon-directory/1`)

A directory is a search service that lists models and says where to get them.
It is deliberately small so that any catalogue can offer it. An Orb has none
configured out of the box; the operator adds the address of each one they
trust. The address must be `https://` (plain `http://` is accepted only for a
directory on the Orb itself), without credentials, query or fragment.

    GET {base}/search?q=<text>&limit=<n>&cursor=<opaque>
        → { "items": [ <listing>, … ], "cursor": "<opaque or empty>" }
    GET {base}/listings/{id}
        → <listing>

```json
{ "schema": "aeon-directory/1",
  "id": "<1–128 chars of A-Z a-z 0-9 . _ - :>",
  "name": "…", "description": "…", "publisher": "…", "license": "…",
  "source": "<https URL of the upstream, or empty>",
  "files": [ { "path": "weights/model.gguf", "size": 0, "sha256": "<hex64>" } ],
  "btih": "<hex40, optional>", "cid": "<IPFS CID, optional>",
  "trackers": [ "<http, https or udp announce URL>" ],
  "updated_ms": 0 }
```

`files` is the whole contract: every file of the model, 1–5000 of them, as a
relative path with its size and SHA-256. At least one of `btih` and `cid` is
required. `publisher` is a display name supplied by the directory. **A listing
is a claim about content, not an identity** — it carries no signature, so a
model pulled from a directory is recorded as a mirror
(`origin.upstream = "directory:<publisher>"`), never as a signed origin.

A receiver rebuilds each listing field by field and drops any that names a
path outside the model folder, a hidden file, a duplicate or overlapping path,
or a file without a valid hash. Answers over 2 MiB and redirects are refused.

**Pulling a listing over BitTorrent**, in this order:

1. Add the torrent by `btih` (and `trackers`).
2. As soon as the torrent's metadata arrives, its file list — each path and
   size, with or without one wrapping folder — must equal `files` exactly. A
   mismatch is rejected before any content is downloaded. When the download
   completes the torrent is stopped; it does not seed during steps 3–5.
3. Copy each listed file out of the torrent's folder without following a
   symbolic link at any step, hashing as it is written. Size and SHA-256 must
   match the listing.
4. Apply the format gate and malware scan from §6 to the copy.
5. Add the copy to IPFS as a mirror: the card holds nothing specific to this
   node and the import parameters are the fixed ones from §3, so every node
   that pulls the same listing produces the same CID.
6. With seeding on: announce the entry with its `btih` and start the torrent
   again so it seeds. With seeding off: keep a private copy, unpin, and remove
   the torrent — nothing is provided on either network.

A failed check at any step removes the torrent and everything it downloaded.
Removing the model later stops both networks. BitTorrent is not run while all
traffic is routed through Tor.

While a torrent downloads, a client trades pieces of that same torrent with
other peers. That is inherent to BitTorrent (an upload limit of zero also
blocks the request messages and stalls the download), and those pieces are
held to the torrent's own piece hashes. What the checks above control is what
a node keeps, what it adds to IPFS, and what it goes on seeding.

## 10. Upstream records and witnesses

A model imported from a provider keeps a record of what that provider
published, so that it stays checkable after the provider changes or removes
the model.

**The record.** At import the Orb pins one revision, reads the file list and
every file at that revision only, and stores:

| Field | huggingface | ollama | civitai |
|---|---|---|---|
| `provider` | `huggingface` | `ollama` | `civitai` |
| `repo` | `org/model` | `namespace/model:tag` | the model id |
| `revision` | the commit id (40 hex) | the manifest digest, `sha256:<hex>`: the SHA-256 of the manifest as the registry served it | the model-version id |
| a file's hash is `provider` when | the file is in LFS (the LFS object id is its SHA-256) | always (the layer digest) | the version lists `hashes.SHA256` for it |

`provider` is 1–32 characters of `a-z 0-9 -`; `repo` and `revision` are one
line of at most 200 characters; `files` holds 1–512 entries with unique
relative paths. Each file is `{path, size, sha256, hash_from}`. `hash_from` is
`provider` when the provider itself published that SHA-256 and the downloaded
bytes matched it, and `computed` when the provider published none and the
importer hashed what it received. **A computed hash is never presented as the
provider's.** Every recorded file must be a file of the model, with the same
size and hash as in the card's `files`; a record that says otherwise is
discarded by the receiver.

**Witnesses.** In an entry and in a card, each element of `witnesses` is
`{key, observed_ms, sig, delegation?}`: one `aeon-witness/1` signature (§2)
over the `upstream_record` beside it and that `observed_ms`. The importing Orb
adds its own at import. An Orb that mirrors the model and later asks the
provider itself, and finds the same hashes at the pinned revision, adds its
own to its copy of the entry. A witness is credited like any other signature
(§1).

Two witnesses **record the same hashes** when their records name the same
`provider`, the same `repo`, and the same set of provider-published
`(path, size, sha256)` lines; the revision may differ, because a later commit
that leaves the weights alone publishes the same hashes. A node reports, per
model, how many distinct credited identities recorded the same hashes and the
earliest `observed_ms` among them, counting across every entry it can see,
whatever its CID. It also reports identities that signed a *different* hash or
size for a path at the same `provider`, `repo` and `revision`, which a pinned
revision cannot honestly have.

What this proves and what it does not: a valid witness proves that the holder
of a key made the statement. It does not prove the time, and identities are
free to create, so a count of witnesses is evidence in proportion to how much
the reader trusts those identities (§7). What a node checked itself, and when,
it knows from its own history; everything else is shown as a claim.

**Re-checking.** A node may ask the provider again what it publishes for the
recorded paths — at the pinned revision, and on the default branch (for
Ollama, where the tag points now; for Civitai, the model's own list of
versions). It reads metadata only. Each scope ends in one of:

| Verdict | Meaning |
|---|---|
| `confirmed` | Every recorded provider hash is published unchanged; files without one are still listed (and, where the provider gives sizes, have the same size). |
| `changed` | A recorded path is published with a different hash or size, or the provider stopped publishing a hash it used to publish. |
| `removed` | The repository, the revision or a recorded file is gone, or access is now refused (401, 403, 404, 410, 451, or the repository is marked gated, private or disabled). |
| `unreachable` | No usable answer: a network error, a timeout, a rate limit, a server error, any other status, or an answer that cannot be read. Evidence of nothing. |

The result of a re-check is the most telling of its two scopes, in the order
`changed`, `removed`, `unreachable`, `confirmed`. A changed or removed
upstream changes nothing about the model on the network: its files, its
manifest and its witnesses are as they were. That is the point.

**The history (local, not on the wire).** Each Orb keeps its own append-only
log, `upstream-history.jsonl`: one JSON object per line, each with `prev`, the
SHA-256 of the previous line's bytes (64 zeros on the first line), and an
`event` of `recorded`, `confirmed`, `changed`, `removed` or `unreachable`.
Lines are only ever appended. A line that does not follow from the one before
it, or cannot be read, is reported and left as found. The chain shows an edit,
an insertion or a removal anywhere except a cut at the very end, and it does
not stop someone with full control of the node from rebuilding the whole file;
what they cannot rebuild are witnesses other nodes already hold.
