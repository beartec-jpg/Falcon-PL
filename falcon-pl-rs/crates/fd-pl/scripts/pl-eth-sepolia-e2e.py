#!/usr/bin/env python3
"""Live Sepolia ETH/USDC dest-lock e2e (testnet auditable).

  deposit on FalconQcBridge → LC mint on FPL → RailWithdraw →
  Falcon QC header on Ethereum → openClaim → take

Run one asset:  FPL_E2E_ASSETS=ETH
Run both:       FPL_E2E_ASSETS=ETH,USDC  (default)

Env:
  FPL_SKIP_DEPOSIT=1 / FPL_SKIP_MINT=1 / FPL_SKIP_WITHDRAW=1 / FPL_SKIP_CLAIM=1
"""
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

_SCRIPT = Path(__file__).resolve()
if str(_SCRIPT.parent) not in sys.path:
    sys.path.insert(0, str(_SCRIPT.parent))
from eth_bridge_config import (  # noqa: E402
    ETH_RPC,
    ETH_RPC_FALLBACKS,
    FALCON_QC_BRIDGE,
    MEMPOOL_API,
    SEPOLIA_USDC,
    dest20_hex,
    parse_cast_uint,
)

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL = Path(os.environ.get("FPL_CTL", str(ROOT / "bin/falcon-pl-ctl")))
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")
ADMIN = Path(os.environ.get("FPL_ADMIN_SOCK", str(ROOT / "run/v1.admin.sock")))
KEYS = Path(os.environ.get("FPL_KEYS", str(ROOT / "keys")))
CAST = Path(os.environ.get("CAST", str(Path.home() / ".foundry/bin/cast")))
PK_FILE = Path(
    os.environ.get("ETH_PK_FILE", str(Path.home() / "falcon-pl-vault-test/eth-wallets/deployer.privhex"))
)
MINT_SCRIPT = Path(os.environ.get("FPL_ETH_MINT", str(ROOT / "scripts/pl-eth-lc-relayer.py")))
CLAIM_SCRIPT = Path(os.environ.get("FPL_ETH_CLAIM", str(ROOT / "scripts/pl-eth-claim-proof.py")))
QC_PROVER = Path(os.environ.get("FPL_QC_PROVER", str(ROOT / "scripts/pl-falcon-qc-prover.py")))
QC_CATCHUP = Path(os.environ.get("FPL_QC_CATCHUP", str(ROOT / "scripts/pl-falcon-qc-catchup.py")))
SAMPLE_GAP = int(os.environ.get("FALCON_QC_SAMPLE_GAP", "48"))
FALCON_QC_V2 = os.environ.get("FALCON_QC_V2", "").lower() in ("1", "true", "yes")
if FALCON_QC_V2:
    raw_gap = os.environ.get("FALCON_QC_SAMPLE_GAP")
    if raw_gap is not None and int(raw_gap or "0") not in (0, 1):
        raise SystemExit("FALCON_QC_V2 forbids FALCON_QC_SAMPLE_GAP not in {0,1} (no jump/success grid)")
    SAMPLE_GAP = 1
    QC_PROVER = Path(
        os.environ.get("FPL_QC_PROVER", str(ROOT / "scripts/pl-falcon-qc-v2-prover.py"))
    )
    QC_CATCHUP = Path(
        os.environ.get("FPL_QC_CATCHUP", str(ROOT / "scripts/pl-falcon-qc-v2-catchup.py"))
    )
