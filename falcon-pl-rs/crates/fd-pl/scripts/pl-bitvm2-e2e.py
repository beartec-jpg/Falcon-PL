#!/usr/bin/env python3
"""BitVM2 non-custodial e2e on Falcon PL 2300 testnet.

Sally deposit → vault mint → FBTC withdraw (bitvm2) → operator pegout + Claim + attest.

Requires BTC_EXIT_MODE=bitvm2 on the node and operator inventory (dave/fee wallet).
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

EXPLORER = os.environ.get("BTC_EXPLORER", "https://mempool.space/testnet/api").rstrip("/")
ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL_BIN = os.environ.get("FPL_CTL", str(ROOT / "bin/falcon-pl-ctl"))
RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")
ADMIN_SOCK = os.environ.get("FPL_ADMIN_SOCK", str(ROOT / "run/v1.admin.sock"))
KEYS = Path(os.environ.get("FPL_KEYS", str(ROOT / "keys")))
WITHDRAW_KEYS = Path(os.environ.get("FPL_WITHDRAW_KEYS", str(Path.home() / "falcon-pl-vault-test/e2e/keys")))
WALLETS = Path(os.environ.get("FPL_WALLETS", str(Path.home() / "falcon-pl-vault-test/btc-wallets")))
VAULT_KEYS = Path(os.environ.get("FPL_VAULT_KEYS", str(Path.home() / "falcon-pl-vault-test/keys")))
NETWORK_ID = os.environ.get("FPL_NETWORK_ID", "2300")
SCHEME = os.environ.get("FPL_SCHEME", "falcon-512")
WITHDRAW_SCHEME = os.environ.get("FPL_WITHDRAW_SCHEME", "hmac")
MIN_CONFS = int(os.environ.get("FPL_MIN_CONFS", "1"))
DEPOSIT_CONFS = int(os.environ.get("FPL_DEPOSIT_CONFS", "6"))
DEPOSIT_FEE = int(os.environ.get("FPL_DEPOSIT_FEE", "1000"))
WITHDRAW_SATS = int(os.environ.get("FPL_WITHDRAW_SATS", "10000"))
WITHDRAW_DEST = os.environ.get("FPL_WITHDRAW_DEST", "")
PRODUCTION = os.environ.get("FPL_PRODUCTION", "0") == "1"
OPERATOR = ROOT / "scripts/pl-bitvm2-operator.py"
UA = {"User-Agent": "fpl-bitvm2-e2e/1.0"}


def log(msg: str) -> None:
    print(msg, flush=True)


def http(url: str, data: bytes | None = None):
    req = urllib.request.Request(url, data=data, headers=UA)
    if data is not None:
        req.add_header("Content-Type", "text/plain")
    with urllib.request.urlopen(req, timeout=30) as r:
        raw = r.read().decode()
    if not raw:
        return None
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return raw


def broadcast(raw_hex: str) -> str:
    req = urllib.request.Request(
        f"{EXPLORER}/tx",
        data=raw_hex.encode(),
        headers={**UA, "Content-Type": "text/plain"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode().strip()


def tip_height() -> int:
    return int(http(f"{EXPLORER}/blocks/tip/height") or 0)


def tx_status(txid: str) -> dict:
    try:
        return http(f"{EXPLORER}/tx/{txid}") or {}
    except Exception:
        return {}


def confirmed(txid: str, need: int) -> bool:
    st = (tx_status(txid).get("status") or {})
    if not st.get("confirmed"):
        return False
    h = int(st.get("block_height") or 0)
    return tip_height() >= h and (tip_height() - h + 1) >= need


def wait_txid(txid: str, need: int, label: str, timeout: int = 7200) -> None:
    t0 = time.time()
    while time.time() - t0 < timeout:
        if confirmed(txid, need):
            log(f"CONFIRMED {label} {txid} ≥{need}")
            return
        log(f"wait {label} {txid} need={need}")
        time.sleep(20)
    raise SystemExit(f"timeout waiting for {label} {txid}")


def ctl(*args: str) -> str:
    cmd = [CTL_BIN, "--addr", RPC, *args]
    log("+ " + " ".join(cmd[:10]) + (" …" if len(cmd) > 10 else ""))
    return subprocess.check_output(cmd, text=True, timeout=120)


def ctl_try(*args: str) -> tuple[int, str, str]:
    cmd = [CTL_BIN, "--addr", RPC, *args]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    return r.returncode, r.stdout, r.stderr


def admin_ctl(*args: str) -> str:
    sock = Path(ADMIN_SOCK)
    if not sock.exists():
        raise RuntimeError(f"admin sock missing: {sock}")
    cmd = [CTL_BIN, "--admin-sock", str(sock), "--addr", RPC, *args]
    log("+ admin " + " ".join(cmd[4:12]) + (" …" if len(cmd) > 12 else ""))
    return subprocess.check_output(cmd, text=True, timeout=120)


def network_rail() -> dict:
    raw = ctl("status")
    body = json.loads(raw).get("body") or json.loads(raw)
    return next((r for r in body.get("rails") or [] if r.get("asset") == "BTC"), {})


def network_vault_spk() -> str:
    rail = network_rail()
    vp = rail.get("vault_pub") or {}
    spk = str(vp.get("spk_hex") or os.environ.get("FPL_VAULT_SPK") or "").strip()
    if not spk:
        raise SystemExit("BTC rail vault_pub.spk_hex missing from status")
    return spk


def seal_ticks(n: int = 5) -> None:
    sock = Path(ADMIN_SOCK)
    if not sock.exists():
        log(f"skip tick — no admin sock at {sock}")
        return
    for _ in range(n):
        subprocess.run(
            [CTL_BIN, "--admin-sock", str(sock), "--addr", RPC, "tick", "--dt-ms", "200"],
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
        time.sleep(0.5)


def wallet(name: str) -> dict:
    return json.loads((WALLETS / f"{name}.json").read_text())


def fund_wallet(from_name: str, to_name: str, amount: int, fee: int = 1000) -> str:
    src = wallet(from_name)
    dst = wallet(to_name)
    src_addr = src.get("address") or src.get("p2pkh")
    dst_addr = dst.get("address") or dst.get("p2pkh")
    utxos = http(f"{EXPLORER}/address/{src_addr}/utxo") or []
    confirmed = [u for u in utxos if (u.get("status") or {}).get("confirmed")]
    if not confirmed:
        raise SystemExit(f"{from_name} has no confirmed UTXOs to fund {to_name}")
    u = max(confirmed, key=lambda x: int(x.get("value") or 0))
    sats = int(u["value"])
    need = amount + fee
    if sats < need:
        raise SystemExit(f"{from_name} UTXO {sats} < need {need}")
    raw = ctl(
        "btc-pay-tx",
        "--privhex-file",
        str(WALLETS / f"{from_name}.privhex"),
        "--prev-txid",
        u["txid"],
        "--vout",
        str(u["vout"]),
        "--sats",
        str(sats),
        "--dest",
        dst_addr,
        "--amount",
        str(amount),
        "--fee",
        str(fee),
    ).strip().splitlines()[-1]
    txid = broadcast(raw)
    log(f"FUND {from_name}→{to_name} {amount} sats tx={txid}")
    wait_txid(txid, MIN_CONFS, f"fund-{to_name}")
    return txid


def merkle_proof(txid: str) -> tuple[str, int, list[str], str, int]:
    t = tx_status(txid)
    st = t.get("status") or {}
    bh = st.get("block_hash")
    height = int(st.get("block_height") or 0)
    if not bh:
        raise RuntimeError(f"{txid} unconfirmed")
    proof = http(f"{EXPLORER}/tx/{txid}/merkle-proof")
    siblings = proof.get("merkle") or []
    pos = int(proof.get("pos") or 0)
    internal = []
    for s in siblings:
        b = bytes.fromhex(s)
        internal.append(b[::-1].hex())
    hdr = http(f"{EXPLORER}/block/{bh}")
    merkle_root = hdr.get("merkle_root") or ""
    return bh, height, internal, merkle_root, pos


def fetch_bytes(url: str) -> bytes:
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read()


def header_hex(block_hash: str) -> str:
    data = fetch_bytes(f"{EXPLORER}/block/{block_hash}/raw")
    if len(data) >= 80:
        return data[:80].hex()
    text = data.decode("utf-8", errors="ignore").strip()
    if len(text) >= 160:
        return text[:160]
    h = http(f"{EXPLORER}/block/{block_hash}/header")
    if isinstance(h, str):
        return h.strip()
    raise RuntimeError(f"no header for {block_hash}")


def tip_headers_since(start_height: int, end_height: int) -> list[tuple[int, str, str]]:
    out = []
    for h in range(start_height, end_height + 1):
        bh = http(f"{EXPLORER}/block-height/{h}")
        if isinstance(bh, str):
            bh = bh.strip()
        raw = header_hex(bh)
        out.append((h, bh, raw))
    return out


def submit_btc_header(height: int, block_hash: str, raw: str) -> None:
    args = [
        "submit-btc-header",
        "--account",
        "alice",
        "--raw",
        raw,
        "--height",
        str(height),
        "--fee",
        "2",
        "--scheme",
        SCHEME,
        "--keys-dir",
        str(KEYS),
        "--network-id",
        NETWORK_ID,
    ]
    code, out, err = ctl_try(*args)
    if code == 0 and "deferred-until-catchup" not in (out + err):
        return
    if PRODUCTION and not Path(ADMIN_SOCK).exists():
        raise SystemExit(
            f"submit-btc-header h={height} failed in production mode: {(err or out).strip()}"
        )
    sock = Path(ADMIN_SOCK)
    if not sock.exists():
        log(f"header {height} skip/err {(err or out).strip()}")
        return
    blk = http(f"{EXPLORER}/block/{block_hash}") or {}
    admin_ctl(
        "rail-header",
        "--asset",
        "BTC",
        "--height",
        str(height),
        "--hash",
        str(blk.get("id") or block_hash),
        "--parent-hash",
        str(blk.get("previousblockhash") or ""),
        "--merkle-root",
        str(blk.get("merkle_root") or ""),
        "--raw",
        raw,
    )


def submit_headers_for_tx(txid: str) -> None:
    t = tx_status(txid)
    lo = int((t.get("status") or {}).get("block_height") or 0)
    hi = max(lo, tip_height())
    log(f"submitting btc headers {lo}..{hi}")
    for height, bh, raw in tip_headers_since(lo, hi):
        try:
            submit_btc_header(height, bh, raw)
        except SystemExit:
            raise
        except Exception as e:
            log(f"header {height} skip/err {e}")


def ensure_bitvm2_mode() -> None:
    raw = ctl("status")
    body = json.loads(raw).get("body") or json.loads(raw)
    mode = str(body.get("btc_exit_mode") or os.environ.get("BTC_EXIT_MODE") or "watch")
    if mode != "bitvm2":
        raise SystemExit(
            f"node btc_exit_mode={mode} — need bitvm2 (set BTC_EXIT_MODE=bitvm2 and restart)"
        )
    log(f"btc_exit_mode={mode}")


def deposit_sally() -> tuple[str, int]:
    vault_spk = network_vault_spk()
    log(f"network vault spk={vault_spk[:20]}…")
    sally = wallet("sally")
    addr = sally.get("address") or sally.get("p2pkh") or sally["address"]
    utxos = http(f"{EXPLORER}/address/{addr}/utxo") or []
    confirmed_utxos = [u for u in utxos if (u.get("status") or {}).get("confirmed")]
    if not confirmed_utxos:
        fund_from = os.environ.get("FPL_FUND_FROM", "dave").strip()
        fund_amt = int(os.environ.get("FPL_FUND_SATS", "40000"))
        log(f"sally empty — funding {fund_amt} from {fund_from}")
        fund_wallet(fund_from, "sally", fund_amt)
        utxos = http(f"{EXPLORER}/address/{addr}/utxo") or []
        confirmed_utxos = [u for u in utxos if (u.get("status") or {}).get("confirmed")]
    if not confirmed_utxos:
        raise SystemExit(f"sally {addr} has no confirmed UTXOs — fund and wait")
    u = max(confirmed_utxos, key=lambda x: int(x.get("value") or 0))
    sats = int(u["value"])
    if sats <= DEPOSIT_FEE + WITHDRAW_SATS + 5000:
        raise SystemExit(f"sally UTXO too small ({sats} sats) for deposit+withdraw test")
    raw = ctl(
        "btc-deposit-tx",
        "--privhex-file",
        str(WALLETS / "sally.privhex"),
        "--prev-txid",
        u["txid"],
        "--vout",
        str(u["vout"]),
        "--sats",
        str(sats),
        "--dest-account",
        "sally",
        "--vault-spk",
        vault_spk,
        "--fee",
        str(DEPOSIT_FEE),
    ).strip().splitlines()[-1]
    txid = broadcast(raw)
    log(f"DEPOSIT_BROADCAST sally {txid} vault_amt={sats - DEPOSIT_FEE}")
    wait_txid(txid, DEPOSIT_CONFS, "vault-deposit")
    submit_headers_for_tx(txid)
    return txid, sats - DEPOSIT_FEE


def wait_rail_mint_confs(deposit_txid: str, min_conf: int | None = None, timeout: int = 7200) -> None:
    """Advance BTC rail headers until deposit has min_confirmations at the rail tip."""
    if min_conf is None:
        min_conf = int(os.environ.get("FPL_BTC_MIN_CONFIRMATIONS", "6") or "6")
    t = tx_status(deposit_txid)
    dep_h = int((t.get("status") or {}).get("block_height") or 0)
    t0 = time.time()
    while time.time() - t0 < timeout:
        tip = tip_height()
        need_tip = dep_h + min_conf - 1
        raw = ctl("status")
        body = json.loads(raw).get("body") or json.loads(raw)
        rail = next((r for r in body.get("rails") or [] if r.get("asset") == "BTC"), {})
        rail_tip = int(rail.get("tip_height") or 0)
        min_c = int(os.environ.get("FPL_BTC_MIN_CONFIRMATIONS") or rail.get("min_confirmations") or min_conf)
        if rail_tip >= dep_h + min_c - 1:
            log(f"rail confs ok rail_tip={rail_tip} dep_h={dep_h} min={min_c}")
            return
        end = max(tip, need_tip)
        log(f"submitting headers through {end} (rail_tip={rail_tip} need>={need_tip})")
        for h in range(rail_tip + 1, end + 1):
            try:
                bh = http(f"{EXPLORER}/block-height/{h}")
                if isinstance(bh, str):
                    bh = bh.strip()
                raw_hdr = header_hex(bh)
                submit_btc_header(h, bh, raw_hdr)
            except (subprocess.CalledProcessError, RuntimeError, OSError, SystemExit) as e:
                log(f"header {h} skip: {e}")
        time.sleep(20)
    raise SystemExit(f"timeout waiting for rail confs on {deposit_txid}")


def mint_sally(deposit_txid: str, amount: int, raw_tx: str) -> None:
    wait_rail_mint_confs(deposit_txid)
    bh, height, siblings, merkle, pos = merkle_proof(deposit_txid)
    args = [
        "rail-deposit",
        "--asset",
        "BTC",
        "--to",
        "sally",
        "--amount",
        str(amount),
        "--external-txid",
        deposit_txid,
        "--block-hash",
        bh,
        "--height",
        str(height),
        "--merkle-root",
        merkle,
        "--raw-tx",
        raw_tx,
        "--merkle-path",
        ",".join(siblings),
        "--merkle-index",
        str(pos),
    ]
    pub_args = [
        "submit-rail-deposit",
        "--account",
        "alice",
        "--asset",
        "BTC",
        "--to",
        "sally",
        "--amount",
        str(amount),
        "--external-txid",
        deposit_txid,
        "--block-hash",
        bh,
        "--height",
        str(height),
        "--merkle-root",
        merkle,
        "--raw-tx",
        raw_tx,
        "--merkle-path",
        ",".join(siblings),
        "--merkle-index",
        str(pos),
        "--scheme",
        SCHEME,
        "--keys-dir",
        str(KEYS),
        "--network-id",
        NETWORK_ID,
    ]
    code, out, err = ctl_try(*pub_args)
    if code == 0 and "deferred-until-catchup" not in (out + err):
        seal_ticks()
        log(f"MINTED sally {amount} FBTC (submit-rail-deposit)")
        return
    if PRODUCTION:
        raise SystemExit(
            f"submit-rail-deposit failed in production mode: {(err or out).strip()}"
        )
    log(f"submit-rail-deposit unavailable ({err.strip() or out.strip()}) — admin rail-deposit")
    admin_ctl(*args)
    log(f"MINTED sally {amount} FBTC (admin)")


def withdraw_bitvm2(from_acct: str, amount: int, dest: str) -> str:
    code, out, err = ctl_try(
        "submit-rail-withdraw",
        "--account",
        from_acct,
        "--asset",
        "BTC",
        "--amount",
        str(amount),
        "--external-to",
        dest,
        "--scheme",
        WITHDRAW_SCHEME,
        "--keys-dir",
        str(WITHDRAW_KEYS),
        "--network-id",
        NETWORK_ID,
        "--fee",
        "2",
    )
    if code == 0 and "deferred-until-catchup" not in (out + err):
        seal_ticks()
        log(f"WITHDRAW_SUBMITTED {out[-300:]}")
        return out
    if PRODUCTION:
        raise SystemExit(
            f"submit-rail-withdraw failed in production mode: {(err or out).strip()}"
        )
    log(f"submit-rail-withdraw unavailable ({err.strip() or out.strip()}) — admin rail-withdraw")
    out = admin_ctl(
        "rail-withdraw",
        "--asset",
        "BTC",
        "--from",
        from_acct,
        "--amount",
        str(amount),
        "--external-to",
        dest,
    )
    log(f"WITHDRAW_SUBMITTED {out[-300:]}")
    return out


def wait_sally_balance(need: int, timeout: int = 300) -> None:
    t0 = time.time()
    while time.time() - t0 < timeout:
        raw = ctl("account", "--account", "sally")
        body = json.loads(raw).get("body") or json.loads(raw)
        assets = body.get("assets") or {}
        bal = int(assets.get("BTC") or 0)
        if bal >= need:
            log(f"sally BTC balance={bal}")
            return
        time.sleep(5)
    raise SystemExit("timeout waiting for sally BTC balance after mint")


def run_operator_once() -> None:
    env = os.environ.copy()
    env.setdefault("FALCON_PL_CTL", CTL_BIN)
    env.setdefault("FALCON_PL_KEYS", str(KEYS))
    env.setdefault("FALCON_PL_RPC", RPC)
    subprocess.run([sys.executable, str(OPERATOR), "--once"], check=True, env=env)


def wait_claim_done(note_hint: str = "", timeout: int = 600) -> None:
    t0 = time.time()
    while time.time() - t0 < timeout:
        raw = ctl("status")
        body = json.loads(raw).get("body") or json.loads(raw)
        rail = next((r for r in body.get("rails") or [] if r.get("asset") == "BTC"), {})
        for c in rail.get("bitvm2_claims") or []:
            st = str(c.get("status") or "")
            if note_hint and note_hint not in str(c.get("note_id") or ""):
                continue
            if st in ("claim_posted", "paid_optimistic", "pegout_posted", "ClaimPosted", "PegoutPosted"):
                log(f"CLAIM_DONE status={st} note={c.get('note_id')} pegout={c.get('pegout_txid_hex')}")
                return
        time.sleep(10)
    raise SystemExit("timeout waiting for bitvm2 claim completion")


def main() -> int:
    if PRODUCTION:
        log("production mode: public SubmitTx only (admin rails disabled)")
    ensure_bitvm2_mode()
    fee = wallet("fee")
    dest = WITHDRAW_DEST or str(fee.get("address") or fee.get("p2pkh"))
    log(f"withdraw dest={dest} amount={WITHDRAW_SATS}")

    deposit_txid = os.environ.get("FPL_DEPOSIT_TXID", "").strip()
    mint_amt = int(os.environ.get("FPL_MINT_AMOUNT", "0") or "0")
    if os.environ.get("FPL_SKIP_TO_WITHDRAW") == "1":
        log("skip deposit/mint — withdraw only")
    elif deposit_txid and mint_amt > 0 and os.environ.get("FPL_FORCE_DEPOSIT") != "1":
        log(f"resume deposit={deposit_txid} mint_amt={mint_amt}")
        submit_headers_for_tx(deposit_txid)
        raw_hex = http(f"{EXPLORER}/tx/{deposit_txid}/hex")
        if not isinstance(raw_hex, str):
            raise SystemExit("could not fetch deposit raw tx")
        mint_sally(deposit_txid, mint_amt, raw_hex.strip())
        wait_sally_balance(WITHDRAW_SATS)
    else:
        if deposit_txid:
            log("FPL_FORCE_DEPOSIT=1 — new deposit to network vault")
        deposit_txid, mint_amt = deposit_sally()
        raw_hex = http(f"{EXPLORER}/tx/{deposit_txid}/hex")
        if not isinstance(raw_hex, str):
            raise SystemExit("could not fetch deposit raw tx")
        mint_sally(deposit_txid, mint_amt, raw_hex.strip())
        wait_sally_balance(WITHDRAW_SATS)

    withdraw_bitvm2("sally", WITHDRAW_SATS, dest)
    log("running operator --once")
    run_operator_once()
    wait_claim_done()
    log("BITVM2_E2E_OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
