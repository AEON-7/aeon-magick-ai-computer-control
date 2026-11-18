/** AEON Model Share origin verification — the transport-independent half of
 * the IPFS/BitTorrent bridge. Byte formats follow the Orb's publisher.rs and
 * docs/MODEL_SHARE_PROTOCOL.md: Ed25519 over exact UTF-8 text, no prehash,
 * 32-byte hex keys, base64 signatures. Uses node:crypto only.
 *
 * An origin statement names every file of a model by path, size and SHA-256,
 * so it verifies the same whether the bytes arrived over IPFS or BitTorrent —
 * which is what lets the first seeder be checked on either network.
 */
import { createPublicKey, verify as cryptoVerify } from 'node:crypto';

const HEX64 = /^[a-f0-9]{64}$/i;
const SPKI_ED25519 = Buffer.from('302a300506032b6570032100', 'hex');

function verifyEd25519(pubKeyHex, bytes, sigB64) {
  if (typeof pubKeyHex !== 'string' || !HEX64.test(pubKeyHex)) return false;
  if (typeof sigB64 !== 'string' || sigB64.length > 256) return false;
  const sig = Buffer.from(sigB64, 'base64');
  if (sig.length !== 64) return false;
  try {
    const key = createPublicKey({ key: Buffer.concat([SPKI_ED25519, Buffer.from(pubKeyHex, 'hex')]), format: 'der', type: 'spki' });
    return cryptoVerify(null, bytes, key, sig);
  } catch {
    return false;
  }
}

/** Manifest order is by path as UTF-8 BYTES (what the Orb's Rust sort does).
 * JavaScript's default string order is UTF-16 and differs for some non-ASCII
 * names, which would silently produce a different signed text. */
export function sortManifest(files) {
  return [...files].sort((a, b) => Buffer.compare(Buffer.from(a.path, 'utf8'), Buffer.from(b.path, 'utf8')));
}

/** Any release record that lists its files as {path, size, sha256}: sorted,
 * that list IS the origin manifest. */
export function manifestFromRelease(release) {
  return sortManifest(release.files.map(({ path, size, sha256 }) => ({ path, size, sha256: sha256.toLowerCase() })));
}

function assertManifest(files) {
  if (!Array.isArray(files) || !files.length) throw new Error('An origin manifest needs at least one file.');
  for (const f of files) {
    if (typeof f?.path !== 'string' || !f.path || /[\x00-\x1f\x7f]/.test(f.path)) throw new Error('Invalid manifest path.');
    if (!Number.isSafeInteger(f.size) || f.size < 0) throw new Error('Invalid manifest size.');
    if (typeof f.sha256 !== 'string' || !HEX64.test(f.sha256)) throw new Error('Invalid manifest SHA-256.');
  }
}

/** aeon-origin/1 — the text an origin signs. `files` in any order. */
export function originSigningBytes({ name, source = '', verified = false, signedMs, files }) {
  assertManifest(files);
  if (!Number.isSafeInteger(signedMs)) throw new Error('Invalid signing time.');
  const lines = sortManifest(files).map(f => `${f.sha256.toLowerCase()} ${f.size} ${f.path}\n`).join('');
  return Buffer.from(`aeon-origin/1\n${name}\n${source}\n${verified ? 1 : 0}\n${signedMs}\n${lines}`, 'utf8');
}

/** aeon-delegate/1 — a user identity authorising an Orb key to publish for it. */
export function verifyDelegation(d) {
  if (!d || !HEX64.test(d.user_key || '') || !HEX64.test(d.orb_key || '') || !Number.isSafeInteger(d.issued_ms)) return false;
  return verifyEd25519(d.user_key, Buffer.from(`aeon-delegate/1\n${d.user_key}\n${d.orb_key}\n${d.issued_ms}`, 'utf8'), d.sig);
}

/** The identity a signature is credited to: the delegating user key when a
 * valid delegation names this signer, else the signer itself. */
export function identityOf(signerKey, delegation) {
  return delegation && delegation.orb_key === signerKey && verifyDelegation(delegation) ? delegation.user_key : signerKey;
}

/** Verify a model card's origin statement against a file manifest.
 * `card` is the parsed model-card.json (or the same fields kept beside a
 * release). Returns who it is credited to; `ok` is false for a missing or
 * invalid signature — never throws on untrusted input. */
export function verifyOrigin(card) {
  const origin = card?.origin;
  const none = { ok: false, signer: '', identity: '', delegated: false, name: '' };
  if (!origin || typeof origin.sig !== 'string' || !origin.sig) return none;
  let bytes;
  try {
    bytes = originSigningBytes({ name: card.name, source: card.source, verified: card.verified === true, signedMs: origin.signed_ms, files: card.files });
  } catch {
    return none;
  }
  if (!verifyEd25519(origin.key, bytes, origin.sig)) return none;
  const identity = identityOf(origin.key, origin.delegation);
  const delegated = identity !== origin.key;
  return { ok: true, signer: origin.key, identity, delegated, name: delegated ? origin.delegation.user_name || '' : origin.name || '' };
}

