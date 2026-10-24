#!/usr/bin/env python3
"""Offline tests for V3 deposit planning in pl-eth-auto-mint.py. No network."""
import importlib.util
import os
import tempfile
import unittest

_HERE = os.path.dirname(os.path.realpath(__file__))
_spec = importlib.util.spec_from_file_location("auto_mint", os.path.join(_HERE, "pl-eth-auto-mint.py"))
mint = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(mint)

E = mint.ETH_V3_DEPOSIT_EXPIRY_SECS
BRIDGE = "0x1111111111111111111111111111111111111111"
DEST = "ab" * 20
TOKEN_ETH = "00" * 20
TOKEN_USDC = mint.USDC_TOKEN
SENDER = "cd" * 20
DEP = "11" * 32
TX = "22" * 32


def word_bytes20(b20: str) -> str:
    return b20.lower()[-40:] + "00" * 12


def word_addr(addr: str) -> str:
    return "00" * 12 + addr.lower()[-40:]


def enc_data(dest: str, token: str, amount: int, ts: int) -> str:
    return "0x" + word_bytes20(dest) + word_addr(token) + f"{amount:064x}" + f"{ts:064x}"


def log(data: str, topic0: str = mint.V3_DEPOSIT_TOPIC0, dest_pad_ok: bool = True, sender: str = SENDER) -> dict:
    return {
        "address": BRIDGE,
        "topics": [
            "0x" + topic0,
            "0x" + DEP,
            "0x" + word_addr(sender),
        ],
        "data": data,
        "transactionHash": "0x" + TX,
        "blockNumber": "0x10",
        "blockHash": "0x" + "33" * 32,
        "transactionIndex": "0x1",
    }


