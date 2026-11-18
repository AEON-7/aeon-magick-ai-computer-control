"""Off-device tests for the Model Share gossip daemon's logic.

    python3 -m unittest discover -s image-builder/tests -v

Covers what does not need an IPFS node: which messages are accepted, which
witness statements survive, how the registry ages and is written, and how
dialing is rationed.
"""
import base64
import importlib.machinery
import importlib.util
import json
import os
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
DAEMON = os.path.join(HERE, "..", "stage-aeon", "02-services", "files", "aeon-modelshare")

_tmp = tempfile.TemporaryDirectory()
os.environ["AEON_MODELS_DIR"] = os.path.join(_tmp.name, "models")
os.environ["AEON_RUN_DIR"] = os.path.join(_tmp.name, "run")
os.environ["AEON_IPFS_SCRIPT"] = "/nonexistent/aeon-ipfs"
os.makedirs(os.environ["AEON_MODELS_DIR"])

_loader = importlib.machinery.SourceFileLoader("aeon_modelshare", DAEMON)
_spec = importlib.util.spec_from_loader("aeon_modelshare", _loader)
ms = importlib.util.module_from_spec(_spec)
_loader.exec_module(ms)

ALICE = "12D3KooWAliceAliceAliceAliceAliceAliceAliceAliceAlice"
BOB = "12D3KooWBobBobBobBobBobBobBobBobBobBobBobBobBobBobBob"
CID = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi"

# The published signature vectors: the same file the supervisor's own tests
# and the JavaScript verifier are held to.
with open(os.path.join(HERE, "..", "..", "docs", "model-share-vectors.json")) as _f:
    VECTORS = json.load(_f)


def announcement(pid, **over):
    ann = {"v": 1, "peer_id": pid, "label": "orb", "addrs": ["203.0.113.7"], "seen": [],
           "updated_ms": 1, "models": [{"cid": CID, "name": "m"}], "starred": [CID]}
    ann.update(over)
    return ann


def envelope(sender, ann, encoding="multibase"):
    raw = json.dumps(ann).encode()
    if encoding == "multibase":
        data = "u" + base64.urlsafe_b64encode(raw).decode().rstrip("=")
    elif encoding == "base64":
        data = base64.b64encode(raw).decode()
    else:
        data = raw.decode()
    return json.dumps({"from": sender, "data": data, "seqno": "uAAAA", "topicIDs": ["uYWVvbg"]})


class Envelopes(unittest.TestCase):
    def test_payload_decodes_from_every_encoding_kubo_has_used(self):
        for enc in ("multibase", "base64", "raw"):
            sender, payload = ms.decode_envelope(envelope(ALICE, announcement(ALICE), enc))
            self.assertEqual(sender, ALICE, enc)
            self.assertEqual(json.loads(payload)["peer_id"], ALICE, enc)

    def test_a_line_that_is_not_an_envelope_is_ignored(self):
        for line in ("", "garbage", "[]", json.dumps(announcement(ALICE)), '{"from": 1, "data": "x"}'):
            self.assertEqual(ms.decode_envelope(line), (None, None), line)


class Acceptance(unittest.TestCase):
    def test_an_announcement_is_accepted_only_from_the_peer_it_names(self):
        _, payload = ms.decode_envelope(envelope(ALICE, announcement(ALICE)))
        ann, why = ms.accept_message(ALICE, payload)
        self.assertIsNone(why)
        self.assertEqual(ann["peer_id"], ALICE)

        # Bob publishes an announcement claiming to be Alice: it would replace
        # Alice's catalog in every index. It must be dropped.
        ann, why = ms.accept_message(BOB, payload)
        self.assertIsNone(ann)
        self.assertIn("claims to be", why)

    def test_malformed_entries_are_dropped_and_sizes_bounded(self):
        models = [
            {"cid": CID, "name": "good"},
            {"cid": "../../etc", "name": "bad cid"},
            {"cid": CID, "name": ""},
            {"cid": CID},
            "not a dict",
            {"cid": CID, "name": "huge", "card": {"description": "x" * (ms.MAX_ENTRY_BYTES + 1)}},
        ]
        ann = ms.sanitize_announcement(announcement(
            ALICE, models=models, label="bad\x00\x1b[31mname" + "x" * 500,
            addrs=["1.2.3.4", 7, "y" * 999] + ["5.6.7.8"] * 40,
            starred=[CID, "nope!", 3], seen=[BOB, "short", 5], evil_extra={"a": 1}))
        self.assertEqual([m["name"] for m in ann["models"]], ["good"])
        self.assertNotIn("\x00", ann["label"])
        self.assertNotIn("\x1b", ann["label"])
        self.assertLessEqual(len(ann["label"]), 64)
        self.assertLessEqual(len(ann["addrs"]), 16)
        self.assertTrue(all(isinstance(a, str) and len(a) <= 256 for a in ann["addrs"]))
        self.assertEqual(ann["starred"], [CID])
        self.assertEqual(ann["seen"], [BOB])
        self.assertNotIn("evil_extra", ann)

    def test_more_models_than_the_cap_are_cut(self):
        many = [{"cid": CID, "name": "m%d" % i} for i in range(ms.MAX_MODELS + 50)]
        self.assertEqual(len(ms.sanitize_announcement(announcement(ALICE, models=many))["models"]), ms.MAX_MODELS)

    def test_the_origin_binding_passes_through_untouched(self):
        ann = ms.sanitize_announcement(announcement(ALICE, orb_key="ab" * 32, orb_sig="c2ln"))
        self.assertEqual((ann["orb_key"], ann["orb_sig"]), ("ab" * 32, "c2ln"))
        self.assertNotIn("orb_key", ms.sanitize_announcement(announcement(ALICE, orb_key="short", orb_sig="x")))

    def test_a_delegation_rides_along_only_when_well_formed(self):
        good = {"user_key": "cd" * 32, "orb_key": "ab" * 32, "sig": "c2ln", "user_name": "alice\x00", "issued_ms": 5, "junk": 1}
        ann = ms.sanitize_announcement(announcement(ALICE, orb_key="ab" * 32, orb_sig="c2ln", delegation=good))
        self.assertEqual(ann["delegation"], {"user_key": "cd" * 32, "orb_key": "ab" * 32, "sig": "c2ln", "user_name": "alice", "issued_ms": 5})
        for bad in ("x", {"user_key": 1}, {"user_key": "cd" * 32, "orb_key": "ab" * 32}, {"user_key": "c" * 500, "orb_key": "a", "sig": "s"}):
            self.assertNotIn("delegation", ms.sanitize_announcement(announcement(ALICE, orb_key="ab" * 32, orb_sig="c2ln", delegation=bad)))
        # Without a binding there is nothing for a delegation to attach to.
        self.assertNotIn("delegation", ms.sanitize_announcement(announcement(ALICE, delegation=good)))

    def test_things_that_are_not_announcements(self):
        for bad in (None, [], "x", {}, {"peer_id": ALICE}, {"peer_id": "x", "models": []},
                    {"peer_id": ALICE, "models": "nope"}):
            self.assertIsNone(ms.sanitize_announcement(bad))


