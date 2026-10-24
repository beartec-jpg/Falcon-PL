#!/usr/bin/env python3
"""Offline tests for pl-falcon-qc-v3-prover.py. No network, no key material logged."""
import importlib.util
import json
import os
import secrets
import subprocess
import sys
import tempfile
import unittest

_HERE = os.path.dirname(os.path.realpath(__file__))
_PATH = os.path.join(_HERE, "pl-falcon-qc-v3-prover.py")
_spec = importlib.util.spec_from_file_location("v3_prover", _PATH)
prov = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(prov)

V2_PATH = os.path.join(_HERE, "pl-falcon-qc-v2-prover.py")
CAST = os.environ.get("CAST", os.path.expanduser("~/.foundry/bin/cast"))
# Throwaway key generated per run. No key literal lives in the repo.
ANVIL_KEY0 = "0x" + secrets.token_hex(32)


def leaf(n: int) -> str:
    return f"{n:064x}"


class V3Prover(unittest.TestCase):
    def test_defaults_are_dry_run_jump_and_estimated_gas(self):
        args = prov.resolve_args(prov.build_parser().parse_args([]))
        self.assertTrue(args.jump)
        self.assertTrue(args.dry_run)
        self.assertFalse(args.send)
        self.assertEqual(args.gas_limit, 0)  # 0 = eth_estimateGas x 1.25
        self.assertEqual(prov.resolve_args(prov.build_parser().parse_args(["--gas-limit", "2700000"])).gas_limit, 2_700_000)
        for bad in ("-1", str(prov.GAS_LIMIT_MAX + 1), str(prov.TX_GAS_CAP)):
            with self.assertRaises(SystemExit):
                prov.resolve_args(prov.build_parser().parse_args(["--gas-limit", bad]))
        # --no-dry-run alone must not turn on broadcasting.
        with self.assertRaises(SystemExit):
            prov.resolve_args(prov.build_parser().parse_args(["--no-dry-run"]))
        both = prov.resolve_args(prov.build_parser().parse_args(["--no-dry-run", "--send"]))
        self.assertFalse(both.dry_run)
        sent = prov.resolve_args(prov.build_parser().parse_args(["--send", "--no-jump", "--height", "9"]))
        self.assertFalse(sent.dry_run)
        self.assertTrue(sent.send)
        self.assertFalse(sent.jump)
        self.assertEqual(sent.height, 9)

    def test_prove_v3_runs_signers_in_parallel_and_keeps_order(self):
        import threading
        import time
        import types
        from pathlib import Path
        from unittest import mock

        active = {"now": 0, "max": 0}
        lock = threading.Lock()

        def prove_one_open(height, parent, header, claim, att, sid, ix, cur_dir, next_dir):
            with lock:
                active["now"] += 1
                active["max"] = max(active["max"], active["now"])
            time.sleep(0.2 if ix == 0 else 0.05)  # first finishes last: order must still hold
            with lock:
                active["now"] -= 1
            return {"ix": ix, "sid": sid}

        fake = types.SimpleNamespace(
            die_if_v1_pk=lambda: None,
            sealed_six=lambda att, f: [f"v{i}" for i in range(6)],
            load_pubs=lambda: {},
            write_ordered_keys=lambda *a: None,
            norm_hex=lambda s: s,
            prove_one_open=prove_one_open,
            next_leaves_from_proof=lambda p: [],
        )
        att = {"header": "h", "parent": "p", "claim_root": "c", "inner_hash": "i", "next_committee_root": "n"}
        picked = [(0, "v1"), (1, "v3"), (2, "v6"), (3, "v11")]
        atts = {sid: dict(att) for _, sid in picked}
        with tempfile.TemporaryDirectory() as d, mock.patch.object(prov, "_v2", return_value=fake), \
                mock.patch.object(prov, "PK", Path(__file__)), mock.patch.object(prov, "WORK", Path(d)), \
                mock.patch.object(prov, "PROVE_PARALLEL", True), mock.patch.object(prov, "PROVE_WORKERS", 4):
            idx, proofs, _ = prov.prove_v3(9, [], picked, atts)
        self.assertEqual(idx, [0, 1, 2, 3])
        self.assertEqual([p["sid"] for p in proofs], ["v1", "v3", "v6", "v11"])
        self.assertGreater(active["max"], 1)

    def test_threshold_must_be_four(self):
        prov.require_v3_threshold(4)
        for bad in (0, 1, 3, 5):
            with self.assertRaises(SystemExit):
                prov.require_v3_threshold(bad)

    def test_no_one_gwei_floor(self):
        self.assertLess(prov.GAS_FLOOR_WEI, prov.ONE_GWEI)
        quoted = 1_000_000  # 0.001 gwei
        got = prov.gas_price_wei(quoted)
        self.assertEqual(got, int(quoted * prov.GAS_BUMP))
        self.assertLess(got, prov.ONE_GWEI)
        self.assertEqual(prov.gas_price_wei(0), prov.GAS_FLOOR_WEI)
        self.assertEqual(prov.gas_price_wei(None), int(prov.GAS_FALLBACK_WEI * prov.GAS_BUMP))
        with self.assertRaises(ValueError):
            prov.gas_price_wei(1, floor=prov.ONE_GWEI)
        with open(_PATH, encoding="utf-8") as f:
            src = f.read()
        with open(V2_PATH, encoding="utf-8") as f:
            v2 = f.read()
        self.assertNotIn("max(1_000_000_000", src)
        self.assertNotIn("ETH_PK_FILE", src)
        self.assertNotIn("privhex", src)
        self.assertNotIn("ETH_SUBMIT_PK_FILE", src)
        self.assertIn("max(1_000_000_000", v2)

    def test_key_from_env_only_and_scrubbed(self):
        old = os.environ.get(prov.KEY_ENV)
        try:
            os.environ.pop(prov.KEY_ENV, None)
            with self.assertRaises(SystemExit):
                prov.load_prover_key()
            os.environ[prov.KEY_ENV] = "/tmp/deployer.privhex"
            with self.assertRaises(SystemExit):
                prov.load_prover_key()
            secret = "ab" * 32
            os.environ[prov.KEY_ENV] = "0x" + secret
            key = prov.load_prover_key()
            self.assertEqual(key, "0x" + secret)
            blown = f"cast send --private-key {key} failed {secret}"
            clean = prov.scrub(blown, key)
            self.assertNotIn(secret, clean)
            self.assertNotIn(key, clean)
            self.assertIn("<redacted>", clean)
        finally:
            if old is None:
                os.environ.pop(prov.KEY_ENV, None)
            else:
                os.environ[prov.KEY_ENV] = old

    def test_jump_window(self):
        self.assertEqual(prov.clamp_jump_target(100, 1_000, 50, 0, True), 150)
        self.assertEqual(prov.clamp_jump_target(100, 130, 50, 0, True), 130)
        self.assertEqual(prov.clamp_jump_target(100, 1_000, 50, 150, True), 150)
        self.assertEqual(prov.clamp_jump_target(100, 1_000, 50, 101, True), 101)
        self.assertEqual(prov.clamp_jump_target(100, 1_000, 50, 0, False), 101)
        with self.assertRaises(ValueError):
            prov.clamp_jump_target(100, 1_000, 50, 100, True)
        with self.assertRaises(ValueError):
            prov.clamp_jump_target(100, 1_000, 50, 151, True)
        with self.assertRaises(ValueError):
            prov.clamp_jump_target(100, 100, 50, 0, True)

    def test_trusted_leaves_not_attestation_order(self):
        seats = ["v3", "v1", "v12", "v6", "v10", "v11"]
        leaves = [leaf(i + 1) for i in range(6)]
        seat_leaves = {sid: leaves[i] for i, sid in enumerate(seats)}
        order = prov.trusted_seat_order(leaves, seat_leaves)
        self.assertEqual(order, seats)
        # v12 is on-chain index 2 but did not attest. Indices stay the leaf
        # positions, not the attestation current_committee order.
        atts = {sid: {"header": "aa"} for sid in ("v1", "v3", "v6", "v10")}
        picked = prov.pick_signers(order, atts, 4)
        self.assertEqual(picked, [(0, "v3"), (1, "v1"), (3, "v6"), (4, "v10")])
        with self.assertRaises(ValueError):
            prov.trusted_seat_order(leaves, {"v3": leaves[0]})
        with self.assertRaises(ValueError):
            prov.pick_signers(order, {"v3": {}}, 4)

    def test_header_mismatch(self):
        atts = {
            "v1": {"header": "aa", "parent": "bb", "claim_root": "cc", "inner_hash": "dd", "next_committee_root": "ee"},
            "v3": {"header": "ff", "parent": "bb", "claim_root": "cc", "inner_hash": "dd", "next_committee_root": "ee"},
        }
        with self.assertRaises(ValueError):
            prov.headers_agree(atts, ["v1", "v3"])

    def test_dry_run_calldata_has_no_key(self):
        meta = {
            "header": "11" * 32,
            "parent": "22" * 32,
            "inner": "33" * 32,
            "claim": "44" * 32,
            "next_root": "55" * 32,
            "next_leaves": [leaf(i + 1) for i in range(6)],
        }
        proof = {
            "a": ["0x1", "0x2"],
            "b": [["0x3", "0x4"], ["0x5", "0x6"]],
            "c": ["0x7", "0x8"],
        }
        secret = "0x" + "ab" * 32
        cmd = prov.submit_command(
            "0x" + "12" * 20,
            400_000,
            meta,
            [0, 1, 3, 4],
            [proof, proof, proof, proof],
            2_000_000,
            1_250_000,
            "http://127.0.0.1:8545",
            secret,
            send=False,
        )
        blob = " ".join(cmd)
        self.assertEqual(cmd[1:3], ["calldata", prov.SUBMIT_ABI])
        self.assertNotIn("0x" + "12" * 20, cmd)  # cast calldata takes no address
        self.assertNotIn("send", cmd)
        self.assertNotIn("--private-key", cmd)
        self.assertNotIn(secret, blob)
        self.assertIn("submitHeaderWithProofs", blob)
        report = prov.dry_run_report(cmd, 2_000_000, 1_250_000)
        self.assertIn("gas_limit=2000000", report)
        self.assertIn("gas_price=1250000", report)
        self.assertNotIn(secret, report)
        with prov.MemfdSigner(ANVIL_KEY0) as signer:
            sent = prov.submit_command(
                "0x" + "12" * 20,
                400_000,
                meta,
                [0, 1, 3, 4],
                [proof, proof, proof, proof],
                2_000_000,
                1_250_000,
                "http://127.0.0.1:8545",
                signer.argv(),
                send=True,
            )
            self.assert_no_key(sent, ANVIL_KEY0)
            self.assertNotIn("--private-key", sent)
            self.assertEqual(sent[sent.index("--keystore") + 1], f"/dev/fd/{signer.ks_fd}")
            self.assertEqual(sent[sent.index("--password-file") + 1], f"/dev/fd/{signer.pw_fd}")
            self.assertIn("--gas-limit", sent)
            self.assertEqual(sent[sent.index("--gas-limit") + 1], "2000000")
        # A raw key or --private-key in the signer argv is refused.
        for bad in (["--private-key", secret], [secret], [secret[2:]]):
            with self.assertRaises(SystemExit):
                prov.submit_command(
                    "0x" + "12" * 20, 400_000, meta, [0, 1, 3, 4], [proof] * 4,
                    2_000_000, 1_250_000, "http://127.0.0.1:8545", bad, send=True,
                )
        with self.assertRaises(SystemExit):
            prov.submit_command(
                "0x" + "12" * 20, 400_000, meta, [0, 1, 3, 4], [proof] * 4,
                2_000_000, 1_250_000, "http://127.0.0.1:8545", None, send=True,
            )

    @unittest.skipUnless(os.path.exists(CAST), "cast not installed")
    def test_dry_run_command_is_valid_cast_calldata(self):
        meta = {
            "header": "11" * 32, "parent": "22" * 32, "inner": "33" * 32, "claim": "44" * 32,
            "next_root": "55" * 32, "next_leaves": [leaf(i + 1) for i in range(6)],
        }
        proof = {"a": ["0x1", "0x2"], "b": [["0x3", "0x4"], ["0x5", "0x6"]], "c": ["0x7", "0x8"]}
        cmd = prov.submit_command(
            "0x" + "12" * 20, 400_000, meta, [0, 1, 3, 4], [proof] * 4,
            2_000_000, 1_250_000, "http://127.0.0.1:8545", None, send=False,
        )
        cmd[0] = CAST
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
        self.assertEqual(r.returncode, 0, r.stderr)
        sel = subprocess.check_output([CAST, "sig", prov.SUBMIT_ABI], text=True, timeout=60).strip()
        out = r.stdout.strip()
        self.assertTrue(out.startswith(sel), (sel, out[:12]))
        # 4-byte selector + static head; every argument is static-size.
        self.assertGreater(len(out), 10 + 64 * 20)

    def test_proof_cache_is_keyed_by_onchain_leaves(self):
        a = [leaf(i + 1) for i in range(6)]
        b = [leaf(i + 2) for i in range(6)]
        wa, wb = prov.work_dir_for_leaves(a), prov.work_dir_for_leaves(b)
        self.assertNotEqual(wa, wb)
        self.assertEqual(wa, prov.work_dir_for_leaves(["0x" + x.upper() for x in a]))
        self.assertEqual(wa.parent, prov.WORK)
        self.assertTrue(wa.name.startswith("leaves-"))
        self.assertEqual(prov.work_dir_for_leaves(None), prov.WORK)

    def assert_no_key(self, argv, key):
        bare = key[2:].lower()
        for a in argv:
            self.assertNotIn(bare, a.lower())

    def test_memfd_signer_holds_no_plain_key_and_closes(self):
        with prov.MemfdSigner(ANVIL_KEY0) as signer:
            ks = os.pread(signer.ks_fd, 1 << 16, 0)
            pw = os.pread(signer.pw_fd, 1 << 16, 0)
            self.assertNotIn(ANVIL_KEY0[2:].encode(), ks)
            self.assertNotIn(ANVIL_KEY0[2:].encode(), pw)
            body = json.loads(ks)
            self.assertEqual(body["version"], 3)
            self.assertEqual(body["crypto"]["cipher"], "aes-128-ctr")
            self.assertEqual(len(pw), 64)
            link = os.readlink(f"/proc/self/fd/{signer.ks_fd}")
            self.assertTrue(link.startswith("/memfd:"), link)
            fds = signer.fds
        for fd in fds:
            with self.assertRaises(OSError):
                os.fstat(fd)

    @unittest.skipUnless(os.path.exists(CAST), "cast not installed")
    def test_cast_decrypts_memfd_keystore(self):
        with prov.MemfdSigner(ANVIL_KEY0) as signer:
            r = subprocess.run(
                [CAST, "wallet", "address", *signer.argv()],
                pass_fds=signer.fds, capture_output=True, text=True, env=prov.child_env(), timeout=60,
            )
        self.assertEqual(r.returncode, 0, r.stderr)
        # Expected address for the throwaway key (test-only, random per run).
        want = subprocess.check_output(
            [CAST, "wallet", "address", "--private-key", ANVIL_KEY0], text=True, timeout=60
        ).strip()
        self.assertEqual(r.stdout.strip().lower(), want.lower())

    def test_take_key_removes_env_and_child_env_is_clean(self):
        old = os.environ.get(prov.KEY_ENV)
        try:
            os.environ[prov.KEY_ENV] = ANVIL_KEY0
            os.environ["ETH_PASSWORD"] = "/tmp/x"
            got = prov.take_prover_key()
            self.assertEqual(got, ANVIL_KEY0)
            self.assertNotIn(prov.KEY_ENV, os.environ)
            env = prov.child_env()
            self.assertNotIn(prov.KEY_ENV, env)
            self.assertNotIn("ETH_PASSWORD", env)
            self.assertNotIn(ANVIL_KEY0[2:], "".join(env.values()).lower())
        finally:
            os.environ.pop("ETH_PASSWORD", None)
            if old is None:
                os.environ.pop(prov.KEY_ENV, None)
            else:
                os.environ[prov.KEY_ENV] = old

    def test_tx_hash_is_transaction_hash_not_block_hash(self):
        blk = "0x" + "aa" * 32
        txh = "0x" + "bb" * 32
        js = '{"blockHash":"%s","transactionHash":"%s","status":"0x1"}' % (blk, txh)
        self.assertEqual(prov.parse_tx_hash(js), txh)
        text = f"blockHash            {blk}\nblockNumber          5\ntransactionHash      {txh}\n"
        self.assertEqual(prov.parse_tx_hash(text), txh)
        self.assertEqual(prov.parse_tx_hash("nothing"), "")

    def test_source_never_passes_private_key_flag(self):
        with open(_PATH, encoding="utf-8") as f:
            src = f.read()
        self.assertNotIn('"--private-key",', src)

    def test_submit_abi_matches_v3(self):
        self.assertTrue(prov.SUBMIT_ABI.startswith("submitHeaderWithProofs(uint64,bytes32,bytes32,bytes32,bytes32,bytes32,bytes32[6],"))
        self.assertIn("uint64[4]", prov.SUBMIT_ABI)
        self.assertIn("uint256[2][4]", prov.SUBMIT_ABI)
        self.assertIn("uint256[2][2][4]", prov.SUBMIT_ABI)
        self.assertIn("uint256[2][4]", prov.SUBMIT_ABI)


