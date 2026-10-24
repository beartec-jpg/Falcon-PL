#!/usr/bin/env python3
"""Automatic dest-lock mint: FalconBridge Deposit → FPL ETH/USDC RailDeposit.

Wallet POST {account, txHash} writes a job. This process (or walletd's worker)
runs ETH LC header-walk + receipt MPT + submit-rail-deposit via watcher-eth.

dest20 is not invertible. The named PL account MUST be supplied (wallet) or
already mapped (name-reservations / prior jobs / genesis).

V3 (`--v3`): scan FalconQcBridgeV3 `Deposit(bytes32,address,bytes20,address,uint256,uint64)`
logs (topic0 below; bridge from `FPL_SEPOLIA_QC_V3_BRIDGE` or `--v3-bridge`) and
submit `EthV3Deposit` via `falcon-pl-ctl submit-eth-v3-deposit`. A known dest20
inside the expiry window is submitted with `credit_to`. A deposit older than E
against LC-head time (execution-head block timestamp, else Sepolia beacon
genesis + 12s * finalized slot) is submitted with empty `credit_to` so FPL
refunds it. `fundPool` / `PoolFunded` is never submitted. Already-resolved
deposits (local done state, or a ctl reply containing "already") are skipped.

Usage:
  pl-eth-auto-mint.py --daemon
  pl-eth-auto-mint.py --once --account dave --txid 0x...
  pl-eth-auto-mint.py --daemon --v3
  pl-eth-auto-mint.py --v3 --once --txid 0x...
"""
from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path

_SCRIPT = Path(__file__).resolve()
if str(_SCRIPT.parent) not in sys.path:
    sys.path.insert(0, str(_SCRIPT.parent))
from eth_bridge_config import ETH_RPC, FALCON_QC_BRIDGE  # noqa: E402

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL = Path(os.environ.get("FPL_CTL", str(ROOT / "bin/falcon-pl-ctl")))
MINT = Path(
    os.environ.get(
        "FPL_ETH_MINT",
        str(ROOT / "scripts/pl-eth-mint.py"),
    )
)
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")
KEYS = os.environ.get("FPL_KEYS", str(ROOT / "keys"))
# Mint account. The LC relayer daemon is watcher-eth-lc. Never the same
# account, and never watcher-browser (BTC headers). Compared casefolded.
RELAYER_ACCOUNT = "watcher-eth-lc"
BTC_WATCHER_ACCOUNT = "watcher-browser"
WATCHER = os.environ.get("FPL_WATCHER_ACCOUNT", "watcher-eth").strip() or "watcher-eth"
SCHEME = os.environ.get("FPL_SCHEME", "falcon-512")
NETWORK_ID = os.environ.get("FPL_NETWORK_ID", "2300")
CAST = Path(os.environ.get("CAST", str(Path.home() / ".foundry/bin/cast")))
ETH_RPC = os.environ.get("ETH_RPC", ETH_RPC)
BRIDGE = os.environ.get("SEPOLIA_BRIDGE", FALCON_QC_BRIDGE)
STATE = Path(os.environ.get("MINT_JOBS", str(ROOT / "run/eth-mint-jobs.json")))
LOCK = Path(os.environ.get("MINT_LOCK", str(ROOT / "run/eth-mint.lock")))
RUN_LOCK = Path(os.environ.get("MINT_RUNLOCK", str(ROOT / "run/eth-mint.runlock")))
NAME_STORE = Path(
    os.environ.get("FALCON_PL_NAME_STORE", str(ROOT / "run/name-reservations.json"))
)
POLL = int(os.environ.get("POLL_SEC", "8"))
LOG_CHUNK = int(os.environ.get("ETH_LOG_CHUNK", "2000"))
LOG_CHUNKS_PER_LOOP = int(os.environ.get("ETH_LOG_CHUNKS", "8"))
ETH_LOCK = "c1113d2059c5d73f45968910fadfff7d88a3c4a4d8a0f754f15faa495d2c467b"
USDC_LOCK = "c8927e1f8b56b080b015ccf240c5d27c6f1525d52a6545a1c03c84326e483c1c"
ETH_TOKEN = "00" * 20
USDC_TOKEN = "1c7d4b196cb0c7b01d743fbc6116a902379c7238"
# keccak256("Deposit(bytes32,address,bytes20,address,uint256,uint64)")
V3_DEPOSIT_TOPIC0 = "e7783c72791736d4d8af95660d4367bed2f98d8331a095422373e557a4318728"
# keccak256("PoolFunded(address,address,uint256)") — fundPool, never minted.
POOL_FUNDED_TOPIC0 = "aa1ff9f7fea9b0b6004b27d6dafaf7545a080426cc7115c6a8bd9019957c4353"
ETH_V3_DEPOSIT_EXPIRY_SECS = 3 * 86_400
# bridge::ETH_EXEC_HEADER_WINDOW (status `eth_exec_window` when reported).
ETH_EXEC_HEADER_WINDOW = 50_400
SEPOLIA_BEACON_GENESIS_UNIX = 1_655_733_600
GENESIS = (
    "alice",
    "bob",
    "carol",
    "dave",
    "sally",
    "faucet",
    "treasury",
    "watcher-browser",
)
ACCT_RE = __import__("re").compile(r"^[A-Za-z0-9._-]{2,64}$")


def assert_mint_account() -> None:
    """Refuse the BTC submitter and the ETH LC relayer account.

    Those daemons share no lockfile. A second signer on the same account
    evicts the other's mempool transactions. Default is watcher-eth.
    """
    key = WATCHER.strip().casefold()
    banned = {
        BTC_WATCHER_ACCOUNT.casefold(): "it signs BTC headers",
        RELAYER_ACCOUNT.casefold(): "it is the ETH LC relayer daemon",
    }
    if key in banned:
        # 78 = sysexits EX_CONFIG. The unit's RestartPreventExitStatus=78
        # keeps a wrong drop-in from restart-looping.
        msg = (
            f"FPL_WATCHER_ACCOUNT={WATCHER.strip()} cannot mint ({banned[key]}). "
            "Auto-mint signs as watcher-eth. The relayer daemon signs as "
            "watcher-eth-lc. Never point both at the same account, and do not "
            "use watcher-browser."
        )
        print(msg, file=sys.stderr, flush=True)
        raise SystemExit(78)


def dest20(account: str) -> str:
    return hashlib.sha256(account.strip().lower().encode()).hexdigest()[:40]


def now_ms() -> int:
    return int(time.time() * 1000)


def parse_int(v) -> int:
    if v is None or v is False:
        return 0
    if isinstance(v, bool):
        return int(v)
    if isinstance(v, int):
        return v
    s = str(v).strip().split()[0].rstrip(",")
    if not s:
        return 0
    return int(s, 0)


def txid_key(txid: str) -> str:
    return txid.strip().lower().removeprefix("0x")


def empty_state() -> dict:
    return {"jobs": {}, "accounts": {}}


def load_state() -> dict:
    try:
        st = json.loads(STATE.read_text())
        if not isinstance(st, dict):
            return empty_state()
        st.setdefault("jobs", {})
        st.setdefault("accounts", {})
        return st
    except (OSError, json.JSONDecodeError):
        return empty_state()


def save_state(st: dict) -> None:
    STATE.parent.mkdir(parents=True, exist_ok=True)
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(st, indent=2) + "\n")
    tmp.replace(STATE)