NETWORK_ID = os.environ.get("FPL_NETWORK_ID", "2300")
SCHEME = os.environ.get("FPL_SCHEME", "falcon-512")
WITHDRAW_DEST = os.environ.get("FPL_WITHDRAW_DEST", "").strip()
ETH_DEPOSIT_WEI = int(os.environ.get("FPL_ETH_DEPOSIT_WEI", str(int(0.005 * 10**18))))
ETH_WITHDRAW_WEI = int(os.environ.get("FPL_ETH_WITHDRAW_WEI", str(int(0.001 * 10**18))))
USDC_DEPOSIT = int(os.environ.get("FPL_USDC_DEPOSIT", "10000000"))  # 10 USDC
USDC_WITHDRAW = int(os.environ.get("FPL_USDC_WITHDRAW", "2000000"))  # 2 USDC
ETH_ACCOUNT = os.environ.get("FPL_ETH_ACCOUNT", "sally")
USDC_ACCOUNT = os.environ.get("FPL_USDC_ACCOUNT", "dave")
HEADER_TIMEOUT = int(os.environ.get("FPL_HEADER_TIMEOUT", "7200"))
PACK_TIMEOUT = int(os.environ.get("FPL_PACK_TIMEOUT", "7200"))
RECEIPT_POLL_SECS = int(os.environ.get("FPL_RECEIPT_POLL_SECS", "15"))
CAST_RECV_TIMEOUT = int(os.environ.get("CAST_RECV_TIMEOUT", "60"))
PRODUCTION = os.environ.get("FPL_PRODUCTION", "0") == "1"
FAUCET_FEE = int(os.environ.get("FPL_FAUCET_FEE", "2"))
FPL_FEE_RESERVE = int(os.environ.get("FPL_FEE_RESERVE", "50000"))
UA = {"User-Agent": "fpl-eth-sepolia-e2e/1.1"}


def norm_hex(s: str) -> str:
    return (s or "").strip().lower().removeprefix("0x")


def sha256(b: bytes) -> bytes:
    return hashlib.sha256(b).digest()


def withdraw_leaf(note32: bytes, dest20: bytes, amount: int, usdc: bool) -> bytes:
    buf = b"evm-wd-v1|" + note32 + dest20 + amount.to_bytes(8, "big") + (b"\x01" if usdc else b"\x00")
    return sha256(buf)


def combined_leaves(st: dict) -> list[bytes]:
    lc = st.get("lc") or {}
    raw = lc.get("claim_leaves")
    out: list[bytes] = []
    if isinstance(raw, list) and raw:
        for h in raw:
            b = bytes.fromhex(norm_hex(str(h)))
            if len(b) == 32:
                out.append(b)
        return out
    rails = {str(r.get("asset")): r for r in (st.get("rails") or []) if isinstance(r, dict)}
    for asset in ("ETH", "USDC"):
        r = rails.get(asset) or {}
        for h in r.get("claim_leaves") or []:
            b = bytes.fromhex(norm_hex(str(h)))
            if len(b) == 32:
                out.append(b)
    return out


def eth_rpcs() -> list[str]:
    out: list[str] = []
    for u in [ETH_RPC, *ETH_RPC_FALLBACKS]:
        if u and u not in out:
            out.append(u)
    return out


def log(msg: str) -> None:
    print(msg, flush=True)


def load_pk() -> str:
    raw = PK_FILE.read_text().strip()
    return raw if raw.startswith("0x") else "0x" + raw


def ctl(*args: str) -> str:
    cmd = [str(CTL), "--addr", RPC, *args]
    log("+ " + " ".join(cmd[:12]) + (" …" if len(cmd) > 12 else ""))
    return subprocess.check_output(cmd, text=True, timeout=120)


def admin_ctl(*args: str) -> str:
    if not ADMIN.exists():
        raise RuntimeError(f"admin sock missing: {ADMIN}")
    cmd = [str(CTL), "--admin-sock", str(ADMIN), "--addr", RPC, *args]
    log("+ admin " + " ".join(args[:10]))
    return subprocess.check_output(cmd, text=True, timeout=120)


def status() -> dict:
    raw = ctl("status")
    i = raw.find("{")
    body = json.JSONDecoder().raw_decode(raw[i:])[0]
    return body.get("body") or body


def account_body(account: str) -> dict:
    raw = ctl("account", "--account", account)
    return json.loads(raw).get("body") or json.loads(raw)


def ensure_fpl_balance(account: str, min_balance: int | None = None) -> None:
    """Non-genesis accounts need FPL native for submit-rail-withdraw tx fees."""
    need = FPL_FEE_RESERVE if min_balance is None else min_balance
    body = account_body(account)
    bal = int(body.get("balance") or 0)
    if bal >= need:
        log(f"{account} FPL balance={bal} ok")
        return
    drip = max(need - bal, 10_000)
    log(f"FAUCET {account} drip={drip} (had {bal}, need {need})")
    ctl(
        "faucet",
        "--to",
        account,
        "--amount",
        str(drip),
        "--fee",
        str(FAUCET_FEE),
        "--sequence",
        "0",
        "--scheme",
        SCHEME,
        "--keys-dir",
        str(KEYS),
        "--network-id",
        NETWORK_ID,
    )
    t0 = time.time()
    while time.time() - t0 < PACK_TIMEOUT:
        body = account_body(account)
        if int(body.get("balance") or 0) >= need:
            log(f"{account} funded FPL balance={body.get('balance')}")
            return
        time.sleep(5)
    raise RuntimeError(f"timeout funding {account} with FPL native for tx fees")


