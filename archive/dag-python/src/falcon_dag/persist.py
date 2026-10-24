"""Snapshot persistence for crash recovery (research prototype)."""

from __future__ import annotations

import json
import os
import tempfile
from pathlib import Path
from typing import TYPE_CHECKING, Any

from falcon_dag.types import (
    GENESIS_BATCH_HASH,
    GENESIS_CERT_ID,
    Batch,
    Certificate,
    Transaction,
)

if TYPE_CHECKING:
    from falcon_dag.engine import FalconDAGEngine

SNAPSHOT_VERSION = 1


def engine_to_snapshot(engine: FalconDAGEngine) -> dict[str, Any]:
    dag = engine.dag
    batches = []
    for bh, b in dag.batches.items():
        if bh == GENESIS_BATCH_HASH:
            continue
        batches.append(b.to_dict())
    certs = []
    for bh, c in dag.certificates.items():
        if bh == GENESIS_BATCH_HASH:
            continue
        certs.append(c.to_dict())
    return {
        "version": SNAPSHOT_VERSION,
        "network_id": engine.config.network_id,
        "validator_id": engine.validator_id,
        "batches": batches,
        "certificates": certs,
        "ordered": list(engine.ordering.ordered),
        "hard_final": sorted(engine.ordering.hard_final),
        "executed_txs": list(engine.execution.executed_txs),
        "soft_final_txs": sorted(engine.execution.soft_final_txs),
        "mempool": [t.to_dict() for t in engine.mempool.ordered()],
        "balances": dict(engine.execution.state.balances),
        "sequences": dict(engine.execution.state.sequences),
        "batches_created": engine.batches_created,
    }


def apply_snapshot(engine: FalconDAGEngine, snap: dict[str, Any]) -> None:
    """Reload DAG + ordering + execution from a snapshot (best-effort)."""
    if int(snap.get("version", 0)) != SNAPSHOT_VERSION:
        raise ValueError(f"unsupported snapshot version {snap.get('version')}")
    if int(snap.get("network_id", -1)) != engine.config.network_id:
        raise ValueError("network_id mismatch")

    # Temporarily relax DoS bounds so large snapshots reload cleanly
    old_cfg = engine.config
    # Ingest batches first (signatures ride on batch objects)
    for bd in snap.get("batches", []):
        batch = Batch.from_dict(bd)
        engine.dag.ingest_batch(batch)
        for t in batch.txs.values():
            engine.dag.ingest_tx(t)

    # Force-install certificate records in causal order by round
    certs = [Certificate.from_dict(cd) for cd in snap.get("certificates", [])]
    certs.sort(key=lambda c: (c.round, c.timestamp, c.batch_hash))
    for cert in certs:
        if cert.batch_hash in engine.dag.certificates:
            continue
        batch = engine.dag.batches.get(cert.batch_hash)
        if batch is not None:
            new = engine.dag._try_certify(cert.batch_hash)
            if new is None and cert.batch_hash not in engine.dag.certificates:
                engine.dag.certificates[cert.batch_hash] = cert
                engine.dag.certs_by_id[cert.cert_id] = cert
        else:
            engine.dag.certificates[cert.batch_hash] = cert
            engine.dag.certs_by_id[cert.cert_id] = cert

    # Mempool
    for td in snap.get("mempool", []):
        engine.submit_tx(Transaction.from_dict(td))

    # Recompute order and execution
    engine.ordering.try_commit()
    engine.execution.apply_new_ordered()

    # Prefer snapshot executed state when present (bit-identical recovery)
    if snap.get("executed_txs") and snap.get("balances") and snap.get("sequences"):
        engine.execution.state.balances = {k: int(v) for k, v in snap["balances"].items()}
        engine.execution.state.sequences = {
            k: int(v) for k, v in snap["sequences"].items()
        }
        engine.execution.executed_txs = list(snap["executed_txs"])
        engine.execution.soft_final_txs = set(snap.get("soft_final_txs", []))
        if snap.get("ordered"):
            engine.ordering.ordered = list(snap["ordered"])
            engine.ordering.ordered_set = set(engine.ordering.ordered)
        if snap.get("hard_final"):
            engine.ordering.hard_final = set(snap["hard_final"])
            engine.ordering.hard_final.add(GENESIS_BATCH_HASH)

    engine.batches_created = int(snap.get("batches_created", engine.batches_created))
    _ = old_cfg  # kept for future soft override


def save_snapshot(path: str | Path, engine: FalconDAGEngine) -> None:
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    data = json.dumps(engine_to_snapshot(engine), separators=(",", ":"))
    # Atomic write
    fd, tmp = tempfile.mkstemp(dir=str(path.parent), prefix=".snap-", suffix=".tmp")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(data)
            f.write("\n")
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except Exception:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def load_snapshot(path: str | Path, engine: FalconDAGEngine) -> bool:
    path = Path(path)
    if not path.is_file():
        return False
    snap = json.loads(path.read_text(encoding="utf-8"))
    apply_snapshot(engine, snap)
    return True
