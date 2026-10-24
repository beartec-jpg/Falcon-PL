#!/usr/bin/env python3
"""FalconQcBridgeV2 prover: leased prove-ahead, submit only tip+1.

Prove height h against Falcon ledger h sealed C(h) (attestation current_committee).
Do not use live qc_committee. Do not jump. One-host mode is lagged unwrap.

Env:
  FALCON_QC_PK          falcon-qc-v2-one/pk.bin (required; refuse v1 path)
  FALCON_QC_KEYS        FalconPublic *.pub.json
  FALCON_QC_SIGS        keys/falcon-qc/sigs-v2
  FALCON_QC_BRIDGE      FalconQcBridgeV2 address
  FALCON_QC_PROVE_AHEAD how many heights ahead of eth tip to prove (default 8)
  FALCON_QC_LEASE_DIR   lockfiles so 4 hosts claim distinct heights
"""
from __future__ import annotations

import gc
import json
import os
import shutil
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

_SCRIPT = Path(__file__).resolve()
if str(_SCRIPT.parent) not in sys.path:
    sys.path.insert(0, str(_SCRIPT.parent))
from eth_bridge_config import (  # noqa: E402
    ETH_RPC,
    ETH_RPC_FALLBACKS,
    LEGACY_BABYJUB_BRIDGE,
    parse_cast_uint,
)

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL = Path(os.environ.get("FPL_CTL", str(ROOT / "bin/falcon-pl-ctl")))
SNARK = Path(
    os.environ.get(
        "FPL_SNARK",
        str(Path.home() / "falcon-pl/falcon-pl-rs/target/release/falcon-pl-snark"),
    )
)
CAST = Path(os.environ.get("CAST", str(Path.home() / ".foundry/bin/cast")))
PK = Path(os.environ.get("FALCON_QC_PK", str(ROOT / "run/snark/falcon-qc-v2-one/pk.bin")))
KEYS = Path(os.environ.get("FALCON_QC_KEYS", str(ROOT / "keys/falcon-qc")))
SIGS = Path(os.environ.get("FALCON_QC_SIGS", str(ROOT / "keys/falcon-qc/sigs-v2")))
WORK = Path(os.environ.get("FALCON_QC_PROVER_DIR", str(ROOT / "run/snark/falcon-qc-v2-prover")))
LEASE = Path(os.environ.get("FALCON_QC_LEASE_DIR", str(WORK / "leases")))
PROVE_ONLY = os.environ.get("FALCON_QC_PROVE_ONLY", "") in ("1", "true", "yes")
BRIDGE = os.environ.get("FALCON_QC_BRIDGE", os.environ.get("FALCON_QC_V2_BRIDGE", "")).strip()
ETH_RPC = os.environ.get("ETH_RPC", ETH_RPC)
SUBMIT_STATE = Path(os.environ.get("FALCON_QC_SUBMIT_STATE", str(WORK / "submit-state.json")))
PK_FILE = Path(
    os.environ.get(
        "ETH_SUBMIT_PK_FILE",
        os.environ.get("ETH_PK_FILE", str(Path.home() / "falcon-pl-vault-test/eth-wallets/deployer.privhex")),
    )
)
THRESHOLD = int(os.environ.get("FALCON_QC_THRESHOLD", "4"))
NETWORK_ID = int(os.environ.get("FPL_NETWORK_ID", "2300"))
POLL = int(os.environ.get("POLL_SEC", "8"))
ONCE = os.environ.get("FALCON_QC_ONCE", "") in ("1", "true", "yes") or "--once" in sys.argv
TARGET_HEIGHT = int(os.environ.get("FALCON_QC_TARGET_HEIGHT", "0") or "0")
PROVE_AHEAD = int(os.environ.get("FALCON_QC_PROVE_AHEAD", "8"))
PROVE_PARALLEL = os.environ.get("FALCON_QC_PROVE_PARALLEL", "1").lower() in ("1", "true", "yes")
PROVE_WORKERS = max(1, min(THRESHOLD, int(os.environ.get("FALCON_QC_PROVE_WORKERS", "4") or "4")))
LEASE_TTL = int(os.environ.get("FALCON_QC_LEASE_TTL", "1800"))
CAST_RECV_TIMEOUT = int(os.environ.get("CAST_RECV_TIMEOUT", "120"))
GAS_BUMP = float(os.environ.get("FALCON_QC_GAS_BUMP", "1.25"))
SUBMIT_ABI = (
    "submitHeaderWithProofs(uint64,bytes32,bytes32,bytes32,bytes32,bytes32,bytes32[6],"
    "uint64[4],uint256[2][4],uint256[2][2][4],uint256[2][4])"
)
SEATS = [
    os.environ.get("FPL_RPC", "127.0.0.1:19301"),
    "100.64.0.2:19302",
    "100.64.0.3:19303",
    "100.64.0.4:19304",
    "100.64.0.5:19305",
    "100.64.0.6:19306",
]
HOST = socket.gethostname()
PID = os.getpid()