def cast_send_to(to: str, sig: str, *args: str, value_wei: int = 0, gas: str | None = None) -> str:
    cmd = [
        str(CAST),
        "send",
        to,
        sig,
        *args,
        "--rpc-url",
        eth_rpcs()[0],
        "--private-key",
        load_pk(),
        "--gas-limit",
        gas or os.environ.get("CAST_GAS", "500000"),
    ]
    if value_wei:
        cmd.extend(["--value", str(value_wei)])
    cmd.extend(["--async", *os.environ.get("CAST_SEND_FLAGS", "").split()])
    r = subprocess.run(cmd, text=True, capture_output=True, timeout=180)
    blob = (r.stdout or "") + (r.stderr or "")
    if r.returncode != 0:
        raise RuntimeError(blob[-800:])
    for tok in blob.replace('"', " ").split():
        if tok.startswith("0x") and len(tok) == 66:
            return tok
    raise RuntimeError("no tx hash: " + blob[-300:])


def cast_send(sig: str, *args: str, value_wei: int = 0, gas: str | None = None) -> str:
    return cast_send_to(FALCON_QC_BRIDGE, sig, *args, value_wei=value_wei, gas=gas)


def cast_call(to: str, sig: str, *args: str) -> str:
    last: Exception | None = None
    for rpc in eth_rpcs():
        try:
            return subprocess.check_output(
                [str(CAST), "call", to, sig, *args, "--rpc-url", rpc],
                text=True,
                timeout=CAST_RECV_TIMEOUT,
            ).strip()
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError) as e:
            last = e
    raise RuntimeError(f"cast call failed on all RPCs: {last}")


def eth_fpl_tip() -> int:
    out = cast_call(FALCON_QC_BRIDGE, "fplTip()(uint64)")
    return parse_cast_uint(out)


def _headers_sig() -> str:
    if FALCON_QC_V2:
        return "headers(uint64)(bytes32,bytes32,bytes32,bytes32,bytes32,bool)"
    return "headers(uint64)(bytes32,bytes32,bytes32,bool)"


def header_finalized(height: int) -> bool:
    out = cast_call(FALCON_QC_BRIDGE, _headers_sig(), str(height))
    lines = [ln.strip() for ln in out.strip().splitlines() if ln.strip()]
    if lines:
        last = lines[-1].lower()
        return last in ("true", "0x1", "1")
    parts = [p.strip() for p in out.replace("(", "").replace(")", "").split(",")]
    if parts:
        return parts[-1].lower() in ("true", "0x1", "1")
    return False


def on_chain_claim_root(height: int) -> str:
    out = cast_call(FALCON_QC_BRIDGE, _headers_sig(), str(height))
    lines = [ln.strip() for ln in out.strip().splitlines() if ln.strip()]
    # v1: hash, parent, claimRoot, finalized  → claim at [2]
    # v2: hash, parent, innerHash, claimRoot, nextRoot, finalized → claim at [3]
    idx = 3 if FALCON_QC_V2 else 2
    if len(lines) > idx:
        return norm_hex(lines[idx])
    parts = [p.strip() for p in out.replace("(", "").replace(")", "").split(",")]
    if len(parts) > idx:
        return norm_hex(parts[idx])
    return ""


def mempool_tx(txid: str) -> dict | None:
    h = txid.lower().removeprefix("0x")
    try:
        req = urllib.request.Request(f"{MEMPOOL_API}/tx/{h}", headers=UA)
        with urllib.request.urlopen(req, timeout=20) as r:
            raw = r.read().decode()
        if not raw or "not found" in raw.lower():
            return None
        return json.loads(raw)
    except (urllib.error.URLError, json.JSONDecodeError, TimeoutError, OSError):
        return None


def mempool_confirmed(txid: str) -> bool:
    t = mempool_tx(txid)
    if not t:
        return False
    st = t.get("status") or {}
    return bool(st.get("confirmed"))