def entry(name="m", **over):
    """A model entry with every field the supervisor knows, as an Orb sends it."""
    m = {"cid": CID, "name": name, "file": "model.gguf", "size_bytes": 20, "sha256": "ab" * 32,
         "card": {"kind": "llm", "base_model": "", "params": "8B", "quant": "q4", "license": "mit",
                  "description": "d", "intended_use": "", "tags": ["a", "b"], "format": "gguf",
                  "image": "", "readme": True, "nsfw": False, "gallery": []},
         "added_at_ms": 1760000000000, "origin_id": "orb-1", "origin_label": "an orb",
         "verified": True, "source": "https://example.test/m",
         "pub_key": "", "pub_name": "", "pub_sig": ""}
    m.update(over)
    return m


def card(**over):
    return dict(entry()["card"], **over)


DELEGATION = {"user_key": "cd" * 32, "user_name": "alice", "orb_key": "ab" * 32, "issued_ms": 5, "sig": "c2ln"}


def raw_announcement(pid, models_json, head='"label": "orb", "addrs": [], "updated_ms": 1'):
    """An announcement as TEXT, for values `json.dumps` would never produce
    but a peer can send: NaN, a number of 400 digits, an unpaired surrogate."""
    return '{"v": 1, "peer_id": "%s", %s, "seen": [], "starred": [], "models": [%s]}' % (pid, head, models_json)


def strictly_readable(text):
    """Would a strict JSON parser read this file? Written out here, on its
    own terms, as what the supervisor's parser (serde_json) requires: no NaN
    or Infinity, every number finite as a double, no unpaired surrogate, and
    nesting well inside its limit of 128."""
    def refuse(name):
        raise ValueError(name)
    try:
        doc = json.loads(text, parse_constant=refuse)
    except ValueError:
        return False

    def walk(v, depth):
        if isinstance(v, bool) or v is None:
            return True
        if isinstance(v, int):
            return v.bit_length() <= 64
        if isinstance(v, float):
            return v == v and v not in (float("inf"), float("-inf"))
        if isinstance(v, str):
            return not any(0xD800 <= ord(ch) <= 0xDFFF for ch in v)
        if depth >= 64:
            return False
        if isinstance(v, list):
            return all(walk(x, depth + 1) for x in v)
        return all(walk(k, depth) and walk(x, depth + 1) for k, x in v.items())
    return walk(doc, 0)


def witnessed(**over):
    """A model entry carrying the published witness vector."""
    w = VECTORS["witness"]
    m = {"cid": CID, "name": "m",
         "upstream_record": {"provider": w["provider"], "repo": w["repo"], "revision": w["revision"],
                             "files": [dict(f) for f in w["files"]]},
         "witnesses": [{"key": w["signer"], "observed_ms": w["observed_ms"], "sig": w["sig"]}]}
    m.update(over)
    return m


def accepted(*models):
    return ms.sanitize_announcement(announcement(ALICE, models=list(models)))["models"]