class V3Plan(unittest.TestCase):
    def test_topic0_and_pool_funded_constant(self):
        self.assertEqual(len(mint.V3_DEPOSIT_TOPIC0), 64)
        self.assertEqual(mint.V3_DEPOSIT_TOPIC0, "e7783c72791736d4d8af95660d4367bed2f98d8331a095422373e557a4318728")
        self.assertNotEqual(mint.POOL_FUNDED_TOPIC0, mint.V3_DEPOSIT_TOPIC0)

    def test_parse_eth_left_aligned_dest(self):
        parsed = mint.parse_v3_deposit_log(log(enc_data(DEST, TOKEN_ETH, 5, 1_000)))
        self.assertIsNotNone(parsed)
        self.assertEqual(parsed["asset"], "ETH")
        self.assertEqual(parsed["dest20"], DEST)
        self.assertEqual(parsed["sender"], SENDER)
        self.assertEqual(parsed["amount"], 5)
        self.assertEqual(parsed["timestamp"], 1_000)
        self.assertEqual(parsed["deposit_id"], DEP)
        self.assertEqual(parsed["tx_index"], 1)
        self.assertEqual(parsed["block_number"], 16)

    def test_parse_usdc(self):
        parsed = mint.parse_v3_deposit_log(log(enc_data(DEST, TOKEN_USDC, 9, 50)))
        self.assertEqual(parsed["asset"], "USDC")
        self.assertEqual(parsed["token"], TOKEN_USDC)

    def test_right_aligned_dest_rejected(self):
        # address-style right alignment is not abi.encode(bytes20).
        bad = "0x" + word_addr(DEST) + word_addr(TOKEN_ETH) + f"{1:064x}" + f"{2:064x}"
        self.assertIsNone(mint.parse_v3_deposit_log(log(bad)))

    def test_nonzero_padding_rejected(self):
        data = enc_data(DEST, TOKEN_ETH, 1, 2)
        raw = data[2:]
        raw = raw[:40] + "01" + raw[42:]
        self.assertIsNone(mint.parse_v3_deposit_log(log("0x" + raw)))

    def test_amount_over_u64_rejected(self):
        data = "0x" + word_bytes20(DEST) + word_addr(TOKEN_ETH) + "ff" * 32 + f"{1:064x}"
        self.assertIsNone(mint.parse_v3_deposit_log(log(data)))

    def test_pool_funded_never_parsed(self):
        data = "0x" + f"{10:064x}"
        lg = log(data, topic0=mint.POOL_FUNDED_TOPIC0)
        lg["topics"] = [
            "0x" + mint.POOL_FUNDED_TOPIC0,
            "0x" + word_addr(SENDER),
            "0x" + word_addr(TOKEN_ETH),
        ]
        self.assertIsNone(mint.parse_v3_deposit_log(lg))
        names = {DEST: "dave"}
        self.assertEqual(mint.jobs_from_v3_logs([lg], names, 10_000, E, BRIDGE), [])

    def test_wrong_topic0_or_topic_count_rejected(self):
        good = enc_data(DEST, TOKEN_ETH, 1, 2)
        v1_topic0 = "668b7640365fc09f7ddaf1edfae6911ae7a7701e5bda817da9430723b9b8b021"
        self.assertIsNone(mint.parse_v3_deposit_log(log(good, topic0=v1_topic0)))
        self.assertIsNone(mint.parse_v3_deposit_log(log(good, topic0="00" * 32)))
        two = log(good)
        two["topics"] = two["topics"][:2]
        self.assertIsNone(mint.parse_v3_deposit_log(two))
        four = log(good)
        four["topics"] = four["topics"] + ["0x" + "00" * 32]
        self.assertIsNone(mint.parse_v3_deposit_log(four))
        self.assertIsNone(mint.parse_v3_deposit_log(log(good + "00" * 32)))
        self.assertIsNone(mint.parse_v3_deposit_log(log(good[:-64])))
        self.assertEqual(mint.jobs_from_v3_logs([log(good, topic0=v1_topic0)], {DEST: "dave"}, 10_000, E, BRIDGE), [])

    def test_expiry_boundary_matches_consensus(self):
        ts = 1_000
        self.assertFalse(mint.deposit_expired(ts, ts + E, E))
        self.assertTrue(mint.deposit_expired(ts, ts + E + 1, E))

    def test_unknown_token_rejected(self):
        self.assertIsNone(mint.parse_v3_deposit_log(log(enc_data(DEST, "ab" * 20, 1, 2))))

    def test_mint_inside_window_and_refund_after(self):
        parsed = mint.parse_v3_deposit_log(log(enc_data(DEST, TOKEN_ETH, 7, 1_000)))
        names = {DEST: "dave"}
        self.assertFalse(mint.deposit_expired(1_000, 1_000 + E, E))
        plan = mint.plan_v3_submit(parsed, names, 1_000 + E, E)
        self.assertEqual(plan, {"credit_to": "dave", "mode": "mint"})
        self.assertTrue(mint.deposit_expired(1_000, 1_000 + E + 1, E))
        plan = mint.plan_v3_submit(parsed, names, 1_000 + E + 1, E)
        self.assertEqual(plan["credit_to"], "")
        self.assertEqual(plan["mode"], "refund")

    def test_unknown_dest_inside_e_skipped_after_e_refunded(self):
        parsed = mint.parse_v3_deposit_log(log(enc_data(DEST, TOKEN_USDC, 3, 50)))
        self.assertIsNone(mint.plan_v3_submit(parsed, {}, 50, E))
        plan = mint.plan_v3_submit(parsed, {}, 50 + E + 1, E)
        self.assertEqual(plan["mode"], "refund")
        self.assertEqual(plan["credit_to"], "")

    def test_no_lc_time_does_not_force_refund(self):
        parsed = mint.parse_v3_deposit_log(log(enc_data(DEST, TOKEN_ETH, 1, 10)))
        self.assertIsNone(mint.plan_v3_submit(parsed, {}, 0, E))
        self.assertEqual(
            mint.plan_v3_submit(parsed, {DEST: "alice"}, 0, E)["mode"],
            "mint",
        )

    def test_wrong_bridge_dropped(self):
        parsed_log = log(enc_data(DEST, TOKEN_ETH, 1, 10))
        jobs = mint.jobs_from_v3_logs([parsed_log], {DEST: "dave"}, 10, E, "0x" + "22" * 20)
        self.assertEqual(jobs, [])

    def test_receipt_with_only_pool_funded_queues_nothing(self):
        funded = {
            "address": BRIDGE,
            "topics": [
                "0x" + mint.POOL_FUNDED_TOPIC0,
                "0x" + word_addr(SENDER),
                "0x" + word_addr(TOKEN_ETH),
            ],
            "data": "0x" + f"{10**18:064x}",
            "transactionHash": "0x" + TX,
        }
        self.assertEqual(mint.jobs_from_v3_logs([funded], {DEST: "dave"}, 10**12, E, BRIDGE), [])

    def test_already_words(self):
        self.assertTrue(mint.v3_already("V3 deposit already resolved"))
        self.assertTrue(mint.v3_already("duplicate tx"))
        self.assertFalse(mint.v3_already("V3 deposit dest unresolved: supply credit_to before expiry"))
        self.assertFalse(mint.v3_already("V3 deposits not active"))

    def test_slot_fallback_and_expiry_from_status(self):
        self.assertEqual(mint.lc_head_unix_fallback({}), 0)
        self.assertEqual(
            mint.lc_head_unix_fallback({"eth_lc": {"finalized_slot": 10}}),
            mint.SEPOLIA_BEACON_GENESIS_UNIX + 120,
        )
        self.assertEqual(mint.v3_expiry_secs({}), E)
        self.assertEqual(mint.v3_expiry_secs({"eth_v3_expiry_secs": 10}), 10)

    def test_bridge_from_env(self):
        old = os.environ.get("FPL_SEPOLIA_QC_V3_BRIDGE")
        os.environ["FPL_SEPOLIA_QC_V3_BRIDGE"] = BRIDGE
        try:
            self.assertEqual(mint.v3_bridge_addr(""), BRIDGE)
            self.assertEqual(mint.v3_bridge_addr(BRIDGE[2:].upper()), BRIDGE)
        finally:
            if old is None:
                os.environ.pop("FPL_SEPOLIA_QC_V3_BRIDGE", None)
            else:
                os.environ["FPL_SEPOLIA_QC_V3_BRIDGE"] = old

    def test_v3_job_key_does_not_collide_with_bare_txid(self):
        self.assertTrue(mint.v3_job_key(DEP).startswith("v3:"))
        self.assertNotEqual(mint.v3_job_key(TX), TX)