class FileLock:
    def __init__(self, path: Path):
        self.path = path
        self.fd = None

    def __enter__(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.fd = open(self.path, "a+")
        fcntl.flock(self.fd.fileno(), fcntl.LOCK_EX)
        return self

    def __exit__(self, *exc):
        if self.fd:
            fcntl.flock(self.fd.fileno(), fcntl.LOCK_UN)
            self.fd.close()
            self.fd = None


def known_accounts(st: dict) -> dict[str, str]:
    out: dict[str, str] = {}
    for name in GENESIS:
        out[dest20(name)] = name
    try:
        rows = json.loads(NAME_STORE.read_text())
    except (OSError, json.JSONDecodeError):
        rows = []
    if isinstance(rows, list):
        for r in rows:
            if not isinstance(r, dict):
                continue
            name = str(r.get("name") or "").strip().lower()
            if name:
                out[dest20(name)] = name
    for d20, name in (st.get("accounts") or {}).items():
        if name:
            out[str(d20).lower()[-40:]] = str(name).strip().lower()
    for job in (st.get("jobs") or {}).values():
        if not isinstance(job, dict):
            continue
        acct = str(job.get("account") or "").strip().lower()
        d20 = str(job.get("dest20") or "").lower()[-40:]
        if acct:
            out[d20 or dest20(acct)] = acct
    return out


def ctl(*args: str, timeout: int = 60) -> str:
    cmd = [str(CTL), "--addr", RPC, *args]
    return subprocess.check_output(cmd, text=True, timeout=timeout)


def account_assets(name: str) -> dict:
    raw = ctl("account", "--account", name)
    d = json.loads(raw)
    body = d.get("body") or d
    assets = body.get("assets") or {}
    return {str(k): int(v or 0) for k, v in assets.items()} if isinstance(assets, dict) else {}


def receipt_logs(txid: str) -> list[dict]:
    raw = subprocess.check_output(
        [str(CAST), "receipt", txid, "--rpc-url", ETH_RPC, "--json"],
        text=True,
        timeout=40,
    )
    rec = json.loads(raw)
    if str(rec.get("status")) in ("0", "0x0"):
        raise RuntimeError(f"sepolia tx failed {txid}")
    return rec.get("logs") or []


def pick_deposit(txid: str, account: str, asset_hint: str = "") -> dict:
    dest = dest20(account)
    picked = None
    for lg in receipt_logs(txid):
        if (lg.get("address") or "").lower() != BRIDGE.lower():
            continue
        topics = lg.get("topics") or []
        if len(topics) < 3:
            continue
        d20 = topics[1].lower().removeprefix("0x")[-40:]
        token = topics[2].lower().removeprefix("0x")[-40:]
        data = (lg.get("data") or "0x")[2:]
        amount = int(data[0:64], 16) if len(data) >= 64 else 0
        if d20 != dest:
            continue
        native = token == ETH_TOKEN
        asset = "ETH" if native else "USDC"
        if asset_hint and asset_hint.upper() != asset:
            continue
        picked = {
            "asset": asset,
            "token": token,
            "amount": amount,
            "dest20": dest,
            "lock": ETH_LOCK if native else USDC_LOCK,
        }
        break
    if not picked:
        raise RuntimeError(f"no Deposit for dest20={dest} on {txid}")
    return picked


def queue_job(account: str, txid: str, asset: str = "") -> dict:
    account = account.strip().lower()
    if not ACCT_RE.match(account):
        raise ValueError("invalid PL account")
    key = txid_key(txid)
    if len(key) != 64 or any(c not in "0123456789abcdef" for c in key):
        raise ValueError("txid must be 32-byte hex")
    with FileLock(LOCK):
        st = load_state()
        existing = st["jobs"].get(key)
        if existing and existing.get("status") in ("queued", "running", "done"):
            if existing.get("account") and existing.get("account") != account:
                raise ValueError("txid already queued for a different account")
            return existing
        picked = pick_deposit("0x" + key, account, asset)
        rec = {
            "txid": key,
            "account": account,
            "asset": picked["asset"],
            "amount": picked["amount"],
            "dest20": picked["dest20"],
            "status": "queued",
            "error": "",
            "mintTx": "",
            "queuedAt": now_ms(),
            "updated": now_ms(),
        }
        st["jobs"][key] = rec
        st["accounts"][picked["dest20"]] = account
        save_state(st)
        return rec


def job_status(txid: str, account: str = "") -> dict:
    key = txid_key(txid)
    with FileLock(LOCK):
        st = load_state()
        job = st["jobs"].get(key) or st["jobs"].get(v3_job_key(key))
    if not job:
        return {"txid": key, "status": "unknown", "error": "no job"}
    if account and job.get("account") and job["account"] != account.strip().lower():
        return {"txid": key, "status": "unknown", "error": "account mismatch"}
    return job


def already_word(msg: str) -> bool:
    m = msg.lower()
    return any(s in m for s in ("already", "spent", "duplicate", "replay", "seen"))


def run_mint(account: str, txid: str, asset: str) -> str:
    env = os.environ.copy()
    env.setdefault("PL_ROOT", str(ROOT))
    env.setdefault("FPL_CTL", str(CTL))
    env.setdefault("FPL_RPC", RPC)
    env.setdefault("FPL_KEYS", KEYS)
    env.setdefault("FPL_WATCHER_ACCOUNT", WATCHER)
    env.setdefault("FPL_SCHEME", SCHEME)
    env.setdefault("FPL_NETWORK_ID", NETWORK_ID)
    env.setdefault("SEPOLIA_BRIDGE", BRIDGE)
    env.setdefault("ETH_RPC", ETH_RPC)
    cmd = [sys.executable, str(MINT), "--account", account, "--txid", txid]
    if asset:
        cmd += ["--asset", asset]
    p = subprocess.run(cmd, text=True, capture_output=True, timeout=240, env=env)
    out = (p.stdout or "") + (p.stderr or "")
    if p.returncode != 0 and not already_word(out):
        raise RuntimeError(out[-800:] or f"mint exit {p.returncode}")
    return out[-500:]


def wait_credit(account: str, asset: str, before: int, amount: int, tries: int = 40) -> int:
    want = before + max(1, amount)
    for i in range(tries):
        got = int(account_assets(account).get(asset) or 0)
        if got >= want or (amount > 0 and got > before):
            return got
        print(f"  credit {account} {asset} {got} want>={want} ({i+1}/{tries})", flush=True)
        time.sleep(2)
    raise RuntimeError(f"{asset} credit did not land on {account}")


def v3_submit_ready(status: dict, bridge: str) -> tuple[bool, str]:
    """Whether the node would admit a V3 deposit tx now.

    The node refuses one (`V3 deposits not active`, retryable) until the next
    ledger is at or after `eth_v3_from_height`, and needs a configured V3
    bridge. Before that a watcher tx would sit in the mempool, the job would
    time out as an error, and the next job would evict it at the same
    sequence. So V3 jobs stay queued, untouched, until this is true.

    `bridge` is the contract this process scans logs from. The node's
    `eth_v3_bridge` must be that same address, or consensus would parse the
    receipt for another contract and fail the deposit permanently.
    """
    if not bridge:
        return False, "no V3 bridge given to this process"
    if not status:
        return False, "node status unavailable"
    try:
        h2 = int(status["eth_v3_from_height"])
    except (KeyError, TypeError, ValueError):
        return False, "node does not report eth_v3_from_height"
    tip = int(status.get("tip_height") or 0)
    if tip + 1 < h2:
        return False, f"V3 deposits not active until height {h2} (tip {tip})"
    node_bridge = str(status.get("eth_v3_bridge") or "").strip().lower()
    if not node_bridge:
        return False, "node has no V3 bridge configured"
    if node_bridge.removeprefix("0x") != bridge.strip().lower().removeprefix("0x"):
        return False, f"node V3 bridge {node_bridge} != scanned bridge {bridge}"
    return True, ""


def is_v3_job(key: str, job: dict) -> bool:
    return str(job.get("kind") or "") == "v3" or str(key).startswith("v3:")


def process_job(key: str, v3_bridge: str = "") -> dict:
    with FileLock(RUN_LOCK):
        with FileLock(LOCK):
            st = load_state()
            job = st["jobs"].get(key) or {}
        if is_v3_job(key, job):
            ready, why = v3_submit_ready(ctl_status(), v3_bridge)
            if not ready:
                # Leave it queued for a later pass; nothing is submitted.
                print(f"  v3 {str(key)[:20]}: waiting, {why}", flush=True)
                return job
            return _process_v3_job_locked(key)
        return _process_job_locked(key)


def _process_job_locked(key: str) -> dict:
    with FileLock(LOCK):
        st = load_state()
        job = st["jobs"].get(key)
        if not job:
            raise RuntimeError("job vanished")
        if job.get("status") == "done":
            return job
        job["status"] = "running"
        job["updated"] = now_ms()
        job["error"] = ""
        st["jobs"][key] = job
        save_state(st)
        account = job["account"]
        asset = job["asset"]
        amount = int(job.get("amount") or 0)
        before = int(account_assets(account).get(asset) or 0)
    try:
        raw = run_mint(account, "0x" + key, asset)
        after = wait_credit(account, asset, before, amount)
        with FileLock(LOCK):
            st = load_state()
            job = st["jobs"].get(key) or job
            job["status"] = "done"
            job["mintTx"] = raw[-200:]
            job["credited"] = after
            job["updated"] = now_ms()
            job["error"] = ""
            st["jobs"][key] = job
            save_state(st)
            return job
    except Exception as e:
        with FileLock(LOCK):
            st = load_state()
            job = st["jobs"].get(key) or job
            # credit may have landed even if wait timed out / replay
            try:
                after = int(account_assets(account).get(asset) or 0)
            except Exception:
                after = 0
            if after > before or already_word(str(e)):
                job["status"] = "done"
                job["credited"] = after
                job["error"] = ""
            else:
                job["status"] = "error"
                job["error"] = str(e)[-500:]
            job["updated"] = now_ms()
            st["jobs"][key] = job
            save_state(st)
            if job["status"] == "error":
                raise
            return job


def next_queued(skip_v3: bool = False) -> str | None:
    """Next queued job. `skip_v3` passes over V3 jobs (not admitted yet)."""
    with FileLock(LOCK):
        st = load_state()
        now = now_ms()
        for key, job in st["jobs"].items():
            if skip_v3 and isinstance(job, dict) and is_v3_job(key, job):
                continue
            if (
                isinstance(job, dict)
                and job.get("status") == "queued"
                and int(job.get("retry_at") or 0) <= now
            ):
                return key
        # resume a stuck running job after process crash
        for key, job in st["jobs"].items():
            if skip_v3 and isinstance(job, dict) and is_v3_job(key, job):
                continue
            if (
                isinstance(job, dict)
                and job.get("status") == "running"
                and now - int(job.get("updated") or 0) > 5 * 60 * 1000
            ):
                job["status"] = "queued"
                job["updated"] = now
                st["jobs"][key] = job
                save_state(st)
                return key
    return None


SEPOLIA_CHAIN_ID = int(os.environ.get("FPL_V3_CHAIN_ID", "11155111"))


def eth_chain_id() -> int:
    raw = subprocess.check_output(
        [str(CAST), "chain-id", "--rpc-url", ETH_RPC],
        text=True,
        timeout=20,
    ).strip()
    return parse_int(raw)


def require_sepolia_chain() -> int:
    """Chain id of ETH_RPC. Raises if it is not Sepolia.

    Call this before any V3 read, plan, or submit. A wrong-chain timestamp
    must not turn a pending deposit into a refund, and a wrong-chain receipt
    must not be queued or sent. The error is not a permanent V3 failure, so
    a queued job stays queued and is retried.
    """
    chain = eth_chain_id()
    if chain != SEPOLIA_CHAIN_ID:
        raise RuntimeError(
            f"ETH_RPC is chain {chain}, want {SEPOLIA_CHAIN_ID} (Sepolia); not scanning or submitting"
        )
    return chain


def latest_eth_block() -> int:
    raw = subprocess.check_output(
        [str(CAST), "block-number", "--rpc-url", ETH_RPC],
        text=True,
        timeout=20,
    ).strip()
    return parse_int(raw)


def scan_logs(st: dict) -> None:
    """Scan Deposit logs in small block ranges so public RPCs do not time out.

    last_block advances even when a chunk has no events, so a dead range cannot
    stall the watcher forever.
    """
    names = known_accounts(st)
    topic = subprocess.check_output(
        [str(CAST), "sig-event", "Deposit(bytes32,address,uint256,bytes32)"],
        text=True,
        timeout=20,
    ).strip()[:66]
    latest = latest_eth_block()
    from_block = parse_int(st.get("last_block"))
    if from_block <= 0:
        from_block = max(0, latest - 200)
        st["last_block"] = from_block
        save_state(st)
    chunks = 0
    while from_block <= latest and chunks < LOG_CHUNKS_PER_LOOP:
        to_block = min(latest, from_block + LOG_CHUNK - 1)
        try:
            raw = subprocess.check_output(
                [
                    str(CAST),
                    "logs",
                    "--from-block",
                    str(max(0, from_block)),
                    "--to-block",
                    str(to_block),
                    "--address",
                    BRIDGE,
                    topic,
                    "--rpc-url",
                    ETH_RPC,
                    "--json",
                ],
                text=True,
                timeout=40,
            )
        except Exception as e:
            print(f"scan logs {from_block}-{to_block}: {e}", flush=True)
            break
        rows = json.loads(raw) if raw.strip() else []
        if isinstance(rows, dict):
            rows = [rows]
        queued = 0
        for lg in rows:
            txid = txid_key(lg.get("transactionHash") or "")
            topics = lg.get("topics") or []
            if len(topics) < 3 or len(txid) != 64:
                continue
            d20 = topics[1].lower().removeprefix("0x")[-40:]
            acct = names.get(d20)
            if not acct:
                continue
            if txid in st["jobs"] and st["jobs"][txid].get("status") in ("queued", "running", "done"):
                continue
            token = topics[2].lower().removeprefix("0x")[-40:]
            data = (lg.get("data") or "0x")[2:]
            amount = int(data[0:64], 16) if len(data) >= 64 else 0
            asset = "ETH" if token == ETH_TOKEN else "USDC"
            st["jobs"][txid] = {
                "txid": txid,
                "account": acct,
                "asset": asset,
                "amount": amount,
                "dest20": d20,
                "status": "queued",
                "error": "",
                "mintTx": "",
                "queuedAt": now_ms(),
                "updated": now_ms(),
                "source": "log",
            }
            st["accounts"][d20] = acct
            queued += 1
            print(f"queued from log {asset} {acct} {txid[:16]}…", flush=True)
        st["last_block"] = to_block
        save_state(st)
        if queued or to_block >= latest or to_block % 10000 < LOG_CHUNK:
            print(f"scan logs {from_block}-{to_block} queued={queued} tip={latest}", flush=True)
        from_block = to_block + 1
        chunks += 1


def _hex_topic(topic: str) -> str:
    return topic.strip().lower().removeprefix("0x")


def v3_bridge_addr(cli: str = "") -> str:
    bridge = (cli or os.environ.get("FPL_SEPOLIA_QC_V3_BRIDGE") or "").strip()
    if not bridge:
        raise SystemExit("V3 bridge unset: pass --v3-bridge or set FPL_SEPOLIA_QC_V3_BRIDGE")
    h = bridge.lower().removeprefix("0x")
    if len(h) != 40 or any(c not in "0123456789abcdef" for c in h):
        raise SystemExit("V3 bridge must be a 20-byte hex address")
    return "0x" + h


def v3_job_key(deposit_id: str) -> str:
    return "v3:" + _hex_topic(deposit_id)


def v3_expiry_secs(status: dict | None) -> int:
    if not status:
        return ETH_V3_DEPOSIT_EXPIRY_SECS
    raw = status.get("eth_v3_expiry_secs")
    if raw is None:
        return ETH_V3_DEPOSIT_EXPIRY_SECS
    try:
        n = int(raw)
    except (TypeError, ValueError):
        return ETH_V3_DEPOSIT_EXPIRY_SECS
    return n if n > 0 else ETH_V3_DEPOSIT_EXPIRY_SECS


def lc_head_unix_fallback(status: dict | None) -> int:
    """Sepolia beacon genesis + 12s * finalized slot, when the head block time is unknown."""
    lc = (status or {}).get("eth_lc") or {}
    try:
        slot = int(lc.get("finalized_slot") or 0)
    except (TypeError, ValueError):
        slot = 0
    if slot <= 0:
        return 0
    return SEPOLIA_BEACON_GENESIS_UNIX + 12 * slot


def deposit_expired(timestamp: int, lc_head_time: int, expiry_secs: int) -> bool:
    """Same comparison as consensus: equal to timestamp+E is still inside the window."""
    if lc_head_time <= 0 or expiry_secs < 0:
        return False
    return lc_head_time > int(timestamp) + int(expiry_secs)


def parse_v3_deposit_log(lg: dict) -> dict | None:
    """Decode one V3 Deposit log. PoolFunded and bad padding return None.

    Data is abi.encode(bytes20 dest20, address token, uint256 amount, uint64 timestamp):
    bytes20 is left-aligned, address and integers are right-aligned. Non-zero padding
    rejects the log. Amount must fit in u64.
    """
    if not isinstance(lg, dict):
        return None
    topics = lg.get("topics") or []
    if len(topics) != 3:
        return None
    topic0 = _hex_topic(str(topics[0]))
    if topic0 == POOL_FUNDED_TOPIC0 or topic0 != V3_DEPOSIT_TOPIC0:
        return None
    deposit_id = _hex_topic(str(topics[1]))
    sender_word = _hex_topic(str(topics[2]))
    if len(deposit_id) != 64 or len(sender_word) != 64:
        return None
    if sender_word[:24] != "00" * 12:
        return None
    sender = sender_word[24:]
    data = _hex_topic(str(lg.get("data") or ""))
    if len(data) != 256:
        return None
    words = [data[i * 64 : (i + 1) * 64] for i in range(4)]
    if words[0][40:] != "00" * 12:
        return None
    dest20 = words[0][:40]
    if words[1][:24] != "00" * 12:
        return None
    token = words[1][24:]
    try:
        amount = int(words[2], 16)
        timestamp = int(words[3], 16)
    except ValueError:
        return None
    if words[3][:48] != "00" * 24:
        return None
    if amount <= 0 or amount > 2**64 - 1:
        return None
    if dest20 == "00" * 20:
        return None
    if token == ETH_TOKEN:
        asset = "ETH"
    elif token == USDC_TOKEN:
        asset = "USDC"
    else:
        return None
    return {
        "deposit_id": deposit_id,
        "sender": sender,
        "dest20": dest20,
        "token": token,
        "asset": asset,
        "amount": amount,
        "timestamp": timestamp,
        "txid": txid_key(str(lg.get("transactionHash") or "")),
        "block_number": parse_int(lg.get("blockNumber")),
        "block_hash": _hex_topic(str(lg.get("blockHash") or "")),
        "tx_index": parse_int(lg.get("transactionIndex")),
    }


def plan_v3_submit(
    parsed: dict,
    names: dict[str, str],
    lc_head_time: int,
    expiry_secs: int = ETH_V3_DEPOSIT_EXPIRY_SECS,
) -> dict | None:
    """Mint when dest20 is a known account and the deposit is inside E.

    After E, submit with empty credit_to so the refund is automatic, whether
    or not dest20 is known. Inside E with an unknown dest, submit nothing
    (consensus would not record the deposit). Never returns a PoolFunded plan.
    """
    if not parsed:
        return None
    expired = deposit_expired(int(parsed.get("timestamp") or 0), lc_head_time, expiry_secs)
    if expired:
        return {"credit_to": "", "mode": "refund"}
    dest = str(parsed.get("dest20") or "").lower()[-40:]
    name = names.get(dest) or names.get(str(parsed.get("dest20") or "").lower())
    if not name:
        return None
    return {"credit_to": str(name).strip().lower(), "mode": "mint"}


def split_v3_logs(
    logs: list,
    names: dict[str, str],
    lc_head_time: int,
    expiry_secs: int,
    bridge: str,
) -> tuple[list[dict], list[dict]]:
    """(plans, waiting) for one receipt or a cast-logs page.

    `waiting` holds parsed V3 deposits with no plan yet (unknown dest inside
    E). They must be kept and re-planned, or they never get their refund
    once E passes. Drops PoolFunded and other contracts.
    """
    want = bridge.strip().lower().removeprefix("0x")
    out: list[dict] = []
    waiting: list[dict] = []
    for lg in logs or []:
        if not isinstance(lg, dict):
            continue
        addr = str(lg.get("address") or "").strip().lower().removeprefix("0x")
        if want and addr and addr != want:
            continue
        parsed = parse_v3_deposit_log(lg)
        if not parsed:
            continue
        if len(parsed.get("txid") or "") != 64:
            continue
        plan = plan_v3_submit(parsed, names, lc_head_time, expiry_secs)
        if not plan:
            waiting.append(parsed)
            continue
        out.append({**parsed, **plan, "kind": "v3"})
    return out, waiting


def jobs_from_v3_logs(
    logs: list,
    names: dict[str, str],
    lc_head_time: int,
    expiry_secs: int,
    bridge: str,
) -> list[dict]:
    """Plans only; see split_v3_logs for the deposits still waiting."""
    return split_v3_logs(logs, names, lc_head_time, expiry_secs, bridge)[0]


def v3_job_record(plan: dict, source: str) -> dict:
    return {
        "kind": "v3",
        "deposit_id": plan["deposit_id"],
        "txid": plan.get("txid") or "",
        "account": plan.get("credit_to") or "",
        "credit_to": plan.get("credit_to") or "",
        "mode": plan.get("mode") or "refund",
        "asset": plan.get("asset") or "ETH",
        "amount": int(plan.get("amount") or 0),
        "dest20": plan.get("dest20") or "",
        "sender": plan.get("sender") or "",
        "timestamp": int(plan.get("timestamp") or 0),
        "block_number": int(plan.get("block_number") or 0),
        "block_hash": plan.get("block_hash") or "",
        "tx_index": int(plan.get("tx_index") or 0),
        "status": "queued",
        "error": "",
        "mintTx": "",
        "queuedAt": now_ms(),
        "updated": now_ms(),
        "source": source,
    }


def _queue_v3_plan_in_state(st: dict, plan: dict, source: str) -> bool:
    """Add a queued job to `st` (caller holds LOCK and saves). False if one exists."""
    key = v3_job_key(plan["deposit_id"])
    cur = st["jobs"].get(key)
    if cur and cur.get("status") in ("queued", "running", "done"):
        return False
    st["jobs"][key] = v3_job_record(plan, source)
    if plan.get("credit_to") and plan.get("dest20"):
        st["accounts"][plan["dest20"]] = plan["credit_to"]
    label = plan.get("credit_to") or "refund"
    print(
        f"queued v3 {plan.get('mode')} {plan.get('asset')} {label} {plan['deposit_id'][:16]}…",
        flush=True,
    )
    return True


def remember_v3_waiting(st: dict, waiting: list[dict]) -> int:
    """Keep unplanned deposits in `st["v3_pending"]` (by deposit id) for re-planning."""
    pend = st.setdefault("v3_pending", {})
    added = 0
    for parsed in waiting:
        dep = str(parsed.get("deposit_id") or "")
        if not dep or v3_job_key(dep) in st["jobs"] or dep in pend:
            continue
        pend[dep] = dict(parsed)
        added += 1
    return added


def replan_v3_pending(
    st: dict,
    names: dict[str, str],
    lc_head_time: int,
    expiry_secs: int,
    source: str = "v3-pending",
) -> int:
    """Re-plan kept deposits: queue a mint once the dest is known, or the
    refund once E has passed. Drops entries that already have a job."""
    pend = st.setdefault("v3_pending", {})
    queued = 0
    for dep in list(pend):
        parsed = pend[dep]
        key = v3_job_key(dep)
        if key in st["jobs"]:
            del pend[dep]
            continue
        plan = plan_v3_submit(parsed, names, lc_head_time, expiry_secs)
        if not plan:
            continue
        if _queue_v3_plan_in_state(st, {**parsed, **plan, "kind": "v3"}, source):
            queued += 1
        del pend[dep]
    return queued


# Node errors after which a V3 deposit tx can never apply (all permanent in
# Node::is_permanent_tx_failure). Anything else is retried.
V3_PERMANENT_ERRORS = (
    "receipt has no matching v3 deposit",
    "v3 deposit token is not eth or usdc",
    "v3 deposit asset mismatch",
    "v3 deposit_id must be 32-byte hex",
    "deposit older than header window",
)


def v3_permanent(msg: str) -> bool:
    m = (msg or "").lower()
    return any(s in m for s in V3_PERMANENT_ERRORS)


def v3_retry_delay_ms(attempts: int) -> int:
    """Backoff for a transient V3 failure: FPL_V3_RETRY_SEC doubling, max 1h."""
    base = int(os.environ.get("FPL_V3_RETRY_SEC", "60"))
    return min(3600, base * (2 ** max(0, min(attempts - 1, 6)))) * 1000


def v3_already(msg: str) -> bool:
    m = (msg or "").lower()
    return any(
        s in m
        for s in (
            "already",
            "v3 deposit already resolved",
            "duplicate",
            "replay",
        )
    )


def ctl_json(raw: str) -> dict:
    i = raw.find("{")
    if i < 0:
        return {}
    try:
        body = json.JSONDecoder().raw_decode(raw[i:])[0]
    except json.JSONDecodeError:
        return {}
    if isinstance(body, dict) and isinstance(body.get("body"), dict):
        return body["body"]
    return body if isinstance(body, dict) else {}


def ctl_status() -> dict:
    try:
        return ctl_json(ctl("status", timeout=20))
    except Exception:
        return {}


def head_block_timestamp(execution_number: int) -> int | None:
    if execution_number <= 0 or not CAST.exists():
        return None
    try:
        raw = subprocess.check_output(
            # cast >= 1.x wants --field; the bare positional form errors on 1.8.5.
            [str(CAST), "block", str(execution_number), "--field", "timestamp", "--rpc-url", ETH_RPC],
            text=True,
            timeout=20,
        ).strip()
        n = parse_int(raw.split()[0] if raw else "0")
        return n or None
    except Exception:
        return None


def resolve_lc_head_time(status: dict | None) -> int:
    """LC execution-head block timestamp, else beacon-slot fallback. 0 if neither is known."""
    lc = (status or {}).get("eth_lc") or {}
    try:
        number = int(lc.get("execution_number") or 0)
    except (TypeError, ValueError):
        number = 0
    ts = head_block_timestamp(number) if number else None
    if ts:
        return ts
    return lc_head_unix_fallback(status)


_RELAYER_MOD = None


def _relayer():
    """Receipt MPT + exec-header submit from the shipped relayer. Do not edit that file.

    Loaded once per process and reused, so every V3 job shares the module's
    single per-process scratch dir (`_tmp_path`) and its one atexit cleanup,
    instead of leaving one dir per deposit until the daemon restarts.
    """
    global _RELAYER_MOD
    if _RELAYER_MOD is not None:
        return _RELAYER_MOD
    import importlib.util

    path = _SCRIPT.parent / "pl-eth-lc-relayer.py"
    spec = importlib.util.spec_from_file_location("pl_eth_lc_relayer", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    # The module default is the LC daemon's account (watcher-eth-lc). Here it
    # submits headers and LC updates for this minter, so it must sign as the
    # mint account, as `pl-eth-lc-relayer.py mint` does (resolve_account).
    mod.ACCOUNT = WATCHER
    mod.assert_mint_account()
    _RELAYER_MOD = mod
    return mod


def ctl_as_watcher(*args: str, timeout: int = 180) -> str:
    """Submit like the relayer: admin sock when the TCP wire is locked."""
    env = os.environ.copy()
    env["FPL_ADMIN_SOCK"] = os.environ.get("FPL_ADMIN_SOCK", str(ROOT / "run/v1.admin.sock"))
    cmd = [str(CTL), "--addr", RPC, *args]
    p = subprocess.run(cmd, text=True, capture_output=True, timeout=timeout, env=env)
    if p.returncode != 0:
        # Keep ctl's message (e.g. the node's "V3 deposit already resolved"
        # admission error) so the caller can classify it.
        out = ((p.stdout or "") + (p.stderr or "")).strip()
        raise RuntimeError(out[-800:] or f"ctl exit {p.returncode}")
    return p.stdout


def exec_headers_needed(dep_n: int, head_n: int, floor: int, tip: int) -> tuple[int, int] | None:
    """Block range [lo, hi] still to submit so stored headers reach dep_n from head_n.

    `floor`..`tip` is the node's contiguous stored range (status
    `eth_lc.eth_exec_floor` / `eth_exec_tip`). When it already covers
    `dep_n`, only the blocks above `tip` are needed. None means nothing.
    """
    lo = dep_n
    if floor > 0 and tip > 0 and floor <= dep_n <= tip:
        lo = tip + 1
    if lo > head_n:
        return None
    return lo, head_n


def account_sequence(account: str) -> int:
    """Committed next sequence of `account` (`falcon-pl-ctl account`).

    ctl's submit commands without --sequence re-read this same committed
    value, and the node evicts a mempool tx with the same account+sequence.
    So a second tx from this account must not be submitted until the first
    has committed (the value moved past it).
    """
    raw = ctl("account", "--account", account, timeout=20)
    i = raw.find("{")
    if i < 0:
        raise RuntimeError(f"account query not json: {raw[:200]}")
    msg = json.JSONDecoder().raw_decode(raw[i:])[0]
    body = msg.get("body") if isinstance(msg, dict) and isinstance(msg.get("body"), dict) else msg
    if not isinstance(body, dict) or "sequence" not in body:
        raise RuntimeError(f"account query missing sequence: {raw[:200]}")
    return int(body["sequence"])


def wait_sequence_past(account: str, before: int, label: str, tries: int | None = None) -> int:
    """Block until `account`'s committed sequence is past `before`."""
    n = tries if tries is not None else int(os.environ.get("FPL_V3_SEQ_TRIES", "90"))
    pause = float(os.environ.get("FPL_V3_SEQ_WAIT_SEC", "2"))
    got = before
    for i in range(n):
        try:
            got = account_sequence(account)
        except Exception as e:
            print(f"  {label}: account query failed: {e}", flush=True)
        if got > before:
            return got
        print(f"  wait {label} commit: {account} seq {got} (submitted at {before}) ({i + 1}/{n})", flush=True)
        time.sleep(pause)
    raise RuntimeError(f"{label} did not commit: {account} sequence still {got} (dropped or evicted?)")


def submit_committed(account: str, label: str, fn):
    """Read the committed sequence, submit one tx via `fn`, wait for it to commit."""
    before = account_sequence(account)
    out = fn()
    wait_sequence_past(account, before, label)
    return out


def ensure_exec_headers(rel, dep_n: int) -> None:
    wait_n = int(os.environ.get("FPL_MINT_LC_TRIES", "360"))
    head_n = 0
    for i in range(1, wait_n + 1):
        lc = rel.cmd_update(soft=True)
        head_n = int((lc or {}).get("execution_number") or 0)
        if dep_n <= head_n:
            break
        print(
            f"v3 deposit block {dep_n} > LC execution {head_n}; wait ({i}/{wait_n})",
            flush=True,
        )
        time.sleep(int(os.environ.get("FPL_LC_WAIT_SEC", "10")))
    else:
        raise RuntimeError(f"deposit block {dep_n} > LC execution {head_n}")
    lc = (rel.status() or {}).get("eth_lc") or {}
    need = exec_headers_needed(
        dep_n, head_n, int(lc.get("eth_exec_floor") or 0), int(lc.get("eth_exec_tip") or 0)
    )
    if need is None:
        print(f"  exec headers {dep_n}..{head_n} already stored", flush=True)
        return
    headers = []
    for n in range(need[0], need[1] + 1):
        headers.append(rel.fetch_header(n))
        if len(headers) % 16 == 0:
            print(f"  fetched headers {headers[0]['number']}..{headers[-1]['number']}", flush=True)
    # The relayer's (#28) sequence-aware path: normalised chunk count, byte
    # caps, oversize-header refusal, explicit sequences, and each wave waits
    # for its last sequence to commit before the next one. Nothing else from
    # this account is in flight when it returns.
    rel.submit_header_chain(headers)
    rel.wait_lc(lambda x: int(x.get("execution_number") or 0) >= head_n, tries=20, label="headers-pack")


def submit_v3_deposit(job: dict) -> str:
    # Recheck even if the job was queued on a good RPC. ETH_RPC may have
    # been pointed at another chain since, and this path fetches headers
    # and receipts from it.
    require_sepolia_chain()
    rel = _relayer()
    st = rel.status()
    if not st.get("eth_lc"):
        raise RuntimeError("ETH light client not bootstrapped")
    dep_n = int(job["block_number"])
    ensure_exec_headers(rel, dep_n)
    block = rel.eth_rpc("eth_getBlockByNumber", [hex(dep_n), False])
    tx_hashes = block.get("transactions") or []
    recs = rel.block_receipts(hex(dep_n), tx_hashes)
    tx_index = int(job.get("tx_index") or 0)
    root, raw_hex, nodes, _raw = rel.build_receipt_proof(recs, tx_index)
    want_root = rel.hx(block["receiptsRoot"]).hex()
    if root != want_root:
        raise RuntimeError(f"receipts_root mismatch built={root} block={want_root}")
    # Per-process mode-0700 scratch dir from the relayer module (#28), not
    # fixed /tmp names another process could pre-create, symlink or swap
    # before ctl reads them. Explicit env overrides are still honoured.
    raw_env = os.environ.get("FPL_V3_RECEIPT_FILE", "").strip()
    path_env = os.environ.get("FPL_V3_MPT_FILE", "").strip()
    raw_file = Path(raw_env) if raw_env else rel._tmp_path("sepolia-v3-receipt.hex")
    path_file = Path(path_env) if path_env else rel._tmp_path("sepolia-v3-mpt-nodes.json")
    raw_file.write_text(raw_hex)
    path_file.write_text(json.dumps(nodes))
    block_hash = job.get("block_hash") or rel.hx(block.get("hash")).hex()
    args = [
        "submit-eth-v3-deposit",
        "--account",
        WATCHER,
        "--asset",
        str(job["asset"]),
        "--deposit-id",
        str(job["deposit_id"]),
        "--external-txid",
        str(job.get("txid") or ""),
        "--block-hash",
        str(block_hash),
        "--height",
        str(dep_n),
        "--raw-tx-file",
        str(raw_file),
        "--merkle-path-file",
        str(path_file),
        "--merkle-index",
        str(tx_index),
        "--scheme",
        SCHEME,
        "--keys-dir",
        KEYS,
        "--network-id",
        NETWORK_ID,
    ]
    credit = str(job.get("credit_to") or "").strip()
    if credit:
        args += ["--credit-to", credit]
    label = f"v3 {job.get('mode') or 'deposit'} {str(job['deposit_id'])[:16]}"
    return submit_v3_until_resolved(args, label)


def submit_v3_until_resolved(args: list[str], label: str, attempts: int | None = None) -> str:
    """Submit an EthV3Deposit and confirm the deposit id is resolved on chain.

    Mint and refund alike wait for the watcher's committed sequence to move
    before anything else is sent from that account. A sequence advance alone
    does not prove *this* tx committed (another submitter on the account can
    take the slot and evict it), so the same submit is repeated: the node's
    mempool admission refuses an id that is already resolved, and that
    refusal is the confirmation. If it is admitted instead, the earlier tx
    was lost and this one is waited for in turn.
    """
    n = attempts if attempts is not None else int(os.environ.get("FPL_V3_CONFIRM_TRIES", "3"))
    last = ""
    for i in range(n):
        try:
            last = submit_committed(WATCHER, label, lambda: ctl_as_watcher(*args, timeout=240))
        except RuntimeError as e:
            if v3_already(str(e)):
                # Resolved (by this run's earlier submit, or by someone else).
                return last or str(e)[-200:]
            raise
        print(f"  {label}: committed a tx at the watcher sequence; confirming ({i + 1}/{n})", flush=True)
    raise RuntimeError(f"{label}: not confirmed resolved after {n} submits")


def _process_v3_job_locked(key: str) -> dict:
    with FileLock(LOCK):
        st = load_state()
        job = st["jobs"].get(key)
        if not job:
            raise RuntimeError("job vanished")
        if job.get("status") == "done":
            return job
        job["status"] = "running"
        job["updated"] = now_ms()
        job["error"] = ""
        st["jobs"][key] = job
        save_state(st)
        account = str(job.get("credit_to") or "")
        asset = job.get("asset") or "ETH"
        amount = int(job.get("amount") or 0)
        mode = job.get("mode") or ("mint" if account else "refund")
    before = 0
    if mode == "mint" and account:
        try:
            before = int(account_assets(account).get(asset) or 0)
        except Exception:
            before = 0
    try:
        # Returns only once admission says the id is already resolved.
        raw = submit_v3_deposit(job)
        after = before
        outcome = "refund"
        if mode == "mint" and account:
            outcome = "mint"
            try:
                after = wait_credit(account, asset, before, amount)
            except RuntimeError:
                # Resolved, but no credit: consensus refunded it (balance
                # overflow, or E passed while queued). Done either way.
                outcome = "refund (planned mint, no credit seen)"
                print(f"  v3 {key[:20]}: resolved without credit to {account}; {outcome}", flush=True)
        with FileLock(LOCK):
            st = load_state()
            job = st["jobs"].get(key) or job
            job["status"] = "done"
            job["outcome"] = outcome
            job["mintTx"] = raw[-200:]
            job["credited"] = after
            job["updated"] = now_ms()
            job["error"] = ""
            st["jobs"][key] = job
            save_state(st)
            return job
    except Exception as e:
        with FileLock(LOCK):
            st = load_state()
            job = st["jobs"].get(key) or job
            after = before
            if mode == "mint" and account:
                try:
                    after = int(account_assets(account).get(asset) or 0)
                except Exception:
                    after = 0
            if (mode == "mint" and after > before) or v3_already(str(e)):
                job["status"] = "done"
                job["credited"] = after
                job["error"] = ""
            elif v3_permanent(str(e)):
                job["status"] = "error"
                job["error"] = str(e)[-500:]
            else:
                # Transient (RPC, lost header chunk, sequence race): the
                # deposit is still unresolved and the scan cursor has moved
                # on, so keep the job queued and retry it after a backoff.
                job["status"] = "queued"
                job["attempts"] = int(job.get("attempts") or 0) + 1
                job["retry_at"] = now_ms() + v3_retry_delay_ms(job["attempts"])
                job["error"] = str(e)[-500:]
            job["updated"] = now_ms()
            st["jobs"][key] = job
            save_state(st)
            if job["status"] != "done":
                raise
            return job


def queue_v3_jobs(plans: list[dict], source: str = "v3-log") -> int:
    queued = 0
    for plan in plans:
        key = v3_job_key(plan["deposit_id"])
        existing = st_job_status(key)
        if existing in ("queued", "running", "done"):
            continue
        rec = v3_job_record(plan, source)
        with FileLock(LOCK):
            st = load_state()
            cur = st["jobs"].get(key)
            if cur and cur.get("status") in ("queued", "running", "done"):
                continue
            st["jobs"][key] = rec
            if rec["credit_to"] and rec["dest20"]:
                st["accounts"][rec["dest20"]] = rec["credit_to"]
            save_state(st)
        queued += 1
        label = rec["credit_to"] or "refund"
        print(f"queued v3 {rec['mode']} {rec['asset']} {label} {rec['deposit_id'][:16]}…", flush=True)
    return queued


def st_job_status(key: str) -> str:
    with FileLock(LOCK):
        st = load_state()
        job = st["jobs"].get(key) or {}
        return str(job.get("status") or "")


# Reorg margin for V3 logs, on top of the LC cap below.
V3_CONFIRMATIONS = max(0, int(os.environ.get("FPL_V3_CONFIRMATIONS", "96") or "96"))


def v3_lc_head(status: dict | None) -> int:
    lc = (status or {}).get("eth_lc") or {}
    return parse_int(lc.get("execution_number")) if isinstance(lc, dict) else 0


def v3_safe_tip(latest: int, status: dict | None = None) -> int:
    """Highest block the V3 cursor may pass.

    The node's LC execution head is the finalised, provable head, so the
    cursor never goes above it: a fixed depth alone is not finality (a
    beacon finality stall lets the tip run on while older blocks can still
    reorg). FPL_V3_CONFIRMATIONS below the live tip is an extra margin.
    No LC head reported means 0: do not advance.
    """
    head = v3_lc_head(status)
    if head <= 0:
        return 0
    return max(0, min(latest - V3_CONFIRMATIONS, head))


def v3_first_scan_block(latest: int, status: dict | None) -> int:
    """Where a V3 scan with no cursor (first run, lost state) starts.

    `FPL_V3_SCAN_FROM` (e.g. the contract's deploy block) if set. Otherwise
    one exec-header window (`eth_exec_window`, 50_400 blocks) back from the
    LC execution head, which is where consensus anchors the provable window
    (`execution_number - window`). The LC can lag the live Sepolia tip, so
    anchoring on `latest` would skip deposits that are still provable and
    refundable. The live tip is used only when the node reports no LC head,
    and the anchor never goes above it.
    """
    env = os.environ.get("FPL_V3_SCAN_FROM", "").strip()
    if env:
        return max(0, parse_int(env))
    st = status or {}
    window = parse_int(st.get("eth_exec_window")) or ETH_EXEC_HEADER_WINDOW
    head = v3_lc_head(st)
    anchor = min(latest, head) if head > 0 else latest
    return max(0, anchor - window)


def scan_v3_logs(st: dict, bridge: str) -> None:
    """Scan V3 Deposit logs only. PoolFunded is a different topic and is ignored."""
    # Before any RPC-derived planning or state write. resolve_lc_head_time
    # reads ETH_RPC, and a wrong-chain timestamp can queue a refund.
    try:
        require_sepolia_chain()
    except RuntimeError as e:
        print(f"v3 scan skipped: {e}", flush=True)
        return
    names = known_accounts(st)
    status = {}
    try:
        status = ctl_status()
    except Exception as e:
        print("v3 status:", e, flush=True)
    lc_time = resolve_lc_head_time(status)
    expiry = v3_expiry_secs(status)
    if replan_v3_pending(st, names, lc_time, expiry):
        save_state(st)
    # Scan only to the finalised LC head (minus a margin); the cursor never
    # passes an unfinal block, and does not move with no LC head.
    latest = v3_safe_tip(latest_eth_block(), status)
    if latest <= 0:
        print("v3 scan skipped: node reports no LC execution head", flush=True)
        return
    from_block = parse_int(st.get("last_block_v3"))
    if from_block <= 0:
        from_block = v3_first_scan_block(latest, status)
        print(f"v3 scan starts at block {from_block} (safe tip {latest})", flush=True)
        st["last_block_v3"] = from_block
        save_state(st)
    topic = "0x" + V3_DEPOSIT_TOPIC0
    chunks = 0
    while from_block <= latest and chunks < LOG_CHUNKS_PER_LOOP:
        to_block = min(latest, from_block + LOG_CHUNK - 1)
        try:
            raw = subprocess.check_output(
                [
                    str(CAST),
                    "logs",
                    "--from-block",
                    str(max(0, from_block)),
                    "--to-block",
                    str(to_block),
                    "--address",
                    bridge,
                    topic,
                    "--rpc-url",
                    ETH_RPC,
                    "--json",
                ],
                text=True,
                timeout=40,
            )
        except Exception as e:
            print(f"v3 scan logs {from_block}-{to_block}: {e}", flush=True)
            break
        rows = json.loads(raw) if raw.strip() else []
        if isinstance(rows, dict):
            rows = [rows]
        plans, waiting = split_v3_logs(rows, names, lc_time, expiry, bridge)
        queued = 0
        for plan in plans:
            if _queue_v3_plan_in_state(st, plan, "v3-log"):
                queued += 1
        # Unknown dest inside E: keep it, or the cursor below skips it for good.
        remember_v3_waiting(st, waiting)
        st["last_block_v3"] = to_block
        save_state(st)
        if queued or to_block >= latest or to_block % 10000 < LOG_CHUNK:
            print(
                f"v3 scan logs {from_block}-{to_block} queued={queued} tip={latest} lc_time={lc_time}",
                flush=True,
            )
        from_block = to_block + 1
        chunks += 1


def daemon(v3: bool = False, v3_only: bool = False, v3_bridge: str = "") -> int:
    bridge = ""
    if v3 or v3_only:
        bridge = v3_bridge_addr(v3_bridge)
    print(
        "eth auto-mint daemon bridge",
        BRIDGE,
        "v3",
        bridge or "off",
        "jobs",
        STATE,
        flush=True,
    )
    held_why = ""
    while True:
        try:
            with FileLock(LOCK):
                st = load_state()
                if not v3_only:
                    try:
                        scan_logs(st)
                    except Exception as e:
                        print("scan logs:", e, flush=True)
                if v3 or v3_only:
                    try:
                        scan_v3_logs(st, bridge)
                    except Exception as e:
                        print("v3 scan logs:", e, flush=True)
            ready, why = v3_submit_ready(ctl_status(), bridge)
            if not ready and why != held_why:
                print("v3 jobs held:", why, flush=True)
            held_why = "" if ready else why
            key = next_queued(skip_v3=not ready)
            if key:
                print("mint", key[:16], flush=True)
                try:
                    job = process_job(key, v3_bridge=bridge)
                    print("done", job.get("account"), job.get("asset"), job.get("credited"), flush=True)
                    continue
                except Exception as e:
                    print("mint fail", key[:16], e, flush=True)
        except Exception as e:
            print("loop:", e, flush=True)
        time.sleep(POLL)
    return 0


def once_v3_txid(txid: str, bridge_cli: str, queue_only: bool = False) -> int:
    """Queue (and unless --queue, submit) every V3 Deposit in one Sepolia tx."""
    bridge = v3_bridge_addr(bridge_cli)
    key = txid_key(txid)
    if len(key) != 64:
        raise SystemExit("txid must be 32-byte hex")
    try:
        require_sepolia_chain()
    except RuntimeError as e:
        raise SystemExit(str(e)) from e
    raw = subprocess.check_output(
        [str(CAST), "receipt", "0x" + key, "--rpc-url", ETH_RPC, "--json"],
        text=True,
        timeout=40,
    )
    rec = json.loads(raw)
    if str(rec.get("status")) in ("0", "0x0"):
        raise SystemExit(f"sepolia tx failed {txid}")
    blk = parse_int(rec.get("blockNumber"))
    status = ctl_status()
    safe = v3_safe_tip(latest_eth_block(), status)
    if blk <= 0 or blk > safe:
        # Not final on the LC yet. Nothing queued; the daemon scan picks it
        # up once the LC head passes it, or rerun this later.
        raise SystemExit(
            f"tx {txid} block {blk} is above the reorg-safe tip {safe} "
            f"(LC execution head, {V3_CONFIRMATIONS} below latest); try again later"
        )
    names = known_accounts(load_state())
    plans, waiting = split_v3_logs(
        rec.get("logs") or [],
        names,
        resolve_lc_head_time(status),
        v3_expiry_secs(status),
        bridge,
    )
    kept = 0
    if waiting:
        # Unknown dest inside E: keep it in v3_pending like the daemon scan,
        # so it is re-planned (mint once named, refund once E passes).
        with FileLock(LOCK):
            st = load_state()
            kept = remember_v3_waiting(st, waiting)
            save_state(st)
    if not plans and not waiting:
        raise SystemExit(f"no V3 Deposit to mint or refund on {txid}")
    n = queue_v3_jobs(plans, source="v3-once") if plans else 0
    print(
        json.dumps(
            {"queued": n, "pending": kept, "plans": plans, "waiting": [w.get("deposit_id") for w in waiting]},
            separators=(",", ":"),
            default=str,
        ),
        flush=True,
    )
    if queue_only:
        return 0
    rc = 0
    for plan in plans:
        job = process_job(v3_job_key(plan["deposit_id"]), v3_bridge=bridge)
        print(json.dumps(job, separators=(",", ":")), flush=True)
        if job.get("status") != "done":
            rc = 1
    return rc


def main() -> int:
    assert_mint_account()
    ap = argparse.ArgumentParser()
    ap.add_argument("--daemon", action="store_true")
    ap.add_argument("--once", action="store_true")
    ap.add_argument("--queue", action="store_true")
    ap.add_argument("--account", default="")
    ap.add_argument("--txid", default="")
    ap.add_argument("--asset", default="")
    ap.add_argument("--status", action="store_true")
    ap.add_argument("--v3", action="store_true", help="also scan V3 Deposit logs and submit EthV3Deposit")
    ap.add_argument("--v3-only", action="store_true", help="V3 scan only; do not scan V1 dest-lock logs")
    ap.add_argument("--v3-bridge", default="", help="V3 bridge (else FPL_SEPOLIA_QC_V3_BRIDGE)")
    args = ap.parse_args()
    v3 = bool(args.v3 or args.v3_only or args.v3_bridge)
    if args.daemon:
        return daemon(v3=v3, v3_only=bool(args.v3_only), v3_bridge=args.v3_bridge)
    if args.status:
        print(json.dumps(job_status(args.txid, args.account), separators=(",", ":")), flush=True)
        return 0
    if v3 and args.txid and not args.account:
        return once_v3_txid(args.txid, args.v3_bridge, queue_only=bool(args.queue))
    if not args.account or not args.txid:
        raise SystemExit("need --account and --txid (or --daemon, or --v3 --txid)")
    rec = queue_job(args.account, args.txid, args.asset)
    print(json.dumps(rec, separators=(",", ":")), flush=True)
    if args.queue:
        return 0
    if args.once or rec.get("status") != "done":
        job = process_job(txid_key(args.txid))
        print(json.dumps(job, separators=(",", ":")), flush=True)
        return 0 if job.get("status") == "done" else 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