class Witnesses(unittest.TestCase):
    def setUp(self):
        ms._sig_cache.clear()

    def test_the_verifier_agrees_with_every_published_vector(self):
        for name in ("publication", "delegation", "origin", "peer_binding", "identity_proof", "witness"):
            v = VECTORS[name]
            key, text, sig = bytes.fromhex(v["signer"]), v["signed_text"].encode(), base64.b64decode(v["sig"])
            self.assertTrue(ms.ed25519_verify(key, text, sig), name)
            self.assertFalse(ms.ed25519_verify(key, text + b" ", sig), name)
            self.assertFalse(ms.ed25519_verify(key, text, sig[:63] + bytes([sig[63] ^ 1])), name)
            self.assertFalse(ms.ed25519_verify(bytes.fromhex(VECTORS["keys"]["user_key" if v["signer"] == VECTORS["keys"]["orb_key"] else "orb_key"]), text, sig), name)
        # Not signatures at all.
        self.assertFalse(ms.ed25519_verify(b"short", b"m", b"x" * 64))
        self.assertFalse(ms.ed25519_verify(b"\xff" * 32, b"m", b"\xff" * 64))

    def test_the_signed_text_is_rebuilt_byte_for_byte(self):
        w = VECTORS["witness"]
        shuffled = dict(witnessed()["upstream_record"], files=list(reversed(w["files"])))
        rec = ms.clean_record(shuffled)
        self.assertEqual(ms.witness_text(rec, w["observed_ms"]).decode(), w["signed_text"])
        # Only what the provider published is in it.
        self.assertNotIn("tokenizer.json", w["signed_text"])

    def test_a_witness_whose_signature_holds_travels_with_its_entry(self):
        (m,) = accepted(witnessed(card={"kind": "llm"}, something_new=7))
        self.assertEqual(len(m["witnesses"]), 1)
        self.assertEqual(m["witnesses"][0]["key"], VECTORS["witness"]["signer"])
        self.assertEqual(m["upstream_record"]["revision"], VECTORS["witness"]["revision"])
        # The rest of the entry passes through as it came.
        self.assertEqual((m["card"], m["something_new"]), ({"kind": "llm"}, 7))

    def test_a_bad_witness_is_dropped_and_the_entry_stays(self):
        good = witnessed()["witnesses"][0]
        flipped = base64.b64encode(bytes([base64.b64decode(good["sig"])[0] ^ 1]) + base64.b64decode(good["sig"])[1:]).decode()
        for name, bad in (
            ("another time", dict(good, observed_ms=good["observed_ms"] + 1)),
            ("altered signature", dict(good, sig=flipped)),
            ("someone else's key", dict(good, key=VECTORS["keys"]["user_key"])),
            ("no signature", dict(good, sig="")),
            ("not base64", dict(good, sig="!!!!")),
            ("no time", {"key": good["key"], "sig": good["sig"]}),
            ("not an object", "witness"),
        ):
            (m,) = accepted(witnessed(witnesses=[bad]))
            self.assertNotIn("witnesses", m, name)
            self.assertEqual(m["name"], "m", name)                 # the model is still listed
            self.assertIn("upstream_record", m, name)              # so is the (unsigned) record
        # A forged statement next to a real one: only the real one survives.
        (m,) = accepted(witnessed(witnesses=[dict(good, observed_ms=5), good, "junk"]))
        self.assertEqual(m["witnesses"], [good])

    def test_a_witness_does_not_hold_for_a_record_it_did_not_sign(self):
        for field, value in (("repo", "famous-lab/model"), ("revision", "f" * 40), ("provider", "civitai")):
            rec = dict(witnessed()["upstream_record"], **{field: value})
            (m,) = accepted(witnessed(upstream_record=rec))
            self.assertNotIn("witnesses", m, field)
        # A different hash for a file the provider published.
        rec = witnessed()["upstream_record"]
        rec["files"][0]["sha256"] = "cd" * 32
        (m,) = accepted(witnessed(upstream_record=rec))
        self.assertNotIn("witnesses", m)
        # The computed file is outside the statement, so it changes nothing.
        rec = witnessed()["upstream_record"]
        rec["files"][1]["sha256"] = "cd" * 32
        (m,) = accepted(witnessed(upstream_record=rec))
        self.assertEqual(len(m["witnesses"]), 1)

    def test_a_delegation_rides_on_a_witness_only_when_the_user_key_signed_it(self):
        d = VECTORS["delegation"]
        delegation = {"user_key": d["user_key"], "user_name": "alice\x00", "orb_key": d["orb_key"],
                      "issued_ms": d["issued_ms"], "sig": d["sig"], "junk": 1}
        good = witnessed()["witnesses"][0]
        (m,) = accepted(witnessed(witnesses=[dict(good, delegation=delegation)]))
        self.assertEqual(m["witnesses"][0]["delegation"],
                         {"user_key": d["user_key"], "user_name": "alice", "orb_key": d["orb_key"],
                          "issued_ms": d["issued_ms"], "sig": d["sig"]})
        # Forged, re-dated, or for another Orb key: the statement stays, credited
        # to the key that signed it; the borrowed name does not.
        for bad in (dict(delegation, user_key="cd" * 32), dict(delegation, issued_ms=d["issued_ms"] + 1),
                    dict(delegation, orb_key="ab" * 32), "x", {"user_key": 1}):
            (m,) = accepted(witnessed(witnesses=[dict(good, delegation=bad)]))
            self.assertEqual(m["witnesses"], [good], bad)

    def test_a_malformed_record_is_removed_together_with_its_witnesses(self):
        def with_file(**change):
            rec = witnessed()["upstream_record"]
            rec["files"][0].update(change)
            return rec
        base = witnessed()["upstream_record"]
        bad_records = [
            "nope", [], None, {},
            dict(base, provider="Hugging Face"), dict(base, repo=""), dict(base, repo="a\nb"),
            dict(base, revision="x" * 201), dict(base, files=[]), dict(base, files="x"),
            dict(base, files=base["files"] + [dict(base["files"][0])]),            # the same path twice
            with_file(path="../escape.gguf"), with_file(path="/etc/passwd"), with_file(path="a\\b"),
            with_file(sha256="abc"), with_file(size=-1), with_file(size="9"), with_file(size=True),
            with_file(hash_from="trust me"),
        ]
        for rec in bad_records:
            (m,) = accepted(witnessed(upstream_record=rec))
            self.assertNotIn("upstream_record", m, rec)
            self.assertNotIn("witnesses", m, rec)
            self.assertEqual(m["cid"], CID)
        # Witnesses with no record to speak about are dropped too.
        m = witnessed()
        del m["upstream_record"]
        (m,) = accepted(m)
        self.assertNotIn("witnesses", m)
        # An entry with neither is not touched at all.
        plain = {"cid": CID, "name": "plain"}
        self.assertIs(accepted(plain)[0], plain)

    def test_one_announcement_cannot_buy_unlimited_signature_checks(self):
        calls = []
        real = ms.ed25519_verify
        ms.ed25519_verify = lambda *a: calls.append(1) or real(*a)
        try:
            good = witnessed()["witnesses"][0]
            # 40 entries, each with 16 never-seen forged statements and, last, the real one.
            def flood(salt):
                return [witnessed(name="m%d" % i, witnesses=[dict(good, observed_ms=salt + i * 100 + j) for j in range(1, 16)] + [good])
                        for i in range(40)]
            models = accepted(*flood(10 ** 6))
            self.assertLessEqual(len(calls), ms.VERIFY_BUDGET)
            self.assertEqual(len(models), 40)                       # every model is still listed
            self.assertTrue(all("upstream_record" in m for m in models))
            # Whatever could not be checked was dropped, not passed on unchecked.
            self.assertTrue(all(w == good for m in models for w in m.get("witnesses", [])))
            # The same announcement again costs nothing for what was already checked,
            # so an honest catalog is verified in full after a few rounds.
            for _ in range(12):
                models = accepted(*flood(10 ** 6))
            self.assertTrue(all(m.get("witnesses") == [good] for m in models))
            # At most 16 statements ride on one entry however many are offered.
            many = [dict(good)] * 40
            self.assertLessEqual(len(accepted(witnessed(witnesses=many))[0]["witnesses"]), ms.MAX_WITNESSES)
        finally:
            ms.ed25519_verify = real

    def test_a_cleaned_entry_reaches_the_registry_file(self):
        reg = ms.Registry(os.path.join(_tmp.name, "witness-reg.json"))
        good = witnessed()["witnesses"][0]
        reg.merge(ms.sanitize_announcement(announcement(ALICE, models=[witnessed(witnesses=[dict(good, observed_ms=1), good])])), at_ms=5)
        self.assertTrue(reg.flush(force=True))
        with open(reg.path) as f:
            stored = json.load(f)["peers"][ALICE]["models"][0]
        self.assertEqual(stored["witnesses"], [good])
        self.assertEqual([f["path"] for f in stored["upstream_record"]["files"]],
                         ["example-1b-q4.gguf", "sub/tokenizer.json", "sub/weights.safetensors"])