def cast_receipt(txid: str) -> dict | None:
    for rpc in eth_rpcs():
        try:
            out = subprocess.check_output(
                [str(CAST), "receipt", txid, "--rpc-url", rpc, "--json"],
                text=True,
                timeout=CAST_RECV_TIMEOUT,
            )
            js = json.loads(out)
            if isinstance(js, dict) and js.get("transactionHash"):
                return js
        except (
            subprocess.CalledProcessError,
            subprocess.TimeoutExpired,
            json.JSONDecodeError,
            FileNotFoundError,
            OSError,
        ):
            continue
    return None


def wait_receipt(txid: str, timeout: int = 900) -> None:
    t0 = time.time()
    log(f"wait receipt {txid} (up to {timeout}s)")
    while time.time() - t0 < timeout:
        try:
            if mempool_confirmed(txid):
                js = cast_receipt(txid)
                if js and str(js.get("status")) in ("0x0", "0"):
                    raise RuntimeError(f"tx failed {txid}")
                log(f"CONFIRMED {txid} (mempool)")
                return
            js = cast_receipt(txid)
            if js:
                if js.get("status") == "0x1":
                    log(f"CONFIRMED {txid}")
                    return
                if str(js.get("status")) in ("0x0", "0"):
                    raise RuntimeError(f"tx failed {txid}")
        except subprocess.TimeoutExpired:
            log(f"receipt poll timeout for {txid}, retrying…")
        time.sleep(RECEIPT_POLL_SECS)
    raise RuntimeError(f"timeout waiting for {txid}")


def ensure_lc() -> None:
    st = status()
    if not st.get("eth_lc"):
        log("bootstrapping eth_lc…")
        subprocess.check_call([sys.executable, str(MINT_SCRIPT), "bootstrap"], env=os.environ.copy())
    lag = int(st.get("tip_lag") or 0)
    if lag > 3:
        raise RuntimeError(f"FPL lag={lag} — sync before e2e")


def deposit_eth(account: str) -> str:
    tx = cast_send("depositEth(bytes20)", dest20_hex(account), value_wei=ETH_DEPOSIT_WEI)
    log(f"DEPOSIT ETH {account} {ETH_DEPOSIT_WEI} wei tx={tx}")
    wait_receipt(tx)
    return tx


def deposit_usdc(account: str) -> str:
    approve_tx = cast_send_to(
        SEPOLIA_USDC,
        "approve(address,uint256)",
        FALCON_QC_BRIDGE,
        str(USDC_DEPOSIT),
    )
    log(f"USDC approve tx={approve_tx}")
    wait_receipt(approve_tx)
    tx = cast_send("depositUsdc(bytes20,uint256)", dest20_hex(account), str(USDC_DEPOSIT))
    log(f"DEPOSIT USDC {account} {USDC_DEPOSIT} tx={tx}")
    wait_receipt(tx)
    return tx


def mint(account: str, txid: str, asset: str) -> None:
    env = os.environ.copy()
    env.setdefault("PL_ROOT", str(ROOT))
    env.setdefault("FPL_CTL", str(CTL))
    env.setdefault("FPL_RPC", RPC)
    # Return quickly when LC is behind so the e2e loop can keep updating
    # instead of sitting silent until a wall-clock kill.
    env.setdefault("FPL_MINT_LC_TRIES", "8")
    env.setdefault("PACK_WAIT_TRIES", "20")
    args = [sys.executable, str(MINT_SCRIPT), "mint", "--account", account, "--txid", txid]
    if asset == "USDC":
        args.extend(["--asset", "USDC"])
    max_tries = int(os.environ.get("FPL_MINT_TRIES", "360"))
    mint_to = int(os.environ.get("FPL_MINT_SUBPROCESS_TIMEOUT", "180"))
    for attempt in range(1, max_tries + 1):
        try:
            r = subprocess.run(args, env=env, capture_output=True, text=True, timeout=mint_to)
            blob = (r.stdout or "") + (r.stderr or "")
            rc = r.returncode
        except subprocess.TimeoutExpired as e:
            blob = str(e.stdout or "") + str(e.stderr or "")
            rc = -1
            log(f"mint {asset} still waiting ({attempt}/{max_tries})")
        if rc == 0 and "deferred-until-catchup" not in blob:
            log(f"MINTED {asset} {account} {blob.strip()[-200:]}")
            return
        lc_lag = "deposit block" in blob and "LC execution" in blob
        deferred = "deferred-until-catchup" in blob
        if rc != 0 and not lc_lag and not deferred and "TimeoutExpired" not in blob and rc != -1:
            raise RuntimeError(f"mint {asset} failed: {blob[-800:]}")
        log(f"mint {asset} wait LC ({attempt}/{max_tries})")
        ur = subprocess.run(
            [sys.executable, str(MINT_SCRIPT), "update"],
            env=env,
            capture_output=True,
            text=True,
            timeout=300,
        )
        ublob = ((ur.stdout or "") + (ur.stderr or "")).strip()
        if ublob:
            log(f"LC update: {ublob[-400:]}")
        time.sleep(int(os.environ.get("FPL_LC_WAIT_SEC", "10")))
    raise RuntimeError(f"mint {asset} timeout after {max_tries} tries")


