#!/usr/bin/env python3
"""Falcon-512 QC catch-up: collect validator sigs and/or sign archive headers.

Modes:
  collect  — rsync falcon-qc/sigs from all validator seats (default)
  sign     — sign archive headers with node Falcon keys (historical gap)

Historical claim_root: uses an existing sig at that height when present, else
32 zero bytes (valid only before ETH/USDC withdraws existed at that height).
Live validators attest the correct claim_root on commit — prefer `collect`.

Env:
  PL_ROOT / FPL_SNARK / FPL_NETWORK_ID
  FALCON_QC_START_HEIGHT / FALCON_QC_END_HEIGHT
  FALCON_QC_SAMPLE_GAP — sign only sampled heights (default 48)
  FALCON_QC_TARGET_HEIGHT — sign one height only
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
BODIES = Path(os.environ.get("PL_BODIES", str(ROOT / "snapshots/bodies/v1")))
OUT = Path(os.environ.get("FALCON_QC_CATCHUP_DIR", str(ROOT / "run/snark/falcon-qc-catchup")))
SIGS = Path(os.environ.get("FALCON_QC_SIGS", str(ROOT / "keys/falcon-qc/sigs")))
START = int(os.environ.get("FALCON_QC_START_HEIGHT", "0"))
END_ENV = os.environ.get("FALCON_QC_END_HEIGHT", "")
TARGET = int(os.environ.get("FALCON_QC_TARGET_HEIGHT", "0") or "0")
SAMPLE_GAP = int(os.environ.get("FALCON_QC_SAMPLE_GAP", "48"))
NETWORK_ID = int(os.environ.get("FPL_NETWORK_ID", "2300"))
SNARK = Path(
    os.environ.get(
        "FPL_SNARK",
        str(Path.home() / "falcon-pl/falcon-pl-rs/target/release/falcon-pl-snark"),
    )
)
SSH_OPTS = [
    "-o",
    "ConnectTimeout=15",
    "-o",
    "BatchMode=yes",
    "-o",
    "StrictHostKeyChecking=accept-new",
]
ZERO32 = "00" * 32

SEATS = [
    ("v1", "host1", "/home/falcon/falcon-pl-public-testnet-2300"),
    ("v2", "root@100.64.0.2", "/root/falcon-pl-public-testnet-2300"),
    ("v3", "host2", "/home/falcon/falcon-pl-public-testnet-2300"),
    ("v4", "root@100.64.0.4", "/root/falcon-pl-public-testnet-2300"),
    ("v5", "root@100.64.0.5", "/root/falcon-pl-public-testnet-2300"),
    ("v6", "root@100.64.0.6", "/root/falcon-pl-public-testnet-2300"),
]


def tip_from_bodies() -> int:
    if END_ENV:
        return int(END_ENV)
    best = START
    for p in BODIES.glob("h-*.hash"):
        try:
            h = int(p.name[2 : p.name.index(".")])
        except ValueError:
            continue
        if h > best:
            best = h
    return best


def claim_root_for_height(h: int) -> str:
    for p in SIGS.glob(f"{h:010}-*.json"):
        try:
            body = json.loads(p.read_text())
            cr = str(body.get("claim_root") or "").strip().lower().removeprefix("0x")
            if len(cr) == 64:
                return cr
        except (OSError, json.JSONDecodeError):
            continue
    return ZERO32


def dump_statements(start_h: int, end_h: int) -> list[dict]:
    rows: list[dict] = []
    for h in range(start_h, end_h + 1):
        if TARGET > 0 and h != TARGET:
            continue
        if TARGET == 0 and SAMPLE_GAP > 1 and h % SAMPLE_GAP != 0 and h != end_h:
            continue
        hp = BODIES / f"h-{h}.hash"
        if not hp.exists():
            print(f"skip missing archive h={h}", flush=True)
            continue
        header = hp.read_text().strip().lower()
        body = json.loads((BODIES / f"{header}.json").read_text())
        parent = str(body.get("parent_hash") or "").strip().lower()
        if body.get("height") != h:
            raise SystemExit(f"body height mismatch {h}")
        rows.append(
            {
                "height": h,
                "parent": parent,
                "header": header,
                "claim_root": claim_root_for_height(h),
                "network_id": NETWORK_ID,
            }
        )
    return rows


def rsync_from(host: str, src: str, dst: Path) -> None:
    dst.mkdir(parents=True, exist_ok=True)
    subprocess.check_call(
        ["rsync", "-az", "-e", "ssh " + " ".join(SSH_OPTS), f"{host}:{src}", str(dst)]
    )


def rsync_to(host: str, src: Path, dst: str) -> None:
    subprocess.check_call(
        ["rsync", "-az", "-e", "ssh " + " ".join(SSH_OPTS), str(src), f"{host}:{dst}"]
    )


def ssh(host: str, cmd: str) -> None:
    subprocess.check_call(["ssh", *SSH_OPTS, host, cmd])


def collect_sigs() -> int:
    SIGS.mkdir(parents=True, exist_ok=True)
    n = 0
    for vid, host, rroot in SEATS:
        print(f"collect {vid} from {host}", flush=True)
        try:
            rsync_from(host, f"{rroot}/keys/falcon-qc/sigs/", SIGS)
            n += 1
        except subprocess.CalledProcessError as e:
            print(f"WARN collect {vid}: {e}", flush=True)
    files = list(SIGS.glob("*.json"))
    print(f"collected {len(files)} sig files from {n} seats", flush=True)
    return 0


def sign_statements(rows: list[dict]) -> int:
    if not rows:
        print("nothing to sign", flush=True)
        return 0
    if not SNARK.exists():
        print(f"missing falcon-pl-snark at {SNARK}", file=sys.stderr)
        return 2
    OUT.mkdir(parents=True, exist_ok=True)
    stmt = OUT / "statements.json"
    stmt.write_text(json.dumps(rows) + "\n")
    collected = OUT / "sigs"
    collected.mkdir(parents=True, exist_ok=True)

    helper = OUT / "falcon_qc_sign_batch.py"
    helper.write_text(
        """#!/usr/bin/env python3
