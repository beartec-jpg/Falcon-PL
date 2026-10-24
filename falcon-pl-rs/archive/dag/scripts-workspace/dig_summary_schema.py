#!/usr/bin/env python3
"""PR10: standardized dig summary schema for comparable runs.

All reliability digs should emit a JSON summary with at least these fields so
results can be compared across N=3/N=5, nightlies, and pre-release gates.

Gap classes match fd_types::BatchDaGapClass (SCREAMING_SNAKE).
"""
from __future__ import annotations

import json
import time
from typing import Any, Dict, List, Optional

SCHEMA_VERSION = 1

# Frozen gap classes (PR1 / architecture contract).
GAP_CLASSES = (
    "NO_BODY_ON_PEERS",
    "PEER_HAS_BODY_NO_SIG",
    "PEER_SIGNED_CREATOR_MISSING_FOREIGN",
    "HAS_FOREIGN_OR_OK",
    "CERTIFIED",
    "UNKNOWN",
)


def empty_gap_counts() -> Dict[str, int]:
    return {g: 0 for g in GAP_CLASSES}


def make_summary(
    *,
    suite: str,
    n_nodes: int,
    hold_s: float,
    rate: Optional[float] = None,
    freeze_events: int = 0,
    gap_counts: Optional[Dict[str, int]] = None,
    hold_tps: Optional[float] = None,
    executed_min: Optional[int] = None,
    executed_max: Optional[int] = None,
    open_body_repool: int = 0,
    peer_signed_gaps: Optional[int] = None,
    pass_: Optional[bool] = None,
    gate: Optional[str] = None,
    notes: str = "",
    extra: Optional[Dict[str, Any]] = None,
) -> Dict[str, Any]:
    gaps = empty_gap_counts()
    if gap_counts:
        for k, v in gap_counts.items():
            key = k.upper()
            if key in gaps:
                gaps[key] = int(v)
    if peer_signed_gaps is None:
        peer_signed_gaps = int(gaps.get("PEER_SIGNED_CREATOR_MISSING_FOREIGN", 0))
    out: Dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "suite": suite,
        "n_nodes": n_nodes,
        "hold_s": hold_s,
        "rate": rate,
        "ts_unix": time.time(),
        "freeze_events": freeze_events,
        "gap_counts": gaps,
        "peer_signed_gaps": peer_signed_gaps,
        "hold_tps": hold_tps,
        "executed_min": executed_min,
        "executed_max": executed_max,
        "open_body_repool": open_body_repool,
        "pass": pass_,
        "gate": gate,
        "notes": notes,
    }
    if extra:
        out["extra"] = extra
    return out


def validate_summary(obj: Dict[str, Any]) -> List[str]:
    """Return list of validation errors (empty = ok)."""
    errs: List[str] = []
    if not isinstance(obj, dict):
        return ["summary must be object"]
    if obj.get("schema_version") != SCHEMA_VERSION:
        errs.append(f"schema_version want {SCHEMA_VERSION}")
    for req in ("suite", "n_nodes", "hold_s", "gap_counts", "freeze_events"):
        if req not in obj:
            errs.append(f"missing {req}")
    gc = obj.get("gap_counts") or {}
    for g in GAP_CLASSES:
        if g not in gc:
            errs.append(f"gap_counts missing {g}")
    return errs


def write_summary(path: str, summary: Dict[str, Any]) -> None:
    with open(path, "w") as f:
        json.dump(summary, f, indent=2)
        f.write("\n")


def evaluate_gate(
    summary: Dict[str, Any],
    *,
    freeze_events_max: int,
    peer_signed_gaps_max: int,
    hold_tps_min: Optional[float] = None,
) -> bool:
    """Promotion criteria for prototype/private-net gates (PR10)."""
    if int(summary.get("freeze_events", 0)) > freeze_events_max:
        return False
    if int(summary.get("peer_signed_gaps", 0)) > peer_signed_gaps_max:
        return False
    if hold_tps_min is not None:
        tps = summary.get("hold_tps")
        if tps is None or float(tps) < hold_tps_min:
            return False
    return True


# Named gates (IMPLEMENTATION_GUIDE + DIG_BASELINES)
GATE_N3_LOCALHOST = {
    "name": "N3_localhost_p20",
    "n_nodes": 3,
    "freeze_events_max": 0,
    "peer_signed_gaps_max": 2,
    "hold_tps_min": 15.0,
}

GATE_N5_LOCALHOST = {
    "name": "N5_localhost_p40",
    "n_nodes": 5,
    "freeze_events_max": 0,
    "peer_signed_gaps_max": 5,
    "hold_tps_min": 20.0,
}


def self_test() -> None:
    s = make_summary(
        suite="self_test",
        n_nodes=3,
        hold_s=10.0,
        freeze_events=0,
        gap_counts={"PEER_SIGNED_CREATOR_MISSING_FOREIGN": 1},
        hold_tps=20.0,
        gate="N3_localhost_p20",
    )
    errs = validate_summary(s)
    assert not errs, errs
    assert evaluate_gate(s, freeze_events_max=0, peer_signed_gaps_max=2, hold_tps_min=15.0)
    assert not evaluate_gate(s, freeze_events_max=0, peer_signed_gaps_max=0)
    print("dig_summary_schema self_test ok")


if __name__ == "__main__":
    self_test()
