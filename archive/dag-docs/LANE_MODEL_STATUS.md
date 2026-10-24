# Lane model — status vs architecture plan

**Date:** 2026-08-05  
**Verdict:** Architecture implemented. Phases A–C landed. Remaining: deeper cert equality, scaling.

## Implemented (closed)

- Global mempool → FNV account lanes → independent DA pack/cert → collect finalized → Bullshark → ordered apply
- Sequence/nonce conflicts; execution-time first-valid wins
- **Phase A:** docs reconciled
- **Phase B:** directed tx delivery to lane owners + stuck metrics + owner-down stall policy
- **Phase C:** honest pull announcers (`any` + cert source, not all-peers-as-announcer); throttled finalized batch inv; `pending_certs_n` metric

## Phase C results (Falcon, 3-node)

`lane_model_3node.py` after Phase C (`/tmp/lane-model-3n-c-*`):

| Check | Result |
|-------|--------|
| Admit 30 txs | PASS |
| All alive | PASS |
| **Ledger exec agree** | **PASS** (all 5) — was FAIL [15,3,3] before |
| Bullshark anchors | PASS [1,1,1] |
| Lanes packed | PASS |
| Certified fully equal | WARN [4,5,5] still can diverge by 1 |

Gate **OVERALL PASS** on ledger agreement.

## Open

1. **Phase C follow-up (optional):** drive cert vectors fully equal; more of the 30 admits into exec
2. **Phase D** Bullshark stress/partition property coverage  
3. **Phase E** Measured TPS vs lane count  

## Not open

- DA/Bullshark phase split refactor — already done