class V3Pending(unittest.TestCase):
    """Unknown dest inside E is kept and re-planned, not dropped by the cursor."""

    def test_unknown_dest_is_kept_then_refunded_after_e(self):
        lg = log(enc_data(DEST, TOKEN_ETH, 3, 1_000))
        plans, waiting = mint.split_v3_logs([lg], {}, 1_000, E, BRIDGE)
        self.assertEqual(plans, [])
        self.assertEqual(len(waiting), 1)
        st = mint.empty_state()
        self.assertEqual(mint.remember_v3_waiting(st, waiting), 1)
        self.assertEqual(mint.remember_v3_waiting(st, waiting), 0)
        self.assertIn(DEP, st["v3_pending"])
        # Still inside E with no dest: nothing queued, entry kept.
        self.assertEqual(mint.replan_v3_pending(st, {}, 1_000 + E, E), 0)
        self.assertIn(DEP, st["v3_pending"])
        # Past E: refund queued, entry dropped.
        self.assertEqual(mint.replan_v3_pending(st, {}, 1_000 + E + 1, E), 1)
        self.assertEqual(st["v3_pending"], {})
        job = st["jobs"][mint.v3_job_key(DEP)]
        self.assertEqual(job["mode"], "refund")
        self.assertEqual(job["credit_to"], "")
        self.assertEqual(job["status"], "queued")
        self.assertEqual(job["block_number"], 0x10)

    def test_unknown_dest_mints_once_the_name_is_known(self):
        lg = log(enc_data(DEST, TOKEN_ETH, 3, 1_000))
        _, waiting = mint.split_v3_logs([lg], {}, 1_000, E, BRIDGE)
        st = mint.empty_state()
        mint.remember_v3_waiting(st, waiting)
        self.assertEqual(mint.replan_v3_pending(st, {DEST: "sally"}, 1_000 + 5, E), 1)
        job = st["jobs"][mint.v3_job_key(DEP)]
        self.assertEqual((job["mode"], job["credit_to"]), ("mint", "sally"))
        self.assertEqual(st["accounts"][DEST], "sally")

    def test_pending_entry_with_a_job_is_dropped_not_requeued(self):
        lg = log(enc_data(DEST, TOKEN_ETH, 3, 1_000))
        _, waiting = mint.split_v3_logs([lg], {}, 1_000, E, BRIDGE)
        st = mint.empty_state()
        mint.remember_v3_waiting(st, waiting)
        st["jobs"][mint.v3_job_key(DEP)] = {"status": "done"}
        self.assertEqual(mint.replan_v3_pending(st, {}, 1_000 + E + 1, E), 0)
        self.assertEqual(st["v3_pending"], {})
        self.assertEqual(st["jobs"][mint.v3_job_key(DEP)], {"status": "done"})