def withdraw(account: str, asset: str, amount: int, dest: str) -> None:
    timeout = int(os.environ.get("FPL_WITHDRAW_PACK_TIMEOUT", str(PACK_TIMEOUT)))
    fee = int(os.environ.get("FPL_WITHDRAW_FEE", "2"))
    t0 = time.time()
    attempt = 0
    last = ""
    while time.time() - t0 < timeout:
        attempt += 1
        bump = fee + attempt - 1
        cmd = [
            str(CTL),
            "--addr",
            RPC,
            "submit-rail-withdraw",
            "--account",
            account,
            "--asset",
            asset,
            "--amount",
            str(amount),
            "--external-to",
            dest,
            "--scheme",
            SCHEME,
            "--keys-dir",
            str(KEYS),
            "--network-id",
            NETWORK_ID,
            "--fee",
            str(bump),
        ]
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
        blob = (r.stdout + r.stderr).strip()
        last = blob
        if r.returncode == 0 and "deferred-until-catchup" not in blob:
            log(f"WITHDRAW {asset} submitted attempt={attempt} fee={bump}")
            wait_t = time.time()
            while time.time() - wait_t < 60:
                if find_withdraw_note(status(), account, asset, amount, dest):
                    log(f"WITHDRAW {asset} public packed")
                    return
                time.sleep(5)
            log(f"WITHDRAW {asset} not packed — bump fee")
            continue
        if PRODUCTION:
            log(f"WITHDRAW {asset} public failed attempt={attempt}: {blob[-200:]}")
            time.sleep(8)
            continue
        log(f"WITHDRAW {asset} public failed — admin")
        admin_ctl(
            "rail-withdraw",
            "--asset",
            asset,
            "--from",
            account,
            "--amount",
            str(amount),
            "--external-to",
            dest,
        )
        return
    raise RuntimeError(f"WITHDRAW {asset} never packed: {last[-800:]}")


def find_withdraw_note(st: dict, account: str, asset: str, amount: int, dest: str) -> dict | None:
    rails = {str(r.get("asset")): r for r in (st.get("rails") or []) if isinstance(r, dict)}
    r = rails.get(asset.upper()) or {}
    dest_n = "0x" + norm_hex(dest)
    for w in r.get("withdrawals") or []:
        if not isinstance(w, dict):
            continue
        if str(w.get("from") or "").strip().lower() != account.strip().lower():
            continue
        if int(w.get("amount") or 0) != amount:
            continue
        got = "0x" + norm_hex(str(w.get("external_to") or ""))
        if got != dest_n:
            continue
        return w
    return None


def withdraw_sealed_in_claim_tree(st: dict, account: str, asset: str, amount: int, dest: str) -> bool:
    w = find_withdraw_note(st, account, asset, amount, dest)
    if not w:
        return False
    nid = norm_hex(str(w.get("note_id") or ""))
    if len(nid) != 64:
        return False
    dest20 = bytes.fromhex(norm_hex(dest)[-40:]) if len(norm_hex(dest)) >= 40 else b""
    if len(dest20) != 20:
        return False
    leaf = withdraw_leaf(bytes.fromhex(nid), dest20, int(w.get("amount") or amount), asset.upper() == "USDC")
    return leaf in combined_leaves(st)


