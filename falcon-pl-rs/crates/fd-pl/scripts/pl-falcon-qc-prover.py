#!/usr/bin/env python3
"""Permissionless Falcon-512 QC prover: ≥4 Falcon sigs → 4× OPEN=1 Groth16 → FalconQcBridge.

Submits `submitHeaderWithProofs` on FalconQcBridge (distinct signer indices 0..7).
Refuses the legacy BabyJub `FalconBridge`.

Env:
  FPL_CTL / FPL_SNARK / FPL_RPC / PL_ROOT
  FALCON_QC_PK       OPEN=1 proving key (falcon-setup-one)
  FALCON_QC_KEYS     FalconPublic *.pub.json (keys/falcon-qc)
  FALCON_QC_SIGS     collected FalconQcAttestation JSON
  FALCON_QC_BRIDGE   FalconQcBridge address (default: live Sepolia QC)
  FALCON_QC_PROVE_ONLY=1  prove but do not submit
  FALCON_QC_PROVE_PARALLEL=1  run 4× falcon-prove-one concurrently (default on)
  FALCON_QC_PROVE_WORKERS=4     max parallel prove subprocesses
"""
from __future__ import annotations

import fcntl
import gc
import json
import threading
from concurrent.futures import ThreadPoolExecutor
import os
import shutil
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
    GROTH16_VERIFIER,
    LEGACY_BABYJUB_BRIDGE,
    bridge_config_summary,
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
PK = Path(os.environ.get("FALCON_QC_PK", str(ROOT / "run/snark/falcon-qc-one/pk.bin")))
KEYS = Path(os.environ.get("FALCON_QC_KEYS", str(ROOT / "keys/falcon-qc")))
SIGS = Path(os.environ.get("FALCON_QC_SIGS", str(ROOT / "keys/falcon-qc/sigs")))
WORK = Path(os.environ.get("FALCON_QC_PROVER_DIR", str(ROOT / "run/snark/falcon-qc-prover")))
PROVE_ONLY = os.environ.get("FALCON_QC_PROVE_ONLY", "") in ("1", "true", "yes")
BRIDGE = os.environ.get("FALCON_QC_BRIDGE", FALCON_QC_BRIDGE).strip()
ETH_RPC = os.environ.get("ETH_RPC", ETH_RPC)
SUBMIT_STATE = Path(os.environ.get("FALCON_QC_SUBMIT_STATE", str(WORK / "submit-state.json")))
SUBMIT_COOLDOWN = int(os.environ.get("FALCON_QC_SUBMIT_COOLDOWN", "300"))
PROOF_FAIL_COOLDOWN = int(os.environ.get("FALCON_QC_PROOF_FAIL_COOLDOWN", "1800"))
CAST_RECV_TIMEOUT = int(os.environ.get("CAST_RECV_TIMEOUT", "120"))
GAS_BUMP = float(os.environ.get("FALCON_QC_GAS_BUMP", "1.25"))
PK_FILE = Path(
    os.environ.get(
        "ETH_SUBMIT_PK_FILE",
        os.environ.get("ETH_PK_FILE", str(Path.home() / "falcon-pl-vault-test/eth-wallets/deployer.privhex")),
    )
)
THRESHOLD = int(os.environ.get("FALCON_QC_THRESHOLD", "4"))
NETWORK_ID = int(os.environ.get("FPL_NETWORK_ID", "2300"))
POLL = int(os.environ.get("POLL_SEC", "8"))
SAMPLE_GAP = int(os.environ.get("FALCON_QC_SAMPLE_GAP", "48"))
ONCE = os.environ.get("FALCON_QC_ONCE", "") in ("1", "true", "yes") or "--once" in sys.argv
TARGET_HEIGHT = int(os.environ.get("FALCON_QC_TARGET_HEIGHT", "0") or "0")
JUMP_LAG = int(os.environ.get("FALCON_QC_JUMP_LAG", str(SAMPLE_GAP * 4)))
STEADY = os.environ.get("FALCON_QC_STEADY", "").lower() in ("1", "true", "yes")
DISASTER_LAG = int(os.environ.get("FALCON_QC_DISASTER_LAG", "2000"))
PROVE_PARALLEL = os.environ.get("FALCON_QC_PROVE_PARALLEL", "1").lower() in ("1", "true", "yes")