def die_if_v1_pk() -> None:
    p = str(PK)
    if "falcon-qc-one" in p and "falcon-qc-v2-one" not in p:
        raise SystemExit("refusing v1 falcon-qc-one pk.bin for v2 prover")
    meta = PK.parent / "ptau.json"
    if meta.is_file():
        t = meta.read_text()
        if "FplFalconQcCircuitV2" not in t and "v2" not in t.lower():
            raise SystemExit(f"{meta} is not a v2 ceremony")


def eth_rpcs() -> list[str]:
    out: list[str] = []
    for u in [ETH_RPC, *ETH_RPC_FALLBACKS]:
        if u and u not in out:
            out.append(u)
    return out


def json_rpc(method: str, params: list, rpc: str):
    req = urllib.request.Request(
        rpc,
        data=json.dumps({"jsonrpc": "2.0", "method": method, "params": params, "id": 1}).encode(),
        headers={"Content-Type": "application/json", "User-Agent": "falcon-qc-v2-prover/1"},
    )
    with urllib.request.urlopen(req, timeout=CAST_RECV_TIMEOUT) as resp:
        body = json.loads(resp.read())
    if body.get("error"):
        raise RuntimeError(str(body["error"]))
    return body.get("result")


def norm_hex(s: str) -> str:
    return (s or "").strip().lower().removeprefix("0x")


def b32(s: str) -> str:
    h = norm_hex(s)
    if len(h) != 64:
        raise ValueError(f"need 32-byte hex, got {s!r}")
    return "0x" + h


def load_pk() -> str:
    raw = PK_FILE.read_text().strip()
    return raw if raw.startswith("0x") else "0x" + raw


def ctl_status(addr: str) -> dict:
    out = subprocess.check_output([str(CTL), "--addr", addr, "status"], text=True, timeout=20)
    i = out.find("{")
    body = json.JSONDecoder().raw_decode(out[i:])[0]
    return body.get("body") or body


def mesh_fpl_tip() -> int:
    best = 0
    for addr in SEATS:
        try:
            h = int(ctl_status(addr).get("tip_height") or 0)
            if h > best:
                best = h
        except Exception:
            continue
    return best


def eth_fpl_tip() -> int:
    if not BRIDGE or not CAST.exists():
        return 0
    for rpc in eth_rpcs():
        try:
            out = subprocess.check_output(
                [str(CAST), "call", BRIDGE, "fplTip()(uint64)", "--rpc-url", rpc],
                text=True,
                timeout=30,
            ).strip()
            return parse_cast_uint(out)
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError):
            continue
    return 0


def load_pubs() -> dict[str, dict]:
    out: dict[str, dict] = {}
    if not KEYS.is_dir():
        return out
    for p in KEYS.glob("*.pub.json"):
        try:
            body = json.loads(p.read_text())
            sid = str(body.get("id") or p.stem.removesuffix(".pub"))
            out[sid] = body
        except (OSError, json.JSONDecodeError):
            continue
    return out


