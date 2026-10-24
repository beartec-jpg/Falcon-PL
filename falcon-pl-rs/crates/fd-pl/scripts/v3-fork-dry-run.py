#!/usr/bin/env python3
"""FalconQcBridgeV3 test plan: anvil Sepolia-fork dry run (docs/V3_TEST_PLAN.md).

Starts a LOCAL anvil forked from public Sepolia, deploys FalconQcBridgeV3 on
the fork only, and walks peg-in -> auto-mint --v3 -> FPL mint/refund -> header
-> openClaim/take, plus replay and double-resolve. Nothing is sent to Sepolia:
every transaction goes to the local anvil, from anvil's public dev accounts
(unlocked, so no key is passed anywhere) or, for the prover send, from anvil
dev key #1 handed to the prover through FPL_V3_PROVER_KEY (never argv).

Real on the fork:
  - FalconQcBridgeV2 / V1 / DestLock, Sepolia USDC, and the production
    Groth16Verifier 0x2Cb7...4F80, all as deployed on Sepolia.
  - The +1 header 353131 with the four real Groth16 proofs committed in
    fixtures/v2-groth16/plus1-submit.json, verified by the real verifier.
  - pl-falcon-qc-v3-prover.py's send path (MemfdSigner + cast send).
  - pl-eth-auto-mint.py --v3 (log scan, plan, receipt MPT proof, the exact
    submit-eth-v3-deposit argv) and the relayer's header/receipt encoding.
  - The FPL consensus code (Rust, three in-process nodes) applying the
    recorded submissions against the fork's real headers and receipts.
  - pl-eth-claim-proof.py for real V2-era notes: one V2 committed (refused on V3,
    S7a) and note 9 (leaf 9, outside V2 header 353130; opened and paid, S7c).

Mocked (each one is reported as such in the summary):
  - The FPL node behind ctl: v3-dry-run-mock-ctl.py records submits.
  - The ETH light client on the Rust side (eth_lc set to a fork block).
  - The LC update step inside auto-mint (relayer cmd_update needs a beacon).
  - Groth16 for headers above 353131: after the real-proof step the verifier
    address gets always-true code (anvil_setCode), because a header that
    commits fork-made refund leaves has no committee signatures.
  - USDC balances for dev accounts (anvil_setStorageAt), and impersonation of
    one real note's dest to call take() on the fork.

Usage:
  v3-fork-dry-run.py [--fork-url URL] [--fork-block N] [--out DIR] [--skip-rust] [--expect-known-findings]
Exit 0 (VERDICT PASS) only if S0-S8 (incl. S7a/b/c) all ran and passed, and every F check passed
(F1-F4 all ran; with --expect-known-findings only F1/F4 may fail, as KNOWN). --skip-rust ends PARTIAL (exit 3).
Any failure, abort or dropped scenario is FAIL (exit 1). report.json is always written.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import traceback
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent
WORKSPACE = CRATE.parent.parent
FOUNDRY = Path.home() / ".foundry/bin"
ANVIL = os.environ.get("ANVIL", str(FOUNDRY / "anvil"))
CAST = os.environ.get("CAST", str(FOUNDRY / "cast"))
FORGE = os.environ.get("FORGE", str(FOUNDRY / "forge"))
DEFAULT_FORK = "https://ethereum-sepolia-rpc.publicnode.com"
SEPOLIA = 11155111
ANVIL_MNEMONIC = "test test test test test test test test test test test junk"

USDC = "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238"
GROTH16 = "0x2Cb70e9f082F2DF91E9A5e6E7C9DF6b8b11B4F80"
V2 = "0x811854827627024B38926Ea9DCc0f88ACd5fB23e"
V1 = "0xf8F1471643792eb1cD5d0C31061629777E55bc48"
DESTLOCK = "0xdBF6855b00B78c047A729A21E13bfE5f4C991C05"
OLD_DEPLOYER = "0xdb52847ee70ced3128f49309c3dc65b69d7466f4"
MAX_JUMP = 50_000
# V3 constructor anchor (FM ruling, Copilot r4226045086 on #38; Copilot on #45): V2 header 353130 and
# the 9 leaves its claim root commits, deployed as a FalconQcBridgeV3AnchorSet. S0 re-derives the
# leaves from the real fixture vs the fork's V2 root; V3's constructor re-checks the root on-chain.
ANCHOR_LEAF_COUNT = 9
CTOR_SIG = "constructor(address,address,address,address,address,uint64,address,uint64,address)"
E_SECS = 3 * 86_400
SEAL_PREFIX = b"pl-ledger:v4-seal"
# Runtime code that returns abi true for any call: PUSH1 1 PUSH1 0 MSTORE PUSH1 32 PUSH1 0 RETURN.
ALWAYS_TRUE = "0x600160005260206000f3"
DEPOSIT_SIG = "Deposit(bytes32,address,bytes20,address,uint256,uint64)"
PEGIN_SIG = "pegIns(bytes32)(address,bytes20,address,uint256,uint64,bool,bool)"
CLAIM_SIG = "openClaim(bytes32,address,uint256,bool,bytes32[],uint32,uint64)"

FIX_PLUS1 = HERE / "fixtures/v2-groth16/plus1-submit.json"
FIX_CLAIM = HERE / "fixtures/eth-claim-proof/v2-header-353130.json"

RESULTS: list[dict] = []
# openClaim gas per claim (tag -> gasUsed), summarised in report.json meta.
OPENCLAIM_GAS: dict[str, int] = {}
# walletd's openClaim gas limit (V2's 400k, reused by the V3 claim path in #38). F4 checks
# every V3 openClaim of this run against it. Sepolia's state-creation pricing (EIP-8037)
# makes the 5 new Claim slots cost ~111k each, so this fails today.
WALLETD_OPENCLAIM_GAS_LIMIT = int(os.environ.get("V3_DRY_WALLETD_OPENCLAIM_GAS", "400000"))
# Scenarios a full run must contain; --skip-rust stops after S5 and is PARTIAL.
REQUIRED_FULL = ["S0", "S1", "S2", "S3", "S4", "S5", "S6", "S7a", "S7b", "S7c", "S8"]
REQUIRED_SKIP_RUST = ["S0", "S1", "S2", "S3", "S4", "S5"]
FINDINGS_ALL = ["F1", "F2", "F3", "F4"]
# The only findings --expect-known-findings may waive (open, tracked). F2/F3 are fixed (#41):
# a failing F2/F3 (or any other F check) stays fatal even with the flag.
KNOWN_FINDINGS = ["F1", "F4"]


# --------------------------------------------------------------------------- utils


def sha256(b: bytes) -> bytes:
    return hashlib.sha256(b).digest()


def h32(x: str) -> str:
    x = x.strip().lower().removeprefix("0x")
    return "0x" + x.rjust(64, "0")


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class Fork:
    """Local anvil fork. Every send in this script goes through here."""

    def __init__(self, fork_url: str, fork_block: int, logdir: Path):
        self.port = free_port()
        self.url = f"http://127.0.0.1:{self.port}"
        cmd = [ANVIL, "--fork-url", fork_url, "--port", str(self.port), "--silent"]
        if fork_block:
            cmd += ["--fork-block-number", str(fork_block)]
        self.log = open(logdir / "anvil.log", "w")
        try:
            self.proc = subprocess.Popen(cmd, stdout=self.log, stderr=subprocess.STDOUT)
        except BaseException:
            self.log.close()
            raise
        try:
            self._wait_ready()
        except BaseException:
            self.stop()
            raise

    def _wait_ready(self) -> None:
        for _ in range(300):
            try:
                if int(self.rpc("eth_chainId", []), 16) == SEPOLIA:
                    break
            except Exception:
                pass
            time.sleep(0.2)
        else:
            raise SystemExit("anvil fork did not start")
        client = str(self.rpc("web3_clientVersion", [])).lower()
        if "anvil" not in client:
            raise SystemExit(f"refusing: {self.url} is not anvil ({client})")
        self.accounts = [a.lower() for a in self.rpc("eth_accounts", [])]
        self.fork_block = int(self.rpc("eth_blockNumber", []), 16)

    def stop(self) -> None:
        if self.log.closed:
            return
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self.log.close()

    def rpc(self, method: str, params: list):
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
        req = urllib.request.Request(self.url, data=body, headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=120) as r:
            out = json.loads(r.read().decode())
        if out.get("error"):
            raise RuntimeError(f"{method}: {out['error']}")
        return out.get("result")

    def cast(self, *args: str, timeout: int = 120) -> str:
        # --rpc-url right after the subcommand: `cast send --create` treats trailing words as ctor args.
        r = subprocess.run([CAST, args[0], "--rpc-url", self.url, *args[1:]], text=True, capture_output=True, timeout=timeout)
        if r.returncode != 0:
            raise RuntimeError((r.stdout + r.stderr).strip()[-1500:])
        return r.stdout.strip()

    def call(self, to: str, sig: str, *args: str, sender: str = "") -> str:
        extra = ["--from", sender] if sender else []
        return self.cast("call", *extra, to, sig, *args)

    def revert_reason(self, to: str, sig: str, *args: str, sender: str = "", value: int = 0) -> str:
        """eth_call that must revert. Returns the reason string, raises if it succeeds."""
        extra = ["--from", sender] if sender else []
        if value:
            extra += ["--value", str(value)]
        r = subprocess.run(
            [CAST, "call", *extra, to, sig, *args, "--rpc-url", self.url],
            text=True,
            capture_output=True,
            timeout=120,
        )
        if r.returncode == 0:
            raise AssertionError(f"{sig} did not revert: {r.stdout.strip()}")
        blob = r.stdout + r.stderr
        m = re.search(r"execution reverted(?::|,)?\s*(?:revert:\s*)?([^\n\"]*)", blob)
        return (m.group(1).strip() if m else blob.strip()[-300:])

    def _guard_send(self, sender: str) -> None:
        # Every transaction in this script goes through here: local anvil only, never 0xDb52.
        if not re.fullmatch(r"http://127\.0\.0\.1:\d+", self.url) or self.proc.poll() is not None:
            raise SystemExit(f"refusing to send: {self.url} is not this script's local anvil")
        if sender.lower() == OLD_DEPLOYER:
            raise SystemExit("refusing to act as the old deployer")

    def send(self, sender: str, to: str, sig: str, *args: str, value: int = 0) -> dict:
        self._guard_send(sender)
        cmd = ["send", "--unlocked", "--from", sender, to, sig, *args, "--json"]
        if value:
            cmd += ["--value", str(value)]
        rec = json.loads(self.cast(*cmd))
        if int(str(rec.get("status")), 0) != 1:
            raise RuntimeError(f"{sig} failed: {rec}")
        return rec

    def deploy(self, sender: str, initcode: str) -> dict:
        """Contract creation on the local anvil, same guards as send()."""
        self._guard_send(sender)
        rec = json.loads(self.cast("send", "--unlocked", "--from", sender, "--json", "--create", initcode))
        if int(str(rec.get("status")), 0) != 1:
            raise RuntimeError(f"deploy failed: {rec}")
        return rec

    def mine(self, n: int) -> int:
        self.rpc("anvil_mine", [hex(n)])
        return int(self.rpc("eth_blockNumber", []), 16)

    def block(self, n: int) -> dict:
        return self.rpc("eth_getBlockByNumber", [hex(n), False])


def scenario(sid: str, title: str, real: list[str], mocked: list[str]):
    def deco(fn):
        def run(*a, **kw):
            rec = {"id": sid, "title": title, "real": real, "mocked": mocked, "status": "FAIL", "evidence": {}}
            RESULTS.append(rec)
            print(f"\n=== {sid}: {title}", flush=True)
            try:
                out = fn(rec["evidence"], *a, **kw)
                rec["status"] = "PASS"
                print(f"--- {sid} PASS", flush=True)
                return out
            except Exception as e:
                rec["error"] = f"{type(e).__name__}: {e}"
                rec["traceback"] = traceback.format_exc()[-3000:]
                print(f"--- {sid} FAIL: {rec['error']}", flush=True)
                raise
        return run
    return deco


def check(cond: bool, msg: str) -> None:
    if not cond:
        raise AssertionError(msg)


# --------------------------------------------------------------------------- steps


def build_v3(work: Path) -> dict[str, str]:
    """Creation bytecode of FalconQcBridgeV3Checks, FalconQcBridgeV3AnchorSet and FalconQcBridgeV3 from one build."""
    proj = work / "forge"
    proj.mkdir(parents=True, exist_ok=True)
    subprocess.run([FORGE, "init", "--force", "--no-git", "--quiet", str(proj)], check=True, capture_output=True)
    for f in ("src/Counter.sol", "test/Counter.t.sol", "script/Counter.s.sol"):
        (proj / f).unlink(missing_ok=True)
    toml = (proj / "foundry.toml").read_text()
    toml = toml.replace('libs = ["lib"]\n', 'libs = ["lib"]\nsolc = "0.8.20"\nvia_ir = true\noptimizer = true\noptimizer_runs = 200\n', 1)
    (proj / "foundry.toml").write_text(toml)
    for name in ("FalconQcBridgeV3.sol", "FalconQcBridgeV3Checks.sol"):
        shutil.copy(HERE / name, proj / "src" / name)
    subprocess.run([FORGE, "build", "--root", str(proj)], check=True, capture_output=True, text=True)
    out = {}
    for name, src in (("FalconQcBridgeV3Checks", "FalconQcBridgeV3Checks"), ("FalconQcBridgeV3AnchorSet", "FalconQcBridgeV3Checks"), ("FalconQcBridgeV3", "FalconQcBridgeV3")):
        art = json.loads((proj / f"out/{src}.sol/{name}.json").read_text())
        out[name] = art["bytecode"]["object"]
        out[name + ".runtime"] = art["deployedBytecode"]["object"]
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--fork-url", default=os.environ.get("V3_DRY_FORK_URL", DEFAULT_FORK))
    ap.add_argument("--fork-block", type=int, default=0)
    ap.add_argument("--out", default="")
    ap.add_argument("--skip-rust", action="store_true", help="stop after the auto-mint step (no cargo); the run is PARTIAL")
    ap.add_argument(
        "--expect-known-findings",
        action="store_true",
        help="failing F1/F4 (open, tracked) are reported as KNOWN and do not fail the run; any other failing F check and every S0-S8 failure still fail it",
    )
    args = ap.parse_args()
    for tool in (ANVIL, CAST, FORGE):
        if not Path(tool).exists():
            raise SystemExit(f"missing {tool}: install foundry (foundryup)")
    out = Path(args.out or tempfile.mkdtemp(prefix="v3-fork-dry-run-")).resolve()
    out.mkdir(parents=True, exist_ok=True)
    print(f"output dir {out}", flush=True)
    started = time.strftime("%Y-%m-%d %H:%M:%S %Z")
    meta = {"started": started, "fork_url": args.fork_url, "fork_block": None, "anvil_url": None}
    run_error = None
    exit_code = None
    fork = None
    try:
        # Fork startup is inside the protected lifecycle: if anvil or the fork RPC
        # fails, the run is FAIL and report.json is still written.
        fork = Fork(args.fork_url, args.fork_block, out)
        meta["fork_block"], meta["anvil_url"] = fork.fork_block, fork.url
        run_all(fork, out, args, meta)
    except BaseException as e:  # noqa: BLE001 - also SystemExit from a send guard: always write the report
        # A failure between scenario wrappers (or inside one, re-raised) means later
        # scenarios never ran: record it and never report ok.
        run_error = f"{type(e).__name__}: {e}"
        meta["run_error"] = run_error
        meta["run_traceback"] = traceback.format_exc()
        if isinstance(e, KeyboardInterrupt):
            exit_code = 130
    finally:
        if fork is not None:
            fork.stop()
    ran = {r["id"]: r for r in RESULTS}
    required = REQUIRED_SKIP_RUST if args.skip_rust else REQUIRED_FULL
    # A full run must also have run every F check; a dropped one is never a PASS.
    missing = [sid for sid in REQUIRED_FULL + FINDINGS_ALL if sid not in ran]
    scen_fail = [sid for sid in required if sid in ran and ran[sid]["status"] != "PASS"]
    findings = [r for r in RESULTS if r["id"].startswith("F")]
    find_fail = [r["id"] for r in findings if r["status"] != "PASS"]
    if args.expect_known_findings:
        for r in findings:
            if r["status"] != "PASS" and r["id"] in KNOWN_FINDINGS:
                r["status"] = "KNOWN"
        find_fail = [r["id"] for r in findings if r["status"] not in ("PASS", "KNOWN")]
    if run_error or scen_fail or (missing and not args.skip_rust):
        verdict = "FAIL"
    elif missing:
        verdict = "PARTIAL"
    elif find_fail:
        verdict = "FAIL"
    else:
        verdict = "PASS"
    ok = verdict == "PASS"
    meta["finished"] = time.strftime("%Y-%m-%d %H:%M:%S %Z")
    meta["verdict"] = verdict
    meta["missing_scenarios"] = missing
    meta["openclaim_gas"] = dict(OPENCLAIM_GAS)
    meta["walletd_openclaim_gas_limit"] = WALLETD_OPENCLAIM_GAS_LIMIT
    report = {"meta": meta, "ok": ok, "verdict": verdict, "scenarios": RESULTS}
    (out / "report.json").write_text(json.dumps(report, indent=2, default=str) + "\n")
    print("\n================ V3 fork dry run summary ================")
    print(f"fork {args.fork_url} @ block {meta['fork_block']} (local anvil, nothing sent to Sepolia)")
    for r in RESULTS:
        mock = f"  [mocked: {'; '.join(r['mocked'])}]" if r["mocked"] else ""
        print(f"{r['status']:5} {r['id']:4} {r['title']}{mock}")
        if r.get("error"):
            print(f"        {r['error'][:400]}")
    if OPENCLAIM_GAS:
        print(f"openClaim gas (walletd limit {WALLETD_OPENCLAIM_GAS_LIMIT}): " + ", ".join(f"{k} {v}" for k, v in OPENCLAIM_GAS.items()))
    if run_error:
        print(f"ABORTED: run stopped early, later scenarios did not run: {run_error[:400]}")
    if missing:
        print(f"not run: {', '.join(missing)}" + (" (--skip-rust)" if args.skip_rust else ""))
    known = [r["id"] for r in findings if r["status"] == "KNOWN"]
    print(f"VERDICT: {verdict}" + (" (known findings open: " + ", ".join(known) + ")" if known else ""))
    print(f"report: {out / 'report.json'}")
    if exit_code is not None:
        return exit_code
    return {"PASS": 0, "FAIL": 1, "PARTIAL": 3}[verdict]


def run_all(fork: Fork, out: Path, args, meta: dict) -> None:
    acct = fork.accounts
    deployer, prover_addr, alice, bob, claimer = acct[0], acct[1], acct[2], acct[3], acct[4]
    plus1 = json.loads(FIX_PLUS1.read_text())
    ctx: dict = {"out": out}

    # ---- S0 pre-flight: the fork really is Sepolia with V2 stuck at the fixture anchor.
    @scenario("S0", "Fork pre-flight: Sepolia V2 anchor matches the committed real-proof fixture", ["Sepolia state via fork"], ["EIP-7702 delegation code cleared from the 5 dev accounts used (on public Sepolia they delegate to sweepers)"])
    def s0(ev):
        ev["fork_block"] = fork.fork_block
        # anvil's public dev accounts carry EIP-7702 delegations on Sepolia (0xef0100...),
        # e.g. sweepers that forward any ETH they receive. Clear them on the fork so
        # balances measure V3, not someone's delegate.
        cleared = {}
        for a in acct[:5]:
            code = fork.rpc("eth_getCode", [a, "latest"])
            if code not in ("0x", "0x0", None):
                cleared[a] = code[:48]
                fork.rpc("anvil_setCode", [a, "0x"])
        ev["cleared_7702"] = cleared
        tip = int(fork.call(V2, "fplTip()(uint64)").split()[0])
        ev["v2_fplTip"] = tip
        check(tip == plus1["anchor"]["tip"], f"V2 fplTip {tip} != fixture anchor {plus1['anchor']['tip']}")
        hdr = fork.call(V2, "headers(uint64)(bytes32,bytes32,bytes32,bytes32,bytes32,bool)", str(tip)).split()
        a = plus1["anchor"]
        check(hdr[0] == a["hash"] and hdr[3] == a["claimRoot"] and hdr[4] == a["committeeRoot"], f"V2 header {hdr}")
        check(fork.call(V2, "verifier()(address)").lower() == GROTH16.lower(), "verifier")
        check(len(fork.rpc("eth_getCode", [GROTH16, "latest"])) > 1000, "no verifier code")
        ev["v2_header"] = hdr
        # anchorLeafCount: the real FPL leaves (#33 fixture) whose root equals V2 headers(353130).claimRoot.
        cp0 = load_module("claim_proof_s0", HERE / "pl-eth-claim-proof.py")
        lc = [bytes.fromhex(x) for x in json.loads(FIX_CLAIM.read_text())["status"]["lc"]["claim_leaves"]]
        hits = [n for n in range(1, len(lc) + 1) if "0x" + cp0.merkle_root(lc[:n]).hex() == hdr[3]]
        ev["anchor_leaf_count_candidates"] = hits
        check(hits == [ANCHOR_LEAF_COUNT], f"V2 anchor root matches leaf counts {hits}, expected [{ANCHOR_LEAF_COUNT}]")
        # The anchor set holds each note's withdrawal preimage, in FPL leaf order (5 ETH, then 4 USDC).
        fx0 = json.loads(FIX_CLAIM.read_text())["status"]
        entries = []
        for leaf in lc[:ANCHOR_LEAF_COUNT]:
            m = [(r["asset"], w) for r in fx0["rails"] for w in r["withdrawals"]
                 if cp0.withdraw_leaf(bytes.fromhex(w["note_id"]), bytes.fromhex(w["external_to"][2:]), int(w["amount"]), r["asset"] == "USDC") == leaf]
            check(len(m) == 1, f"anchor leaf {leaf.hex()} matches {len(m)} withdrawals")
            asset, w = m[0]
            entries.append({"note_id": "0x" + w["note_id"], "dest": w["external_to"], "amount": int(w["amount"]), "usdc": asset == "USDC"})
        ctx["anchor_entries"] = entries
        ev["anchor_entries"] = entries

    s0()

    # ---- S1 deploy on the fork only.
    @scenario("S1", "Deploy FalconQcBridgeV3Checks, FalconQcBridgeV3AnchorSet, then FalconQcBridgeV3 on the fork (anchored from real V2)", ["V3 + Checks bytecode (solc 0.8.20 via_ir)", "V2/V1/DestLock/USDC/verifier addresses"], [])
    def s1(ev):
        code = build_v3(out)
        # Deployment 1 of 3: FalconQcBridgeV3Checks (no constructor arguments).
        rec_ck = fork.deploy(deployer, code["FalconQcBridgeV3Checks"])
        checks = rec_ck["contractAddress"]
        ev["checks"] = checks
        ev["checks_deploy_tx"] = rec_ck["transactionHash"]
        ev["checks_deploy_gas"] = int(str(rec_ck["gasUsed"]), 0)
        ev["checks_runtime_bytes"] = len(code["FalconQcBridgeV3Checks.runtime"]) // 2 - 1
        # Deployment 2 of 3: FalconQcBridgeV3AnchorSet(the 9 notes V2 header 353130 commits, from S0).
        ents = ctx["anchor_entries"]
        set_ctor = subprocess.check_output(
            [CAST, "abi-encode", "constructor(bytes32[],address[],uint64[],bool[])",
             "[" + ",".join(e["note_id"] for e in ents) + "]", "[" + ",".join(e["dest"] for e in ents) + "]",
             "[" + ",".join(str(e["amount"]) for e in ents) + "]", "[" + ",".join("true" if e["usdc"] else "false" for e in ents) + "]"],
            text=True,
        ).strip()
        rec_set = fork.deploy(deployer, code["FalconQcBridgeV3AnchorSet"] + set_ctor[2:])
        anchor_set = rec_set["contractAddress"]
        ev["anchor_set"] = anchor_set
        ev["anchor_set_deploy_gas"] = int(str(rec_set["gasUsed"]), 0)
        set_code = fork.rpc("eth_getCode", [anchor_set, "latest"])
        want = "0x00" + "".join(e["note_id"][2:] + e["dest"][2:].lower() + f"{e['amount']:016x}" + ("01" if e["usdc"] else "00") for e in ents)
        check(set_code.lower() == want, "anchor set code is not 0x00 || entries")
        ev["anchor_set_root"] = fork.call(checks, "anchorSetRoot(address)(bytes32,uint256)", anchor_set).split()[0]
        check(ev["anchor_set_root"] == plus1["anchor"]["claimRoot"], "anchor set root != V2 anchor claimRoot")
        # Deployment 3 of 3: FalconQcBridgeV3 bound to the checks and anchor-set addresses.
        anchor_h = str(plus1["anchor"]["tip"])
        ctor = subprocess.check_output(
            [CAST, "abi-encode", CTOR_SIG, USDC, GROTH16, V2, V1, DESTLOCK, str(MAX_JUMP), checks, anchor_h, anchor_set], text=True
        ).strip()
        ev["ctor_args"] = {"usdc": USDC, "groth16": GROTH16, "v2": V2, "v1": V1, "destLock": DESTLOCK, "maxJump": MAX_JUMP, "checks": checks, "anchorHeight": int(anchor_h), "anchorSet": anchor_set}
        rec = fork.deploy(deployer, code["FalconQcBridgeV3"] + ctor[2:])
        v3 = rec["contractAddress"]
        ctx["v3"] = v3
        ctx["deploy_block"] = int(str(rec["blockNumber"]), 0)
        ev["v3"] = v3
        ev["deploy_tx"] = rec["transactionHash"]
        ev["deploy_gas"] = int(str(rec["gasUsed"]), 0)
        ev["v3_runtime_bytes"] = len(code["FalconQcBridgeV3.runtime"]) // 2 - 1
        ev["total_deploy_gas"] = ev["checks_deploy_gas"] + ev["anchor_set_deploy_gas"] + ev["deploy_gas"]
        # EIP-7825 caps one tx at 2**24 gas. Each deployment must stay under ~15M with margin.
        ev["eip7825_tx_cap"] = 2**24
        ev["deploy_gas_budget"] = 15_000_000
        for k in ("checks_deploy_gas", "anchor_set_deploy_gas", "deploy_gas"):
            check(ev[k] < ev["deploy_gas_budget"], f"{k} {ev[k]} is not under {ev['deploy_gas_budget']}")
        check(fork.call(v3, "checks()(address)").lower() == checks.lower(), "checks address")
        # A V3 bound to any other code is refused (here: the verifier's address).
        def ctor_refused(reason: str, *argv: str) -> None:
            bad = subprocess.check_output([CAST, "abi-encode", CTOR_SIG, *argv], text=True).strip()
            # --rpc-url before --create: cast treats words after the initcode as constructor args.
            r = subprocess.run([CAST, "call", "--rpc-url", fork.url, "--from", deployer, "--create", code["FalconQcBridgeV3"] + bad[2:]], text=True, capture_output=True, timeout=120)
            check(r.returncode != 0 and reason in (r.stdout + r.stderr), f"ctor {argv} not refused with {reason!r}: {(r.stdout + r.stderr)[-300:]}")

        base = [USDC, GROTH16, V2, V1, DESTLOCK, str(MAX_JUMP)]
        ctor_refused("checks", *base, GROTH16, anchor_h, anchor_set)
        ev["wrong_checks_refused"] = True
        # anchorHeight must equal V2 fplTip; the anchor set must hash to V2's anchor root.
        ctor_refused("tip", *base, checks, str(int(anchor_h) + 1), anchor_set)
        ctor_refused("set", *base, checks, anchor_h, "0x0000000000000000000000000000000000000000")
        ctor_refused("set", *base, checks, anchor_h, checks)
        ev["wrong_anchor_refused"] = True
        check(int(fork.call(v3, "anchorHeight()(uint64)").split()[0]) == int(anchor_h), "anchorHeight")
        check(fork.call(v3, "anchorSet()(address)").lower() == anchor_set.lower(), "anchorSet")
        check(int(fork.call(v3, "anchorLeafCount()(uint32)").split()[0]) == ANCHOR_LEAF_COUNT, "anchorLeafCount")
        a = plus1["anchor"]
        check(int(fork.call(v3, "fplTip()(uint64)").split()[0]) == a["tip"], "fplTip")
        check(fork.call(v3, "currentCommitteeRoot()(bytes32)") == a["committeeRoot"], "committee root")
        leaves = [fork.call(v3, "currentLeaves(uint256)(bytes32)", str(i)) for i in range(6)]
        check(leaves == a["leaves"], f"leaves {leaves}")
        check(int(fork.call(v3, "maxJump()(uint64)").split()[0]) == MAX_JUMP, "maxJump")
        check(int(fork.call(v3, "fplNetworkId()(uint32)").split()[0]) == 2300, "net")
        for target in (v3, checks):
            for sig in ("owner()", "guardian()", "pause()", "rescue()"):
                fork.revert_reason(target, sig)
        ev["no_owner_guardian_pause_rescue"] = True

    s1()
    v3 = ctx["v3"]

    # ---- S2 prover: real proofs through the prover's own send path.
    os.environ["ETH_RPC"] = fork.url
    os.environ["CAST"] = CAST
    prov = load_module("v3_prover_dry", HERE / "pl-falcon-qc-v3-prover.py")
    prov.ETH_RPC_FALLBACKS = []  # the fork only, never a public RPC

    def prover_send(height: int, meta_h: dict, idx: list[int], proofs: list[dict], gas_limit: int = 0, must_succeed: bool = True) -> str:
        # gas_limit 0 = the prover's default path (estimate from the signer's address x1.25).
        gas_limit = gas_limit or prov.GAS_LIMIT_DEFAULT
        key1 = subprocess.check_output([CAST, "wallet", "private-key", ANVIL_MNEMONIC, "1"], text=True).strip()
        os.environ[prov.KEY_ENV] = key1
        del key1
        key = prov.take_prover_key()
        check(prov.KEY_ENV not in os.environ, "prover key left in env")
        quote = prov.parse_gas_quote(fork.cast("gas-price"))
        gp = prov.gas_price_wei(quote)
        with prov.MemfdSigner(key) as signer:
            if gas_limit == 0:
                sender = prov.signer_address(signer.argv(), signer.fds)
                call_args = prov.submit_command(v3, height, meta_h, idx, proofs, 1, gp, fork.url, None, send=False)[3:]
                gas_limit = prov.resolve_gas_limit(0, v3, call_args, fork.url, sender)
                ctx["last_auto_gas_limit"] = gas_limit
            cmd = prov.submit_command(v3, height, meta_h, idx, proofs, gas_limit, gp, fork.url, signer.argv(), send=True)
            check(key[2:] not in " ".join(cmd).lower(), "key in argv")
            # Same guard as Fork.send(): this script's local anvil only, never 0xDb52, and the
            # prover's argv must name no RPC other than the fork.
            fork._guard_send(prover_addr)
            urls = [cmd[i + 1] for i, a in enumerate(cmd[:-1]) if a == "--rpc-url"]
            check(urls == [fork.url], f"prover send RPC {urls} is not the local fork")
            tx = prov.emit(cmd, gas_limit, gp, send=True, key=key, pass_fds=signer.fds)
        rec = json.loads(fork.cast("receipt", tx, "--json"))
        if not must_succeed:
            return tx
        check(int(str(rec["status"]), 0) == 1, f"submit {height} reverted (gasUsed {int(str(rec['gasUsed']), 0)} of {gas_limit})")
        check(rec["from"].lower() == prover_addr, "prover tx not from dev account #1")
        return tx

    def meta_of(d: dict) -> dict:
        return {
            "header": d["hash"], "parent": d["parent"], "inner": d["innerHash"], "claim": d["claimRoot"],
            "next_root": d["nextCommitteeRoot"], "next_leaves": d["nextLeaves"],
        }

    @scenario("S2", "Prover --send of header 353131 with 4 real Groth16 proofs, real verifier", ["Groth16Verifier 0x2Cb7..4F80", "plus1-submit.json proofs", "prover MemfdSigner + cast send", "FPL_V3_PROVER_KEY env only"], [])
    def s2(ev):
        tip = prov.eth_call_uint(v3, "fplTip()(uint64)", [fork.url])
        mj = prov.eth_call_uint(v3, "maxJump()(uint64)", [fork.url])
        onchain = prov.load_onchain_leaves(v3, [fork.url])
        check([prov.b32(x) for x in plus1["anchor"]["leaves"]] == onchain, "prover read wrong leaves")
        target = prov.clamp_jump_target(tip, plus1["height"], mj, plus1["height"], True)
        check(target == plus1["height"], f"jump target {target}")
        m = meta_of(plus1)
        dry = prov.submit_command(v3, target, m, plus1["idx"], plus1["proofs"], 1, 1, fork.url, None, send=False)
        calldata = subprocess.check_output(dry, text=True).strip()
        ev["dry_run_calldata_bytes"] = len(calldata) // 2 - 1
        # Negative first: tampered proof and tampered next leaves fail in the real verifier.
        bad = json.loads(json.dumps(plus1["proofs"]))
        bad[0]["a"][0] = hex(int(bad[0]["a"][0], 16) ^ 1)
        args_bad = prov.submit_command(v3, target, m, plus1["idx"], bad, 1, 1, fork.url, None, send=False)[3:]
        ev["tampered_proof"] = fork.revert_reason(v3, prov.SUBMIT_ABI, *args_bad)
        m2 = dict(m, next_leaves=list(m["next_leaves"]))
        m2["next_leaves"][0] = "0x" + "11" * 32
        args_bad2 = prov.submit_command(v3, target, m2, plus1["idx"], plus1["proofs"], 1, 1, fork.url, None, send=False)[3:]
        ev["tampered_leaves"] = fork.revert_reason(v3, prov.SUBMIT_ABI, *args_bad2)
        # A flipped coordinate is off-curve, so the verifier's pairing check itself reverts ("pair").
        check(ev["tampered_proof"] in ("proof", "pair"), f"tampered proof: {ev['tampered_proof']}")
        check(ev["tampered_leaves"] == "proof", f"tampered leaves: {ev['tampered_leaves']}")
        # Gas the real 4-proof submit needs, from the fork.
        need = int(fork.cast("estimate", "--from", prover_addr, v3, prov.SUBMIT_ABI, *dry[3:]).split()[0])
        ev["estimated_gas"] = need
        ev["prover_default_gas_limit"] = prov.GAS_LIMIT_DEFAULT
        # Send exactly as the prover does by default (--gas-limit 0: estimate x1.25).
        tx0 = prover_send(target, m, plus1["idx"], plus1["proofs"], must_succeed=False)
        r0 = json.loads(fork.cast("receipt", tx0, "--json"))
        auto = ctx.get("last_auto_gas_limit", 0)
        ev["default_limit_send"] = {"tx": tx0, "status": int(str(r0["status"]), 0), "gasUsed": int(str(r0["gasUsed"]), 0), "gas_limit": auto}
        gas_limit = auto
        tx = tx0
        if int(str(r0["status"]), 0) != 1:
            # F2: the prover's default limit cannot carry a real submit. Retry with headroom.
            ctx["prover_gas_bug"] = {"need": need, "default": auto}
            gas_limit = (need * 12 // 10 + 9_999) // 10_000 * 10_000
            tx = prover_send(target, m, plus1["idx"], plus1["proofs"], gas_limit=gas_limit)
        else:
            check(r0["from"].lower() == prover_addr, "prover tx not from dev account #1")
        ctx["prover_default_send"] = ev["default_limit_send"]
        ev["gas_limit_used"] = gas_limit
        ev["submit_tx"] = tx
        ev["submit_gas"] = int(json.loads(fork.cast("receipt", tx, "--json"))["gasUsed"], 0)
        check(int(fork.call(v3, "fplTip()(uint64)").split()[0]) == plus1["height"], "fplTip not moved")
        check(fork.call(v3, "currentCommitteeRoot()(bytes32)") == plus1["nextCommitteeRoot"], "committee not rotated")
        ev["replay_same_header"] = fork.revert_reason(v3, prov.SUBMIT_ABI, *dry[3:])
        check(ev["replay_same_header"] == "range", f"replay: {ev['replay_same_header']}")

    s2()

    # ---- S3 deposits on the fork.
    am_names = {"mint_dest": "sally", "unknown_dest": "v3dry-nobody"}

    def dest20(name: str) -> str:
        # Same as btc_spv::dest_id20 and pl-eth-auto-mint.py dest20().
        return "0x" + sha256(name.strip().lower().encode())[:20].hex()

    @scenario("S3", "Deposits: ETH + USDC to a known name (mint) and to an unknown name (refund later); fundPool", ["V3 depositEth/depositUsdc/fundPool", "Sepolia USDC FiatToken"], ["dev-account USDC balances via anvil_setStorageAt"])
    def s3(ev):
        # USDC for alice/bob/deployer: find the FiatToken balance slot on the fork.
        slot = None
        for cand in range(0, 20):
            key = subprocess.check_output([CAST, "index", "address", alice, str(cand)], text=True).strip()
            prev = fork.rpc("eth_getStorageAt", [USDC, key, "latest"])
            fork.rpc("anvil_setStorageAt", [USDC, key, h32(hex(10**12))])
            if int(fork.call(USDC, "balanceOf(address)(uint256)", alice).split()[0]) == 10**12:
                slot = cand
                break
            fork.rpc("anvil_setStorageAt", [USDC, key, h32(prev)])
        check(slot is not None, "USDC balance slot not found")
        ev["usdc_balance_slot"] = slot
        for who in (bob, deployer):
            key = subprocess.check_output([CAST, "index", "address", who, str(slot)], text=True).strip()
            fork.rpc("anvil_setStorageAt", [USDC, key, h32(hex(10**12))])
        for who in (alice, bob, deployer):
            fork.send(who, USDC, "approve(address,uint256)", v3, str(10**12))
        d_known, d_unknown = dest20(am_names["mint_dest"]), dest20(am_names["unknown_dest"])
        plan = [
            ("D1", alice, "ETH", 10**16, d_known, "mint"),
            ("D2", alice, "USDC", 5_000_000, d_known, "mint"),
            ("D3", bob, "ETH", 2 * 10**16, d_unknown, "refund"),
            ("D4", bob, "USDC", 7_000_000, d_unknown, "refund"),
        ]
        chain = int(fork.cast("chain-id"))
        deps = {}
        for i, (tag, who, asset, amt, d20, expect) in enumerate(plan):
            if asset == "ETH":
                rec = fork.send(who, v3, "depositEth(bytes20)", d20, value=amt)
            else:
                rec = fork.send(who, v3, "depositUsdc(bytes20,uint256)", d20, str(amt))
            logs = [lg for lg in rec["logs"] if lg["address"].lower() == v3.lower()]
            check(len(logs) == 1, f"{tag}: logs {logs}")
            lg = logs[0]
            topic0 = "0x" + subprocess.check_output([CAST, "keccak", DEPOSIT_SIG], text=True).strip().removeprefix("0x")
            check(lg["topics"][0] == topic0, f"{tag} topic0")
            want_id = subprocess.check_output([CAST, "keccak", subprocess.check_output([CAST, "abi-encode", "f(uint256,address,uint256)", str(chain), v3, str(i)], text=True).strip()], text=True).strip()
            check(lg["topics"][1] == want_id, f"{tag} depositId {lg['topics'][1]} != {want_id}")
            check(lg["topics"][2] == h32(who), f"{tag} sender topic")
            peg = fork.call(v3, PEGIN_SIG, want_id).split("\n")
            check(peg[0].lower() == who and peg[5] == "true" and peg[6] == "false", f"{tag} pegIn {peg}")
            deps[tag] = {
                "tag": tag, "sender": who, "asset": asset, "amount": amt, "dest20": d20, "expect": expect,
                "deposit_id": want_id, "tx": rec["transactionHash"], "block": int(str(rec["blockNumber"]), 0),
                "refund_note": fork.call(v3, "refundNoteId(bytes32)(bytes32)", want_id),
                "mint_note": fork.call(v3, "mintNoteId(bytes32)(bytes32)", want_id),
            }
        fp = fork.send(deployer, v3, "fundPool(uint256)", str(20_000_000), value=10**17)
        ctx["fund_pool_tx"] = fp["transactionHash"]
        ctx["fund_pool_block"] = int(str(fp["blockNumber"]), 0)
        ev["fundPool_logs"] = len(fp["logs"])
        ev["zero_deposit"] = fork.revert_reason(v3, "depositEth(bytes20)", d_known, sender=alice)
        ev["zero_dest"] = fork.revert_reason(v3, "depositEth(bytes20)", "0x" + "00" * 20, sender=alice, value=1)
        check(ev["zero_deposit"] == "zero" and ev["zero_dest"] == "dest", "deposit guards")
        ev["pools"] = {"eth": fork.call(v3, "ethPool()(uint256)").split()[0], "usdc": fork.call(v3, "usdcPool()(uint256)").split()[0]}
        ev["deposits"] = deps
        ctx["deps"] = deps

    s3()
    deps = ctx["deps"]

    # Confirmations: auto-mint's cursor stays 96 below latest and at/below the LC head.
    head_a = fork.mine(110)
    ctx["head_a"] = head_a

    # ---- auto-mint in-process, FPL ctl mocked.
    run = out / "automint"
    run.mkdir(exist_ok=True)
    mock_state = run / "mock-ctl.json"
    mock_ctl = HERE / "v3-dry-run-mock-ctl.py"
    env = {
        "PL_ROOT": str(run), "FPL_CTL": str(mock_ctl), "FPL_V3_DRY_MOCK_STATE": str(mock_state),
        "ETH_RPC": fork.url, "CAST": CAST, "MINT_JOBS": str(run / "jobs.json"), "MINT_LOCK": str(run / "jobs.lock"),
        "MINT_RUNLOCK": str(run / "run.lock"), "FALCON_PL_NAME_STORE": str(run / "names.json"),
        "FPL_V3_SCAN_FROM": str(ctx["deploy_block"]), "FPL_V3_SEQ_WAIT_SEC": "0.05", "FPL_V3_RETRY_SEC": "1",
        "FPL_WATCHER_ACCOUNT": "watcher-eth",
    }
    os.environ.update(env)

    def mock_status(head: int) -> dict:
        blk = fork.block(head)
        # A real LC's finalized slot is the beacon slot of its execution head, so
        # genesis + 12 * slot is that head's time. Keep the mock consistent.
        slot = (int(blk["timestamp"], 16) - 1_655_733_600) // 12
        return {
            "tip_height": 10, "eth_v3_from_height": 1, "eth_v3_bridge": v3.lower(), "eth_v3_expiry_secs": E_SECS,
            "eth_exec_window": 50_400,
            "eth_lc": {
                "execution_number": head, "execution_block_hash": blk["hash"][2:], "finalized_slot": slot,
                "eth_exec_floor": ctx["deploy_block"] - 1, "eth_exec_tip": head, "period": 3,
            },
        }

    def set_mock(**kw) -> None:
        st = json.loads(mock_state.read_text()) if mock_state.exists() else {}
        st.update(kw)
        mock_state.write_text(json.dumps(st, indent=2))

    def get_mock() -> dict:
        return json.loads(mock_state.read_text())

    set_mock(status=mock_status(head_a), amounts={d["deposit_id"][2:]: d["amount"] for d in deps.values()}, phase="pre")
    am = load_module("am_dry", HERE / "pl-eth-auto-mint.py")
    rel = am._relayer()
    lc_head = {"n": head_a}

    def fake_cmd_update(*, soft: bool = False) -> dict:
        # MOCK: the real cmd_update walks the beacon light client (Lodestar).
        # Anvil blocks have no beacon chain, so report the mock LC head.
        return mock_status(lc_head["n"])["eth_lc"]

    rel.cmd_update = fake_cmd_update

    def automint_pass() -> list[str]:
        with am.FileLock(am.LOCK):
            st = am.load_state()
            am.scan_v3_logs(st, v3)
        done = []
        for _ in range(20):
            ready, why = am.v3_submit_ready(am.ctl_status(), v3)
            check(ready, f"v3 not ready: {why}")
            key = am.next_queued(skip_v3=False)
            if not key:
                break
            job = am.process_job(key, v3_bridge=v3)
            check(job.get("status") == "done", f"job {key} {job.get('status')} {job.get('error')}")
            done.append(key)
        return done

    @scenario("S4", "auto-mint --v3 inside E: queues and submits the 2 mints, keeps the 2 unknown-dest deposits pending, ignores fundPool", ["pl-eth-auto-mint.py scan/plan/submit", "relayer receipt MPT from fork blocks"], ["FPL node (mock ctl records submits)", "relayer cmd_update (LC step)"])
    def s4(ev):
        done = automint_pass()
        st = am.load_state()
        subs = get_mock().get("submissions", [])
        ev["jobs_done"] = done
        ev["pending"] = sorted(st.get("v3_pending", {}).keys())
        ev["submissions"] = [{k: s[k] for k in ("deposit_id", "asset", "credit_to", "height", "merkle_index")} for s in subs]
        want_mint = {deps["D1"]["deposit_id"][2:], deps["D2"]["deposit_id"][2:]}
        check({s["deposit_id"] for s in subs} == want_mint, f"mint submissions {subs}")
        check(all(s["credit_to"] == "sally" for s in subs), "credit_to")
        check(set(ev["pending"]) == {deps["D3"]["deposit_id"][2:], deps["D4"]["deposit_id"][2:]}, "pending")
        # Each recorded receipt proof reproduces the block's receiptsRoot (relayer MPT, fork block).
        for s in subs:
            blk = fork.block(s["height"])
            check(s["block_hash"].removeprefix("0x") == blk["hash"][2:], "block hash")
        # The one-shot path refuses a fundPool-only tx.
        old = sys.argv
        sys.argv = ["pl-eth-auto-mint.py", "--v3", "--txid", ctx["fund_pool_tx"], "--v3-bridge", v3]
        try:
            am.main()
            raise AssertionError("fundPool tx was accepted")
        except SystemExit as e:
            ev["fund_pool_once"] = str(e.code)
            check("no V3 Deposit" in str(e.code), f"fundPool: {e.code}")
        finally:
            sys.argv = old
        check(len(get_mock().get("submissions", [])) == 2, "fundPool produced a submit")

    s4()

    # ---- S5 time warp past E, refunds.
    fork.rpc("evm_increaseTime", [E_SECS + 3600])
    head_b = fork.mine(110)
    ctx["head_b"] = head_b
    lc_head["n"] = head_b
    set_mock(status=mock_status(head_b), phase="post")

    @scenario("S5", "Warp past E = 3 days (anvil evm_increaseTime): auto-mint re-plans the pending deposits as refunds; replays are no-ops", ["pl-eth-auto-mint.py replan + submit", "fork block timestamps"], ["FPL node (mock ctl)", "relayer cmd_update"])
    def s5(ev):
        ta = int(fork.block(head_a)["timestamp"], 16)
        tb = int(fork.block(head_b)["timestamp"], 16)
        ev["lc_head_time_pre"], ev["lc_head_time_post"] = ta, tb
        for d in deps.values():
            blk = fork.block(d["block"])
            d["timestamp"] = int(blk["timestamp"], 16)
        check(all(tb > d["timestamp"] + E_SECS for d in deps.values()), "warp did not pass E")
        check(all(ta <= d["timestamp"] + E_SECS for d in deps.values()), "pre head already past E")
        done = automint_pass()
        subs = get_mock().get("submissions", [])
        ev["jobs_done"] = done
        refunds = [s for s in subs if s["phase"] == "post"]
        ev["refund_submissions"] = [{k: s[k] for k in ("deposit_id", "asset", "credit_to")} for s in refunds]
        check({s["deposit_id"] for s in refunds} == {deps["D3"]["deposit_id"][2:], deps["D4"]["deposit_id"][2:]}, "refunds")
        check(all(s["credit_to"] == "" for s in refunds), "refund credit_to must be empty")
        check(not am.load_state().get("v3_pending"), "pending not drained")
        # Replay: the one-shot path on an already-minted deposit submits nothing.
        before = len(subs)
        old = sys.argv
        sys.argv = ["pl-eth-auto-mint.py", "--v3", "--txid", deps["D1"]["tx"], "--v3-bridge", v3]
        try:
            rc = am.main()
        finally:
            sys.argv = old
        ev["replay_once_rc"] = rc
        check(len(get_mock().get("submissions", [])) == before, "replay submitted again")
        # A forced re-submit of a resolved id is refused at (mock) admission and auto-mint treats it as resolved.
        d1 = next(s for s in subs if s["deposit_id"] == deps["D1"]["deposit_id"][2:])
        got = am.submit_v3_until_resolved(["submit-eth-v3-deposit", *d1["argv"]], "replay-d1", attempts=1)
        ev["forced_resubmit"] = got
        check("already resolved" in got, "forced resubmit")

    s5()
    if args.skip_rust:
        return

    # ---- S6 FPL consensus (Rust) on the fork's real headers and receipts.
    @scenario("S6", "FPL consensus (Rust, 3 nodes) applies the recorded submits on real fork headers: 2 mints inside E, 2 refunds after E, replay + fundPool refused", ["fd-pl consensus apply/pack/admission", "fork exec headers via EthExecHeaders tx", "fork receipts + MPT"], ["ETH light client (eth_lc set to the fork head)", "in-process Network harness instead of the 2300 seats"])
    def s6(ev):
        subs = get_mock()["submissions"]
        lo = min(d["block"] for d in deps.values())
        lc = {}
        for phase, a, b in (("pre", lo, head_a), ("post", head_a + 1, head_b)):
            hdrs = [rel.fetch_header(n) for n in range(a, b + 1)]
            lc[phase] = {"number": b, "hash": hdrs[-1]["hash"], "receipts_root": hdrs[-1]["receipts_root"], "headers": hdrs}
        by_id = {d["deposit_id"][2:]: d for d in deps.values()}
        case_subs = []
        for s in subs:
            d = by_id[s["deposit_id"]]
            case_subs.append({**{k: s[k] for k in ("phase", "asset", "deposit_id", "credit_to", "external_txid", "block_hash", "height", "raw_tx", "merkle_path", "merkle_index")}, "expect": d["expect"], "amount": d["amount"]})
        # Inside E, the pending (unknown dest) deposits submitted with empty credit_to must not resolve.
        probes = []
        for tag in ("D3", "D4"):
            d = deps[tag]
            post = next(c for c in case_subs if c["deposit_id"] == d["deposit_id"][2:])
            probes.append({**post, "phase": "pre"})
        # fundPool receipt with the relayer's MPT builder.
        blk = fork.rpc("eth_getBlockByNumber", [hex(ctx["fund_pool_block"]), False])
        recs = rel.block_receipts(hex(ctx["fund_pool_block"]), blk["transactions"])
        idx = next(i for i, r in enumerate(recs) if r["transactionHash"].lower() == ctx["fund_pool_tx"].lower())
        root, raw_hex, nodes, _ = rel.build_receipt_proof(recs, idx)
        check(root == blk["receiptsRoot"][2:], "fundPool receipts root")
        fp = {"deposit_id": "ab" * 32, "external_txid": ctx["fund_pool_tx"][2:], "block_hash": blk["hash"][2:], "height": ctx["fund_pool_block"], "raw_tx": raw_hex, "merkle_path": nodes, "merkle_index": idx}
        case = {"bridge": v3, "lc": lc, "submissions": case_subs, "unresolved_probes": probes, "fund_pool": fp}
        cpath = out / "fork-case.json"
        rpath = out / "fork-result.json"
        cpath.write_text(json.dumps(case))
        rpath.unlink(missing_ok=True)
        env2 = dict(os.environ)
        env2.pop("RUSTFLAGS", None)
        env2.update({"FPL_V3_FORK_CASE": str(cpath), "FPL_V3_FORK_RESULT": str(rpath)})
        r = subprocess.run(
            ["cargo", "test", "-p", "fd-pl", "--lib", "eth_v3_fork_dry_run_tests::v3_fork_dry_run_case", "--", "--ignored", "--nocapture"],
            cwd=WORKSPACE, env=env2, text=True, capture_output=True, timeout=3600,
        )
        (out / "cargo-fork-case.log").write_text(r.stdout + r.stderr)
        check(r.returncode == 0 and rpath.exists(), "rust replay failed, see cargo-fork-case.log: " + (r.stdout + r.stderr)[-1500:])
        res = json.loads(rpath.read_text())
        ctx["rust"] = res
        ev["nodes"] = res["nodes"]
        ev["tip_height"] = res["tip_height"]
        ev["balances"] = res["balances"]
        ev["total_minted"] = res["total_minted"]
        ev["claim_root"] = res["claim_root"]
        ev["refunds"] = [{k: x[k] for k in ("asset", "amount", "refund_to", "note_id")} for x in res["refunds"]]
        ev["replays_refused"] = len(res["replays"])
        ev["fund_pool_err"] = res["fund_pool_err"]
        ev["log"] = res["log"]
        check(res["balances"]["sally"] == {"ETH": deps["D1"]["amount"], "USDC": deps["D2"]["amount"]}, "sally balances")
        check(len(res["refunds"]) == 2, "two refunds")
        for x in res["refunds"]:
            d = by_id[x["deposit_id"].removeprefix("0x")] if x["deposit_id"].removeprefix("0x") in by_id else None
            check(d is not None, f"refund id {x['deposit_id']}")
            check(x["refund_to"].removeprefix("0x") == d["sender"][2:], "refund_to")
            check("0x" + x["note_id"] == d["refund_note"], f"refund note {x['note_id']} vs contract {d['refund_note']}")
            leaf = fork.call(v3, "withdrawLeaf(bytes32,address,uint256,bool)(bytes32)", d["refund_note"], d["sender"], str(d["amount"]), "true" if d["asset"] == "USDC" else "false")
            check(leaf == "0x" + x["leaf"], "refund leaf != contract withdrawLeaf")

    s6()

    # ---- S7 claim path.
    claim_tool = HERE / "pl-eth-claim-proof.py"

    def claim_proof(status: dict, *cli: str) -> tuple[int, str]:
        set_mock(claim_status=status)
        env3 = dict(os.environ, FPL_V3_DRY_MOCK_STATUS="claim")
        r = subprocess.run([sys.executable, str(claim_tool), *cli], text=True, capture_output=True, env=env3, timeout=120)
        return r.returncode, (r.stdout + r.stderr).strip()

    def open_and_take(ev: dict, tag: str, note: str, dest: str, amount: int, usdc: bool, proof: list[str], index: int, height: int, impersonate: bool = False):
        proof_arg = "[" + ",".join(proof) + "]"
        args_ = (note, dest, str(amount), "true" if usdc else "false", proof_arg, str(index), str(height))
        rec_open = fork.send(claimer, v3, CLAIM_SIG, *args_)
        g = int(str(rec_open["gasUsed"]), 0)
        ev[f"{tag}_openClaim_gas"] = g
        ev[f"{tag}_proof_len"] = len(proof)
        OPENCLAIM_GAS[tag] = g  # checked against walletd's limit in F4
        ev[f"{tag}_reopen"] = fork.revert_reason(v3, CLAIM_SIG, *args_, sender=claimer)
        check(ev[f"{tag}_reopen"] == "exists", f"{tag} reopen: {ev[f'{tag}_reopen']}")
        ev[f"{tag}_take_by_other"] = fork.revert_reason(v3, "take(bytes32)", note, sender=claimer)
        check(ev[f"{tag}_take_by_other"] == "dest", "take by non-dest")
        if impersonate:
            fork.rpc("anvil_impersonateAccount", [dest])
            fork.rpc("anvil_setBalance", [dest, hex(10**18)])
        if usdc:
            b0 = int(fork.call(USDC, "balanceOf(address)(uint256)", dest).split()[0])
        else:
            b0 = int(fork.rpc("eth_getBalance", [dest, "latest"]), 16)
        rec = fork.send(dest, v3, "take(bytes32)", note)
        ev[f"{tag}_take_gas"] = int(str(rec["gasUsed"]), 0)
        if usdc:
            b1 = int(fork.call(USDC, "balanceOf(address)(uint256)", dest).split()[0])
            check(b1 - b0 == amount, f"{tag} USDC paid {b1 - b0}")
        else:
            b1 = int(fork.rpc("eth_getBalance", [dest, "latest"]), 16)
            gas = int(str(rec["gasUsed"]), 0) * int(str(rec["effectiveGasPrice"]), 0)
            check(b1 - b0 + gas == amount, f"{tag} ETH paid {b1 - b0 + gas} (b0={b0} b1={b1} gas={gas} rec={ {k: rec.get(k) for k in ('from', 'to', 'gasUsed', 'effectiveGasPrice', 'blockNumber')} })")
        if impersonate:
            fork.rpc("anvil_stopImpersonatingAccount", [dest])
        ev[f"{tag}_take_again"] = fork.revert_reason(v3, "take(bytes32)", note, sender=dest)
        check(ev[f"{tag}_take_again"] == "state", "take twice")
        ev[f"{tag}_paid"] = amount

    @scenario("S7a", "V2-committed notes are refused on V3: real note vs real-proof header 353131 and vs the anchor -> 'anchor'; padded copy of leaf 8 -> 'pad'", ["header 353131 (real proofs)", "committed V2-era claim leaves (fixture from #33)", "pl-eth-claim-proof.py --header-claim-root", "V3 anchorHeight / anchorLeafCount"], ["FPL node status (fixture served by mock ctl)"])
    def s7a(ev):
        fx = json.loads(FIX_CLAIM.read_text())
        root = fork.call(v3, "headers(uint64)(bytes32,bytes32,bytes32,bytes32,bytes32,bool)", str(plus1["height"])).split()[3]
        check(root == fx["header"]["claimRoot"], "353131 does not carry the V2 anchor root")
        note = None
        for w in next(r for r in fx["status"]["rails"] if r["asset"] == "ETH")["withdrawals"]:
            if w["external_to"].lower() != OLD_DEPLOYER:
                note = w
                break
        check(note is not None, "no non-deployer ETH note in fixture")
        rc, txt = claim_proof(fx["status"], "--note", note["note_id"], "--asset", "ETH", "--fpl-height", str(plus1["height"]), "--header-claim-root", root)
        check(rc == 0, f"claim tool: {txt}")
        rec = json.loads(txt.splitlines()[-1])
        ev["claim_tool"] = {k: rec.get(k) for k in ("noteId", "dest", "amount", "index", "status", "ready")}
        check(int(rec["index"]) < ANCHOR_LEAF_COUNT, f"note index {rec['index']} is not V2-committed")
        proof_arg = "[" + ",".join(rec["proof"]) + "]"
        args_ = (rec["noteId"], rec["dest"], str(rec["amount"]), "false", proof_arg, str(rec["index"]))
        ev["v2_note_vs_353131"] = fork.revert_reason(v3, CLAIM_SIG, *args_, str(plus1["height"]), sender=claimer)
        check(ev["v2_note_vs_353131"] == "anchor", ev["v2_note_vs_353131"])
        ev["v2_note_vs_anchor"] = fork.revert_reason(v3, CLAIM_SIG, *args_, str(plus1["anchor"]["tip"]), sender=claimer)
        check(ev["v2_note_vs_anchor"] == "anchor", ev["v2_note_vs_anchor"])
        # Leaf 8 (USDC, last of the 9) has a padded copy at index 9 under the same root.
        cp7 = load_module("claim_proof_s7a", claim_tool)
        lc = [bytes.fromhex(x) for x in fx["status"]["lc"]["claim_leaves"]]
        w8 = next(w for w in next(r for r in fx["status"]["rails"] if r["asset"] == "USDC")["withdrawals"]
                  if fork.call(v3, "withdrawLeaf(bytes32,address,uint256,bool)(bytes32)", "0x" + w["note_id"], w["external_to"], str(w["amount"]), "true") == "0x" + lc[8].hex())
        r9, p8 = cp7.build_tree(lc[:9], 8)
        check("0x" + r9 == root, "leaf 8 path")
        a8 = ("0x" + w8["note_id"], w8["external_to"], str(w8["amount"]), "true", "[" + ",".join("0x" + x for x in p8) + "]")
        ev["leaf8_at_8"] = fork.revert_reason(v3, CLAIM_SIG, *a8, "8", str(plus1["height"]), sender=claimer)
        ev["leaf8_padded_at_9"] = fork.revert_reason(v3, CLAIM_SIG, *a8, "9", str(plus1["height"]), sender=claimer)
        check(ev["leaf8_at_8"] == "anchor", ev["leaf8_at_8"])
        check(ev["leaf8_padded_at_9"] == "pad", ev["leaf8_padded_at_9"])
        ctx["s7a_note"] = rec

    try:
        s7a()
    except Exception:
        pass

    # Mock verifier from here on.
    fork.rpc("anvil_setCode", [GROTH16, ALWAYS_TRUE])
    dummy = [{"a": ["0x1", "0x2"], "b": [["0x3", "0x4"], ["0x5", "0x6"]], "c": ["0x7", "0x8"]}] * 4

    def mock_header(height: int, claim_root: str, tag: str) -> dict:
        tip = int(fork.call(v3, "fplTip()(uint64)").split()[0])
        parent = fork.call(v3, "headers(uint64)(bytes32,bytes32,bytes32,bytes32,bytes32,bool)", str(tip)).split()[0]
        nr = fork.call(v3, "currentCommitteeRoot()(bytes32)")
        leaves = [fork.call(v3, "currentLeaves(uint256)(bytes32)", str(i)) for i in range(6)]
        inner = "0x" + sha256(f"v3-fork-dry-run mock inner {height} {tag}".encode()).hex()
        cr = h32(claim_root)
        hsh = "0x" + sha256(SEAL_PREFIX + bytes.fromhex(inner[2:]) + bytes.fromhex(nr[2:]) + bytes.fromhex(cr[2:])).hex()
        m = {"header": hsh, "parent": parent, "inner": inner, "claim": cr, "next_root": nr, "next_leaves": leaves}
        prover_send(height, m, [0, 1, 2, 3], dummy)
        check(int(fork.call(v3, "fplTip()(uint64)").split()[0]) == height, "mock header not accepted")
        return m

    def rail_leaves(status: dict, asset: str) -> list[bytes]:
        r = next((r for r in status["rails"] if r["asset"] == asset), None)
        return [bytes.fromhex(x) for x in (r or {}).get("claim_leaves", [])]

    def fpl_order(extra_eth: list[bytes], extra_usdc: list[bytes]) -> list[bytes]:
        # FPL's header tree (lc::evm_claim_leaves) is per rail: every ETH leaf, then every USDC
        # leaf, each rail append-only. New leaves join the real #33 rails, so a new ETH leaf
        # shifts every existing USDC leaf (including the V2-anchor ones) up by one.
        fx_status = json.loads(FIX_CLAIM.read_text())["status"]
        return rail_leaves(fx_status, "ETH") + extra_eth + rail_leaves(fx_status, "USDC") + extra_usdc

    @scenario("S7b", "Refund notes: FPL claim root in a V3 header, openClaim + take pays each sender once (ETH and USDC)", ["refund leaves + claim root from the Rust consensus run", "V3 openClaim/take", "prover send path"], ["Groth16 verifier (always-true code at 0x2Cb7..4F80) for the header carrying the fork refund root"])
    def s7b(ev):
        res = ctx["rust"]
        h = plus1["height"] + 1
        cp = load_module("claim_proof_dry", claim_tool)
        rust_leaves = [bytes.fromhex(l) for l in res["claim_leaves"]]
        r_eth, r_usdc = rail_leaves(res["status"], "ETH"), rail_leaves(res["status"], "USDC")
        check(r_eth + r_usdc == rust_leaves, "Rust rails do not make up its claim leaves")
        cum = fpl_order(r_eth, r_usdc)
        cum_root, _ = cp.build_tree(cum, 0)
        mock_header(h, cum_root, "refund-root")
        ev["header"] = h
        ev["header_claim_root"] = "0x" + cum_root
        ev["tree"] = f"real ETH rail + {len(r_eth)} Rust ETH leaves, then real USDC rail + {len(r_usdc)} Rust USDC leaves (FPL per-rail order)"
        # Copilot on #45: the ETH append moves V2-anchor USDC leaf 8 to index 9 (>= 9 anchor leaves).
        # It must still be refused ("anchor"): V3 checks set membership, not an index cutoff.
        fx = json.loads(FIX_CLAIM.read_text())
        lc = fx["status"]["lc"]["claim_leaves"]
        l8 = bytes.fromhex(lc[ANCHOR_LEAF_COUNT - 1])
        i8 = cum.index(l8)
        ev["anchor_leaf8_new_index"] = i8
        check(i8 >= ANCHOR_LEAF_COUNT, f"leaf 8 did not shift ({i8})")
        w8 = next(w for w in next(r for r in fx["status"]["rails"] if r["asset"] == "USDC")["withdrawals"]
                  if fork.call(v3, "withdrawLeaf(bytes32,address,uint256,bool)(bytes32)", "0x" + w["note_id"], w["external_to"], str(w["amount"]), "true") == "0x" + lc[ANCHOR_LEAF_COUNT - 1])
        _, p8 = cp.build_tree(cum, i8)
        ev["anchor_leaf8_shifted"] = fork.revert_reason(v3, CLAIM_SIG, "0x" + w8["note_id"], w8["external_to"], str(w8["amount"]), "true", "[" + ",".join("0x" + x for x in p8) + "]", str(i8), str(h), sender=claimer)
        check(ev["anchor_leaf8_shifted"] == "anchor", ev["anchor_leaf8_shifted"])
        # Note 9 (not V2-committed) also shifted (index 10 here) and is still provable there.
        i9 = cum.index(bytes.fromhex(lc[ANCHOR_LEAF_COUNT]))
        _, p9 = cp.build_tree(cum, i9)
        ev["note9_index_in_this_tree"] = i9
        check(cp.merkle_verify(cum[i9], [bytes.fromhex(x) for x in p9], i9, bytes.fromhex(cum_root)), "note 9 path")
        # Does the shipped claim tool find a refund note in a real node status?
        x0 = res["refunds"][0]
        rc, txt = claim_proof(res["status"], "--note", x0["note_id"], "--asset", x0["asset"], "--fpl-height", str(h), "--header-claim-root", res["claim_root"])
        ev["claim_tool_on_refund_note"] = {"rc": rc, "out": txt[-300:]}
        ctx["claim_tool_refund_rc"] = rc
        for x in res["refunds"]:
            root, path = cp.build_tree(rust_leaves, int(x["index"]))
            check(root == res["claim_root"] and path == x["path"], "claim-proof build_tree disagrees with Rust")
            d = next(d for d in deps.values() if d["deposit_id"][2:] == x["deposit_id"].removeprefix("0x"))
            ci = cum.index(bytes.fromhex(x["leaf"]))
            _, cpath = cp.build_tree(cum, ci)
            ev[f"{d['tag']}_index"] = ci
            open_and_take(ev, d["tag"], "0x" + x["note_id"], d["sender"], d["amount"], d["asset"] == "USDC", ["0x" + p for p in cpath], ci, h)
            peg = fork.call(v3, PEGIN_SIG, d["deposit_id"]).split("\n")
            check(peg[6] == "true", f"{d['tag']} not resolved on-chain")
        ev["pools_after"] = {"eth": fork.call(v3, "ethPool()(uint256)").split()[0], "usdc": fork.call(v3, "usdcPool()(uint256)").split()[0]}

    try:
        s7b()
    except Exception:
        pass

    @scenario("S7c", "Scott's note 9 (leaf index 9, not in V2 header 353130) opens and pays on V3 against a header carrying the real 10-leaf FPL claim root", ["real 10-leaf lc claim_root + note 9 withdrawal (fixture from #33)", "pl-eth-claim-proof.py --header-claim-root", "V3 openClaim/take"], ["Groth16 verifier (always-true) for the header carrying that root; impersonating the note's dest to call take()"])
    def s7c(ev):
        fx = json.loads(FIX_CLAIM.read_text())
        lc_root = "0x" + fx["status"]["lc"]["claim_root"]
        h = int(fork.call(v3, "fplTip()(uint64)").split()[0]) + 1
        mock_header(h, lc_root, "lc-10-leaf")
        ev["header"] = h
        lc9 = fx["status"]["lc"]["claim_leaves"][ANCHOR_LEAF_COUNT]
        w9 = next(w for w in next(r for r in fx["status"]["rails"] if r["asset"] == "USDC")["withdrawals"]
                  if fork.call(v3, "withdrawLeaf(bytes32,address,uint256,bool)(bytes32)", "0x" + w["note_id"], w["external_to"], str(w["amount"]), "true") == "0x" + lc9)
        ev["note9"] = {"note_id": w9["note_id"], "dest": w9["external_to"], "amount": w9["amount"], "asset": "USDC"}
        rc, txt = claim_proof(fx["status"], "--note", w9["note_id"], "--asset", "USDC", "--fpl-height", str(h), "--header-claim-root", lc_root)
        check(rc == 0, f"claim tool: {txt}")
        rec = json.loads(txt.splitlines()[-1])
        ev["claim_tool"] = {k: rec.get(k) for k in ("noteId", "dest", "amount", "index", "status", "ready")}
        check(int(rec["index"]) == ANCHOR_LEAF_COUNT, f"note 9 at index {rec['index']}, expected {ANCHOR_LEAF_COUNT}")
        check(rec.get("ready") is True, f"not ready: {rec}")
        # The V2 anchor root does not commit it (only the 10-leaf root does).
        check(fx["header"]["claimRoot"] != lc_root, "anchor root equals lc root")
        # The V2-committed note from S7a is still refused under this newer root.
        s7 = ctx.get("s7a_note")
        if s7:
            cp = load_module("claim_proof_s7c", claim_tool)
            lc = [bytes.fromhex(x) for x in fx["status"]["lc"]["claim_leaves"]]
            _, p = cp.build_tree(lc, int(s7["index"]))
            ev["v2_note_vs_lc_root"] = fork.revert_reason(v3, CLAIM_SIG, s7["noteId"], s7["dest"], str(s7["amount"]), "false", "[" + ",".join("0x" + x for x in p) + "]", str(s7["index"]), str(h), sender=claimer)
            check(ev["v2_note_vs_lc_root"] == "anchor", ev["v2_note_vs_lc_root"])
        fork.send(deployer, v3, "fundPool(uint256)", str(int(rec["amount"])))
        open_and_take(ev, "real", rec["noteId"], rec["dest"], int(rec["amount"]), True, rec["proof"], int(rec["index"]), h, impersonate=True)

    try:
        s7c()
    except Exception:
        pass

    @scenario("S8", "Double-resolve on-chain: mint note of a refunded deposit -> 'resolved'; refund leaf with wrong dest -> 'refund-dest'", ["V3 _consumeLinkedDeposit"], ["Groth16 verifier (always-true); hand-built claim root (FPL never emits these leaves)"])
    def s8(ev):
        cp = load_module("claim_proof_dry2", claim_tool)
        d3, d1 = deps["D3"], deps["D1"]
        l_mint = fork.call(v3, "withdrawLeaf(bytes32,address,uint256,bool)(bytes32)", d3["mint_note"], d3["sender"], str(d3["amount"]), "false")
        l_bad = fork.call(v3, "withdrawLeaf(bytes32,address,uint256,bool)(bytes32)", d1["refund_note"], claimer, str(d1["amount"]), "false")
        # Both are ETH leaves: FPL order puts them after the real ETH rail, before the USDC rail.
        e = [bytes.fromhex(l_mint[2:]), bytes.fromhex(l_bad[2:])]
        leaves = fpl_order(e, [])
        i0, i1 = leaves.index(e[0]), leaves.index(e[1])
        root, _ = cp.build_tree(leaves, i0)
        h = int(fork.call(v3, "fplTip()(uint64)").split()[0]) + 1
        mock_header(h, root, "double-resolve")
        _, p0 = cp.build_tree(leaves, i0)
        _, p1 = cp.build_tree(leaves, i1)
        ev["mint_note_after_refund"] = fork.revert_reason(v3, CLAIM_SIG, d3["mint_note"], d3["sender"], str(d3["amount"]), "false", "[" + ",".join("0x" + p for p in p0) + "]", str(i0), str(h), sender=claimer)
        ev["refund_wrong_dest"] = fork.revert_reason(v3, CLAIM_SIG, d1["refund_note"], claimer, str(d1["amount"]), "false", "[" + ",".join("0x" + p for p in p1) + "]", str(i1), str(h), sender=claimer)
        check(ev["mint_note_after_refund"] == "resolved", ev["mint_note_after_refund"])
        check(ev["refund_wrong_dest"] == "refund-dest", ev["refund_wrong_dest"])

    try:
        s8()
    except Exception:
        pass

    @scenario("F1", "pl-eth-claim-proof.py can build an openClaim proof for a V3 refund note from node status", ["pl-eth-claim-proof.py", "node status_json from the Rust run"], ["FPL node status served by mock ctl"])
    def f1(ev):
        rc = ctx.get("claim_tool_refund_rc")
        ev["rc"] = rc
        check(rc == 0, "claim tool cannot find refund notes (they are claim leaves but not rail `withdrawals`)")

    try:
        f1()
    except Exception:
        pass

    @scenario("F3", "pl-eth-auto-mint.py reads the LC head block time with this cast (head_block_timestamp)", ["cast on this box", "fork block"], [])
    def f3(ev):
        head = ctx.get("head_b") or ctx.get("head_a")
        want = int(fork.block(head)["timestamp"], 16)
        got = am.head_block_timestamp(head)
        ev.update({"block": head, "want": want, "got": got, "cast": subprocess.check_output([CAST, "--version"], text=True).splitlines()[0]})
        check(got == want, "head_block_timestamp returned %r (cast block N timestamp is rejected by this cast; auto-mint falls back to genesis + 12 * finalized_slot)" % (got,))

    try:
        f3()
    except Exception:
        pass

    @scenario("F2", "pl-falcon-qc-v3-prover.py default --gas-limit covers a real 4-proof submitHeaderWithProofs", ["real verifier gas on the fork (S2)"], [])
    def f2(ev):
        bug = ctx.get("prover_gas_bug")
        sent = ctx.get("prover_default_send") or {}
        ev.update(bug or {"ok": True})
        ev["default_send"] = sent
        check(not bug, f"real submit needs {bug and bug['need']} gas; the prover's default gas limit was {bug and bug['default']}")
        check(sent.get("status") == 1, f"default-path send did not succeed: {sent}")
        check(sent["gasUsed"] < sent["gas_limit"] < 2**24, f"gas limit {sent['gas_limit']} not between used {sent['gasUsed']} and the EIP-7825 cap")

    try:
        f2()
    except Exception:
        pass

    @scenario("F4", "Every V3 openClaim in this run fits walletd's openClaim gas limit", ["openClaim receipts on the fork (S7c real note 9, S7b refunds)"], [])
    def f4(ev):
        ev["walletd_limit"] = WALLETD_OPENCLAIM_GAS_LIMIT
        ev["openclaim_gas"] = dict(OPENCLAIM_GAS)
        check(bool(OPENCLAIM_GAS), "no openClaim ran")
        worst = max(OPENCLAIM_GAS.values())
        ev["max"] = worst
        ev["suggested_limit_x1_25"] = (worst * 125 // 100 + 4_999) // 5_000 * 5_000
        check(worst < WALLETD_OPENCLAIM_GAS_LIMIT, f"openClaim used up to {worst} gas; walletd sends {WALLETD_OPENCLAIM_GAS_LIMIT} (suggest {ev['suggested_limit_x1_25']})")

    try:
        f4()
    except Exception:
        pass


if __name__ == "__main__":
    sys.exit(main())