def _resolve_prove_workers() -> int:
    raw = os.environ.get("FALCON_QC_PROVE_WORKERS", "").strip()
    if raw:
        return max(1, min(int(raw), THRESHOLD))
    # ~6GiB peak RSS per OPEN=1 falcon-prove-one; leave headroom for OS + ctl.
    per_gb = float(os.environ.get("FALCON_QC_PROVE_MEM_GB", "6"))
    try:
        total_gb = (os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE")) / (1024**3)
        budget_gb = max(0.0, total_gb - 3.0)
        return max(1, min(THRESHOLD, int(budget_gb // per_gb)))
    except (AttributeError, OSError, ValueError):
        return min(4, THRESHOLD)


PROVE_WORKERS = _resolve_prove_workers()
PROVER_LOCK = WORK / "prover.lock"
CATCHUP_SCRIPT = ROOT / "scripts/pl-falcon-qc-catchup.py"
LAST_CATCHUP_TS = WORK / "last-catchup.ts"
CATCHUP_INTERVAL = int(os.environ.get("FALCON_QC_CATCHUP_SEC", "600"))
SUBMIT_ABI = (
    "submitHeaderWithProofs(uint64,bytes32,bytes32,bytes32,uint64[4],"
    "uint256[2][4],uint256[2][2][4],uint256[2][4])"
)
SEATS = [
    os.environ.get("FPL_RPC", "127.0.0.1:19301"),
    "100.64.0.2:19302",
    "100.64.0.3:19303",
    "100.64.0.4:19304",
    "100.64.0.5:19305",
    "100.64.0.6:19306",
]


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
        headers={"Content-Type": "application/json", "User-Agent": "falcon-qc-prover/1"},
    )
    with urllib.request.urlopen(req, timeout=CAST_RECV_TIMEOUT) as resp:
        body = json.loads(resp.read())
    if body.get("error"):
        raise RuntimeError(str(body["error"]))
    return body.get("result")


def load_submit_state() -> dict:
    try:
        return json.loads(SUBMIT_STATE.read_text())
    except (OSError, json.JSONDecodeError):
        return {}


def save_submit_state(st: dict) -> None:
    SUBMIT_STATE.parent.mkdir(parents=True, exist_ok=True)
    tmp = SUBMIT_STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(st) + "\n")
    tmp.replace(SUBMIT_STATE)


def cast_run(cmd: list[str], timeout: int = CAST_RECV_TIMEOUT) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, text=True, capture_output=True, timeout=timeout)


def wallet_address() -> str:
    return cast_run(
        [str(CAST), "wallet", "address", "--private-key", load_pk()],
        timeout=30,
    ).stdout.strip()


def eth_header_finalized(height: int) -> bool:
    if not BRIDGE or not CAST.exists():
        return False
    for rpc in eth_rpcs():
        try:
            out = subprocess.check_output(
                [
                    str(CAST),
                    "call",
                    BRIDGE,
                    "headers(uint64)(bytes32,bytes32,bytes32,bool)",
                    str(height),
                    "--rpc-url",
                    rpc,
                ],
                text=True,
                timeout=CAST_RECV_TIMEOUT,
            ).strip()
            lines = [ln.strip() for ln in out.splitlines() if ln.strip()]
            if lines:
                return lines[-1].lower() in ("true", "0x1", "1")
            parts = [p.strip() for p in out.replace("(", "").replace(")", "").split(",")]
            if len(parts) >= 4:
                return parts[-1].lower() in ("true", "0x1", "1")
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError):
            continue
    return False


def pending_nonce_gap() -> tuple[int, int]:
    """Return (confirmed_nonce, pending_nonce)."""
    addr = wallet_address()
    for rpc in eth_rpcs():
        try:
            confirmed = int(json_rpc("eth_getTransactionCount", [addr, "latest"], rpc), 16)
            pending = int(json_rpc("eth_getTransactionCount", [addr, "pending"], rpc), 16)
            return confirmed, pending
        except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, OSError, ValueError, RuntimeError):
            continue
    return 0, 0


def legacy_gas_price(rpc: str, bump: float = 1.0) -> str:
    try:
        gp = subprocess.check_output(
            [str(CAST), "gas-price", "--rpc-url", rpc], text=True, timeout=30
        ).strip()
        base = int(gp, 16) if gp.startswith("0x") else int(gp or "1000000000")
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError, ValueError):
        base = 2_000_000_000
    return str(max(1_000_000_000, int(base * bump)))