# Stand-in for cast. The prover's first cast call is `cast call fplTip()`; this
# reports what the *running* prover (our parent, after any re-exec) exposes in
# /proc/<pid>/environ and cmdline, plus our own environ/argv, then fails so the
# prover stops (--once). Never prints the needle itself.
FAKE_CAST = r"""#!PYTHON
import json, os, sys
with open(os.environ["FAKE_CAST_NEEDLE_FILE"], "rb") as f:
    needle = f.read().strip().lower()
def rd(p):
    try:
        with open(p, "rb") as f:
            return f.read()
    except OSError as e:
        return ("ERR " + type(e).__name__).encode()
ppid = os.getppid()
penv = rd(f"/proc/{ppid}/environ")
pcmd = rd(f"/proc/{ppid}/cmdline")
me = rd("/proc/self/environ")
rep = {
    "ppid": ppid,
    "parent_environ_readable": not penv.startswith(b"ERR"),
    "parent_environ_has_key": bool(needle) and needle in penv.lower(),
    "parent_environ_has_key_var": b"FPL_V3_PROVER_KEY=" in penv,
    "parent_cmdline_has_key": bool(needle) and needle in pcmd.lower(),
    "parent_cmdline": [a.decode("utf-8", "replace") for a in pcmd.split(b"\0") if a and needle not in a.lower()],
    "own_environ_has_key": bool(needle) and needle in me.lower(),
    "own_argv_has_key": bool(needle) and needle in " ".join(sys.argv).lower().encode(),
    "own_env_has_cred_dir": "CREDENTIALS_DIRECTORY" in os.environ,
}
with open(os.environ["FAKE_CAST_REPORT"], "a") as f:
    f.write(json.dumps(rep) + "\n")
sys.exit(3)
"""