class EntryTypes(unittest.TestCase):
    """One entry with a field of the wrong type used to empty every Orb's
    index: the supervisor read the whole registry in one typed parse."""

    def kept(self, *models):
        return [m["name"] for m in accepted(*models)]

    def test_a_well_typed_entry_is_passed_on_as_it_came(self):
        for good in (entry(), entry(delegation=None), entry(delegation=dict(DELEGATION)),
                     entry(delegation={"user_key": "k", "orb_key": "o", "sig": "s"}),
                     entry(size_bytes=0), entry(size_bytes=2 ** 64 - 1), entry(added_at_ms=-(2 ** 63)),
                     entry(card={}), entry(card=card(tags=[], gallery=["g.png"])),
                     entry(btih="ab" * 20, upstream="hailo-model-zoo"), {"cid": CID, "name": "bare"}):
            (m,) = accepted(good)
            self.assertIs(m, good)

    def test_a_known_field_of_the_wrong_type_drops_only_that_entry(self):
        bad = [entry("bad", **{k: v}) for k in ms._ENTRY_TEXT for v in (5, None, ["x"], {"a": 1}, True)]
        bad += [entry("bad", size_bytes=v) for v in ("x", "20", -1, 1.5, 20.0, True, None, 2 ** 64, [20])]
        bad += [entry("bad", added_at_ms=v) for v in ("yesterday", 1.5, True, None, 2 ** 63, -(2 ** 63) - 1)]
        bad += [entry("bad", verified=v) for v in ("yes", "true", 1, 0, None, [])]
        bad += [entry("bad", card=v) for v in ("a card", None, [], 7, True)]
        bad += [entry("bad", card=card(**{k: v})) for k in ms._CARD_TEXT for v in (3, None, ["x"], False)]
        bad += [entry("bad", card=card(**{k: v})) for k in ms._CARD_LISTS for v in ("llm", None, {"a": 1}, ["a", 7], [None], [["a"]])]
        bad += [entry("bad", card=card(**{k: v})) for k in ms._CARD_FLAGS for v in ("no", 1, 0, None)]
        bad += [entry("bad", delegation=v) for v in (
            "mine", [], 7, True, {}, {"user_key": "k", "orb_key": "o"},
            dict(DELEGATION, user_key=1), dict(DELEGATION, orb_key=None), dict(DELEGATION, sig=["s"]),
            dict(DELEGATION, user_name=5), dict(DELEGATION, issued_ms="5"), dict(DELEGATION, issued_ms=1.5),
            dict(DELEGATION, issued_ms=2 ** 63))]
        self.assertGreater(len(bad), 100)
        for m in bad:
            self.assertEqual(self.kept(entry("first"), m, entry("last")), ["first", "last"], m)
        # All of them in one announcement, between good entries: same answer.
        self.assertEqual(self.kept(entry("first"), *bad, entry("last")), ["first", "last"])

    def test_a_field_nobody_here_knows_passes_through_untouched(self):
        extra = {"future_field": {"anything": [1, 2.5, None, True, "two", {"deeper": []}]},
                 "x_note": "caf\u00e9 \U0001f600", "size": "not size_bytes, so not ours to judge",
                 "tags": 7, "kind": ["also only known inside card"]}
        m = entry(**extra)
        before = json.loads(json.dumps(m))
        (out,) = accepted(m)
        self.assertIs(out, m)
        self.assertEqual(out, before)
        # Unknown members of the card too.
        (out,) = accepted(entry(card=card(future={"a": 1}, context_length=8192)))
        self.assertEqual((out["card"]["future"], out["card"]["context_length"]), ({"a": 1}, 8192))
        # And beside an upstream record, which IS rebuilt: the rest is not.
        (out,) = accepted(witnessed(**extra))
        self.assertEqual({k: out[k] for k in extra}, extra)
        self.assertIn("upstream_record", out)

    def test_the_size_and_count_limits_still_hold(self):
        big = entry("big", card=card(description="x" * ms.MAX_ENTRY_BYTES))
        self.assertEqual(self.kept(entry("first"), big, entry("last")), ["first", "last"])
        many = [entry("m%d" % i) for i in range(ms.MAX_MODELS + 50)]
        self.assertEqual(len(accepted(*many)), ms.MAX_MODELS)

    def test_an_entry_that_cannot_be_processed_is_dropped_alone(self):
        real = ms.clean_provenance

        def explode(m, budget):
            if m["name"] == "boom":
                raise RuntimeError("something nobody thought of")
            return real(m, budget)
        ms.clean_provenance = explode
        try:
            self.assertEqual(self.kept(entry("first"), entry("boom"), entry("last")), ["first", "last"])
        finally:
            ms.clean_provenance = real