/** Compare the files actually on disk with a manifest: exact set equality on
 * (path, size, sha256). Returns the first path that differs, if any. */
export function compareToManifest(actual, manifest) {
  const key = f => `${f.sha256.toLowerCase()} ${f.size} ${f.path}`;
  const want = new Set(manifest.map(key));
  const have = new Set(actual.map(key));
  const odd = actual.find(f => !want.has(key(f))) || manifest.find(f => !have.has(key(f)));
  return { ok: !odd && want.size === have.size, mismatch: odd ? odd.path : '' };
}

/** aeon-witness/1 — the text a witness signs: "I saw this provider publish
 * these hashes for this repository at this revision, at this time". `record`
 * is an upstream record ({provider, repo, revision, files}); each file carries
 * `hash_from`, and only the files whose SHA-256 the provider itself published
 * ("provider") are part of the statement. A hash computed by the importer is
 * not something the provider said, so it is never signed as if it were. */
export function witnessSigningBytes(record, observedMs) {
  const oneLine = s => typeof s === 'string' && s.length > 0 && [...s].length <= 200 && !/[\x00-\x1f\x7f-\x9f]/.test(s);
  if (typeof record?.provider !== 'string' || !/^[a-z0-9-]{1,32}$/.test(record.provider)) throw new Error('Invalid provider.');
  if (!oneLine(record.repo) || !oneLine(record.revision)) throw new Error('Invalid repository or revision.');
  if (!Number.isSafeInteger(observedMs) || observedMs <= 0) throw new Error('Invalid observation time.');
  assertManifest(record.files);
  if (record.files.some(f => f.hash_from !== 'provider' && f.hash_from !== 'computed')) throw new Error('Invalid hash source.');
  const published = record.files.filter(f => f.hash_from === 'provider');
  if (!published.length) throw new Error('The provider published no hash: there is nothing to witness.');
  const lines = sortManifest(published).map(f => `${f.sha256.toLowerCase()} ${f.size} ${f.path}\n`).join('');
  return Buffer.from(`aeon-witness/1\n${record.provider}\n${record.repo}\n${record.revision}\n${observedMs}\n${lines}`, 'utf8');
}

/** Verify one witness ({key, observed_ms, sig, delegation?}) against the
 * upstream record it travels with. Returns who it is credited to and the time
 * the signer CLAIMS: a signature proves who made the statement, not when, so
 * `observedMs` is never evidence by itself. `ok` is false for a missing or
 * invalid signature — never throws on untrusted input. */
export function verifyWitness(record, witness) {
  const none = { ok: false, signer: '', identity: '', delegated: false, name: '', observedMs: 0 };
  if (!witness || typeof witness.sig !== 'string' || !witness.sig) return none;
  let bytes;
  try {
    bytes = witnessSigningBytes(record, witness.observed_ms);
  } catch {
    return none;
  }
  if (!verifyEd25519(witness.key, bytes, witness.sig)) return none;
  const identity = identityOf(witness.key, witness.delegation);
  const delegated = identity !== witness.key;
  return { ok: true, signer: witness.key, identity, delegated, name: delegated ? witness.delegation.user_name || '' : '', observedMs: witness.observed_ms };
}

/** aeon-peer/1 — an Orb key vouching for its own libp2p peer id. A host whose
 * bound key signed a model is that model's origin seeder. */
export function verifyPeerBinding({ orbKey, peerId, orbSig }) {
  if (typeof peerId !== 'string' || !/^[A-Za-z0-9]{32,128}$/.test(peerId)) return false;
  return verifyEd25519(orbKey, Buffer.from(`aeon-peer/1\n${orbKey}\n${peerId}`, 'utf8'), orbSig);
}

/** aeon-identity-proof/1 — a user key claiming an account elsewhere. Finds the
 * block inside arbitrary pasted text and returns its fields when the
 * signature is the key's own; null otherwise. It cannot tell WHERE the block
 * was posted — that half of the check belongs to the caller. */
export function verifyIdentityProof(text) {
  if (typeof text !== 'string' || text.length > 16384) return null;
  const m = text.match(/-----BEGIN AEON IDENTITY PROOF-----\s*([\s\S]*?)-----END AEON IDENTITY PROOF-----/);
  if (!m) return null;
  const lines = m[1].split('\n').map(l => l.trim()).filter(Boolean);
  if (lines.shift() !== 'aeon-identity-proof/1') return null;
  const out = {};
  for (const label of ['key', 'name', 'claim', 'issued', 'sig']) {
    const line = lines.shift();
    if (typeof line !== 'string' || !line.startsWith(`${label}:`)) return null;
    out[label] = line.slice(label.length + 1).trim();
  }
  const key = out.key.toLowerCase();
  const issuedMs = Number(out.issued);
  if (!HEX64.test(key) || !Number.isSafeInteger(issuedMs)) return null;
  const bytes = Buffer.from(`aeon-identity-proof/1\n${key}\n${out.name}\n${out.claim}\n${issuedMs}`, 'utf8');
  return verifyEd25519(key, bytes, out.sig) ? { key, name: out.name, claim: out.claim, issuedMs } : null;
}