def load_atts(height: int) -> dict[str, dict]:
    out: dict[str, dict] = {}
    if not SIGS.is_dir():
        return out
    for p in SIGS.glob(f"{height:010}-*.json"):
        try:
            att = json.loads(p.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        sig = att.get("sig") or {}
        sid = (sig.get("id") if isinstance(sig, dict) else None) or att.get("id")
        if sid and isinstance(sig, dict) and sig.get("sig_hex"):
            out[str(sid)] = att
    return out


def sealed_six(att: dict, field: str) -> list[str]:
    ids = att.get(field) or []
    if not isinstance(ids, list):
        return []
    return [str(x) for x in ids if str(x)]


def write_ordered_keys(dest: Path, ids: list[str], pubs: dict[str, dict]) -> None:
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    for i, sid in enumerate(ids):
        pk = pubs.get(sid)
        if not pk:
            raise RuntimeError(f"no pub.json for sealed id {sid}")
        (dest / f"{i:02d}-{sid}.pub.json").write_text(json.dumps(pk) + "\n")


def try_lease(height: int) -> bool:
    LEASE.mkdir(parents=True, exist_ok=True)
    path = LEASE / f"{height}.lease"
    now = time.time()
    if path.exists():
        try:
            st = json.loads(path.read_text())
            if now - float(st.get("ts") or 0) < LEASE_TTL and int(st.get("pid") or 0) != PID:
                if st.get("host") != HOST:
                    return False
        except (OSError, json.JSONDecodeError, ValueError):
            pass
    path.write_text(json.dumps({"host": HOST, "pid": PID, "ts": now}) + "\n")
    return True


def input_signer_index(body: dict, fallback: int) -> int:
    inp = body.get("input") or []
    if isinstance(inp, list) and len(inp) >= 10:
        v = inp[9]
        if isinstance(v, str):
            return int(v, 16) if v.startswith("0x") else int(v)
        return int(v)
    return fallback


def next_leaves_from_proof(body: dict) -> list[str]:
    inp = body.get("input") or []
    if not isinstance(inp, list) or len(inp) < 16:
        raise RuntimeError("v2 proof public inputs must be 16 Fr")
    out = []
    for v in inp[10:16]:
        if isinstance(v, str):
            h = v[2:] if v.startswith("0x") else v
        else:
            h = format(int(v), "064x")
        h = h.lower().zfill(64)
        if len(h) > 64:
            h = h[-64:]
        out.append("0x" + h)
    return out


def prove_one_open(
    height: int,
    parent: str,
    header: str,
    claim: str,
    att: dict,
    sid: str,
    signer_index: int,
    cur_dir: Path,
    next_dir: Path,
) -> dict:
    work = WORK / f"h-{height}" / f"idx-{signer_index}-{sid}"
    proof = work / "proof.json"
    if proof.exists() and proof.stat().st_size > 0:
        try:
            body = json.loads(proof.read_text())
            if body.get("a") and body.get("input") and len(body.get("input") or []) == 16:
                return body
        except (OSError, json.JSONDecodeError):
            pass
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True, exist_ok=True)
    sigdir = work / "sigs"
    sigdir.mkdir()
    (sigdir / f"{sid}.json").write_text(json.dumps(att) + "\n")
    subprocess.check_call(
        [
            str(SNARK),
            "falcon-prove-one",
            "--pk",
            str(PK),
            "--keys-dir",
            str(cur_dir),
            "--next-keys-dir",
            str(next_dir),
            "--sigs-dir",
            str(sigdir),
            "--height",
            str(height),
            "--parent",
            parent if parent.startswith("0x") else "0x" + parent,
            "--header",
            header if header.startswith("0x") else "0x" + header,
            "--claim-root",
            (claim if claim.startswith("0x") else "0x" + claim) if claim else "0x" + ("00" * 32),
            "--network-id",
            str(NETWORK_ID),
            "--domain",
            "v2",
            "--out",
            str(proof),
        ]
    )
    body = json.loads(proof.read_text())
    pub_idx = input_signer_index(body, signer_index)
    if pub_idx != signer_index:
        raise RuntimeError(f"h={height} {sid} public index {pub_idx} != sealed {signer_index}")
    if len(body.get("input") or []) != 16:
        raise RuntimeError(f"v2 proof publics {len(body.get('input') or [])} != 16")
    return body