class Storable(unittest.TestCase):
    """Python reads and writes values a strict JSON parser refuses. One of them
    anywhere in the registry makes the whole FILE unreadable to the
    supervisor, whatever care it takes entry by entry."""

    GOOD = json.dumps({"cid": CID, "name": "first"}), json.dumps({"cid": CID, "name": "last"})

    def setUp(self):
        ms._sig_cache.clear()

    def stored(self, *announcements):
        """Accept each (sender, text), merge it, write the registry, and hand
        back the file's text and what it holds."""
        reg = ms.Registry(os.path.join(_tmp.name, "storable-%s.json" % self.id().rsplit(".", 1)[-1]))
        reg.peers.clear()
        for sender, text in announcements:
            ann, _ = ms.accept_message(sender, text)
            if ann is not None:
                reg.merge(ann, at_ms=5)
        self.assertTrue(reg.flush(force=True))
        with open(reg.path) as f:
            text = f.read()
        return text, json.loads(text)["peers"]

    @staticmethod
    def evil(inner):
        return '{"cid": "%s", "name": "evil", %s}' % (CID, inner)

    def test_the_strict_reader_used_here_refuses_what_it_should(self):
        for bad in ('{"x": NaN}', '{"x": -Infinity}', '{"x": 1e400}', '{"x": %s}' % ("9" * 400),
                    '{"x": "\\ud800"}', '{"\\udc00": 1}', '{"x": %s%s}' % ("[" * 200, "]" * 200)):
            self.assertFalse(strictly_readable(bad), bad[:40])
        self.assertTrue(strictly_readable('{"x": [1, -2.5, 1e300, 18446744073709551615, "\\ud83d\\ude00", null, true]}'))

    def test_a_value_no_strict_parser_reads_drops_only_that_entry(self):
        nest = lambda k: "[" * k + "]" * k
        hostile = {
            "NaN": '"x": NaN', "-Infinity": '"x": -Infinity', "a float out of range": '"x": 1e400',
            "an integer of 400 digits": '"x": %s' % ("9" * 400),
            "an integer just past 64 bits": '"x": %d' % (2 ** 64),
            "an unpaired surrogate": '"x": "\\ud800"', "one as a key": '"\\udc00": 1',
            "one inside the card": '"card": {"description": "a\\ud800b"}',
            "one in a known text field": '"source": "\\udfff"',
            "one deep inside": '"x": {"a": [{"b": "\\ud800"}]}',
            "NaN deep inside": '"x": {"a": [1, {"b": [NaN]}]}',
            "nesting past the limit": '"x": %s' % nest(ms.MAX_NESTING),
            "nesting far past it": '"x": %s' % nest(200),
        }
        first, last = self.GOOD
        for what, inner in hostile.items():
            text, peers = self.stored((ALICE, raw_announcement(ALICE, "%s, %s, %s" % (first, self.evil(inner), last))),
                                      (BOB, raw_announcement(BOB, first)))
            self.assertEqual([m["name"] for m in peers[ALICE]["models"]], ["first", "last"], what)
            self.assertEqual([m["name"] for m in peers[BOB]["models"]], ["first"], what)
            self.assertTrue(strictly_readable(text), what)
        # The same in the entry's own name: there is no good half of it to keep.
        text, peers = self.stored((ALICE, raw_announcement(ALICE, '%s, {"cid": "%s", "name": "evil\\ud800"}, %s' % (first, CID, last))))
        self.assertEqual([m["name"] for m in peers[ALICE]["models"]], ["first", "last"])
        self.assertTrue(strictly_readable(text))
        # What IS storable is kept: a pair of surrogates is one character, and
        # nesting up to the limit is nobody's business.
        fine = self.evil('"x": "\\ud83d\\ude00", "y": %s, "z": [1e300, -1.5, 18446744073709551615, -9223372036854775808]' % nest(ms.MAX_NESTING - 1))
        text, peers = self.stored((ALICE, raw_announcement(ALICE, fine)))
        (m,) = peers[ALICE]["models"]
        self.assertEqual((m["x"], m["z"]), ("\U0001f600", [1e300, -1.5, 2 ** 64 - 1, -(2 ** 63)]))
        self.assertTrue(strictly_readable(text))

    def test_what_a_peer_says_about_itself_is_storable_too(self):
        first, _ = self.GOOD
        binding = '"orb_key": "%s", "orb_sig": "c2ln"' % ("ab" * 32)
        cases = {
            "label": '"label": "orb\\ud800!", "addrs": [], "updated_ms": 1',
            "addrs": '"label": "orb", "addrs": ["203.0.113.7", "\\ud800", "198.51.100.9"], "updated_ms": 1',
            "updated_ms": '"label": "orb", "addrs": [], "updated_ms": %s' % ("9" * 400),
            "updated_ms nan": '"label": "orb", "addrs": [], "updated_ms": NaN',
            "orb_key": '"label": "orb", "addrs": [], "updated_ms": 1, "orb_key": "%s", "orb_sig": "c2ln"' % ("\\ud800" * 64),
            "orb_sig": '"label": "orb", "addrs": [], "updated_ms": 1, "orb_key": "%s", "orb_sig": "\\ud800"' % ("ab" * 32),
            "delegation time": '"label": "orb", "addrs": [], "updated_ms": 1, %s, "delegation": {"user_key": "k", "orb_key": "o", "sig": "s", "issued_ms": %s}' % (binding, "9" * 400),
            "delegation key": '"label": "orb", "addrs": [], "updated_ms": 1, %s, "delegation": {"user_key": "\\ud800", "orb_key": "o", "sig": "s", "user_name": "a\\ud800"}' % binding,
        }
        for what, head in cases.items():
            text, peers = self.stored((ALICE, raw_announcement(ALICE, first, head)), (BOB, raw_announcement(BOB, first)))
            self.assertTrue(strictly_readable(text), what)
            # The peer is still listed with its model: only the bad part went.
            self.assertEqual([m["name"] for m in peers[ALICE]["models"]], ["first"], what)
            self.assertEqual([m["name"] for m in peers[BOB]["models"]], ["first"], what)
        _, peers = self.stored((ALICE, raw_announcement(ALICE, first, cases["label"])))
        self.assertEqual(peers[ALICE]["label"], "orb!")
        _, peers = self.stored((ALICE, raw_announcement(ALICE, first, cases["addrs"])))
        self.assertEqual(peers[ALICE]["addrs"], ["203.0.113.7", "198.51.100.9"])
        _, peers = self.stored((ALICE, raw_announcement(ALICE, first, cases["updated_ms"])))
        self.assertEqual(peers[ALICE]["sent_ms"], 0)
        for what in ("orb_key", "orb_sig"):
            _, peers = self.stored((ALICE, raw_announcement(ALICE, first, cases[what])))
            self.assertNotIn("orb_key", peers[ALICE], what)
        _, peers = self.stored((ALICE, raw_announcement(ALICE, first, cases["delegation time"])))
        self.assertEqual(peers[ALICE]["delegation"]["issued_ms"], 0)
        _, peers = self.stored((ALICE, raw_announcement(ALICE, first, cases["delegation key"])))
        self.assertNotIn("delegation", peers[ALICE])

    def test_a_signed_statement_with_an_unstorable_time_is_dropped_and_the_rest_kept(self):
        # No test can sign, so every signature is taken to hold here: what is
        # under test is what happens to a statement whose signature DOES hold.
        real = ms.ed25519_verify
        ms.ed25519_verify = lambda *a: True
        try:
            good = witnessed()["witnesses"][0]
            deleg = {"user_key": "cd" * 32, "orb_key": good["key"], "sig": "c2ln", "issued_ms": 5}
            (m,) = accepted(witnessed(witnesses=[dict(good, observed_ms=2 ** 63), dict(good, observed_ms=10 ** 400), good]))
            self.assertEqual(m["witnesses"], [good])
            for bad in (dict(deleg, issued_ms=2 ** 63), dict(deleg, issued_ms=10 ** 400),
                        dict(deleg, user_key="\ud800" * 64), dict(deleg, user_key=5)):
                (m,) = accepted(witnessed(witnesses=[dict(good, delegation=bad)]))
                self.assertEqual(m["witnesses"], [good], bad)
            (m,) = accepted(witnessed(witnesses=[dict(good, delegation=deleg)]))
            self.assertEqual(m["witnesses"][0]["delegation"]["issued_ms"], 5)
            # A record that cannot be encoded is not a record: it goes, with
            # its witnesses, and the entry stays.
            base = witnessed()["upstream_record"]
            unencodable_path = json.loads(json.dumps(base))
            unencodable_path["files"][0]["path"] = "a\ud800.gguf"
            for rec in (dict(base, repo="org/\ud800"), dict(base, revision="\udfff"), unencodable_path):
                (m,) = accepted(witnessed(upstream_record=rec))
                self.assertNotIn("upstream_record", m)
                self.assertNotIn("witnesses", m)
                self.assertEqual(m["cid"], CID)
        finally:
            ms.ed25519_verify = real

    def test_a_payload_nested_too_deep_to_parse_is_not_an_announcement(self):
        for payload in ("[" * 100000 + "]" * 100000, '{"peer_id": "%s", "models": %s}' % (ALICE, "[" * 100000 + "]" * 100000)):
            ann, why = ms.accept_message(ALICE, payload)
            self.assertIsNone(ann)
            self.assertTrue(why)

    def test_a_record_that_makes_the_file_unreadable_is_not_carried_over(self):
        # A registry written by a build from before this check, holding one
        # peer whose entry no strict parser reads, one whose record is not a
        # record, and two good peers.
        path = os.path.join(_tmp.name, "carried-over.json")
        good = '{"peer_id": "%%s", "label": "orb", "addrs": [], "updated_ms": 5, "sent_ms": 1, "models": [{"cid": "%s", "name": "kept", "size_bytes": "x"}], "starred": []}' % CID
        poisoned = good.replace('"size_bytes": "x"', '"x": %s')
        with open(path, "w") as f:
            f.write('{"peers": {"%s": %s, "%s": %s, "nan": %s, "big": %s, "sur": %s, "deep": %s, "junk": 7, "time": %s}}' % (
                ALICE, good % ALICE, BOB, good % BOB,
                poisoned % ("nan", "NaN"), poisoned % ("big", "9" * 400), poisoned % ("sur", '"\\ud800"'),
                poisoned % ("deep", "[" * 200 + "]" * 200), (good % "time").replace('"updated_ms": 5', '"updated_ms": "now"')))
        with open(path) as f:
            self.assertFalse(strictly_readable(f.read()))
        reg = ms.Registry(path)
        self.assertEqual(sorted(reg.peers), sorted([ALICE, BOB]))
        # An entry with a mistyped field is left for the supervisor, which
        # skips it alone; it does not make the file unreadable.
        self.assertEqual(reg.peers[ALICE]["models"][0]["size_bytes"], "x")
        self.assertTrue(reg.flush(force=True))
        with open(path) as f:
            self.assertTrue(strictly_readable(f.read()))
        # A file that is not a registry at all is an empty one.
        for junk in ("[]", "7", '{"peers": []}', "not json"):
            with open(path, "w") as f:
                f.write(junk)
            self.assertEqual(ms.Registry(path).peers, {})