import json, subprocess, sys
from pathlib import Path
root = Path(sys.argv[1])
vid = sys.argv[2]
rows = json.loads((root / "run/snark/falcon-qc-catchup/statements.json").read_text())
snark = root / "bin/falcon-pl-snark"
secret = root / "keys" / f"{vid}.json"
out_dir = root / "run/snark/falcon-qc-catchup" / f"sigs-{vid}"
out_dir.mkdir(parents=True, exist_ok=True)
for row in rows:
    h = int(row["height"])
    att_path = out_dir / f"{h:010}-{vid}.json"
    sig_path = out_dir / f"{h:010}-{vid}.sig.json"
    subprocess.check_call([
        str(snark), "falcon-sign",
        "--secret", str(secret),
        "--height", str(h),
        "--parent", row["parent"],
        "--header", row["header"],
        "--claim-root", row["claim_root"],
        "--network-id", str(row.get("network_id", 2300)),
        "--out", str(sig_path),
    ])
    sig = json.loads(sig_path.read_text())
    att = {
        "height": h,
        "header": row["header"],
        "parent": row["parent"],
        "claim_root": row["claim_root"],
        "sig": sig,
    }
    att_path.write_text(json.dumps(att) + "\\n")
print(f"signed {len(rows)} for {vid}")
"""
    )
    helper.chmod(0o755)

    for vid, host, rroot in SEATS:
        print(f"falcon-sign {vid} on {host} ({len(rows)} stmts)", flush=True)
        rsync_to(host, SNARK, f"{rroot}/bin/falcon-pl-snark")
        ssh(host, f"mkdir -p {rroot}/run/snark/falcon-qc-catchup {rroot}/keys/falcon-qc/sigs")
        rsync_to(host, stmt, f"{rroot}/run/snark/falcon-qc-catchup/statements.json")
        rsync_to(host, helper, f"{rroot}/run/snark/falcon-qc-catchup/falcon_qc_sign_batch.py")
        ssh(
            host,
            f"python3 {rroot}/run/snark/falcon-qc-catchup/falcon_qc_sign_batch.py {rroot} {vid}",
        )
        rsync_from(host, f"{rroot}/run/snark/falcon-qc-catchup/sigs-{vid}/", collected)
        rsync_to(host, Path(str(collected) + "/"), f"{rroot}/keys/falcon-qc/sigs/")

    for p in collected.glob("*.json"):
        dest = SIGS / p.name
        if not dest.exists():
            dest.write_bytes(p.read_bytes())
    print(f"signed {len(rows)} heights → {len(list(SIGS.glob('*.json')))} sig files under {SIGS}", flush=True)
    return 0


def cmd_sign() -> int:
    end = tip_from_bodies()
    start = START if START > 0 else max(1, end - SAMPLE_GAP * 4)
    if TARGET > 0:
        start = end = TARGET
    if end < start:
        print(f"nothing to sign start={start} end={end}")
        return 0
    print(f"sign headers {start}..{end} gap={SAMPLE_GAP} target={TARGET or 'all-sampled'}", flush=True)
    rows = dump_statements(start, end)
    return sign_statements(rows)


def main() -> int:
    mode = (sys.argv[1] if len(sys.argv) > 1 else "collect").strip().lower()
    SIGS.mkdir(parents=True, exist_ok=True)
    if mode == "collect":
        return collect_sigs()
    if mode == "sign":
        return cmd_sign()
    raise SystemExit(f"unknown mode {mode!r} — use collect or sign")


if __name__ == "__main__":
    raise SystemExit(main())