def prove_four(height: int, atts: dict[str, dict]) -> tuple[list[int], list[dict], dict]:
    sample = next(iter(atts.values()))
    cur_ids = sealed_six(sample, "current_committee")
    next_ids = sealed_six(sample, "next_committee")
    if len(cur_ids) != 6 or len(next_ids) != 6:
        raise RuntimeError(
            f"h={height} need sealed 6+6 in attestation, got cur={len(cur_ids)} next={len(next_ids)}"
        )
    idx_map = {sid: i for i, sid in enumerate(cur_ids)}
    valid = {k: v for k, v in atts.items() if k in idx_map}
    picked = sorted(valid.items(), key=lambda kv: idx_map[kv[0]])[:THRESHOLD]
    if len(picked) < THRESHOLD:
        raise RuntimeError(f"need {THRESHOLD} sealed-committee sigs, got {len(picked)}")
    pubs = load_pubs()
    cur_dir = WORK / f"h-{height}" / "cur6"
    next_dir = WORK / f"h-{height}" / "next6"
    write_ordered_keys(cur_dir, cur_ids, pubs)
    write_ordered_keys(next_dir, next_ids, pubs)
    parent = norm_hex(sample.get("parent") or "")
    header = norm_hex(sample.get("header") or "")
    claim = norm_hex(sample.get("claim_root") or "")
    inner = norm_hex(sample.get("inner_hash") or "")
    next_root = norm_hex(sample.get("next_committee_root") or "")
    tasks = []
    seen: set[int] = set()
    for sid, att in picked:
        ix = idx_map[sid]
        if ix in seen:
            raise RuntimeError(f"duplicate sealed index {ix}")
        seen.add(ix)
        tasks.append((ix, sid, att))
    workers = min(PROVE_WORKERS, len(tasks)) if PROVE_PARALLEL else 1
    t0 = time.time()
    if workers > 1:
        print(f"prove h={height} parallel workers={workers} sealed={cur_ids}", flush=True)
        with ThreadPoolExecutor(max_workers=workers) as ex:
            futs = [
                ex.submit(
                    prove_one_open, height, parent, header, claim, att, sid, ix, cur_dir, next_dir
                )
                for ix, sid, att in tasks
            ]
            proofs = [f.result() for f in futs]
    else:
        proofs = []
        for ix, sid, att in tasks:
            proofs.append(
                prove_one_open(height, parent, header, claim, att, sid, ix, cur_dir, next_dir)
            )
            gc.collect()
    print(f"proved h={height} wall={time.time() - t0:.1f}s", flush=True)
    meta = {
        "parent": parent,
        "header": header,
        "claim": claim,
        "inner": inner,
        "next_root": next_root,
        "next_leaves": next_leaves_from_proof(proofs[0]),
    }
    return [ix for ix, _, _ in tasks], proofs, meta


def fmt_idx(indices: list[int]) -> str:
    return "[" + ",".join(str(i) for i in indices) + "]"


def fmt_a(proofs: list[dict]) -> str:
    parts = [f"[{int(p['a'][0], 16)},{int(p['a'][1], 16)}]" for p in proofs]
    return "[" + ",".join(parts) + "]"


def fmt_b(proofs: list[dict]) -> str:
    parts = []
    for p in proofs:
        b = p["b"]
        parts.append(
            f"[[{int(b[0][0], 16)},{int(b[0][1], 16)}],"
            f"[{int(b[1][0], 16)},{int(b[1][1], 16)}]]"
        )
    return "[" + ",".join(parts) + "]"


def fmt_c(proofs: list[dict]) -> str:
    parts = [f"[{int(p['c'][0], 16)},{int(p['c'][1], 16)}]" for p in proofs]
    return "[" + ",".join(parts) + "]"


def fmt_leaves(leaves: list[str]) -> str:
    return "[" + ",".join(b32(x) for x in leaves) + "]"


def wait_receipt(txid: str, timeout: int = 900) -> bool:
    t0 = time.time()
    while time.time() - t0 < timeout:
        for rpc in eth_rpcs():
            try:
                js = json_rpc("eth_getTransactionReceipt", [txid], rpc)
                if js is None:
                    continue
                status = js.get("status")
                if status == "0x1" or str(status) == "1":
                    return True
                if status in ("0x0", "0") or str(status) == "0":
                    raise RuntimeError(f"tx reverted {txid}")
            except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, RuntimeError, OSError) as e:
                if isinstance(e, RuntimeError) and "reverted" in str(e):
                    raise
                continue
        time.sleep(6)
    return False