def flush_stuck_nonce() -> bool:
    """Cancel-replace the lowest stuck pending nonce (legacy self-send)."""
    confirmed, pending = pending_nonce_gap()
    if pending <= confirmed:
        return False
    stuck_nonce = confirmed
    addr = wallet_address()
    print(f"flush stuck nonce={stuck_nonce} confirmed={confirmed} pending={pending}", flush=True)
    for rpc in eth_rpcs():
        try:
            gas_price = legacy_gas_price(rpc, GAS_BUMP * 2)
            cmd = [
                str(CAST),
                "send",
                addr,
                "--value",
                "0",
                "--nonce",
                str(stuck_nonce),
                "--gas-price",
                gas_price,
                "--gas-limit",
                "21000",
                "--legacy",
                "--rpc-url",
                rpc,
                "--private-key",
                load_pk(),
            ]
            r = cast_run(cmd, timeout=180)
            blob = (r.stdout or "") + (r.stderr or "")
            if r.returncode != 0:
                print(f"nonce flush warn: {blob[-400:]}", flush=True)
                continue
            tx = next((t for t in blob.replace('"', " ").split() if t.startswith("0x") and len(t) == 66), "")
            print(f"nonce flush tx={tx} gas_price={gas_price}", flush=True)
            if tx and wait_receipt(tx, timeout=180):
                return True
            time.sleep(8)
            c2, p2 = pending_nonce_gap()
            if p2 <= c2:
                return True
            return bool(tx)
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError) as e:
            print(f"nonce flush err: {e}", flush=True)
            continue
    return False


def wait_for_clear_nonce(max_wait: int = 300) -> None:
    """Block until no pending txs; cancel-replace if a single nonce is stuck."""
    t0 = time.time()
    last_flush = 0.0
    while time.time() - t0 < max_wait:
        confirmed, pending = pending_nonce_gap()
        if pending <= confirmed:
            return
        print(f"wait nonce confirmed={confirmed} pending={pending}", flush=True)
        elapsed = time.time() - t0
        if elapsed >= 20 and time.time() - last_flush >= 40:
            flush_stuck_nonce()
            last_flush = time.time()
            confirmed, pending = pending_nonce_gap()
            if pending <= confirmed:
                return
        time.sleep(12)
    confirmed, pending = pending_nonce_gap()
    if pending <= confirmed:
        return
    flush_stuck_nonce()
    time.sleep(10)
    confirmed, pending = pending_nonce_gap()
    if pending <= confirmed:
        return
    raise RuntimeError(f"nonce still stuck after {max_wait}s (confirmed={confirmed} pending={pending})")


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
            except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, RuntimeError, OSError, json.JSONDecodeError) as e:
                if isinstance(e, RuntimeError) and "reverted" in str(e):
                    raise
                continue
        time.sleep(6)
    return False


def submit_cooldown_active(height: int, eth_tip: int) -> bool:
    st = load_submit_state()
    if st.get("proof_fail_height") == height:
        cd = PROOF_FAIL_COOLDOWN
        if time.time() - float(st.get("proof_fail_at") or 0) < cd:
            return True
    if st.get("last_height") != height or st.get("last_eth_tip") != eth_tip:
        return False
    return time.time() - float(st.get("last_attempt") or 0) < SUBMIT_COOLDOWN


def mark_proof_fail(height: int, err: str) -> None:
    st = load_submit_state()
    st.update({"proof_fail_height": height, "proof_fail_at": time.time(), "proof_fail_err": err[-400:]})
    save_submit_state(st)


def clear_proof_fail_after_collect() -> None:
    st = load_submit_state()
    if st.pop("proof_fail_height", None) is not None:
        st.pop("proof_fail_at", None)
        st.pop("proof_fail_err", None)
        save_submit_state(st)
        print("cleared proof_fail after sig collect", flush=True)


def verify_proofs_local(height: int, indices: list[int], proofs: list[dict]) -> None:
    vk = PK.parent / "vk.bin"
    if not vk.exists() or not SNARK.exists():
        return
    for i, ix in enumerate(indices):
        proof_json = _proof_json_path(height, ix)
        if proof_json is None:
            proof_json = WORK / f"preflight-h{height}-i{i}.json"
            proof_json.write_text(json.dumps(proofs[i]) + "\n")
        ark = proof_json.with_suffix(".ark")
        if not ark.exists():
            # falcon-prove-one writes proof.ark beside proof.json in work dir
            print(f"skip local vk preflight idx={i} (no {ark.name})", flush=True)
            continue
        r = subprocess.run(
            [str(SNARK), "verify", "--vk", str(vk), "--proof", str(proof_json)],
            capture_output=True,
            text=True,
            timeout=120,
        )
        if r.returncode != 0 or "true" not in (r.stdout or "").lower():
            raise RuntimeError(f"local vk preflight failed idx={i}: {(r.stdout or '') + (r.stderr or '')}")


def norm_hex(s: str) -> str:
    return (s or "").strip().lower().removeprefix("0x")


def b32(s: str) -> str:
    h = norm_hex(s)
    if len(h) != 64:
        raise ValueError(f"need 32-byte hex, got {s!r}")
    return "0x" + h


def load_pk() -> str:
    raw = PK_FILE.read_text().strip()
    if not raw.startswith("0x"):
        raw = "0x" + raw
    return raw


