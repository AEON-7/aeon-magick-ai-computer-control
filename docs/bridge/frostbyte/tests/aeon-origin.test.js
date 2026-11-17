import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {
  compareToManifest, identityOf, manifestFromRelease, originSigningBytes, sortManifest,
  verifyDelegation, verifyIdentityProof, verifyOrigin, verifyPeerBinding,
} from '../src/shared/aeon-origin.js';

// In FrostByte the fixture sits in tests/fixtures; in the Orb repo it is docs/model-share-vectors.json.
const fixture = ['./fixtures/model-share-vectors.json', '../../../model-share-vectors.json']
  .map(p => new URL(p, import.meta.url)).find(u => fs.existsSync(u));
const v = JSON.parse(fs.readFileSync(fixture));

const card = (over = {}) => ({
  name: v.origin.name, source: v.origin.source, verified: v.origin.verified, files: v.origin.files,
  origin: { key: v.origin.signer, name: 'orb', signed_ms: v.origin.signed_ms, sig: v.origin.sig },
  ...over,
});
const delegation = { user_key: v.delegation.user_key, user_name: v.delegation.user_name, orb_key: v.delegation.orb_key, issued_ms: v.delegation.issued_ms, sig: v.delegation.sig };

test('signing text matches the Orb byte for byte', () => {
  const bytes = originSigningBytes({ name: v.origin.name, source: v.origin.source, verified: true, signedMs: v.origin.signed_ms, files: [...v.origin.files].reverse() });
  assert.equal(bytes.toString('utf8'), v.origin.signed_text);
});

test('an Orb origin statement verifies and is credited to its signer', () => {
  assert.deepEqual(verifyOrigin(card()), { ok: true, signer: v.origin.signer, identity: v.origin.signer, delegated: false, name: 'orb' });
});

test('with a delegation it is credited to the user identity', () => {
  const r = verifyOrigin(card({ origin: { ...card().origin, delegation } }));
  assert.equal(r.ok, true);
  assert.equal(r.identity, v.delegation.user_key);
  assert.equal(r.delegated, true);
  assert.equal(r.name, 'alice');
});

test('any change to the files or the claims breaks it', () => {
  const swapped = v.origin.files.map((f, i) => i === 1 ? { ...f, sha256: 'cd'.repeat(32) } : f);
  assert.equal(verifyOrigin(card({ files: swapped })).ok, false);
  assert.equal(verifyOrigin(card({ files: v.origin.files.slice(0, 2) })).ok, false);
  assert.equal(verifyOrigin(card({ files: [...v.origin.files, { path: 'extra.bin', size: 1, sha256: '00'.repeat(32) }] })).ok, false);
  assert.equal(verifyOrigin(card({ source: 'https://evil.example/m' })).ok, false);
  assert.equal(verifyOrigin(card({ verified: false })).ok, false);
  assert.equal(verifyOrigin(card({ name: 'Other' })).ok, false);
});

test('missing or malformed input is a clean false, never a throw', () => {
  for (const bad of [null, {}, card({ origin: null }), card({ origin: { key: 'zz', sig: 'x', signed_ms: 1 } }), card({ files: [] }), card({ files: [{ path: 'a\nb', size: 1, sha256: '00'.repeat(32) }] })]) {
    assert.equal(verifyOrigin(bad).ok, false);
  }
});

test('a forged or re-targeted delegation transfers no credit', () => {
  assert.equal(verifyDelegation(delegation), true);
  assert.equal(verifyDelegation({ ...delegation, issued_ms: delegation.issued_ms + 1 }), false);
  const otherOrb = 'ab'.repeat(32);
  assert.equal(identityOf(otherOrb, delegation), otherOrb);
  assert.equal(identityOf(v.delegation.orb_key, { ...delegation, user_key: 'cd'.repeat(32) }), v.delegation.orb_key);
});

test('a peer binding is good for exactly one peer', () => {
  const b = { orbKey: v.peer_binding.orb_key, peerId: v.peer_binding.peer_id, orbSig: v.peer_binding.sig };
  assert.equal(verifyPeerBinding(b), true);
  assert.equal(verifyPeerBinding({ ...b, peerId: b.peerId.slice(0, -1) + '2' }), false);
});

test('a release file list is the manifest, in UTF-8 byte order', () => {
  const release = { files: [{ path: 'b.bin', size: 2, sha256: 'BB'.repeat(32) }, { path: 'a.bin', size: 1, sha256: 'aa'.repeat(32) }] };
  assert.deepEqual(manifestFromRelease(release).map(f => f.path), ['a.bin', 'b.bin']);
  assert.equal(manifestFromRelease(release)[1].sha256, 'bb'.repeat(32));
  // U+FF5E sorts before U+1F600 as UTF-8 bytes but after it as UTF-16 code units.
  assert.deepEqual(sortManifest([{ path: '\u{1F600}' }, { path: '～' }]).map(f => f.path), ['～', '\u{1F600}']);
});

test('downloaded files are held to the manifest exactly', () => {
  const m = v.origin.files;
  assert.deepEqual(compareToManifest(m, m), { ok: true, mismatch: '' });
  assert.equal(compareToManifest(m.slice(1), m).mismatch, m[0].path);
  assert.equal(compareToManifest([...m, { path: 'x', size: 1, sha256: '00'.repeat(32) }], m).mismatch, 'x');
  assert.equal(compareToManifest(m.map((f, i) => i ? f : { ...f, size: f.size + 1 }), m).ok, false);
});

test('an identity proof verifies as posted and not when edited', () => {
  const p = v.identity_proof;
  const found = verifyIdentityProof(`my bio\n\n${p.block}\n\nmore text`);
  assert.deepEqual(found, { key: p.key, name: p.name, claim: p.claim, issuedMs: p.issued_ms });
  assert.equal(verifyIdentityProof(p.block.replace(p.claim, 'huggingface.co/mallory')), null);
  assert.equal(verifyIdentityProof(p.block.replace(`name: ${p.name}`, 'name: bob')), null);
  assert.equal(verifyIdentityProof('nothing here'), null);
  assert.equal(verifyIdentityProof(p.block.replace('-----END AEON IDENTITY PROOF-----', '')), null);
});