def submit_proofs(height: int, meta: dict, indices: list[int], proofs: list[dict]) -> str:
    if PROVE_ONLY or not BRIDGE:
        raise RuntimeError("prove-only (FALCON_QC_PROVE_ONLY=1 or empty FALCON_QC_BRIDGE)")
    if BRIDGE.lower() == LEGACY_BABYJUB_BRIDGE.lower():
        raise RuntimeError("refusing Falcon-512 proof on legacy BabyJub bridge")
    eth_tip = eth_fpl_tip()
    if height != eth_tip + 1:
        raise RuntimeError(f"submit only tip+1 (eth={eth_tip} want={eth_tip + 1} got={height})")
    rpc = eth_rpcs()[0]
    try:
        gp = subprocess.check_output([str(CAST), "gas-price", "--rpc-url", rpc], text=True, timeout=30).strip()
        gas_price = str(max(1_000_000_000, int(int(gp, 16) if gp.startswith("0x") else int(gp) * GAS_BUMP)))
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError, ValueError):
        gas_price = "2000000000"
    cmd = [
        str(CAST),
        "send",
        BRIDGE,
        SUBMIT_ABI,
        str(height),
        b32(meta["header"]),
        b32(meta["parent"]),
        b32(meta["inner"]),
        b32(meta["claim"]),
        b32(meta["next_root"]),
        fmt_leaves(meta["next_leaves"]),
        fmt_idx(indices),
        fmt_a(proofs),
        fmt_b(proofs),
        fmt_c(proofs),
        "--rpc-url",
        rpc,
        "--private-key",
        load_pk(),
        "--gas-limit",
        "3600000",
        "--gas-price",
        gas_price,
        "--legacy",
    ]
    r = subprocess.run(cmd, text=True, capture_output=True, timeout=180)
    blob = (r.stdout or "") + (r.stderr or "")
    if r.returncode != 0:
        raise RuntimeError(blob[-1200:])
    tx = next((t for t in blob.replace('"', " ").split() if t.startswith("0x") and len(t) == 66), "")
    if not tx:
        raise RuntimeError("cast send produced no tx hash: " + blob[-400:])
    if not wait_receipt(tx):
        raise RuntimeError(f"no receipt for {tx}")
    tip_after = eth_fpl_tip()
    if tip_after < height:
        raise RuntimeError(f"receipt ok but fplTip {tip_after} < {height}")
    SUBMIT_STATE.parent.mkdir(parents=True, exist_ok=True)
    SUBMIT_STATE.write_text(
        json.dumps({"last_height": height, "last_tx": tx, "last_eth_tip": tip_after, "last_attempt": time.time()})
        + "\n"
    )
    return tx


def poll_v2_atts() -> None:
    SIGS.mkdir(parents=True, exist_ok=True)
    for addr in SEATS:
        try:
            st = ctl_status(addr)
        except Exception:
            continue
        fq = st.get("falcon_qc") or {}
        for att in fq.get("attestations_v2") or []:
            if not isinstance(att, dict):
                continue
            h = int(att.get("height") or 0)
            sid = ((att.get("sig") or {}).get("id") if isinstance(att.get("sig"), dict) else None) or ""
            if h and sid:
                dest = SIGS / f"{h:010}-{sid}.json"
                if not dest.exists():
                    dest.write_text(json.dumps(att) + "\n")


def run_once() -> None:
    die_if_v1_pk()
    if not PK.is_file():
        raise SystemExit(f"missing v2 pk.bin at {PK} (run falcon-qc-v2-setup-one-ptau.sh)")
    WORK.mkdir(parents=True, exist_ok=True)
    poll_v2_atts()
    mesh = mesh_fpl_tip()
    eth = eth_fpl_tip()
    print(f"v2 prover mesh={mesh} eth_tip={eth} ahead={PROVE_AHEAD} host={HOST}", flush=True)
    lo = eth + 1
    hi = min(mesh, eth + max(1, PROVE_AHEAD))
    if TARGET_HEIGHT > 0:
        lo = hi = TARGET_HEIGHT
    for h in range(lo, hi + 1):
        if not try_lease(h):
            print(f"skip h={h} leased", flush=True)
            continue
        atts = load_atts(h)
        if len(atts) < THRESHOLD:
            print(f"h={h} sigs={len(atts)} need={THRESHOLD}", flush=True)
            continue
        proof_dir = WORK / f"h-{h}"
        ready = proof_dir.is_dir() and len(list(proof_dir.glob("idx-*/proof.json"))) >= THRESHOLD
        if not ready:
            try:
                prove_four(h, atts)
            except Exception as e:
                print(f"prove h={h} err: {e}", flush=True)
                continue
        if h == eth + 1 and not PROVE_ONLY:
            sample = next(iter(atts.values()))
            try:
                indices, proofs, meta = prove_four(h, atts)
                if not meta.get("inner"):
                    meta["inner"] = norm_hex(sample.get("inner_hash") or "")
                tx = submit_proofs(h, meta, indices, proofs)
                print(f"submitted h={h} tx={tx}", flush=True)
            except Exception as e:
                print(f"submit h={h} err: {e}", flush=True)


def main() -> int:
    while True:
        try:
            run_once()
        except Exception as e:
            print(f"loop err: {e}", flush=True)
        if ONCE:
            return 0
        time.sleep(POLL)


if __name__ == "__main__":
    raise SystemExit(main())