class Lines(unittest.TestCase):
    def test_a_line_split_across_reads_is_reassembled(self):
        buf = ms.LineBuffer()
        line = envelope(ALICE, announcement(ALICE))
        self.assertEqual(buf.feed(line[:40]), [])          # caught the writer mid-line
        self.assertEqual(buf.feed(line[40:90]), [])
        self.assertEqual(buf.feed(line[90:] + "\n" + "next"), [line])
        self.assertEqual(buf.feed("-line\n\n"), ["next-line"])

    def test_a_line_that_never_ends_cannot_grow_forever(self):
        buf = ms.LineBuffer(limit=100)
        self.assertEqual(buf.feed("x" * 500), [])
        self.assertEqual(buf.feed("ok\n"), ["ok"])


class RegistryBehaviour(unittest.TestCase):
    def setUp(self):
        self.path = os.path.join(_tmp.name, "reg-%s.json" % self.id())
        self.reg = ms.Registry(self.path)

    def clean(self, pid, **over):
        return ms.sanitize_announcement(announcement(pid, **over))

    def test_freshness_is_local_receive_time_not_the_senders_claim(self):
        # A peer claiming a time far in the future must still expire…
        self.reg.merge(self.clean(ALICE, updated_ms=10 ** 15), at_ms=1_000)
        self.assertEqual(self.reg.peers[ALICE]["updated_ms"], 1_000)
        self.reg.prune(1_000 + ms.PEER_TTL_MS + 1)
        self.assertNotIn(ALICE, self.reg.peers)
        # …and one whose clock reads 1970 must NOT be dropped on arrival.
        self.reg.merge(self.clean(BOB, updated_ms=5), at_ms=2_000_000)
        self.reg.prune(2_000_000 + 1000)
        self.assertIn(BOB, self.reg.peers)

    def test_an_unchanged_catalog_does_not_rewrite_the_file_every_message(self):
        t = 1_000_000.0
        self.reg.merge(self.clean(ALICE), at_ms=1)
        self.assertTrue(self.reg.flush(now=t))                 # new content: written
        writes = 0
        for i in range(1, 26):                                 # 25 identical re-announcements over 50 s
            self.reg.merge(self.clean(ALICE), at_ms=1 + i)
            writes += self.reg.flush(now=t + i * 2)
        self.assertEqual(writes, 0)                            # nothing changed → nothing written
        # Last-seen times still reach disk on the slow heartbeat…
        self.assertTrue(self.reg.flush(now=t + ms.FLUSH_IDLE_S + 1))
        # …and a real change is written promptly, not a minute later.
        t2 = t + ms.FLUSH_IDLE_S + 1
        self.reg.merge(self.clean(ALICE, models=[{"cid": CID, "name": "renamed"}]), at_ms=99)
        self.assertFalse(self.reg.flush(now=t2 + 1))           # coalesce bursts for a few seconds
        self.assertTrue(self.reg.flush(now=t2 + ms.FLUSH_CHANGED_S))
        with open(self.path) as f:
            self.assertEqual(json.load(f)["peers"][ALICE]["models"][0]["name"], "renamed")

    def test_the_registry_is_capped(self):
        old = ms.MAX_PEERS
        ms.MAX_PEERS = 3
        try:
            for i in range(6):
                pid = "12D3KooW" + ("%02d" % i) * 22
                self.reg.merge(self.clean(pid), at_ms=100 + i)
            self.assertEqual(len(self.reg.peers), 3)
            self.assertIn("12D3KooW" + "05" * 22, self.reg.peers)     # newest kept
            self.assertNotIn("12D3KooW" + "00" * 22, self.reg.peers)  # oldest dropped
        finally:
            ms.MAX_PEERS = old

    def test_it_reloads_what_it_wrote(self):
        self.reg.merge(self.clean(ALICE, orb_key="ab" * 32, orb_sig="c2ln"), at_ms=5)
        self.assertTrue(self.reg.flush(force=True))
        again = ms.Registry(self.path)
        self.assertEqual(again.peers[ALICE]["orb_key"], "ab" * 32)
        self.assertEqual(again.peers[ALICE]["models"][0]["cid"], CID)