def committee_index(keys_dir: Path) -> dict[str, int]:
    out: dict[str, int] = {}
    pubs = sorted(keys_dir.glob("*.pub.json"))
    for i, p in enumerate(pubs[:8]):
        try:
            body = json.loads(p.read_text())
            sid = str(body.get("id") or p.stem.removesuffix(".pub"))
            out[sid] = i
        except (OSError, json.JSONDecodeError):
            continue
    return out


def committee_atts(atts: dict) -> dict:
    """Only committee signers count toward threshold (ignore stray v8+ ids)."""
    idx = committee_index(KEYS)
    return {k: v for k, v in atts.items() if k in idx}


def pick_on_grid(candidates: list[tuple], mesh: int) -> tuple:
    """Prefer highest attested height on SAMPLE_GAP grid (contract allows jumps)."""
    grid = [e for e in candidates if e[0] % SAMPLE_GAP == 0 or e[0] == mesh]
    pool = grid if grid else candidates
    return max(pool, key=lambda x: x[0])


def sigs_ready_for_height(h: int) -> bool:
    if not SIGS.is_dir() or h <= 0:
        return False
    idx = committee_index(KEYS)
    seen: set[str] = set()
    for p in SIGS.glob(f"{h:010}-*.json"):
        try:
            att = json.loads(p.read_text())
            sid = ((att.get("sig") or {}).get("id") if isinstance(att.get("sig"), dict) else None) or att.get("id")
            if sid and sid in idx:
                seen.add(sid)
        except (OSError, json.JSONDecodeError):
            continue
    return len(seen) >= THRESHOLD


def prove_in_flight() -> bool:
    r = subprocess.run(["pgrep", "-f", "falcon-prove-one"], capture_output=True)
    return r.returncode == 0


def maybe_collect_sigs(mesh: int, eth_tip: int, *, force: bool = False) -> None:
    if prove_in_flight() and not force:
        print("skip sig collect — prove in flight", flush=True)
        return
    lag = max(0, mesh - eth_tip)
    if not force and mesh <= eth_tip and lag <= SAMPLE_GAP:
        return
    jump_h = mesh if mesh % SAMPLE_GAP == 0 else mesh - (mesh % SAMPLE_GAP)
    if not force and sigs_ready_for_height(jump_h):
        return
    if not CATCHUP_SCRIPT.is_file():
        return
    try:
        last = float(LAST_CATCHUP_TS.read_text().strip()) if LAST_CATCHUP_TS.is_file() else 0.0
    except (OSError, ValueError):
        last = 0.0
    if not force and time.time() - last < CATCHUP_INTERVAL:
        return
    print(f"auto sig collect mesh={mesh} eth={eth_tip} lag={lag} force={force}", flush=True)
    subprocess.run(
        [sys.executable, str(CATCHUP_SCRIPT), "collect"],
        cwd=str(ROOT),
        env={**os.environ, "PL_ROOT": str(ROOT)},
        timeout=600,
        check=False,
    )
    LAST_CATCHUP_TS.write_text(str(time.time()) + "\n")
    clear_proof_fail_after_collect()


def ctl_status(addr: str) -> dict:
    out = subprocess.check_output([str(CTL), "--addr", addr, "status"], text=True, timeout=20)
    i = out.find("{")
    body = json.JSONDecoder().raw_decode(out[i:])[0]
    return body.get("body") or body


def ingest_att(cache: dict, att: dict) -> None:
    h = int(att.get("height") or 0)
    if h <= 0:
        return
    header = norm_hex(att.get("header") or "")
    parent = norm_hex(att.get("parent") or "")
    claim = norm_hex(att.get("claim_root") or "")
    sig = att.get("sig") or att
    sid = (sig.get("id") if isinstance(sig, dict) else None) or att.get("id")
    if not (header and parent and sid):
        return
    if isinstance(sig, dict) and not sig.get("sig_hex"):
        return
    key = (h, header, parent, claim)
    cache.setdefault(key, {})[sid] = att


def _sig_height(path: Path) -> int | None:
    """Parse height from sig filename `{height:010}-{signer}.json`."""
    try:
        return int(path.name.split("-", 1)[0])
    except (ValueError, IndexError):
        return None


