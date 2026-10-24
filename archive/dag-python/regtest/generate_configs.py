#!/usr/bin/env python3
"""Generate 5-validator Falcon DAG regtest configs + Falcon-512 keys."""

from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

N = 5
BASE_RPC = 18001
BASE_PEER = 19001
NETWORK_ID = 9002


def main() -> None:
    out = Path(__file__).resolve().parent / "configs"
    out.mkdir(parents=True, exist_ok=True)
    ids = [f"val{i+1}" for i in range(N)]
    unl = {vid: 1.0 for vid in ids}
    genesis = {f"acct{i}": 1_000_000_000 for i in range(1, 21)}

    keys_dir = ROOT / "data" / "regtest" / "keys"
    crypto_scheme = "falcon-512"
    try:
        from falcon_dag.crypto import Falcon512KeyRing, falcon_available

        if falcon_available():
            if not (keys_dir / "unl_public.json").is_file():
                ring = Falcon512KeyRing.generate_unl(ids)
                ring.save_dir(keys_dir)
                print(f"generated Falcon-512 keys → {keys_dir}")
            else:
                print(f"reusing Falcon-512 keys → {keys_dir}")
            m = Falcon512KeyRing.load_for_validator(keys_dir, ids[0]).metrics()
            print(
                f"  scheme={m['scheme']} pk={m['public_key_bytes']}B "
                f"sample_sig≈{m.get('sample_sig_bytes')}B"
            )
        else:
            crypto_scheme = "hmac-sha256-dev"
            print("falcon-python missing; configs use hmac-sha256-dev")
    except Exception as e:
        crypto_scheme = "hmac-sha256-dev"
        print(f"Falcon key setup failed ({e}); using hmac-sha256-dev")

    for i, vid in enumerate(ids):
        peers = [["127.0.0.1", BASE_PEER + j] for j in range(N) if j != i]
        cfg = {
            "validator_id": vid,
            "network_id": NETWORK_ID,
            "rpc_host": "127.0.0.1",
            "rpc_port": BASE_RPC + i,
            "peer_host": "127.0.0.1",
            "peer_port": BASE_PEER + i,
            "peers": peers,
            "unl": unl,
            "unl_threshold": 0.80,
            "max_batch_parents": 6,
            "target_batch_interval_s": 0.35,
            "max_batch_txs": 128,
            "hard_finality_h": 6,
            "worker_sharding": True,
            "worker_index": i,
            "worker_count": N,
            "adaptive_interval": True,
            "require_sig_verify": True,
            "crypto_scheme": crypto_scheme,
            "keys_dir": str(keys_dir) if crypto_scheme.startswith("falcon") else None,
            "data_dir": str(ROOT / "data" / "regtest" / "nodes" / vid),
            "snapshot_interval_s": 5.0,
            "max_uncertified_batches": 2000,
            "max_uncertified_per_creator": 64,
            "genesis_balances": genesis,
        }
        (out / f"{vid}.json").write_text(json.dumps(cfg, indent=2) + "\n")
        print(f"wrote {out / f'{vid}.json'}")
    (out / "network.json").write_text(
        json.dumps(
            {
                "n": N,
                "network_id": NETWORK_ID,
                "architecture": "narwhal-da + federated-ordering",
                "crypto_scheme": crypto_scheme,
                "rpc_ports": [BASE_RPC + i for i in range(N)],
            },
            indent=2,
        )
        + "\n"
    )


if __name__ == "__main__":
    main()
