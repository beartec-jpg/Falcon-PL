#!/usr/bin/env python3
"""BitVM2 operator sidecar (Phase D production).

Polls FPL for open BitVM2 withdraw claims, fronts BTC on testnet, posts Claim
OP_RETURN, and attests on FPL.  Progress is checkpointed locally so restarts
and transient mesh lag are safe.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

ROOT = Path(os.environ.get("PL_ROOT", Path.home() / "falcon-pl-public-testnet-2300"))
CTL = os.environ.get("FALCON_PL_CTL", str(ROOT / "bin/falcon-pl-ctl"))
KEYS = os.environ.get("FALCON_PL_KEYS", str(ROOT / "keys"))
ADDR = os.environ.get("FALCON_PL_RPC", "127.0.0.1:19301")
SYNCED_ADDR = os.environ.get("FALCON_PL_RPC_SYNCED", "").strip()
NID = os.environ.get("FALCON_PL_NETWORK_ID", "2300")
OPERATOR = os.environ.get("BITVM2_OPERATOR_ACCOUNT", "v1")
SCHEME = os.environ.get("FALCON_PL_SCHEME", "falcon-512")
POLL_SECS = int(os.environ.get("BITVM2_POLL_SECS", "15"))
MIN_CONFS = int(os.environ.get("BITVM2_MIN_CONFS", "1"))
CLAIM_FEE_SATS = int(os.environ.get("BITVM2_CLAIM_FEE_SATS", "454"))
MIN_INVENTORY_SATS = int(os.environ.get("BITVM2_MIN_INVENTORY_SATS", "15000"))
DUST_SATS = 546
ATTEST_TIMEOUT = int(os.environ.get("BITVM2_ATTEST_TIMEOUT", "180"))
ATTEST_RETRIES = int(os.environ.get("BITVM2_ATTEST_RETRIES", "8"))
CATCHUP_BACKOFF = float(os.environ.get("BITVM2_CATCHUP_BACKOFF", "30"))
CHECKPOINT = Path(
    os.environ.get("BITVM2_CHECKPOINT", str(ROOT / "run/bitvm2-operator.json"))
)
PRIVHEX = os.environ.get(
    "BITVM2_PRIVHEX_FILE",
    str(Path.home() / "falcon-pl-vault-test/btc-wallets/dave.privhex"),
)
OPERATOR_ADDR = os.environ.get("BITVM2_OPERATOR_ADDR", "")
EXPLORER = os.environ.get("BTC_EXPLORER", "https://mempool.space/testnet/api").rstrip("/")
PROVE_BIN = os.environ.get(
    "FALCON_PL_BITVM2_PROVE",
    str(ROOT / "bin/falcon-pl-bitvm2-prove"),
)
ADMIN_SOCK = os.environ.get("FPL_ADMIN_SOCK", str(ROOT / "run/v1.admin.sock"))
UA = {"User-Agent": "fpl-bitvm2-operator/2.0"}
CTL_BTC = os.environ.get(
    "FALCON_PL_CTL_BTC",
    str(ROOT / "bin/falcon-pl-ctl-bitvm2"),
)
INSTANCE_JSON = Path(
    os.environ.get("BITVM2_INSTANCE_JSON", str(ROOT / "run/bitvm2-instance.json"))
)
INSTANCE_PAYOUT = False  # A1 dave-fronting retired
PAYOUT_FEE_SATS = int(os.environ.get("BITVM2_PAYOUT_FEE_SATS", "1000"))
KICKOFF_FEE_SATS = int(os.environ.get("BITVM2_KICKOFF_FEE_SATS", "1000"))
CLAIM_CSV = int(os.environ.get("BITVM2_CLAIM_CSV", "6"))
# #47: Kickoff signs with the instance claimer key only (sign_kickoff). Setup
# secrets are not read by this sidecar.
CLAIMER_FILE = os.environ.get("BITVM2_CLAIMER_FILE") or PRIVHEX


def log(msg: str) -> None:
    print(msg, flush=True)


def load_checkpoint() -> dict[str, Any]:
    if not CHECKPOINT.exists():
        return {"jobs": {}}
    try:
        return json.loads(CHECKPOINT.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return {"jobs": {}}


def save_checkpoint(data: dict[str, Any]) -> None:
    CHECKPOINT.parent.mkdir(parents=True, exist_ok=True)
    tmp = CHECKPOINT.with_suffix(".tmp")
    tmp.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(CHECKPOINT)


def job(data: dict[str, Any], note_id: str) -> dict[str, Any]:
    jobs = data.setdefault("jobs", {})
    if note_id not in jobs:
        jobs[note_id] = {"note_id": note_id, "phase": "new", "updated_at": time.time()}
    return jobs[note_id]


def ctl(*args: str, rpc: str | None = None, timeout: int = 120) -> str:
    target = rpc or ADDR
    cmd = [CTL, "--addr", target, *args]
    env = os.environ.copy()
    if ADMIN_SOCK and os.path.exists(ADMIN_SOCK):
        env["FPL_ADMIN_SOCK"] = ADMIN_SOCK
    p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, env=env)
    if p.returncode != 0:
        raise RuntimeError((p.stderr or p.stdout or f"exit {p.returncode}").strip())
    return p.stdout


def ctl_try(*args: str, rpc: str | None = None, timeout: int = 120) -> tuple[int, str, str]:
    target = rpc or ADDR
    cmd = [CTL, "--addr", target, *args]
    env = os.environ.copy()
    if ADMIN_SOCK and os.path.exists(ADMIN_SOCK):
        env["FPL_ADMIN_SOCK"] = ADMIN_SOCK
    p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, env=env)
    return p.returncode, p.stdout, p.stderr


def http_json(url: str, data: bytes | None = None, timeout: int = 30) -> Any:
    req = urllib.request.Request(url, data=data, headers=UA)
    if data is not None:
        req.add_header("Content-Type", "text/plain")
    with urllib.request.urlopen(req, timeout=timeout) as r:
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


def ctl_btc(*args: str, timeout: int = 60) -> str:
    bin_ = CTL_BTC if Path(CTL_BTC).is_file() else CTL
    p = subprocess.run([bin_, *args], capture_output=True, text=True, timeout=timeout)
    if p.returncode != 0:
        raise RuntimeError((p.stderr or p.stdout or f"exit {p.returncode}").strip()[-800:])
    return p.stdout


def load_instance() -> dict[str, Any] | None:
    if not INSTANCE_JSON.exists():
        return None
    try:
        return json.loads(INSTANCE_JSON.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return None


def instance_csv_mature(inst: dict[str, Any]) -> tuple[bool, str]:
    txid = str(inst.get("funding_txid") or "")
    csv = int(inst.get("csv_blocks") or 6)
    if not txid:
        return False, "no funding_txid"
    t = http_json(f"{EXPLORER}/tx/{txid}") or {}
    st = t.get("status") or {}
    if not st.get("confirmed"):
        return False, "instance fund unconfirmed"
    h = int(st.get("block_height") or 0)
    tip = int(http_json(f"{EXPLORER}/blocks/tip/height") or 0)
    need = h + csv
    if tip < need:
        return False, f"csv wait tip={tip} need>={need}"
    return True, f"mature tip={tip} fund_h={h} csv={csv}"


def instance_utxo_live(inst: dict[str, Any]) -> bool:
    addr = str(inst.get("address") or "")
    if not addr:
        return False
    utxos = http_json(f"{EXPLORER}/address/{addr}/utxo") or []
    return bool(utxos)


def abort_preimage(note_id: str) -> bytes:
    seed_path = Path(KEYS) / "bitvm-abort-seed"
    seed = seed_path.read_bytes()[:32]
    if len(seed) < 32:
        raise RuntimeError("bitvm-abort-seed missing")
    import hashlib

    h = hashlib.sha256()
    h.update(b"fpl-bitvm-abort-v1|")
    h.update(seed)
    h.update(note_id.encode())
    return h.digest()


def abort_commit(pre: bytes) -> bytes:
    import hashlib

    return hashlib.sha256(pre).digest()


def save_abort_secret(note_id: str, pre: bytes) -> None:
    d = Path(KEYS) / "bitvm-abort"
    d.mkdir(parents=True, exist_ok=True)
    p = d / note_id
    p.write_text(pre.hex() + "\n", encoding="utf-8")
    os.chmod(p, 0o600)


def live_instance_utxo(inst: dict[str, Any]) -> dict[str, Any] | None:
    addr = str(inst.get("address") or "")
    utxos = http_json(f"{EXPLORER}/address/{addr}/utxo") or []
    conf = [u for u in utxos if (u.get("status") or {}).get("confirmed")]
    if not conf:
        return None
    return max(conf, key=lambda u: int(u.get("value") or 0))


def kickoff_user_from_instance(note_id: str, dest: str, amount: int, inst: dict[str, Any]) -> str:
    u = live_instance_utxo(inst)
    if not u:
        raise RuntimeError("instance has no confirmed UTXO")
    sats = int(u["value"])
    if sats < amount + KICKOFF_FEE_SATS + DUST_SATS:
        raise RuntimeError(f"instance {sats} cannot pay dest-lock {amount}+fee")
    pre = abort_preimage(note_id)
    commit = abort_commit(pre)
    save_abort_secret(note_id, pre)
    # #47: match falcon-pl-ctl btc-bitvm2-user-kickoff (claimer key + setup x-only).
    # The Kickoff path reads no setup secret; setup keys are presign-only.
    setup_x = [str(x) for x in (inst.get("setup_xonly") or [])]
    if not setup_x:
        raise RuntimeError("instance has no setup_xonly")
    raw = ctl_btc(
        "btc-bitvm2-user-kickoff",
        "--utxo",
        f"{u['txid']}:{int(u['vout'])}:{sats}",
        "--claimer-secret-file",
        CLAIMER_FILE,
        "--setup-xonly",
        ",".join(setup_x),
        "--dest",
        dest,
        "--amount",
        str(amount),
        "--abort-commit-hex",
        commit.hex(),
        "--claim-csv",
        str(CLAIM_CSV),
        "--fee",
        str(KICKOFF_FEE_SATS),
        "--csv",
        str(int(inst.get("csv_blocks") or 6)),
        "--assert-csv",
        str(int(inst.get("assert_csv_blocks") or 6)),
    ).strip().splitlines()[-1]
    txid = broadcast(raw)
    inst["funding_txid"] = txid
    inst["funding_vout"] = 1
    inst["funding_sats"] = sats - KICKOFF_FEE_SATS - amount
    inst.pop("payout_txid", None)
    INSTANCE_JSON.write_text(json.dumps(inst, indent=2) + "\n", encoding="utf-8")
    return txid


def payout_instance_to_operator(inst: dict[str, Any]) -> str:
    sats = int(inst.get("funding_sats") or 0)
    if sats <= PAYOUT_FEE_SATS + DUST_SATS:
        raise RuntimeError("instance too small to payout")
    setup = ",".join(inst.get("setup_xonly") or [])
    utxo = f"{inst['funding_txid']}:{int(inst.get('funding_vout') or 0)}:{sats}"
    csv = str(int(inst.get("csv_blocks") or 6))
    acsv = str(int(inst.get("assert_csv_blocks") or csv))
    raw = ctl_btc(
        "btc-bitvm2-payout",
        "--utxo",
        utxo,
        "--operator-secret-file",
        PRIVHEX,
        "--setup-xonly",
        setup,
        "--dest",
        operator_address(),
        "--fee",
        str(PAYOUT_FEE_SATS),
        "--csv",
        csv,
        "--assert-csv",
        acsv,
    ).strip().splitlines()[-1]
    return broadcast(raw)


def operator_address() -> str:
    if OPERATOR_ADDR:
        return OPERATOR_ADDR
    w = json.loads(Path(PRIVHEX.replace(".privhex", ".json")).read_text(encoding="utf-8"))
    return str(w.get("address") or w.get("p2pkh") or "").strip()


def confirmed_balance(addr: str) -> int:
    utxos = http_json(f"{EXPLORER}/address/{addr}/utxo") or []
    return sum(int(u.get("value") or 0) for u in utxos if (u.get("status") or {}).get("confirmed"))


def tx_confirmed(txid: str, min_confs: int = MIN_CONFS) -> bool:
    t = http_json(f"{EXPLORER}/tx/{txid}") or {}
    st = t.get("status") or {}
    if not st.get("confirmed"):
        return False
    tip = int(http_json(f"{EXPLORER}/blocks/tip/height") or 0)
    bh = int(st.get("block_height") or 0)
    return tip >= bh and (tip - bh + 1) >= min_confs


def best_utxo(addr: str, *, allow_unconfirmed: bool = False) -> dict:
    utxos = http_json(f"{EXPLORER}/address/{addr}/utxo") or []
    confirmed = [u for u in utxos if (u.get("status") or {}).get("confirmed")]
    pool = confirmed or (utxos if allow_unconfirmed else [])
    if not pool:
        raise RuntimeError(f"operator {addr} has no UTXOs")
    return max(pool, key=lambda x: int(x.get("value") or 0))


def pegout_fee_sats(input_sats: int, amount: int) -> int:
    """Fee leaving ≥ DUST change for the claim tx, or max fee with dust absorbed."""
    max_fee = max(0, input_sats - amount)
    for fee in range(500, max_fee + 1):
        change = input_sats - amount - fee
        if change >= DUST_SATS + CLAIM_FEE_SATS:
            return fee
    return max(500, max_fee)


def status_rpc() -> tuple[dict, str]:
    for rpc in [ADDR, SYNCED_ADDR]:
        if not rpc:
            continue
        try:
            raw = ctl("status", rpc=rpc, timeout=30)
            body = json.loads(raw).get("body") or json.loads(raw)
            return body, rpc
        except Exception:
            continue
    raise RuntimeError("no FPL status from primary or synced RPC")


def rail_btc(st: dict) -> dict:
    for r in st.get("rails") or []:
        if str(r.get("asset") or "").upper() == "BTC":
            return r
    raise RuntimeError("no BTC rail in status")


def prove_withdraw(
    note_id: str,
    dest: str,
    amount: int,
    pegout_txid: str,
    tip_height: int,
    tip_hash: str,
    leaves: list,
) -> dict:
    cmd = [
        PROVE_BIN,
        "--note-id",
        note_id,
        "--dest",
        dest,
        "--amount",
        str(amount),
        "--pegout-txid",
        pegout_txid,
        "--height",
        str(tip_height),
        "--tip-hash",
        tip_hash,
        "--leaves-json",
        json.dumps(leaves),
    ]
    p = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
    if p.returncode != 0:
        raise RuntimeError((p.stderr or p.stdout or "prove failed")[-800:])
    return json.loads(p.stdout)


def sign_and_broadcast_pay(dest: str, amount_sats: int, fee_sats: int) -> str:
    addr = operator_address()
    u = best_utxo(addr)
    sats = int(u["value"])
    raw = ctl(
        "btc-pay-tx",
        "--privhex-file",
        PRIVHEX,
        "--prev-txid",
        u["txid"],
        "--vout",
        str(int(u["vout"])),
        "--sats",
        str(sats),
        "--dest",
        dest,
        "--amount",
        str(amount_sats),
        "--fee",
        str(fee_sats),
    ).strip().splitlines()[-1]
    return broadcast(raw)


def sign_and_broadcast_claim(statement_digest_hex: str, fee_sats: int = CLAIM_FEE_SATS) -> str:
    addr = operator_address()
    u = best_utxo(addr, allow_unconfirmed=True)
    sats = int(u["value"])
    raw = ctl(
        "btc-claim-tx",
        "--privhex-file",
        PRIVHEX,
        "--prev-txid",
        u["txid"],
        "--vout",
        str(int(u["vout"])),
        "--sats",
        str(sats),
        "--statement-digest-hex",
        statement_digest_hex,
        "--fee",
        str(fee_sats),
    ).strip().splitlines()[-1]
    return broadcast(raw)


def record_pegout_on_fpl(note_id: str, pegout_txid: str) -> None:
    if not ADMIN_SOCK or not os.path.exists(ADMIN_SOCK):
        return
    ctl(
        "--admin-sock",
        ADMIN_SOCK,
        "rail-bitvm2-pegout",
        "--note-id",
        note_id,
        "--operator",
        OPERATOR,
        "--pegout-txid-hex",
        pegout_txid,
    )


def submit_attest_public(
    note_id: str,
    pegout_txid: str,
    claim_txid: str,
    proof_hex: str,
    public_hex: list,
    digest: str,
    rpc: str,
) -> tuple[bool, str]:
    code, out, err = ctl_try(
        "submit-bitvm2-attest",
        "--account",
        OPERATOR,
        "--note-id",
        note_id,
        "--pegout-txid-hex",
        pegout_txid,
        "--claim-txid-hex",
        claim_txid,
        "--proof-hex",
        proof_hex,
        "--statement-digest-hex",
        digest,
        "--public-hex",
        ",".join(public_hex),
        "--scheme",
        SCHEME,
        "--keys-dir",
        KEYS,
        "--network-id",
        NID,
        "--fee",
        "2",
        rpc=rpc,
        timeout=ATTEST_TIMEOUT,
    )
    msg = (out + err).strip()
    if code == 0 and "deferred-until-catchup" not in msg:
        return True, msg
    return False, msg


def submit_attest_admin(
    note_id: str,
    pegout_txid: str,
    claim_txid: str,
    proof_hex: str,
    public_hex: list,
    digest: str,
) -> None:
    if not ADMIN_SOCK or not os.path.exists(ADMIN_SOCK):
        raise RuntimeError("attest blocked: no admin sock and public SubmitTx unavailable")
    ctl(
        "--admin-sock",
        ADMIN_SOCK,
        "rail-bitvm2-attest",
        "--note-id",
        note_id,
        "--operator",
        OPERATOR,
        "--pegout-txid-hex",
        pegout_txid,
        "--claim-txid-hex",
        claim_txid,
        "--proof-hex",
        proof_hex,
        "--statement-digest-hex",
        digest,
        "--public-hex",
        ",".join(public_hex),
        timeout=ATTEST_TIMEOUT,
    )


def submit_attest_with_retry(
    note_id: str,
    pegout_txid: str,
    claim_txid: str,
    proved: dict,
    rpc: str,
) -> None:
    digest = proved["statement_digest_hex"]
    proof_hex = proved["proof_hex"]
    public_hex = proved["public_hex"]
    last_err = ""
    for attempt in range(ATTEST_RETRIES):
        ok, msg = submit_attest_public(
            note_id, pegout_txid, claim_txid, proof_hex, public_hex, digest, rpc
        )
        if ok:
            log(f"  attest submitted (public) for {note_id[:16]}")
            return
        last_err = msg
        if "deferred-until-catchup" in msg:
            log(f"  attest catchup wait ({attempt + 1}/{ATTEST_RETRIES}): {msg[:80]}")
            time.sleep(CATCHUP_BACKOFF)
            continue
        break
    log(f"  public attest unavailable ({last_err[:120]}) — admin rail-bitvm2-attest")
    submit_attest_admin(note_id, pegout_txid, claim_txid, proof_hex, public_hex, digest)
    log(f"  attest submitted (admin) for {note_id[:16]}")


def claim_done_on_fpl(note_id: str, rpc: str) -> bool:
    try:
        raw = ctl("status", rpc=rpc, timeout=30)
        body = json.loads(raw).get("body") or json.loads(raw)
        rail = rail_btc(body)
        for c in rail.get("bitvm2_claims") or []:
            if str(c.get("note_id")) != note_id:
                continue
            st = str(c.get("status") or "").lower()
            return st in ("claim_posted", "paid_optimistic")
    except Exception:
        pass
    return False


def process_job(j: dict[str, Any], st: dict, leaves: list, rpc: str, ckpt: dict[str, Any]) -> bool:
    note_id = str(j["note_id"])
    dest = str(j.get("dest") or "")
    amount = int(j.get("amount") or 0)
    phase = str(j.get("phase") or "new")
    _ = leaves
    _ = rpc

    # A1 dave-fronting / instance→operator payout is retired.
    if phase in (
        "pegout_pending",
        "pegout_broadcast",
        "pegout_confirmed",
        "claim_broadcast",
        "instance_payout_wait",
        "instance_paid",
        "attest_done",
    ):
        log(f"drop A1 job {note_id[:16]} phase={phase} (no operator front)")
        ckpt.get("jobs", {}).pop(note_id, None)
        save_checkpoint(ckpt)
        return True

    if phase in ("new", "kickoff_pending"):
        inst = load_instance()
        if not inst:
            raise RuntimeError("no bitvm2 instance")
        log(f"kickoff {note_id[:16]} dest-lock amount={amount} (instance, no dave front)")
        txid = kickoff_user_from_instance(note_id, dest, amount, inst)
        j["kickoff_txid"] = txid
        j["pegout_txid"] = txid
        j["phase"] = "kickoff_broadcast"
        j["updated_at"] = time.time()
        save_checkpoint(ckpt)
        log(f"  kickoff txid={txid}")
        phase = j["phase"]

    if phase == "kickoff_broadcast":
        txid = str(j.get("kickoff_txid") or "")
        if not txid:
            j["phase"] = "kickoff_pending"
            save_checkpoint(ckpt)
            return False
        if not tx_confirmed(txid):
            log(f"  wait kickoff confirm {txid[:16]}")
            return False
        try:
            record_pegout_on_fpl(note_id, txid)
        except Exception as e:
            log(f"  record kickoff on FPL: {e}")
        log(f"  kickoff confirmed {txid[:16]} — user takes dest-lock after CSV")
        ckpt.get("jobs", {}).pop(note_id, None)
        save_checkpoint(ckpt)
        return True

    if phase == "pegout_confirmed":
        pegout = str(j["pegout_txid"])
        proved = prove_withdraw(note_id, dest, amount, pegout, tip_h, tip_hash, leaves)
        j["proved"] = proved
        claim_tx = sign_and_broadcast_claim(proved["statement_digest_hex"])
        j["claim_txid"] = claim_tx
        j["phase"] = "claim_broadcast"
        j["updated_at"] = time.time()
        save_checkpoint(ckpt)
        log(f"  claim txid={claim_tx}")
        phase = j["phase"]

    if phase == "claim_broadcast":
        claim_tx = str(j.get("claim_txid") or "")
        if claim_tx and not tx_confirmed(claim_tx):
            log(f"  wait claim confirm {claim_tx[:16]}")
            return False
        proved = j.get("proved") or {}
        pegout = str(j.get("pegout_txid") or "")
        if not proved or not pegout or not claim_tx:
            j["phase"] = "pegout_confirmed"
            save_checkpoint(ckpt)
            return False
        submit_attest_with_retry(note_id, pegout, claim_tx, proved, rpc)
        inst = load_instance()
        j["phase"] = (
            "instance_payout_wait"
            if INSTANCE_PAYOUT and inst and instance_utxo_live(inst)
            else "attest_done"
        )
        j["updated_at"] = time.time()
        save_checkpoint(ckpt)
        phase = j["phase"]

    if phase == "instance_payout_wait":
        inst = load_instance()
        if not inst or not INSTANCE_PAYOUT:
            j["phase"] = "attest_done"
            save_checkpoint(ckpt)
            return True
        if not instance_utxo_live(inst):
            log("  instance already spent")
            j["phase"] = "instance_paid"
            save_checkpoint(ckpt)
            ckpt.get("jobs", {}).pop(note_id, None)
            return True
        ok, why = instance_csv_mature(inst)
        if not ok:
            log(f"  wait instance csv ({why})")
            return False
        txid = payout_instance_to_operator(inst)
        j["instance_payout_txid"] = txid
        j["phase"] = "instance_paid"
        j["updated_at"] = time.time()
        save_checkpoint(ckpt)
        log(f"  instance payout (no watch key) txid={txid}")
        inst["payout_txid"] = txid
        INSTANCE_JSON.write_text(json.dumps(inst, indent=2) + "\n", encoding="utf-8")
        ckpt.get("jobs", {}).pop(note_id, None)
        return True

    if phase in ("attest_done", "instance_paid"):
        ckpt.get("jobs", {}).pop(note_id, None)
        save_checkpoint(ckpt)
        return True

    return False


def discover_jobs(st: dict, ckpt: dict[str, Any]) -> None:
    rail = rail_btc(st)
    for c in rail.get("bitvm2_claims") or []:
        st_name = str(c.get("status") or "").lower()
        if st_name != "awaiting_pegout":
            continue
        note_id = str(c["note_id"])
        j = job(ckpt, note_id)
        j.setdefault("dest", str(c.get("dest") or ""))
        j.setdefault("amount", int(c.get("amount") or 0))
        if j.get("phase") == "new":
            j["phase"] = "kickoff_pending"
        j["updated_at"] = time.time()
    save_checkpoint(ckpt)


def process_once() -> int:
    st, rpc = status_rpc()
    mode = str(st.get("btc_exit_mode") or os.environ.get("BTC_EXIT_MODE") or "watch")
    if mode != "bitvm2":
        log(f"btc_exit_mode={mode} — idle")
        return 0
    ckpt = load_checkpoint()
    discover_jobs(st, ckpt)
    leaves = list(rail_btc(st).get("bitvm2_claim_leaves") or [])
    done = 0
    for note_id, j in list(ckpt.get("jobs", {}).items()):
        try:
            if process_job(j, st, leaves, rpc, ckpt):
                done += 1
        except Exception as e:
            j["last_error"] = str(e)
            j["updated_at"] = time.time()
            save_checkpoint(ckpt)
            log(f"job {note_id[:16]} error: {e}")
    return done


def main() -> None:
    once = "--once" in sys.argv
    inst = load_instance()
    inst_addr = (inst or {}).get("address") or "none"
    log(
        f"bitvm2 operator A2 dest-lock instance={inst_addr} (no dave front, no watch)"
    )
    while True:
        try:
            n = process_once()
            if n:
                log(f"completed {n} job(s)")
        except Exception as e:
            log(f"operator error: {e}")
        if once:
            break
        time.sleep(POLL_SECS)


if __name__ == "__main__":
    main()
