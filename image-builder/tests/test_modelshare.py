"""Off-device tests for the Model Share gossip daemon's logic.

    python3 -m unittest discover -s image-builder/tests -v

Covers what does not need an IPFS node: which messages are accepted, how the
registry ages and is written, and how dialing is rationed.
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