class ProverEnvironAfterReexec(unittest.TestCase):
    """Run the real script and inspect the running prover's /proc/<pid>/environ."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name
        self.cast = os.path.join(self.dir, "cast")
        with open(self.cast, "w") as f:
            f.write(FAKE_CAST.replace("PYTHON", sys.executable, 1))
        os.chmod(self.cast, 0o700)
        self.report = os.path.join(self.dir, "report.jsonl")
        self.key = "0x" + secrets.token_hex(32)
        # The needle goes to the fake cast in a file, never via env.
        self.needle = os.path.join(self.dir, "needle")
        fd = os.open(self.needle, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.write(fd, self.key[2:].encode())
        os.close(fd)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_prover(self, extra_env: dict, args: list[str]) -> tuple[subprocess.CompletedProcess, int, list[dict]]:
        env = {
            k: v
            for k, v in os.environ.items()
            if k not in (prov.KEY_ENV, prov.KEY_FD_ENV, "CREDENTIALS_DIRECTORY")
        }
        env.update(
            {
                "CAST": self.cast,
                "FAKE_CAST_REPORT": self.report,
                "FAKE_CAST_NEEDLE_FILE": self.needle,
                "PL_ROOT": self.dir,
                "FPL_CTL": os.path.join(self.dir, "no-ctl"),
            }
        )
        env.update(extra_env)
        p = subprocess.Popen(
            [sys.executable, _PATH, "--once", "--bridge", "0x" + "12" * 20, *args],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        out, err = p.communicate(timeout=120)
        r = subprocess.CompletedProcess(p.args, p.returncode, out, err)
        reps: list[dict] = []
        if os.path.exists(self.report):
            with open(self.report) as f:
                reps = [json.loads(line) for line in f if line.strip()]
        self.assertNotIn(self.key[2:], (out + err).lower())
        return r, p.pid, reps

    def assert_clean(self, pid: int, reps: list[dict]) -> None:
        self.assertTrue(reps, "fake cast never ran")
        for rep in reps:
            self.assertEqual(rep["ppid"], pid, "re-exec must keep the pid")
            self.assertTrue(rep["parent_environ_readable"], rep)
            self.assertFalse(rep["parent_environ_has_key"], rep)
            self.assertFalse(rep["parent_environ_has_key_var"], rep)
            self.assertFalse(rep["parent_cmdline_has_key"], rep)
            self.assertFalse(rep["own_environ_has_key"], rep)
            self.assertFalse(rep["own_argv_has_key"], rep)
            self.assertFalse(rep["own_env_has_cred_dir"], rep)
            self.assertIn(_PATH, rep["parent_cmdline"])

    def test_send_env_key_reexecs_and_environ_has_no_key(self):
        r, pid, reps = self.run_prover({prov.KEY_ENV: self.key}, ["--send"])
        # Reaching cast on --send proves the re-exec'd image loaded the key
        # from the pipe (take_prover_key runs before the first cast call).
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertIn("loop err", r.stdout)
        self.assert_clean(pid, reps)

    def test_dry_run_env_key_is_dropped_from_environ(self):
        r, pid, reps = self.run_prover({prov.KEY_ENV: self.key}, [])
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assert_clean(pid, reps)

    def test_send_systemd_credential_key_environ_has_no_key(self):
        creds = os.path.join(self.dir, "creds")
        os.mkdir(creds, 0o700)
        path = os.path.join(creds, prov.CRED_NAME)
        with open(path, "w") as f:
            f.write(self.key + "\n")
        os.chmod(path, 0o400)
        r, pid, reps = self.run_prover({"CREDENTIALS_DIRECTORY": creds}, ["--send"])
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assert_clean(pid, reps)

    def test_credential_and_env_together_refused(self):
        creds = os.path.join(self.dir, "creds")
        os.mkdir(creds, 0o700)
        with open(os.path.join(creds, prov.CRED_NAME), "w") as f:
            f.write(self.key)
        r, _, reps = self.run_prover(
            {"CREDENTIALS_DIRECTORY": creds, prov.KEY_ENV: self.key}, ["--send"]
        )
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("not both", r.stderr)
        self.assertEqual(reps, [])

    def test_send_bad_env_key_fails_closed_before_exec(self):
        for bad in ("/tmp/deployer.privhex", "0x1234", "zz" * 32):
            if os.path.exists(self.report):
                os.remove(self.report)
            r, _, reps = self.run_prover({prov.KEY_ENV: bad}, ["--send"])
            self.assertNotEqual(r.returncode, 0)
            self.assertIn(prov.KEY_ENV, r.stderr)
            self.assertNotIn(bad, r.stderr)
            self.assertEqual(reps, [])

    def test_send_without_any_key_fails_closed(self):
        r, _, reps = self.run_prover({}, ["--send"])
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("is unset", r.stderr)
        self.assertEqual(reps, [])

    def test_key_fd_marker_must_be_a_pipe(self):
        fd = os.open(os.devnull, os.O_RDONLY)
        try:
            os.environ[prov.KEY_FD_ENV] = str(fd)
            with self.assertRaises(SystemExit):
                prov.take_prover_key()
            self.assertNotIn(prov.KEY_FD_ENV, os.environ)
        finally:
            os.environ.pop(prov.KEY_FD_ENV, None)
            try:
                os.close(fd)
            except OSError:
                pass
        r, w = os.pipe()
        os.write(w, self.key.encode())
        os.close(w)
        os.environ[prov.KEY_FD_ENV] = str(r)
        try:
            self.assertEqual(prov.take_prover_key(), self.key)
        finally:
            os.environ.pop(prov.KEY_FD_ENV, None)
        with self.assertRaises(OSError):
            os.fstat(r)


class OneRpcPerRun(unittest.TestCase):
    def test_reads_and_send_use_the_rpc_that_answered(self):
        calls: list[list[str]] = []
        sent: list[list[str]] = []
        dead, good = "http://dead.invalid", "http://good.invalid"

        def fake_cast(args, timeout=30):
            calls.append(list(args))
            rpc = args[args.index("--rpc-url") + 1]
            if rpc == dead:
                raise RuntimeError("connection refused")
            if "chain-id" in args:
                return "11155111"
            if "gas-price" in args:
                return "1000000"
            if "currentLeaves(uint256)(bytes32)" in args:
                return "0x" + leaf(int(args[args.index("currentLeaves(uint256)(bytes32)") + 1]) + 1)
            if "fplTip()(uint64)" in args:
                return "100"
            if "maxJump()(uint64)" in args:
                return "50"
            raise AssertionError(args)

        meta = {
            "header": "11" * 32, "parent": "22" * 32, "inner": "33" * 32, "claim": "44" * 32,
            "next_root": "55" * 32, "next_leaves": [leaf(i + 1) for i in range(6)],
        }
        proof = {"a": ["0x1", "0x2"], "b": [["0x3", "0x4"], ["0x5", "0x6"]], "c": ["0x7", "0x8"]}
        patches = {
            "_cast": fake_cast,
            "eth_rpcs": lambda: [dead, good],
            "mesh_fpl_tip": lambda: 120,
            "poll_atts": lambda: None,
            "load_seat_leaves": lambda: {f"v{i}": leaf(i + 1) for i in range(6)},
            "load_atts": lambda h: {f"v{i}": {} for i in range(6)},
            "prove_v3": lambda h, order, picked, atts, leaves=None: ([i for i, _ in picked], [proof] * 4, meta),
            "emit": lambda cmd, gl, gp, send, key, pass_fds=(): sent.append(cmd) or "0x" + "aa" * 32,
        }
        saved = {k: getattr(prov, k) for k in patches}
        saved_cast = prov.CAST
        try:
            for k, v in patches.items():
                setattr(prov, k, v)
            prov.CAST = prov.Path(_PATH)  # exists; never executed (_cast is patched)
            args = prov.resolve_args(prov.build_parser().parse_args(["--send", "--bridge", "0x" + "12" * 20]))
            prov.run_once(args, ANVIL_KEY0)
        finally:
            for k, v in saved.items():
                setattr(prov, k, v)
            prov.CAST = saved_cast
        self.assertEqual(len(sent), 1)
        cmd = sent[0]
        self.assertEqual(cmd[cmd.index("--rpc-url") + 1], good)
        # Only the health probe touched the dead RPC; every read went to `good`.
        dead_calls = [c for c in calls if dead in c]
        self.assertEqual(len(dead_calls), 1)
        self.assertIn("chain-id", dead_calls[0])
        self.assertTrue(all(good in c for c in calls if dead not in c))

    def test_pick_rpc_skips_wrong_chain(self):
        saved = (prov._cast, prov.eth_rpcs)
        try:
            prov.eth_rpcs = lambda: ["http://mainnet.invalid", "http://sepolia.invalid"]

            def chain(args, timeout=30):
                rpc = args[args.index("--rpc-url") + 1]
                return "1" if "mainnet" in rpc else "11155111"

            prov._cast = chain
            self.assertEqual(prov.pick_rpc(), "http://sepolia.invalid")
            prov.eth_rpcs = lambda: ["http://mainnet.invalid"]
            with self.assertRaises(RuntimeError):
                prov.pick_rpc()
        finally:
            prov._cast, prov.eth_rpcs = saved

    def test_pick_rpc_errors_when_none_answer(self):
        saved = (prov._cast, prov.eth_rpcs)
        try:
            prov.eth_rpcs = lambda: ["http://a.invalid", "http://b.invalid"]

            def boom(args, timeout=30):
                raise RuntimeError("down")

            prov._cast = boom
            with self.assertRaises(RuntimeError):
                prov.pick_rpc()
        finally:
            prov._cast, prov.eth_rpcs = saved



class GasLimit(unittest.TestCase):
    """F2 (V3_TEST_PLAN.md): a real 4-proof submit used 2,207,860 gas; 2,000,000 ran out."""

    def test_buffer_fallback_and_cap(self):
        self.assertEqual(prov.gas_limit_from_estimate(2_207_860), 2_759_825)
        self.assertGreater(prov.gas_limit_from_estimate(2_207_860), 2_207_860)
        self.assertEqual(prov.gas_limit_from_estimate(None), 2_700_000)
        self.assertEqual(prov.gas_limit_from_estimate(0), 2_700_000)
        self.assertEqual(prov.gas_limit_from_estimate(15_000_000), prov.GAS_LIMIT_MAX)
        self.assertEqual(prov.gas_limit_from_estimate(prov.GAS_LIMIT_MAX), prov.GAS_LIMIT_MAX)
        with self.assertRaises(prov.EstimateTooHigh):
            prov.gas_limit_from_estimate(prov.GAS_LIMIT_MAX + 1)
        self.assertLess(prov.GAS_LIMIT_MAX, prov.TX_GAS_CAP)
        self.assertEqual(prov.TX_GAS_CAP, 2**24)

    def _fake_cast(self, body: str) -> str:
        d = tempfile.mkdtemp()
        path = os.path.join(d, "cast")
        with open(path, "w") as f:
            f.write("#!/bin/sh\n" + body + "\n")
        os.chmod(path, 0o755)
        self.addCleanup(lambda: __import__("shutil").rmtree(d, ignore_errors=True))
        return path

    def _with_cast(self, path: str):
        old = prov.CAST
        prov.CAST = prov.Path(path)
        self.addCleanup(setattr, prov, "CAST", old)

    def test_estimate_used_with_buffer_and_argv_has_no_key(self):
        log = tempfile.mktemp()
        self.addCleanup(lambda: os.path.exists(log) and os.unlink(log))
        self._with_cast(self._fake_cast(f'echo "$@" > {log}; echo 2207860'))
        got = prov.resolve_gas_limit(0, "0x" + "12" * 20, ["1", "2"], "http://127.0.0.1:8545", "0x" + "34" * 20)
        self.assertEqual(got, 2_759_825)
        argv = open(log).read()
        self.assertIn("estimate --rpc-url http://127.0.0.1:8545 --from 0x" + "34" * 20, argv)
        self.assertIn(prov.SUBMIT_ABI, argv)
        self.assertNotIn("--private-key", argv)

    def test_explicit_limit_skips_estimate(self):
        self._with_cast(self._fake_cast("exit 99"))
        self.assertEqual(prov.resolve_gas_limit(3_000_000, "0x" + "12" * 20, [], "http://x"), 3_000_000)

    def test_rpc_failure_falls_back(self):
        self._with_cast(self._fake_cast('echo "error sending request: connection refused" >&2; exit 1'))
        self.assertEqual(prov.resolve_gas_limit(0, "0x" + "12" * 20, [], "http://x"), 2_700_000)
        self._with_cast(self._fake_cast("echo nonsense"))
        self.assertEqual(prov.resolve_gas_limit(0, "0x" + "12" * 20, [], "http://x"), 2_700_000)

    def test_revert_is_not_sent(self):
        self._with_cast(self._fake_cast('echo "Error: server returned an error response: error code 3: execution reverted: range" >&2; exit 1'))
        with self.assertRaises(prov.EstimateReverted):
            prov.resolve_gas_limit(0, "0x" + "12" * 20, [], "http://x")

    def test_estimate_above_max_is_not_sent(self):
        self._with_cast(self._fake_cast("echo 16000001"))
        with self.assertRaises(prov.EstimateTooHigh):
            prov.resolve_gas_limit(0, "0x" + "12" * 20, [], "http://x")

if __name__ == "__main__":
    unittest.main()
