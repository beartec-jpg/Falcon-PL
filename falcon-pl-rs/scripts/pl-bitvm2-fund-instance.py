#!/usr/bin/env python3
"""Fund a BitVM2 instance from an existing testnet wallet (no faucet).

Uses the fee wallet UTXO (operator-controlled test dest), not the NUMS bridge pool.
Writes setup keys (0600) + instance JSON. Does not print secrets.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import urllib.request
from pathlib import Path

from cryptography.hazmat.primitives.asymmetric import ec

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL = os.environ.get(
    "FALCON_PL_CTL",
    str(Path.home() / "falcon-pl/falcon-pl-rs/target/debug/falcon-pl-ctl"),
)
WALLETS = Path.home() / "falcon-pl-vault-test/btc-wallets"
SETUP_DIR = Path(os.environ.get("BITVM2_SETUP_DIR", str(Path.home() / "falcon-pl-vault-test/bitvm2-setup")))
OUT = Path(os.environ.get("BITVM2_INSTANCE_JSON", str(ROOT / "run/bitvm2-instance.json")))
DAVE_PRIV = WALLETS / "dave.privhex"
FEE_PRIV = WALLETS / "fee.privhex"
FEE_ADDR = "tb1qpjttdgpgz4s24vka49gzt3t5fwp0t7ljtzmhe0"
EXPLORER = os.environ.get("BTC_EXPLORER", "https://mempool.space/testnet/api").rstrip("/")
CSV = int(os.environ.get("BITVM2_PAYOUT_CSV", "6"))  # demo; product default is 144
ASSERT_CSV = int(os.environ.get("BITVM2_ASSERT_CSV", "6"))
FUND_SATS = int(os.environ.get("BITVM2_FUND_SATS", "9000"))
FEE_SATS = int(os.environ.get("BITVM2_FUND_FEE", "1000"))
UA = {"User-Agent": "fpl-bitvm2-fund/1.0"}


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
    x = sk.public_key().public_numbers().x
    return x.to_bytes(32, "big").hex()


def write_secret(path: Path, secret: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(secret.hex() + "\n", encoding="utf-8")
    tmp.chmod(0o600)
    tmp.replace(path)
    path.chmod(0o600)


def ctl(*args: str) -> str:
    p = subprocess.run([CTL, *args], capture_output=True, text=True, timeout=60)
    if p.returncode != 0:
        raise RuntimeError((p.stderr or p.stdout or f"exit {p.returncode}").strip()[-1200:])
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


def main() -> int:
    if not Path(CTL).is_file():
        raise SystemExit(f"ctl missing: {CTL}")
    dave = read_privhex(DAVE_PRIV)
    op_x = xonly_hex(dave)
    SETUP_DIR.mkdir(parents=True, exist_ok=True)
    keys = []
    for i in (0, 1):
        p = SETUP_DIR / f"setup-{i}.privhex"
        if p.exists():
            sec = read_privhex(p)
        else:
            sec = os.urandom(32)
            write_secret(p, sec)
            log(f"wrote {p} (0600)")
        keys.append((p, xonly_hex(sec)))
    setup_hex = [k[1] for k in keys]
    log(f"operator_xonly={op_x}")
    log(f"setup_xonly={','.join(setup_hex)}")
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
    )
    inst = json.loads(raw[raw.find("{") :])
    addr = inst["address"]
    log(f"instance_address={addr}")
    log(f"merkle_root={inst['merkle_root']}")
    log("watch_key=none key_path=unspendable-NUMS")

    if OUT.exists():
        prev = json.loads(OUT.read_text(encoding="utf-8"))
        if prev.get("funding_txid") and prev.get("address") == addr:
            log(f"already funded txid={prev['funding_txid']} — not sending again")
            print(json.dumps(prev, indent=2))
            return 0

    utxos = http_json(f"{EXPLORER}/address/{FEE_ADDR}/utxo") or []
    confirmed = [u for u in utxos if (u.get("status") or {}).get("confirmed")]
    need = FUND_SATS + FEE_SATS
    cand = [u for u in confirmed if int(u.get("value") or 0) >= need]
    if not cand:
        raise SystemExit(f"fee wallet {FEE_ADDR} has no UTXO ≥ {need} (have {confirmed})")
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

    ch = ctl(
        "btc-bitvm2-challenge",
        "--utxo",
        f"{txid}:0:{FUND_SATS}",
        "--operator-xonly",
        op_x,
        "--setup-secret-files",
        f"{keys[0][0]},{keys[1][0]}",
        "--fee",
        str(FEE_SATS),
        "--csv",
        str(CSV),
        "--assert-csv",
        str(ASSERT_CSV),
    ).strip().splitlines()[-1]

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
        "operator_xonly": op_x,
        "setup_xonly": setup_hex,
        "funding_txid": txid,
        "funding_vout": 0,
        "funding_sats": FUND_SATS,
        "fund_from": FEE_ADDR,
        "challenge_raw_hex": ch,
        "notes": "Demo CSV=6. Challenge tx is presigned (n-of-n); do not broadcast unless disputing. Payout after CSV via btc-bitvm2-payout. Not a Groth16-in-script Disprove.",
    }
    OUT.parent.mkdir(parents=True, exist_ok=True)
    tmp = OUT.with_suffix(".tmp")
    tmp.write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
    tmp.replace(OUT)
    log(f"wrote {OUT}")
    print(json.dumps({k: v for k, v in doc.items() if k != "challenge_raw_hex"}, indent=2))
    log(f"challenge_raw_hex_len={len(ch)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