def withdraw_ledger_height(account: str, asset: str, amount: int, dest: str, timeout: int | None = None) -> int:
    """Height of the FPL block that sealed this withdraw into claim_root."""
    if timeout is None:
        timeout = PACK_TIMEOUT
    start_tip = int(status().get("tip_height") or 0)
    t0 = time.time()
    while time.time() - t0 < timeout:
        st = status()
        tip = int(st.get("tip_height") or 0)
        if withdraw_sealed_in_claim_tree(st, account, asset, amount, dest) and tip > start_tip:
            lc_root = norm_hex(str((st.get("lc") or {}).get("claim_root") or ""))
            log(f"withdraw sealed {asset} {account} at fpl_h={tip} claim_root={lc_root[:16]}…")
            return tip
        if find_withdraw_note(st, account, asset, amount, dest) and not withdraw_sealed_in_claim_tree(
            st, account, asset, amount, dest
        ):
            log(f"withdraw in mempool/list but not yet in claim_leaves (tip={tip})")
        time.sleep(5)
    raise RuntimeError(f"timeout waiting for {asset} withdraw to seal for {account}")


def run_qc_catchup(start_h: int, end_h: int) -> None:
    if not QC_CATCHUP.exists():
        log(f"skip catchup — missing {QC_CATCHUP}")
        return
    env = os.environ.copy()
    env.setdefault("PL_ROOT", str(ROOT))
    env["FALCON_QC_START_HEIGHT"] = str(start_h)
    env["FALCON_QC_END_HEIGHT"] = str(end_h)
    log(f"qc catchup collect {start_h}..{end_h}")
    subprocess.run(
        [sys.executable, str(QC_CATCHUP), "collect"],
        env=env,
        check=False,
        timeout=3600,
    )


def trigger_qc_prover(fpl_h: int) -> None:
    if not QC_PROVER.exists():
        log(f"skip prover trigger — missing {QC_PROVER}")
        return
    env = os.environ.copy()
    env.setdefault("PL_ROOT", str(ROOT))
    env["FALCON_QC_TARGET_HEIGHT"] = str(fpl_h)
    env["FALCON_QC_ONCE"] = "1"
    log(f"trigger qc prover target_h={fpl_h}")
    subprocess.run(
        [sys.executable, str(QC_PROVER)],
        env=env,
        check=False,
        timeout=7200,
    )


def header_hash_parent(height: int) -> tuple[str, str, bool]:
    """Return (hash, parent, finalized) from on-chain headers(height)."""
    out = cast_call(FALCON_QC_BRIDGE, _headers_sig(), str(height))
    lines = [ln.strip() for ln in out.strip().splitlines() if ln.strip()]
    if len(lines) >= 2:
        fin = lines[-1].lower() in ("true", "0x1", "1")
        return norm_hex(lines[0]), norm_hex(lines[1]), fin
    parts = [p.strip() for p in out.replace("(", "").replace(")", "").split(",")]
    if len(parts) >= 2:
        fin = parts[-1].lower() in ("true", "0x1", "1")
        return norm_hex(parts[0]), norm_hex(parts[1]), fin
    return "", "", False


def assert_sequential_chain(tip: int) -> None:
    """V2: proven height is tip, parent links to tip-1. No jumps."""
    if not FALCON_QC_V2 or tip <= 1:
        return
    h, parent, fin = header_hash_parent(tip)
    if not fin or not h:
        raise RuntimeError(f"v2 tip {tip} not finalized")
    prev_h, _prev_p, prev_fin = header_hash_parent(tip - 1)
    if not prev_fin or not prev_h:
        raise RuntimeError(f"v2 parent height {tip - 1} not finalized (jump?)")
    if parent != prev_h:
        raise RuntimeError(f"v2 parent mismatch tip={tip} parent={parent[:16]} prev_hash={prev_h[:16]}")
    log(f"v2 sequential ok tip={tip} parent={parent[:16]}…")


def wait_header(min_h: int) -> int:
    """Wait until Ethereum has a finalized QC header at or after min_h.

    v1 FalconQcBridge allows sampled jumps, so headers[min_h] may never exist.
    V2 is sequential: headers[min_h] must exist (height == tip+1 chain).
    openClaim uses a submitted height (eth fplTip >= min_h).
    """
    t0 = time.time()
    while time.time() - t0 < HEADER_TIMEOUT:
        tip = eth_fpl_tip()
        if FALCON_QC_V2:
            tip_ok = tip >= min_h and header_finalized(min_h) and header_finalized(tip)
        else:
            tip_ok = tip >= min_h and header_finalized(tip)
        log(f"wait header min_h={min_h} eth_tip={tip} tip_finalized={tip_ok} v2={FALCON_QC_V2}")
        if tip_ok:
            if FALCON_QC_V2:
                assert_sequential_chain(tip)
            return tip
        time.sleep(30)
    raise RuntimeError(f"timeout waiting for Ethereum header >= {min_h}")