class OneTxInFlight(unittest.TestCase):
    """Watcher txs re-read the committed sequence, so never two in flight."""

    def setUp(self):
        self.saved = {k: getattr(mint, k) for k in ("account_sequence", "time")}
        self.committed = 7
        self.events: list[tuple] = []
        self.pending = 0
        mint.account_sequence = self.fake_seq
        os.environ["FPL_V3_SEQ_WAIT_SEC"] = "0"

    def tearDown(self):
        for k, v in self.saved.items():
            setattr(mint, k, v)
        os.environ.pop("FPL_V3_SEQ_WAIT_SEC", None)

    def fake_seq(self, account):
        # The node commits whatever is pending on the next poll.
        seq = self.committed
        if self.pending:
            self.committed += self.pending
            self.pending = 0
        return seq

    def submit(self, what):
        # A second tx before the first commits would collide on one sequence.
        if self.pending:
            self.events.append(("evicted", what))
        self.events.append(("submit", what, self.committed))
        self.pending += 1
        return what

    def test_header_chunks_and_deposit_wait_for_commit(self):
        class Rel:
            ACCOUNT = "watcher-eth"
            HEADER_CHUNK = 0  # junk count must not hang; the relayer normalises it

            def __init__(rel):
                rel.chains = []

            def cmd_update(rel, soft=True):
                return {"execution_number": 15}

            def fetch_header(rel, n):
                return {"number": n}

            def submit_headers(rel, chunk):
                raise AssertionError("must go through submit_header_chain")

            def submit_header_chain(rel, headers):
                rel.chains.append([h["number"] for h in headers])
                self.submit(("headers", headers[0]["number"], headers[-1]["number"]))
                self.fake_seq("watcher-eth")  # the wave commits before return

            def wait_lc(rel, pred, tries=20, label=""):
                return {}

            def status(rel):
                return {"eth_lc": {"eth_exec_floor": 0, "eth_exec_tip": 0}}

        rel = Rel()
        mint.ensure_exec_headers(rel, 10)
        self.assertEqual(rel.chains, [[10, 11, 12, 13, 14, 15]])
        out = mint.submit_committed("watcher-browser", "v3 refund", lambda: self.submit("refund-1"))
        self.assertEqual(out, "refund-1")
        mint.submit_committed("watcher-browser", "v3 refund", lambda: self.submit("refund-2"))
        self.assertNotIn("evicted", [e[0] for e in self.events])
        seqs = [e[2] for e in self.events if e[0] == "submit"]
        self.assertEqual(seqs, sorted(set(seqs)), "each tx must see a fresh committed sequence")

    def test_stuck_tx_raises_instead_of_moving_on(self):
        mint.account_sequence = lambda account: 7
        os.environ["FPL_V3_SEQ_TRIES"] = "3"
        try:
            with self.assertRaises(RuntimeError):
                mint.submit_committed("watcher-browser", "v3 refund", lambda: "x")
        finally:
            os.environ.pop("FPL_V3_SEQ_TRIES", None)

class ExecHeadersNeeded(unittest.TestCase):
    def test_only_blocks_above_the_stored_tip(self):
        self.assertEqual(mint.exec_headers_needed(10, 15, 0, 0), (10, 15))
        self.assertEqual(mint.exec_headers_needed(10, 15, 5, 12), (13, 15))
        self.assertIsNone(mint.exec_headers_needed(10, 15, 5, 15))
        self.assertIsNone(mint.exec_headers_needed(10, 15, 5, 20))
        # Deposit below the stored floor: submit the whole walk.
        self.assertEqual(mint.exec_headers_needed(3, 15, 5, 12), (3, 15))

    def test_ctl_error_text_reaches_the_caller(self):
        import tempfile

        saved = mint.CTL
        with tempfile.TemporaryDirectory() as d:
            fake = os.path.join(d, "ctl")
            with open(fake, "w") as f:
                f.write("#!/bin/sh\necho 'Error: V3 deposit already resolved' >&2\nexit 1\n")
            os.chmod(fake, 0o700)
            try:
                mint.CTL = mint.Path(fake)
                with self.assertRaises(RuntimeError) as cm:
                    mint.ctl_as_watcher("submit-eth-v3-deposit")
            finally:
                mint.CTL = saved
        self.assertTrue(mint.v3_already(str(cm.exception)), str(cm.exception))

class ConfirmResolved(unittest.TestCase):
    """A sequence advance is not proof; the node's 'already resolved' is."""

    def setUp(self):
        self.saved = {k: getattr(mint, k) for k in ("account_sequence", "ctl_as_watcher")}
        os.environ["FPL_V3_SEQ_WAIT_SEC"] = "0"
        self.seq = 5
        self.resolved = False
        self.submits = 0

    def tearDown(self):
        for k, v in self.saved.items():
            setattr(mint, k, v)
        os.environ.pop("FPL_V3_SEQ_WAIT_SEC", None)

    def run_with(self, outcomes):
        """outcomes[i] for submit i: 'evicted' (slot taken by another tx), 'applied'."""

        def seq(account):
            return self.seq

        def ctl(*args, timeout=180):
            if self.resolved:
                raise RuntimeError("Error: V3 deposit already resolved")
            what = outcomes[self.submits]
            self.submits += 1
            self.seq += 1  # someone's tx committed at this slot either way
            if what == "applied":
                self.resolved = True
            return f"submit-eth-v3-deposit tx={self.submits}"

        mint.account_sequence = seq
        mint.ctl_as_watcher = ctl
        return mint.submit_v3_until_resolved(["submit-eth-v3-deposit"], "v3 refund", attempts=3)

    def test_evicted_refund_is_resubmitted_until_resolved(self):
        out = self.run_with(["evicted", "applied"])
        self.assertTrue(self.resolved)
        self.assertEqual(self.submits, 2)
        self.assertIn("tx=2", out)

    def test_applied_first_time_is_confirmed_by_admission(self):
        self.run_with(["applied"])
        self.assertEqual(self.submits, 1)

    def test_never_resolved_raises(self):
        with self.assertRaises(RuntimeError):
            self.run_with(["evicted", "evicted", "evicted"])
        self.assertFalse(self.resolved)