def scan_sigs_dir(
    cache: dict, min_h: int = 0, max_h: int = 0, *, grid_only: bool = False, lag: int = 0
) -> None:
    """Load attestations for heights in [min_h, max_h] via filename prefix (no full-archive walk)."""
    if not SIGS.is_dir():
        return
    if max_h < min_h:
        return
    scanned = 0
    mesh = max_h
    if lag > JUMP_LAG:
        # Jump catch-up: only load sigs at grid heights near mesh (+ withdraw barrier).
        heights: set[int] = set()
        barrier = mesh_open_withdraw_barrier()
        if barrier > min_h:
            heights.add(barrier)
        for h in range(max(min_h + 1, mesh - SAMPLE_GAP * 8), mesh + 1):
            if h % SAMPLE_GAP == 0 or h == mesh:
                heights.add(h)
        for h in sorted(heights):
            for p in SIGS.glob(f"{h:010}-*.json"):
                try:
                    ingest_att(cache, json.loads(p.read_text()))
                    scanned += 1
                except (OSError, json.JSONDecodeError):
                    continue
        print(f"scan_sigs jump heights={sorted(heights)} loaded={scanned}", flush=True)
        return
    for h in range(min_h, max_h + 1):
        if grid_only and SAMPLE_GAP > 1 and h % SAMPLE_GAP != 0 and h != mesh:
            continue
        for p in SIGS.glob(f"{h:010}-*.json"):
            try:
                ingest_att(cache, json.loads(p.read_text()))
                scanned += 1
            except (OSError, json.JSONDecodeError):
                continue
    print(f"scan_sigs {min_h}..{max_h} grid_only={grid_only} loaded={scanned}", flush=True)


def poll_seats(cache: dict) -> None:
    for addr in SEATS:
        try:
            st = ctl_status(addr)
        except Exception as e:
            print(f"status {addr}: {e}", flush=True)
            continue
        fq = st.get("falcon_qc") or {}
        for att in fq.get("attestations") or []:
            if not isinstance(att, dict):
                continue
            ingest_att(cache, att)
            h = int(att.get("height") or 0)
            sid = ((att.get("sig") or {}).get("id") if isinstance(att.get("sig"), dict) else None) or ""
            if h and sid:
                SIGS.mkdir(parents=True, exist_ok=True)
                dest = SIGS / f"{h:010}-{sid}.json"
                if not dest.exists():
                    dest.write_text(json.dumps(att) + "\n")


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


def input_signer_index(body: dict, fallback: int) -> int:
    inp = body.get("input")
    if not inp:
        return fallback
    if isinstance(inp, list):
        last = inp[-1]
        if isinstance(last, str):
            return int(last, 16) if last.startswith("0x") else int(last)
        return int(last)
    if isinstance(inp, str):
        return int(inp, 16) if inp.startswith("0x") else int(inp)
    return fallback


def _proof_json_path(height: int, signer_index: int) -> Path | None:
    base = WORK / f"h-{height}"
    if not base.is_dir():
        return None
    matches = sorted(base.glob(f"idx-{signer_index}-*/proof.json"))
    return matches[0] if matches else None


def prove_one_open(
    height: int,
    parent: str,
    header: str,
    claim: str,
    att: dict,
    sid: str,
    signer_index: int,
) -> dict:
    work = WORK / f"h-{height}" / f"idx-{signer_index}-{sid}"
    proof = work / "proof.json"
    ark = proof.with_suffix(".ark")
    if proof.exists() and ark.exists() and proof.stat().st_size > 0 and ark.stat().st_size > 0:
        try:
            body = json.loads(proof.read_text())
            if body.get("a") and body.get("input"):
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
            str(KEYS),
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
            "--out",
            str(proof),
        ]
    )
    body = json.loads(proof.read_text())
    pub_idx = input_signer_index(body, signer_index)
    if pub_idx != signer_index:
        print(f"WARN h={height} {sid} public index {pub_idx} != committee {signer_index}", flush=True)
    return body


def prove_four(height: int, parent: str, header: str, claim: str, atts: dict) -> tuple[list[int], list[dict]]:
    idx_map = committee_index(KEYS)
    valid = {k: v for k, v in atts.items() if k in idx_map}
    picked = sorted(valid.items(), key=lambda kv: idx_map[kv[0]])[:THRESHOLD]
    if len(picked) < THRESHOLD:
        raise RuntimeError(f"need {THRESHOLD} committee sigs, got {len(picked)}")
    tasks: list[tuple[int, str, dict]] = []
    seen: set[int] = set()
    for sid, att in picked:
        if sid not in idx_map:
            raise RuntimeError(f"signer {sid} not in committee keys under {KEYS}")
        ix = idx_map[sid]
        if ix in seen:
            raise RuntimeError(f"duplicate committee index {ix}")
        seen.add(ix)
        tasks.append((ix, sid, att))

    workers = min(PROVE_WORKERS, len(tasks)) if PROVE_PARALLEL else 1
    t0 = time.time()
    if workers > 1:
        print(f"prove h={height} parallel workers={workers}", flush=True)
        with ThreadPoolExecutor(max_workers=workers) as ex:
            futs = [
                ex.submit(prove_one_open, height, parent, header, claim, att, sid, ix)
                for ix, sid, att in tasks
            ]
            proofs = [f.result() for f in futs]
    else:
        proofs = []
        for ix, sid, att in tasks:
            proofs.append(prove_one_open(height, parent, header, claim, att, sid, ix))
            gc.collect()
    print(f"proved h={height} wall={time.time() - t0:.1f}s parallel={workers > 1}", flush=True)
    indices = [ix for ix, _, _ in tasks]
    return indices, proofs


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