class Dialing(unittest.TestCase):
    def setUp(self):
        self.calls = []
        self.swarm = {}                      # pid → [addr lines] the fake node is connected to
        self.reachable = set()
        ms.SELF_PID = "12D3KooWSelfSelfSelfSelfSelfSelfSelfSelfSelfSelfSelf"

        class R:
            def __init__(self, out=""):
                self.stdout, self.stderr, self.returncode = out, "", 0

        def fake_sh(*args, stdin=None, timeout=None):
            self.calls.append(args)
            if args[0] == "peers":
                return R("\n".join(l for ls in self.swarm.values() for l in ls))
            if args[0] == "connect":
                pid = args[1].rsplit("/p2p/", 1)[-1]
                if pid in self.reachable:
                    self.swarm[pid] = ["/ip4/203.0.113.9/tcp/4001/p2p/" + pid]
            return R()

        self._sh, ms.sh = ms.sh, fake_sh
        self.d = ms.Dialer()

    def tearDown(self):
        ms.sh = self._sh
        ms.SELF_PID = ""

    def run_queue(self):
        while not self.d._q.empty():
            pid, addrs = self.d._q.get()
            ok = self.d._dial(pid, addrs)
            with self.d._lock:
                self.d._pending.discard(pid)
                if ok:
                    self.d._fail.pop(pid, None)
                else:
                    n = self.d._fail.get(pid, (0, 0.0))[0] + 1
                    self.d._fail[pid] = (n, ms.time.time() + min(1800, 60 * 2 ** (n - 1)))

    def count(self, verb):
        return sum(1 for c in self.calls if c[0] == verb)

    def test_a_reachable_peer_is_dialed_once_then_left_alone(self):
        self.reachable.add(ALICE)
        self.assertTrue(self.d.want(ALICE))
        self.assertFalse(self.d.want(ALICE))          # already queued
        self.run_queue()
        self.assertEqual(self.count("connect"), 1)
        self.assertGreaterEqual(self.count("peer"), 1)  # pinned as Peering
        self.calls.clear()
        for _ in range(100):                           # 100 more announcements from Alice
            self.assertFalse(self.d.want(ALICE))
        self.run_queue()
        self.assertEqual(self.count("connect"), 0)

    def test_an_unreachable_peer_backs_off_instead_of_being_redialed_every_pass(self):
        self.assertTrue(self.d.want(BOB))
        self.run_queue()
        self.assertEqual(self.count("connect"), 1)
        self.assertFalse(self.d.want(BOB))             # inside the back-off window
        with self.d._lock:                             # window elapses → retried, and it doubles
            n, _ = self.d._fail[BOB]
            self.d._fail[BOB] = (n, 0.0)
        self.assertTrue(self.d.want(BOB))
        self.run_queue()
        self.assertEqual(self.d._fail[BOB][0], 2)
        self.assertGreater(self.d._fail[BOB][1] - ms.time.time(), 100)

    def test_we_never_dial_ourselves_or_nonsense(self):
        for pid in (ms.SELF_PID, ms.RENDEZVOUS_CID, "", "short", "has space " * 6, None):
            self.assertFalse(self.d.want(pid), pid)

    def test_one_announcement_cannot_trigger_a_dial_storm(self):
        ms.REG = ms.Registry(os.path.join(_tmp.name, "storm.json"))
        ms.DIALER = self.d
        strangers = ["12D3KooW" + ("%02d" % i) * 22 for i in range(120)]
        line = envelope(ALICE, announcement(ALICE, seen=strangers))
        self.assertTrue(ms.handle_line(line, at_ms=1))
        # Alice herself + at most MAX_SEEN_DIAL of the 120 peers she named.
        self.assertLessEqual(self.d._q.qsize(), 1 + ms.MAX_SEEN_DIAL)

    def test_a_spoofed_announcement_changes_nothing(self):
        ms.REG = ms.Registry(os.path.join(_tmp.name, "spoof.json"))
        ms.DIALER = self.d
        self.assertTrue(ms.handle_line(envelope(ALICE, announcement(ALICE)), at_ms=1))
        forged = announcement(ALICE, models=[{"cid": CID, "name": "malware"}])
        self.assertFalse(ms.handle_line(envelope(BOB, forged), at_ms=2))
        self.assertEqual(ms.REG.peers[ALICE]["models"][0]["name"], "m")
        self.assertNotIn(BOB, ms.REG.peers)


