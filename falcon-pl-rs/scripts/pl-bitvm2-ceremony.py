#!/usr/bin/env python3
"""Safe BitVM2 ceremony: payout+setup instance, Lamport PK, split setup-1 to root.

Does not print secrets. Requires sudo for the independent setup key.
"""
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import urllib.request
from pathlib import Path

from cryptography.hazmat.primitives.asymmetric import ec

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL = os.environ.get(
    "FALCON_PL_CTL_BTC",
    str(Path.home() / "falcon-pl/falcon-pl-rs/target/debug/falcon-pl-ctl"),
)
WALLETS = Path.home() / "falcon-pl-vault-test/btc-wallets"
SETUP_DIR = Path.home() / "falcon-pl-vault-test/bitvm2-setup"
OUT = ROOT / "run/bitvm2-instance.json"
LAMPORT_PK = SETUP_DIR / "lamport-pk.json"
LAMPORT_SK = SETUP_DIR / "lamport-sk.json"
DAVE_PRIV = WALLETS / "dave.privhex"
FEE_PRIV = WALLETS / "fee.privhex"
FEE_ADDR = "tb1qpjttdgpgz4s24vka49gzt3t5fwp0t7ljtzmhe0"
EXPLORER = os.environ.get("BTC_EXPLORER", "https://mempool.space/testnet/api").rstrip("/")
CSV = int(os.environ.get("BITVM2_PAYOUT_CSV", "6"))
ASSERT_CSV = int(os.environ.get("BITVM2_ASSERT_CSV", "6"))
FUND_SATS = int(os.environ.get("BITVM2_FUND_SATS", "9000"))
FEE_SATS = int(os.environ.get("BITVM2_FUND_FEE", "1000"))
ROOT_KEY = "/root/falcon-bitvm2-setup/setup-1.privhex"
UA = {"User-Agent": "fpl-bitvm2-ceremony/1.0"}
BITS = 256


def log(msg: str) -> None:
    print(msg, flush=True)


def read_privhex(path: Path) -> bytes:
    t = path.read_text(encoding="utf-8").strip().removeprefix("0x")
    b = bytes.fromhex(t)
    if len(b) != 32:
        raise SystemExit(f"{path} is not 32 bytes")
    return b


def xonly_hex(secret: bytes) -> str:
    n = int.from_bytes(secret, "big")
    sk = ec.derive_private_key(n, ec.SECP256K1())
    return sk.public_key().public_numbers().x.to_bytes(32, "big").hex()


def write_secret(path: Path, secret: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(secret.hex() + "\n", encoding="utf-8")
    os.chmod(tmp, 0o600)
    tmp.replace(path)
    os.chmod(path, 0o600)


def ctl(*args: str) -> str:
    p = subprocess.run([CTL, *args], capture_output=True, text=True, timeout=120)
    if p.returncode != 0:
        raise RuntimeError((p.stderr or p.stdout or f"exit {p.returncode}").strip()[-1500:])
    return p.stdout


def http_json(url: str):
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read())