class HeldBeforeActivation(unittest.TestCase):
    """Before H2 the node refuses V3 txs; jobs must stay queued, not error."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        d = self.tmp.name
        self.saved = {k: getattr(mint, k) for k in ("STATE", "LOCK", "RUN_LOCK", "ctl_status", "_process_v3_job_locked")}
        mint.STATE = mint.Path(d) / "jobs.json"
        mint.LOCK = mint.Path(d) / "lock"
        mint.RUN_LOCK = mint.Path(d) / "runlock"
        self.calls = []
        mint._process_v3_job_locked = lambda key: self.calls.append(key) or {"status": "done"}
        st = mint.load_state()
        st["jobs"]["v3:" + DEP] = {"kind": "v3", "status": "queued", "deposit_id": DEP}
        st["jobs"]["ab" * 32] = {"kind": "v1", "status": "queued"}
        mint.save_state(st)

    def tearDown(self):
        for k, v in self.saved.items():
            setattr(mint, k, v)
        self.tmp.cleanup()

    def test_ready_rule(self):
        ok = {"tip_height": 99, "eth_v3_from_height": 100, "eth_v3_bridge": BRIDGE}
        r = mint.v3_submit_ready
        self.assertEqual(r(ok, BRIDGE), (True, ""))
        self.assertEqual(r({**ok, "eth_v3_bridge": BRIDGE.upper().replace("0X", "0x")}, BRIDGE[2:]), (True, ""))
        self.assertFalse(r({**ok, "tip_height": 98}, BRIDGE)[0])
        self.assertFalse(r({**ok, "eth_v3_from_height": 2**64 - 1}, BRIDGE)[0])
        self.assertFalse(r({**ok, "eth_v3_bridge": None}, BRIDGE)[0])
        self.assertFalse(r({}, BRIDGE)[0])
        self.assertFalse(r({"tip_height": 5}, BRIDGE)[0])
        self.assertFalse(r(ok, "")[0], "a process with no bridge never submits V3")
        other = "0x" + "22" * 20
        ready, why = r(ok, other)
        self.assertFalse(ready)
        self.assertIn("!=", why)

    def test_job_stays_queued_and_nothing_is_submitted(self):
        mint.ctl_status = lambda: {"tip_height": 5, "eth_v3_from_height": 2**64 - 1, "eth_v3_bridge": BRIDGE}
        self.assertEqual(mint.next_queued(skip_v3=True), "ab" * 32)
        job = mint.process_job("v3:" + DEP, v3_bridge=BRIDGE)
        self.assertEqual(job["status"], "queued")
        self.assertEqual(self.calls, [])
        self.assertEqual(mint.load_state()["jobs"]["v3:" + DEP]["status"], "queued")
        mint.ctl_status = lambda: {"tip_height": 5, "eth_v3_from_height": 6, "eth_v3_bridge": BRIDGE}
        self.assertEqual(mint.process_job("v3:" + DEP, v3_bridge="0x" + "22" * 20)["status"], "queued")
        self.assertEqual(self.calls, [])
        mint.process_job("v3:" + DEP, v3_bridge=BRIDGE)
        self.assertEqual(self.calls, ["v3:" + DEP])

    def test_first_scan_covers_the_header_window(self):
        os.environ.pop("FPL_V3_SCAN_FROM", None)
        self.assertEqual(mint.v3_first_scan_block(100_000, {}), 100_000 - 50_400)
        self.assertEqual(mint.v3_first_scan_block(100_000, {"eth_exec_window": 1000}), 99_000)
        self.assertEqual(mint.v3_first_scan_block(10, None), 0)
        # LC head lags the live tip: anchor on the LC head, so blocks that
        # are still provable (head - window ..) are not skipped.
        lagging = {"eth_lc": {"execution_number": 90_000}}
        self.assertEqual(mint.v3_first_scan_block(100_000, lagging), 90_000 - 50_400)
        # Never anchor above the live tip.
        ahead = {"eth_lc": {"execution_number": 200_000}}
        self.assertEqual(mint.v3_first_scan_block(100_000, ahead), 100_000 - 50_400)
        os.environ["FPL_V3_SCAN_FROM"] = "12345"
        try:
            self.assertEqual(mint.v3_first_scan_block(100_000, {}), 12345)
        finally:
            os.environ.pop("FPL_V3_SCAN_FROM", None)



class RelayerAccount(unittest.TestCase):
    def test_loaded_relayer_signs_as_the_mint_account(self):
        saved = os.environ.pop("FPL_WATCHER_ACCOUNT", None)
        try:
            rel = mint._relayer()
            self.assertEqual(rel.ACCOUNT, mint.WATCHER)
            self.assertNotEqual(rel.ACCOUNT.casefold(), "watcher-eth-lc")
            # Real relayer chunker terminates on a junk count.
            saved_chunk = rel.HEADER_CHUNK
            rel.HEADER_CHUNK = 0
            try:
                hs = [{"number": n, "x": "0" * 10} for n in range(5)]
                self.assertEqual(sum(len(c) for c in rel.header_chunks(hs)), 5)
            finally:
                rel.HEADER_CHUNK = saved_chunk
        finally:
            if saved is not None:
                os.environ["FPL_WATCHER_ACCOUNT"] = saved


class V3ScratchFiles(unittest.TestCase):
    def test_receipt_and_mpt_files_use_the_per_process_dir(self):
        import inspect
        src = inspect.getsource(mint.submit_v3_deposit)
        self.assertNotIn("/tmp/sepolia-v3", src)
        self.assertIn("rel._tmp_path(", src)
        rel = mint._relayer()
        p = rel._tmp_path("sepolia-v3-receipt.hex")
        self.assertNotEqual(str(p.parent), "/tmp")
        self.assertEqual(os.stat(p.parent).st_mode & 0o777, 0o700)


class RelayerModuleCached(unittest.TestCase):
    def test_one_module_and_one_scratch_dir_per_process(self):
        a = mint._relayer()
        b = mint._relayer()
        self.assertIs(a, b)
        self.assertEqual(a._tmp_path("x.hex").parent, b._tmp_path("y.hex").parent)


class ResolvedAsRefund(unittest.TestCase):
    """A planned mint that consensus refunds is done, not an error."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        d = self.tmp.name
        names = ("STATE", "LOCK", "RUN_LOCK", "submit_v3_deposit", "account_assets", "wait_credit")
        self.saved = {k: getattr(mint, k) for k in names}
        mint.STATE = mint.Path(d) / "jobs.json"
        mint.LOCK = mint.Path(d) / "lock"
        mint.RUN_LOCK = mint.Path(d) / "runlock"
        mint.submit_v3_deposit = lambda job: "already resolved"
        mint.account_assets = lambda account: {"ETH": 5}
        st = mint.load_state()
        st["jobs"]["v3:" + DEP] = {
            "kind": "v3", "status": "queued", "deposit_id": DEP,
            "credit_to": "sally", "asset": "ETH", "amount": 10, "mode": "mint",
        }
        mint.save_state(st)

    def tearDown(self):
        for k, v in self.saved.items():
            setattr(mint, k, v)
        self.tmp.cleanup()

    def test_no_credit_after_confirmed_resolution_is_a_refund(self):
        def no_credit(*a, **k):
            raise RuntimeError("ETH credit did not land on sally")

        mint.wait_credit = no_credit
        job = mint._process_v3_job_locked("v3:" + DEP)
        self.assertEqual(job["status"], "done")
        self.assertTrue(job["outcome"].startswith("refund"))

    def test_credit_seen_is_a_mint(self):
        mint.wait_credit = lambda *a, **k: 15
        job = mint._process_v3_job_locked("v3:" + DEP)
        self.assertEqual((job["status"], job["outcome"], job["credited"]), ("done", "mint", 15))