def ensure_header_on_eth(min_h: int) -> int:
    tip = eth_fpl_tip()
    if tip >= min_h and header_finalized(tip):
        log(f"header ready min_h={min_h} eth_fpl_tip={tip}")
        return tip
    if not FALCON_QC_V2 and tip + SAMPLE_GAP * 2 < min_h:
        run_qc_catchup(tip + 1, min_h)
    elif FALCON_QC_V2 and tip + 1 < min_h:
        run_qc_catchup(tip + 1, min_h)
    trigger_qc_prover(min_h)
    t0 = time.time()
    nudge = 0
    while time.time() - t0 < HEADER_TIMEOUT:
        tip = eth_fpl_tip()
        tip_ok = tip >= min_h and header_finalized(tip)
        log(f"wait header min_h={min_h} eth_tip={tip} tip_finalized={tip_ok}")
        if tip_ok:
            if FALCON_QC_V2:
                assert_sequential_chain(tip)
            return tip
        nudge += 1
        if nudge % 10 == 0:
            log(f"nudge qc prover for min_h={min_h}")
            trigger_qc_prover(min_h)
        time.sleep(30)
    raise RuntimeError(f"timeout waiting for Ethereum header >= {min_h}")


def load_claim_proof(account: str, asset: str, dest: str, amount: int, fpl_h: int) -> dict:
    env = os.environ.copy()
    env.setdefault("PL_ROOT", str(ROOT))
    env.setdefault("FPL_RPC", RPC)
    out = subprocess.check_output(
        [
            sys.executable,
            str(CLAIM_SCRIPT),
            "--account",
            account,
            "--asset",
            asset,
            "--dest",
            dest,
            "--amount",
            str(amount),
            "--fpl-height",
            str(fpl_h),
        ],
        text=True,
        env=env,
        timeout=60,
    )
    return json.loads(out)


def claim_take(account: str, asset: str, dest: str, amount: int, min_h: int) -> None:
    claim_h = ensure_header_on_eth(min_h)
    t0 = time.time()
    rec: dict | None = None
    while time.time() - t0 < HEADER_TIMEOUT:
        rec = load_claim_proof(account, asset, dest, amount, claim_h)
        if not rec.get("ready"):
            raise RuntimeError(f"claim not ready: {rec}")
        chain_root = on_chain_claim_root(claim_h)
        proof_root = norm_hex(str(rec.get("claimRoot") or ""))
        if chain_root and proof_root and chain_root != proof_root:
            log(
                f"claim_root mismatch at h={claim_h}: chain={chain_root[:16]}… "
                f"proof={proof_root[:16]}… — wait for a newer submitted header"
            )
            trigger_qc_prover(max(min_h, eth_fpl_tip() + 1))
            time.sleep(30)
            claim_h = eth_fpl_tip()
            if claim_h < min_h or not header_finalized(claim_h):
                claim_h = ensure_header_on_eth(min_h)
            continue
        break
    else:
        raise RuntimeError(f"timeout matching claim_root for {asset} min_h={min_h}")
    assert rec is not None
    note = rec["noteId"]
    proof = rec["proof"]
    idx = rec["index"]
    is_usdc = "true" if asset == "USDC" else "false"
    proof_arg = "[" + ",".join(proof) + "]"
    if FALCON_QC_V2:
        assert_sequential_chain(claim_h)
    log(f"openClaim {asset} fplHeight={claim_h} note={note[:18]}…")
    open_tx = cast_send(
        "openClaim(bytes32,address,uint256,bool,bytes32[],uint32,uint64)",
        note,
        dest,
        str(amount),
        is_usdc,
        proof_arg,
        str(idx),
        str(claim_h),
    )
    log(f"OPEN_CLAIM {asset} tx={open_tx}")
    wait_receipt(open_tx)
    take_tx = cast_send("take(bytes32)", note)
    log(f"TAKE {asset} tx={take_tx}")
    wait_receipt(take_tx)