def broadcast(raw_hex: str) -> str:
    req = urllib.request.Request(
        f"{EXPLORER}/tx",
        data=raw_hex.strip().encode(),
        headers={**UA, "Content-Type": "text/plain"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode().strip()


def sudo(*args: str) -> str:
    p = subprocess.run(["sudo", *args], capture_output=True, text=True, timeout=60)
    if p.returncode != 0:
        raise RuntimeError((p.stderr or p.stdout).strip()[-800:])
    return p.stdout


def gen_lamport() -> None:
    zero = [os.urandom(32) for _ in range(BITS)]
    one = [os.urandom(32) for _ in range(BITS)]
    pk = {
        "zero_hex": [hashlib.sha256(z).hexdigest() for z in zero],
        "one_hex": [hashlib.sha256(o).hexdigest() for o in one],
    }
    sk = {
        "zero": [z.hex() for z in zero],
        "one": [o.hex() for o in one],
    }
    SETUP_DIR.mkdir(parents=True, exist_ok=True)
    LAMPORT_PK.write_text(json.dumps(pk) + "\n", encoding="utf-8")
    os.chmod(LAMPORT_PK, 0o644)
    LAMPORT_SK.write_text(json.dumps(sk) + "\n", encoding="utf-8")
    os.chmod(LAMPORT_SK, 0o600)


def main() -> int:
    if not Path(CTL).is_file():
        raise SystemExit(f"ctl missing: {CTL}")
    dave = read_privhex(DAVE_PRIV)
    op_x = xonly_hex(dave)
    SETUP_DIR.mkdir(parents=True, exist_ok=True)

    s0_path = SETUP_DIR / "setup-0.privhex"
    if s0_path.exists():
        s0 = read_privhex(s0_path)
    else:
        s0 = os.urandom(32)
        write_secret(s0_path, s0)
        log(f"wrote {s0_path}")

    s1 = os.urandom(32)
    tmp1 = Path("/tmp/bitvm2-setup-1.privhex")
    write_secret(tmp1, s1)
    sudo("mkdir", "-p", "/root/falcon-bitvm2-setup")
    sudo("install", "-m", "400", "-o", "root", "-g", "root", str(tmp1), ROOT_KEY)
    tmp1.unlink(missing_ok=True)
    # do not keep setup-1 on falcon home
    old1 = SETUP_DIR / "setup-1.privhex"
    if old1.exists():
        old1.unlink()
        log("removed falcon-readable setup-1")

    if not LAMPORT_PK.exists() or not LAMPORT_SK.exists():
        gen_lamport()
        log(f"wrote {LAMPORT_PK} + lamport-sk (600)")

    setup_hex = [xonly_hex(s0), xonly_hex(s1)]
    log(f"operator_xonly={op_x}")
    log(f"setup_0={setup_hex[0]}")
    log(f"setup_1={setup_hex[1]} (root-only {ROOT_KEY})")
    raw = ctl(
        "btc-bitvm2-instance",
        "--operator-xonly",
        op_x,
        "--setup-xonly",
        ",".join(setup_hex),
        "--csv",
        str(CSV),
        "--assert-csv",
        str(ASSERT_CSV),
        "--lamport-pk-json",
        str(LAMPORT_PK),
    )
    inst = json.loads(raw[raw.find("{") :])
    addr = inst["address"]
    log(f"instance_address={addr}")
    log(f"leaves={len(inst.get('leaves') or [])} (want 2)")
    log(f"assert_spk={inst['assert_spk'][:20]}…")

    utxos = http_json(f"{EXPLORER}/address/{FEE_ADDR}/utxo") or []
    confirmed = [u for u in utxos if (u.get("status") or {}).get("confirmed")]
    need = FUND_SATS + FEE_SATS
    cand = [u for u in confirmed if int(u.get("value") or 0) >= need]
    if not cand:
        raise SystemExit(f"fee wallet has no confirmed UTXO ≥ {need}")
    u = min(cand, key=lambda x: int(x["value"]))
    log(f"funding from fee {u['txid']}:{u['vout']} value={u['value']}")
    pay = ctl(
        "btc-pay-tx",
        "--privhex-file",
        str(FEE_PRIV),
        "--prev-txid",
        u["txid"],
        "--vout",
        str(int(u["vout"])),
        "--sats",
        str(int(u["value"])),
        "--dest",
        addr,
        "--amount",
        str(FUND_SATS),
        "--fee",
        str(FEE_SATS),
    ).strip().splitlines()[-1]
    txid = broadcast(pay)
    log(f"broadcast fund txid={txid}")

    # setup-1 via sudo to a 600 temp, sign, shred
    tmp1 = Path("/tmp/bitvm2-setup-1-sign.privhex")
    blob = sudo("cat", ROOT_KEY)
    write_secret(tmp1, bytes.fromhex(blob.strip()))
    ch = ctl(
        "btc-bitvm2-challenge",
        "--utxo",
        f"{txid}:0:{FUND_SATS}",
        "--operator-xonly",
        op_x,
        "--setup-secret-files",
        f"{s0_path},{tmp1}",
        "--fee",
        str(FEE_SATS),
        "--csv",
        str(CSV),
        "--assert-csv",
        str(ASSERT_CSV),
        "--lamport-pk-json",
        str(LAMPORT_PK),
    ).strip().splitlines()[-1]
    tmp1.unlink(missing_ok=True)

    doc = {
        "network": "bitcoin-testnet",
        "address": addr,
        "spk": inst["spk"],
        "assert_spk": inst["assert_spk"],
        "merkle_root": inst["merkle_root"],
        "nums_tag": inst["nums_tag"],
        "csv_blocks": CSV,
        "assert_csv_blocks": ASSERT_CSV,
        "watch_key": "none",
        "key_path": "unspendable-NUMS",
        "leaves": inst.get("leaves"),
        "operator_xonly": op_x,
        "setup_xonly": setup_hex,
        "setup_1_location": ROOT_KEY,
        "lamport_pk": str(LAMPORT_PK),
        "funding_txid": txid,
        "funding_vout": 0,
        "funding_sats": FUND_SATS,
        "fund_from": FEE_ADDR,
        "challenge_raw_hex": ch,
        "kickoff": "frozen",
        "notes": "Safe instance: payout+setup only. Assert has operator-recover + Lamport equivocation. setup-1 is root-only. Kickoff frozen.",
    }
    OUT.parent.mkdir(parents=True, exist_ok=True)
    tmp = OUT.with_suffix(".tmp")
    tmp.write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
    tmp.replace(OUT)
    log(f"wrote {OUT}")
    safe = {k: v for k, v in doc.items() if k != "challenge_raw_hex"}
    print(json.dumps(safe, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