class TransientRetry(unittest.TestCase):
    """A transient V3 failure stays queued with a backoff; permanent ones stop."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        d = self.tmp.name
        names = ("STATE", "LOCK", "RUN_LOCK", "submit_v3_deposit", "account_assets")
        self.saved = {k: getattr(mint, k) for k in names}
        mint.STATE = mint.Path(d) / "jobs.json"
        mint.LOCK = mint.Path(d) / "lock"
        mint.RUN_LOCK = mint.Path(d) / "runlock"
        mint.account_assets = lambda account: {"ETH": 0}
        st = mint.load_state()
        st["jobs"]["v3:" + DEP] = {
            "kind": "v3", "status": "queued", "deposit_id": DEP,
            "credit_to": "", "asset": "ETH", "amount": 10, "mode": "refund",
        }
        mint.save_state(st)

    def tearDown(self):
        for k, v in self.saved.items():
            setattr(mint, k, v)
        self.tmp.cleanup()

    def fail_with(self, msg):
        def boom(job):
            raise RuntimeError(msg)

        mint.submit_v3_deposit = boom
        with self.assertRaises(RuntimeError):
            mint._process_v3_job_locked("v3:" + DEP)
        return mint.load_state()["jobs"]["v3:" + DEP]

    def test_transient_is_requeued_with_backoff(self):
        job = self.fail_with("v3 refund: watcher sequence stuck at 7")
        self.assertEqual(job["status"], "queued")
        self.assertEqual(job["attempts"], 1)
        self.assertGreater(job["retry_at"], mint.now_ms())
        self.assertIsNone(mint.next_queued(), "not before retry_at")
        st = mint.load_state()
        st["jobs"]["v3:" + DEP]["retry_at"] = 0
        mint.save_state(st)
        self.assertEqual(mint.next_queued(), "v3:" + DEP)
        self.assertEqual(mint.v3_retry_delay_ms(1), 60_000)
        self.assertEqual(mint.v3_retry_delay_ms(30), 3_600_000)

    def test_permanent_is_an_error(self):
        job = self.fail_with("tx rejected: receipt has no matching V3 Deposit")
        self.assertEqual(job["status"], "error")
        self.assertIsNone(mint.next_queued())


class ScanChainGuard(unittest.TestCase):
    def test_wrong_chain_never_moves_the_cursor(self):
        saved = {k: getattr(mint, k) for k in ("eth_chain_id", "latest_eth_block", "ctl_status", "save_state", "replan_v3_pending", "resolve_lc_head_time")}
        try:
            mint.eth_chain_id = lambda: 1
            mint.ctl_status = lambda: {}
            mint.save_state = lambda st: None

            def no_latest():
                raise AssertionError("must not read the tip of another chain")

            def no_plan(*_a, **_k):
                raise AssertionError("must not replan from a wrong-chain timestamp")

            def no_time(*_a, **_k):
                raise AssertionError("must not read a block timestamp before the chain check")

            mint.latest_eth_block = no_latest
            mint.replan_v3_pending = no_plan
            mint.resolve_lc_head_time = no_time
            st = {"jobs": {}, "v3_pending": {"x": {}}, "last_block_v3": 123}
            mint.scan_v3_logs(st, BRIDGE)
            self.assertEqual(st["last_block_v3"], 123)
            self.assertIn("x", st["v3_pending"])
        finally:
            for k, v in saved.items():
                setattr(mint, k, v)

    def test_submit_refuses_wrong_chain_before_headers(self):
        saved = {k: getattr(mint, k) for k in ("eth_chain_id", "_relayer")}
        try:
            mint.eth_chain_id = lambda: 1

            def no_relayer():
                raise AssertionError("must not fetch headers or receipts")

            mint._relayer = no_relayer
            with self.assertRaises(RuntimeError) as ctx:
                mint.submit_v3_deposit({"block_number": 1, "deposit_id": DEP, "asset": "ETH"})
            self.assertIn("chain 1", str(ctx.exception))
            self.assertFalse(mint.v3_permanent(str(ctx.exception)))
        finally:
            for k, v in saved.items():
                setattr(mint, k, v)

    def test_once_refuses_wrong_chain_before_receipt(self):
        saved_chain = mint.eth_chain_id
        saved_out = mint.subprocess.check_output
        try:
            mint.eth_chain_id = lambda: 1

            def no_receipt(*_a, **_k):
                raise AssertionError("must not read a receipt")

            mint.subprocess.check_output = no_receipt
            with self.assertRaises(SystemExit) as ctx:
                mint.once_v3_txid(TX, BRIDGE, queue_only=True)
            self.assertIn("chain 1", str(ctx.exception))
        finally:
            mint.eth_chain_id = saved_chain
            mint.subprocess.check_output = saved_out


class V3ReorgDepth(unittest.TestCase):
    def test_safe_tip_and_scan_never_passes_it(self):
        self.assertGreaterEqual(mint.V3_CONFIRMATIONS, 64)
        lc = lambda n: {"eth_lc": {"execution_number": n}}
        self.assertEqual(mint.v3_safe_tip(1000, lc(5000)), 1000 - mint.V3_CONFIRMATIONS)
        # Finality stall: LC head lags far behind; never pass it.
        self.assertEqual(mint.v3_safe_tip(10_000, lc(9_000)), 9_000)
        # No LC head: do not advance.
        self.assertEqual(mint.v3_safe_tip(10_000, {}), 0)
        self.assertEqual(mint.v3_safe_tip(5, lc(5)), 0)
        names = ("eth_chain_id", "latest_eth_block", "ctl_status", "save_state", "resolve_lc_head_time", "replan_v3_pending")
        saved = {k: getattr(mint, k) for k in names}
        saved_out = mint.subprocess.check_output
        calls = []
        try:
            mint.eth_chain_id = lambda: mint.SEPOLIA_CHAIN_ID
            mint.latest_eth_block = lambda: 10_000
            mint.ctl_status = lambda: {"eth_lc": {"execution_number": 9_950}}
            mint.save_state = lambda st: None
            mint.resolve_lc_head_time = lambda status: 0
            mint.replan_v3_pending = lambda *a, **k: False

            def fake_out(args, **_k):
                calls.append(int(args[args.index("--to-block") + 1]))
                return "[]"

            mint.subprocess.check_output = fake_out
            st = {"jobs": {}, "last_block_v3": 9_800}
            mint.scan_v3_logs(st, BRIDGE)
            safe = min(10_000 - mint.V3_CONFIRMATIONS, 9_950)
            self.assertTrue(calls)
            self.assertLessEqual(max(calls), safe)
            self.assertEqual(st["last_block_v3"], safe)
            # LC head stalled below the depth margin: cap at the LC head.
            calls.clear()
            mint.ctl_status = lambda: {"eth_lc": {"execution_number": 9_850}}
            st = {"jobs": {}, "last_block_v3": 9_800}
            mint.scan_v3_logs(st, BRIDGE)
            self.assertEqual(max(calls), 9_850)
            self.assertEqual(st["last_block_v3"], 9_850)
            # No LC head: no logs read, cursor unchanged.
            calls.clear()
            mint.ctl_status = lambda: {}
            st = {"jobs": {}, "last_block_v3": 9_800}
            mint.scan_v3_logs(st, BRIDGE)
            self.assertEqual(calls, [])
            self.assertEqual(st["last_block_v3"], 9_800)
        finally:
            mint.subprocess.check_output = saved_out
            for k, v in saved.items():
                setattr(mint, k, v)

    def test_once_refuses_an_unfinal_receipt(self):
        saved = {k: getattr(mint, k) for k in ("eth_chain_id", "latest_eth_block", "queue_v3_jobs", "ctl_status")}
        saved_out = mint.subprocess.check_output
        try:
            mint.eth_chain_id = lambda: mint.SEPOLIA_CHAIN_ID
            mint.latest_eth_block = lambda: 10_000
            # 0x270f = 9999: above both the depth margin and the LC head.
            mint.ctl_status = lambda: {"eth_lc": {"execution_number": 9_990}}
            mint.subprocess.check_output = lambda *a, **k: '{"status":"0x1","blockNumber":"0x270f","logs":[]}'

            def no_queue(*_a, **_k):
                raise AssertionError("must not queue an unfinal deposit")

            mint.queue_v3_jobs = no_queue
            with self.assertRaises(SystemExit) as ctx:
                mint.once_v3_txid(TX, BRIDGE, queue_only=True)
            self.assertIn("reorg-safe", str(ctx.exception))
        finally:
            mint.subprocess.check_output = saved_out
            for k, v in saved.items():
                setattr(mint, k, v)


class OnceKeepsWaiting(unittest.TestCase):
    def test_unknown_dest_inside_e_goes_to_v3_pending(self):
        names = ("eth_chain_id", "latest_eth_block", "ctl_status", "STATE", "LOCK", "resolve_lc_head_time")
        saved = {k: getattr(mint, k) for k in names}
        saved_out = mint.subprocess.check_output
        tmp = tempfile.TemporaryDirectory()
        try:
            mint.STATE = mint.Path(tmp.name) / "jobs.json"
            mint.LOCK = mint.Path(tmp.name) / "lock"
            mint.eth_chain_id = lambda: mint.SEPOLIA_CHAIN_ID
            mint.latest_eth_block = lambda: 10_000
            mint.ctl_status = lambda: {"eth_lc": {"execution_number": 9_900}}
            ts0 = 1_000_000
            mint.resolve_lc_head_time = lambda status: ts0 + 60
            lg = log(enc_data("ef" * 20, TOKEN_ETH, 5, ts0))
            lg["blockNumber"] = "0x10"
            import json as _j
            mint.subprocess.check_output = lambda *a, **k: _j.dumps(
                {"status": "0x1", "blockNumber": "0x10", "logs": [lg]}
            )
            rc = mint.once_v3_txid(TX, BRIDGE, queue_only=True)
            self.assertEqual(rc, 0)
            st = mint.load_state()
            self.assertIn(DEP, st.get("v3_pending") or {})
            self.assertNotIn("v3:" + DEP, st["jobs"])
        finally:
            mint.subprocess.check_output = saved_out
            for k, v in saved.items():
                setattr(mint, k, v)
            tmp.cleanup()



class HeadTimestampCastField(unittest.TestCase):
    """F3 (V3_TEST_PLAN.md): cast 1.8.5 rejects `cast block N timestamp`; use --field."""

    def test_uses_field_flag_and_falls_back(self):
        seen = []
        old_co, old_cast = mint.subprocess.check_output, mint.CAST

        class P:
            def exists(self):
                return True

            def __str__(self):
                return "cast"

        def fake(argv, **kw):
            seen.append(argv)
            if "--field" not in argv:
                raise mint.subprocess.CalledProcessError(2, argv)
            return "1791508668\n"

        mint.subprocess.check_output, mint.CAST = fake, P()
        try:
            self.assertEqual(mint.head_block_timestamp(11874000), 1791508668)
        finally:
            mint.subprocess.check_output, mint.CAST = old_co, old_cast
        self.assertEqual(seen[0][1:5], ["block", "11874000", "--field", "timestamp"])

        def boom(argv, **kw):
            raise mint.subprocess.CalledProcessError(1, argv)

        mint.subprocess.check_output, mint.CAST = boom, P()
        try:
            self.assertIsNone(mint.head_block_timestamp(11874000))
            status = {"eth_lc": {"execution_number": 11874000, "finalized_slot": 100}}
            self.assertEqual(mint.resolve_lc_head_time(status), mint.lc_head_unix_fallback(status))
        finally:
            mint.subprocess.check_output, mint.CAST = old_co, old_cast

if __name__ == "__main__":
    unittest.main()