def submit_proofs(
    height: int, parent: str, header: str, claim: str, indices: list[int], proofs: list[dict]
) -> str:
    if PROVE_ONLY or not BRIDGE:
        raise RuntimeError("prove-only mode (FALCON_QC_PROVE_ONLY=1 or empty bridge)")
    if BRIDGE.lower() == LEGACY_BABYJUB_BRIDGE.lower():
        raise RuntimeError("refusing Falcon-512 proof on legacy BabyJub bridge")

    wait_for_clear_nonce()
    tip_before = eth_fpl_tip()
    last_err = ""
    for attempt in range(3):
        rpc = eth_rpcs()[attempt % len(eth_rpcs())]
        try:
            gas_price = legacy_gas_price(rpc, GAS_BUMP ** attempt)
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError, ValueError):
            gas_price = str(int(2_000_000_000 * (GAS_BUMP ** attempt)))
        cmd = [
            str(CAST),
            "send",
            BRIDGE,
            SUBMIT_ABI,
            str(height),
            b32(header),
            b32(parent),
            b32(claim or ("00" * 32)),
            fmt_idx(indices),
            fmt_a(proofs),
            fmt_b(proofs),
            fmt_c(proofs),
            "--rpc-url",
            rpc,
            "--private-key",
            load_pk(),
            "--gas-limit",
            "3200000",
            "--gas-price",
            gas_price,
            "--legacy",
        ]
        extra = [f for f in os.environ.get("CAST_SEND_FLAGS", "").split() if f not in ("--async", "")]
        cmd += extra
        r = cast_run(cmd, timeout=180)
        blob = (r.stdout or "") + (r.stderr or "")
        if r.returncode != 0:
            last_err = blob[-1200:]
            if "underpriced" in blob.lower() or "replacement" in blob.lower():
                wait_for_clear_nonce(max_wait=120)
                time.sleep(4)
                continue
            raise RuntimeError(last_err)
        tx = next((t for t in blob.replace('"', " ").split() if t.startswith("0x") and len(t) == 66), "")
        if not tx:
            raise RuntimeError("cast send produced no tx hash: " + blob[-400:])
        if not wait_receipt(tx):
            last_err = f"no receipt for {tx}"
            continue
        tip_after = eth_fpl_tip()
        if tip_after >= height:
            save_submit_state(
                {
                    "last_height": height,
                    "last_tx": tx,
                    "last_eth_tip": tip_after,
                    "last_attempt": time.time(),
                }
            )
            return tx
        last_err = f"receipt ok but fplTip {tip_before}->{tip_after} (need>={height})"
        print(f"WARN submit h={height} tx={tx} tip unchanged", flush=True)
    raise RuntimeError(last_err or "submit failed")


def mesh_fpl_tip() -> int:
    best = 0
    for addr in SEATS:
        try:
            st = ctl_status(addr)
            h = int(st.get("tip_height") or 0)
            if h > best:
                best = h
        except Exception:
            continue
    return best


def mesh_open_withdraw_barrier() -> int:
    """Lowest mesh tip with open EVM rail withdrawals (needs sequential fplTip catch-up)."""
    barrier = 0
    for addr in SEATS:
        try:
            st = ctl_status(addr)
            mesh = int(st.get("tip_height") or 0)
            for r in st.get("rails") or []:
                if not isinstance(r, dict):
                    continue
                asset = str(r.get("asset") or "").upper()
                if asset not in ("ETH", "USDC"):
                    continue
                if int(r.get("open_withdrawals") or 0) <= 0:
                    continue
                if barrier <= 0 or mesh < barrier:
                    barrier = mesh
        except Exception:
            continue
    return barrier


def hub_lc_claim_root() -> str:
    for addr in SEATS:
        try:
            st = ctl_status(addr)
            return norm_hex(str((st.get("lc") or {}).get("claim_root") or ""))
        except Exception:
            continue
    return ""


def attestation_matches_hub(h: int, claim: str) -> bool:
    """At mesh tip, attestation claim_root must match live lc.claim_root."""
    mesh = mesh_fpl_tip()
    if mesh <= 0 or h < mesh:
        return True
    hub = hub_lc_claim_root()
    if not hub:
        return True
    if claim != hub:
        print(
            f"skip h={h} stale attestation claim={claim[:16]}… hub={hub[:16]}… mesh={mesh}",
            flush=True,
        )
        return False
    return True


