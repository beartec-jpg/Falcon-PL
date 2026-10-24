#!/usr/bin/env python3
"""DEPRECATED: BabyJub LC prover for legacy FalconBridge.

Use pl-falcon-qc-prover.py with FalconQcBridge (Falcon-512 QC).
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import time
import warnings
from pathlib import Path

warnings.warn(
    "pl-lc-prover.py is deprecated — use pl-falcon-qc-prover.py",
    DeprecationWarning,
    stacklevel=1,
)

_SCRIPT = Path(__file__).resolve()
if str(_SCRIPT.parent) not in sys.path:
    sys.path.insert(0, str(_SCRIPT.parent))
from eth_bridge_config import LEGACY_BABYJUB_BRIDGE  # noqa: E402

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL = Path(os.environ.get("FPL_CTL", str(ROOT / "bin/falcon-pl-ctl")))
SNARK = Path(
    os.environ.get(
        "FPL_SNARK",
        str(Path.home() / "falcon-pl/falcon-pl-rs/target/release/falcon-pl-snark"),
    )
)
CAST = Path(os.environ.get("CAST", str(Path.home() / ".foundry/bin/cast")))
PK = Path(os.environ.get("LC_PK", str(ROOT / "run/snark/ceremony/pk.bin")))
KEYS = Path(os.environ.get("LC_KEYS", str(ROOT / "keys/lc")))
SIGS = Path(os.environ.get("LC_SIGS", str(ROOT / "run/snark/catchup/sigs")))
WORK = Path(os.environ.get("LC_PROVER_DIR", str(ROOT / "run/snark/prover")))
BRIDGE = os.environ.get("SEPOLIA_BRIDGE", LEGACY_BABYJUB_BRIDGE)
ETH_RPC = os.environ.get("ETH_RPC", "https://ethereum-sepolia-rpc.publicnode.com")
PK_FILE = Path(
    os.environ.get("ETH_PK_FILE", str(Path.home() / "falcon-pl-vault-test/eth-wallets/deployer.privhex"))
)
THRESHOLD = int(os.environ.get("LC_THRESHOLD", "4"))
NETWORK_ID = int(os.environ.get("FPL_NETWORK_ID", "2300"))
POLL = int(os.environ.get("POLL_SEC", "8"))
SAMPLE_GAP = int(os.environ.get("LC_SAMPLE_GAP", "48"))
ONCE = os.environ.get("LC_PROVER_ONCE", "") in ("1", "true", "yes")
SUBMIT_ABI = (
    "submitHeaderWithProof(uint64,bytes32,bytes32,bytes32,uint256[2],uint256[2][2],uint256[2])"
)
SEATS = [
    os.environ.get("FPL_RPC", "127.0.0.1:19301"),
    "100.64.0.2:19302",
    "100.64.0.3:19303",
    "100.64.0.4:19304",
    "100.64.0.5:19305",
    "100.64.0.6:19306",
]


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
    key = (h, header, parent, claim)
    cache.setdefault(key, {})[sid] = {
        "height": h,
        "header": header,
        "parent": parent,
        "claim_root": claim,
        "sig": sig if isinstance(sig, dict) and "s" in sig else att.get("sig"),
    }


def ingest_file(cache: dict, path: Path) -> None:
    try:
        att = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return
    ingest_att(cache, att)


def scan_sigs_dir(cache: dict) -> None:
    if not SIGS.is_dir():
        return
    for p in SIGS.glob("*.json"):
        ingest_file(cache, p)


def poll_seats(cache: dict) -> None:
    for addr in SEATS:
        try:
            st = ctl_status(addr)
        except Exception as e:
            print(f"status {addr}: {e}", flush=True)
            continue
        lc = st.get("lc") or {}
        for att in lc.get("attestations") or []:
            if isinstance(att, dict):
                ingest_att(cache, att)
                # persist
                h = int(att.get("height") or 0)
                sid = ((att.get("sig") or {}).get("id") if isinstance(att.get("sig"), dict) else None) or ""
                if h and sid:
                    SIGS.mkdir(parents=True, exist_ok=True)
                    dest = SIGS / f"{h:010}-{sid}.json"
                    if not dest.exists():
                        dest.write_text(json.dumps(att) + "\n")


def eth_tip() -> int:
    out = subprocess.check_output(
        [str(CAST), "call", BRIDGE, "fplTip()(uint64)", "--rpc-url", ETH_RPC],
        text=True,
        timeout=30,
    )
    s = out.strip().split()[0]
    return int(s, 0)


def proof_args(p: dict) -> list[str]:
    a = f"[{int(p['a'][0], 16)},{int(p['a'][1], 16)}]"
    b = (
        f"[[{int(p['b'][0][0], 16)},{int(p['b'][0][1], 16)}],"
        f"[{int(p['b'][1][0], 16)},{int(p['b'][1][1], 16)}]]"
    )
    c = f"[{int(p['c'][0], 16)},{int(p['c'][1], 16)}]"
    return [a, b, c]


def prove_one(height: int, parent: str, header: str, claim: str, atts: dict) -> dict:
    work = WORK / f"h-{height}"
    proof = work / "proof.json"
    if proof.exists() and proof.stat().st_size > 0:
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
    for sid, att in atts.items():
        (sigdir / f"{sid}.json").write_text(json.dumps(att) + "\n")
    proof = work / "proof.json"
    subprocess.check_call(
        [
            str(SNARK),
            "prove",
            "--pk",
            str(PK),
            "--keys-dir",
            str(KEYS),
            "--sigs-dir",
            str(sigdir),
            "--height",
            str(height),
            "--parent",
            parent,
            "--header",
            header,
            "--claim-root",
            claim or ("00" * 32),
            "--network-id",
            str(NETWORK_ID),
            "--threshold",
            str(THRESHOLD),
            "--out",
            str(proof),
        ]
    )
    return json.loads(proof.read_text())


def submit(height: int, parent: str, header: str, claim: str, proof: dict) -> str:
    cmd = [
        str(CAST),
        "send",
        BRIDGE,
        SUBMIT_ABI,
        str(height),
        b32(header),
        b32(parent),
        b32(claim or ("00" * 32)),
        *proof_args(proof),
        "--rpc-url",
        ETH_RPC,
        "--private-key",
        load_pk(),
        "--gas-limit",
        "800000",
    ]
    extra = os.environ.get("CAST_SEND_FLAGS", "").split()
    cmd += extra
    r = subprocess.run(cmd, text=True, capture_output=True)
    blob = (r.stdout or "") + (r.stderr or "")
    if r.returncode != 0:
        raise RuntimeError(blob[-800:])
    hx = ""
    for tok in blob.replace('"', " ").split():
        if tok.startswith("0x") and len(tok) == 66:
            hx = tok
            break
    if not hx:
        raise RuntimeError("cast send produced no tx hash: " + blob[-400:])
    deadline = time.time() + 180
    js = {}
    while time.time() < deadline:
        try:
            st = subprocess.run(
                [str(CAST), "receipt", hx, "--rpc-url", ETH_RPC, "--json"],
                text=True,
                capture_output=True,
                timeout=12,
            )
        except subprocess.TimeoutExpired:
            time.sleep(4)
            continue
        raw = (st.stdout or "").strip()
        if raw.startswith("{"):
            try:
                js = json.loads(raw)
            except json.JSONDecodeError:
                js = {}
            if js.get("blockNumber") or js.get("status") is not None:
                break
        time.sleep(4)
    if not js:
        raise RuntimeError(f"no receipt after 180s {hx}")
    if str(js.get("status")) in ("0", "0x0"):
        raise RuntimeError(f"tx failed {hx}")
    return hx


def pick(cache: dict, height: int) -> tuple[str, str, str, dict] | None:
    best = None
    for (h, header, parent, claim), atts in cache.items():
        if h != height:
            continue
        if len(atts) < THRESHOLD:
            continue
        if best is None or len(atts) > len(best[3]):
            best = (header, parent, claim, atts)
    return best


def main() -> int:
    WORK.mkdir(parents=True, exist_ok=True)
    SIGS.mkdir(parents=True, exist_ok=True)
    cache: dict = {}
    scan_sigs_dir(cache)
    print(
        f"prover bridge={BRIDGE} sigs={len(cache)} pk={PK.exists()} snark={SNARK.exists()}",
        flush=True,
    )
    while True:
        scan_sigs_dir(cache)
        poll_seats(cache)
        try:
            tip = eth_tip()
        except Exception as e:
            print(f"eth tip: {e}", flush=True)
            time.sleep(POLL)
            continue
        nxt = tip + 1
        latest = None
        for (h, header, parent, claim), atts in cache.items():
            if h <= tip or len(atts) < THRESHOLD:
                continue
            if latest is None or h > latest[0]:
                latest = (h, header, parent, claim, atts)
        # Sampled headers only. tip+1 crawl burns Sepolia ETH (~0.0005/proof).
        got = None
        target = nxt
        if latest and latest[0] >= tip + SAMPLE_GAP:
            target, header, parent, claim, atts = latest
            got = (header, parent, claim, atts)
        n4 = len(got[3]) if got else (len(latest[4]) if latest else 0)
        print(
            f"eth_tip={tip} next={nxt} target={target} latest={latest[0] if latest else 0} "
            f"gap={SAMPLE_GAP} sigs={n4}",
            flush=True,
        )
        if got:
            header, parent, claim, atts = got
            try:
                proof = prove_one(target, parent, header, claim, atts)
                hx = submit(target, parent, header, claim, proof)
                print(f"SUBMIT h={target} tx={hx}", flush=True)
                if ONCE:
                    return 0
                time.sleep(POLL)
                continue
            except Exception as e:
                print(f"prove/submit h={target}: {e}", flush=True)
                if ONCE:
                    return 1
        if ONCE:
            return 2
        time.sleep(POLL)


if __name__ == "__main__":
    sys.exit(main())