class Announcing(unittest.TestCase):
    def test_an_oversized_catalog_is_trimmed_to_fit_pubsub(self):
        big = [{"cid": CID, "name": "m%d" % i, "card": {"description": "d" * 4000}} for i in range(400)]
        ann = announcement(ALICE, models=big, seen=[BOB] * 100)
        self.assertGreater(len(json.dumps(ann)), ms.MAX_ANNOUNCE_BYTES)
        fitted = ms.fit_announcement(ann)
        self.assertLessEqual(len(json.dumps(fitted)), ms.MAX_ANNOUNCE_BYTES)
        self.assertGreater(len(fitted["models"]), 100)       # most of it survives
        self.assertEqual(fitted["models"][0]["name"], "m0")  # newest-first order kept

    def test_a_normal_catalog_is_sent_whole(self):
        ann = announcement(ALICE, seen=[BOB])
        self.assertIs(ms.fit_announcement(ann), ann)

    def test_the_binding_is_included_only_when_it_is_for_this_peer(self):
        os.makedirs(os.environ["AEON_MODELS_DIR"], exist_ok=True)
        ms.REG = ms.Registry(os.path.join(_tmp.name, "ann.json"))
        with open(ms.BINDING, "w") as f:
            json.dump({"peer_id": ALICE, "orb_key": "ab" * 32, "orb_sig": "c2ln"}, f)
        real_label, real_addrs = ms.self_label, ms.self_addrs
        ms.self_label, ms.self_addrs = (lambda: "orb"), (lambda: [])
        try:
            self.assertEqual(ms.build_announcement(peer_id=ALICE)["orb_key"], "ab" * 32)
            self.assertNotIn("orb_key", ms.build_announcement(peer_id=BOB))  # repo was re-keyed
        finally:
            ms.self_label, ms.self_addrs = real_label, real_addrs


if __name__ == "__main__":
    unittest.main()