def assert_verifier_matches_bridge() -> None:
    if PROVE_ONLY or not BRIDGE or not CAST.exists():
        return
    try:
        out = subprocess.check_output(
            [str(CAST), "call", BRIDGE, "verifier()(address)", "--rpc-url", eth_rpcs()[0]],
            text=True,
            timeout=30,
        ).strip()
        on_chain = out.split()[0].lower()
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError) as e:
        raise RuntimeError(f"cannot read verifier from bridge {BRIDGE}: {e}") from e
    expected = GROTH16_VERIFIER.strip().lower()
    if expected and on_chain != expected:
        raise RuntimeError(
            f"verifier mismatch bridge={on_chain} config={expected} — redeploy bridge or fix sepolia-qc.json"
        )
    print(f"verifier ok on_chain={on_chain}", flush=True)


def pick_target(cache: dict, eth_tip: int) -> tuple[int, str, str, str, dict] | None:
    eligible: list[tuple[int, str, str, str, dict]] = []
    for (h, header, parent, claim), atts in cache.items():
        valid = committee_atts(atts)
        if h <= eth_tip or len(valid) < THRESHOLD:
            continue
        eligible.append((h, header, parent, claim, valid))
    if not eligible:
        return None

    if TARGET_HEIGHT > 0:
        for item in eligible:
            if item[0] == TARGET_HEIGHT:
                return item
        return None

    mesh = mesh_fpl_tip()
    lag = max(0, mesh - eth_tip)
    barrier = mesh_open_withdraw_barrier()

    # Open EVM withdrawals: submit the lowest attested height that unblocks claims.
    if barrier > eth_tip:
        barrier_exact = [e for e in eligible if e[0] == barrier]
        if barrier_exact:
            print(f"withdraw barrier eth={eth_tip} barrier={barrier}", flush=True)
            return barrier_exact[0]
        barrier_above = [e for e in eligible if barrier <= e[0] <= mesh]
        if barrier_above:
            best = min(barrier_above, key=lambda x: x[0])
            print(f"withdraw catch-up eth={eth_tip} barrier={barrier} target_h={best[0]}", flush=True)
            return best

    # Large lag: jump only for disaster / initial catch-up.
    allow_jump = lag > JUMP_LAG and (not STEADY or lag >= DISASTER_LAG)
    if allow_jump:
        jump_pool = [
            e for e in eligible if e[0] > eth_tip and e[0] <= mesh and (e[0] % SAMPLE_GAP == 0 or e[0] == mesh)
        ]
        if jump_pool:
            best = pick_on_grid(jump_pool, mesh)
            print(f"jump target eth={eth_tip} mesh={mesh} lag={lag} target_h={best[0]}", flush=True)
            return best

    # Steady state: lowest attested height on the sample grid above eth_tip.
    next_sample = max(eth_tip + 1, ((eth_tip // SAMPLE_GAP) + 1) * SAMPLE_GAP)
    candidates = [e for e in eligible if next_sample <= e[0] <= mesh]
    if candidates:
        best = min(candidates, key=lambda x: x[0])
        print(f"grid target eth={eth_tip} mesh={mesh} target_h={best[0]}", flush=True)
        return best

    # Fallback: lowest eligible above tip (e.g. withdraw at non-grid height).
    above = [e for e in eligible if e[0] > eth_tip and e[0] <= mesh]
    if above:
        best = min(above, key=lambda x: x[0])
        print(f"sequential target eth={eth_tip} mesh={mesh} target_h={best[0]}", flush=True)
        return best
    return None


def prefetch_height(height: int) -> None:
    """Prove the next grid header while the current submit is in flight."""
    try:
        cache: dict = {}
        scan_sigs_dir(cache, max(0, height - SAMPLE_GAP), height, grid_only=True, lag=0)
        poll_seats(cache)
        for (h, header, parent, claim), atts in cache.items():
            if h != height:
                continue
            valid = committee_atts(atts)
            if len(valid) < THRESHOLD:
                continue
            print(f"prefetch prove h={height}", flush=True)
            prove_four(h, parent, header, claim, valid)
            return
    except Exception as e:
        print(f"prefetch h={height} err={e}", flush=True)


def process_once() -> int:
    cache: dict = {}
    eth_tip = eth_fpl_tip()
    mesh = mesh_fpl_tip()
    lag = max(0, mesh - eth_tip)

    if lag > SAMPLE_GAP:
        maybe_collect_sigs(mesh, eth_tip)

    scan_lo = max(0, eth_tip - SAMPLE_GAP)
    scan_hi = mesh if mesh > 0 else eth_tip + SAMPLE_GAP * 4
    scan_sigs_dir(cache, scan_lo, scan_hi, grid_only=(lag <= JUMP_LAG), lag=lag)
    poll_seats(cache)
    target = pick_target(cache, eth_tip)
    if not target:
        print(f"idle eth_fpl_tip={eth_tip} mesh={mesh} lag={lag} sig_sets={len(cache)}", flush=True)
        maybe_collect_sigs(mesh, eth_tip)
        return 0
    h, header, parent, claim, atts = target
    if eth_header_finalized(h) or eth_tip >= h:
        print(f"skip h={h} already on chain eth_fpl_tip={eth_tip}", flush=True)
        return 0
    if not attestation_matches_hub(h, claim):
        maybe_collect_sigs(mesh, eth_tip, force=True)
        return 0
    if submit_cooldown_active(h, eth_tip):
        print(f"cooldown h={h} eth_fpl_tip={eth_tip} — prior submit did not advance tip", flush=True)
        return 0
    indices, proofs = prove_four(h, parent, header, claim, atts)
    print(f"proved h={h} indices={indices} sigs={len(atts)}", flush=True)
    next_h = ((h // SAMPLE_GAP) + 1) * SAMPLE_GAP
    threading.Thread(target=prefetch_height, args=(next_h,), daemon=True).start()
    if not PROVE_ONLY and BRIDGE:
        verify_proofs_local(h, indices, proofs)
        print(f"submitting h={h} to Sepolia (eth_tip={eth_fpl_tip()})", flush=True)
        try:
            tx = submit_proofs(h, parent, header, claim, indices, proofs)
        except RuntimeError as e:
            if "reverted" in str(e).lower() or "proof" in str(e).lower():
                mark_proof_fail(h, str(e))
                maybe_collect_sigs(mesh_fpl_tip(), eth_fpl_tip(), force=True)
            raise
        print(f"submitted h={h} tx={tx} eth_fpl_tip={eth_fpl_tip()}", flush=True)
    return 1


def main() -> int:
    if not PK.exists():
        print(
            f"no Falcon QC OPEN=1 pk at {PK} — FPL_PTAU=... bash scripts/falcon-qc-setup-one-ptau.sh",
            flush=True,
        )
        return 2
    if not SNARK.exists():
        print(f"no falcon-pl-snark at {SNARK}", flush=True)
        return 2
    KEYS.mkdir(parents=True, exist_ok=True)
    SIGS.mkdir(parents=True, exist_ok=True)
    mode = "prove-only" if (PROVE_ONLY or not BRIDGE) else f"submit→{BRIDGE}"
    cfg = bridge_config_summary()
    mesh = mesh_fpl_tip()
    eth0 = eth_fpl_tip()
    print(
        f"falcon-qc-prover v4 pk={PK} keys={KEYS} mode={mode} snark={SNARK}",
        flush=True,
    )
    print(
        f"bridge_config bridge={cfg['bridge']} eth_fpl_tip={eth0} mesh={mesh} "
        f"lag={max(0, mesh - eth0)} gap={SAMPLE_GAP} jump_lag={JUMP_LAG} "
        f"prove_parallel={PROVE_PARALLEL} prove_workers={PROVE_WORKERS}",
        flush=True,
    )
    if BRIDGE.lower() == LEGACY_BABYJUB_BRIDGE.lower():
        print("FATAL: refusing legacy BabyJub bridge", flush=True)
        return 2
    try:
        assert_verifier_matches_bridge()
    except RuntimeError as e:
        print(f"FATAL: {e}", flush=True)
        return 2
    PROVER_LOCK.parent.mkdir(parents=True, exist_ok=True)
    if PROVER_LOCK.exists():
        try:
            old_pid = int(PROVER_LOCK.read_text().strip())
            os.kill(old_pid, 0)
            print(f"another prover holds lock pid={old_pid} — exit", flush=True)
            return 1
        except (OSError, ValueError, ProcessLookupError):
            print(f"removing stale prover lock at {PROVER_LOCK}", flush=True)
            PROVER_LOCK.unlink(missing_ok=True)
    lock_f = open(PROVER_LOCK, "w")
    try:
        fcntl.flock(lock_f, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        print(f"another prover holds {PROVER_LOCK} — exit", flush=True)
        return 1
    lock_f.write(str(os.getpid()))
    lock_f.flush()
    while True:
        try:
            n = process_once()
            if n:
                print(f"completed {n} header(s)", flush=True)
        except Exception as e:
            print(f"operator error: {e}", flush=True)
        if ONCE:
            return 0
        time.sleep(POLL)


if __name__ == "__main__":
    sys.exit(main())