def dest_address() -> str:
    if WITHDRAW_DEST:
        return WITHDRAW_DEST
    return subprocess.check_output(
        [str(CAST), "wallet", "address", "--private-key", load_pk()],
        text=True,
        timeout=15,
    ).strip()


def main() -> int:
    if PRODUCTION:
        log("production mode: public rails + qc prover daemon only")
    if FALCON_QC_V2:
        log("FALCON_QC_V2 sequential lottery-6 (no kickoff, no sample jump)")
    if not CAST.exists():
        raise SystemExit(f"cast not found at {CAST}")
    assets = [a.strip().upper() for a in os.environ.get("FPL_E2E_ASSETS", "ETH,USDC").split(",") if a.strip()]
    ensure_lc()
    dest = dest_address()
    log(f"assets={assets} dest={dest}")

    if os.environ.get("FPL_SKIP_MINT") != "1":
        if "ETH" in assets:
            eth_tx = os.environ.get("FPL_ETH_DEPOSIT_TXID", "").strip()
            if not eth_tx and os.environ.get("FPL_SKIP_DEPOSIT") != "1":
                eth_tx = deposit_eth(ETH_ACCOUNT)
            elif eth_tx:
                log(f"resume ETH deposit {eth_tx}")
            elif os.environ.get("FPL_SKIP_DEPOSIT") == "1":
                raise SystemExit("FPL_ETH_DEPOSIT_TXID required when FPL_SKIP_DEPOSIT=1")
            mint(ETH_ACCOUNT, eth_tx, "ETH")
        if "USDC" in assets:
            usdc_tx = os.environ.get("FPL_USDC_DEPOSIT_TXID", "").strip()
            if not usdc_tx and os.environ.get("FPL_SKIP_DEPOSIT") != "1":
                usdc_tx = deposit_usdc(USDC_ACCOUNT)
            elif usdc_tx:
                log(f"resume USDC deposit {usdc_tx}")
            elif os.environ.get("FPL_SKIP_DEPOSIT") == "1":
                raise SystemExit("FPL_USDC_DEPOSIT_TXID required when FPL_SKIP_DEPOSIT=1")
            mint(USDC_ACCOUNT, usdc_tx, "USDC")
    else:
        log("skip mint phase")

    claim_heights: dict[str, int] = {}
    if os.environ.get("FPL_SKIP_WITHDRAW") != "1":
        withdraw_accounts: list[str] = []
        if "ETH" in assets:
            withdraw_accounts.append(ETH_ACCOUNT)
        if "USDC" in assets:
            withdraw_accounts.append(USDC_ACCOUNT)
        for acct in dict.fromkeys(withdraw_accounts):
            ensure_fpl_balance(acct)
        if "ETH" in assets:
            withdraw(ETH_ACCOUNT, "ETH", ETH_WITHDRAW_WEI, dest)
            claim_heights["ETH"] = withdraw_ledger_height(ETH_ACCOUNT, "ETH", ETH_WITHDRAW_WEI, dest)
        if "USDC" in assets:
            withdraw(USDC_ACCOUNT, "USDC", USDC_WITHDRAW, dest)
            claim_heights["USDC"] = withdraw_ledger_height(USDC_ACCOUNT, "USDC", USDC_WITHDRAW, dest)

    if os.environ.get("FPL_SKIP_CLAIM") != "1":
        if not claim_heights and os.environ.get("FPL_SKIP_WITHDRAW") == "1":
            fpl_h = int(os.environ.get("FPL_CLAIM_HEIGHT", "0") or status().get("tip_height") or 0)
            if fpl_h <= 0:
                raise SystemExit("FPL_CLAIM_HEIGHT required when FPL_SKIP_WITHDRAW=1")
            claim_heights = {a: fpl_h for a in assets}
        for _asset, fpl_h in claim_heights.items():
            ensure_header_on_eth(fpl_h)
        if "ETH" in assets and "ETH" in claim_heights:
            claim_take(ETH_ACCOUNT, "ETH", dest, ETH_WITHDRAW_WEI, claim_heights["ETH"])
        if "USDC" in assets and "USDC" in claim_heights:
            claim_take(USDC_ACCOUNT, "USDC", dest, USDC_WITHDRAW, claim_heights["USDC"])

    log("ETH_USDC_SEPOLIA_E2E_OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
