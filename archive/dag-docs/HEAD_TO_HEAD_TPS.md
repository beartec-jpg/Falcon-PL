# Head-to-Head TPS: Falcon DAG vs Falcon C++ RPCA

**Date:** 2026-08-03  
**Machine:** local single host  
**Scope:** directional architecture comparison — **not** a mainnet claim  

Scripts:

- `scripts/head_to_head_tps.py` — orchestrates DAG + C++ probes  
- `scripts/falcon_cpp_flood.py` — Falcon-512 payment flood vs `xrpld`  
- qXRP regtest: `./scripts/start-regtest.sh .build/xrpld data/h2h-regtest`

---

## Systems under test

| | **Falcon C++ RPCA** | **Falcon DAG** |
|--|---------------------|----------------|
| Binary / runtime | `qXRP/.build/xrpld` 5 validators | Python 5-node regtest + in-process sim |
| Ports | 5005–5009 | 18001–18005 |
| Consensus | Classic single-ledger RPCA | Narwhal DA + federated cert order |
| Tx / cert crypto | **Falcon-512** payment signatures | **Falcon-512** batch certificates |
| Network ID | 1001 | 9002 |

---

## Measured results (this run)

### Falcon C++ RPCA (`xrpld` 3.2.0-b0)

| Metric | Value |
|--------|-------|
| Server state | proposing (4 proposers) |
| Idle ledger close | **~3.0 s / ledger** (5 ledgers / 15 s) |
| Last close converge | **~2.0 s** |
| Falcon-512 payment flood | **288/300** submit OK |
| Submit TPS (sign+submit path) | **~96 tx/s** |
| Approx settle TPS (incl. wait) | **~22 tx/s** |

Notes:

- Stock `load-test.py` failed: it still proposes **classical** wallets; Falcon requires `key_type=falcon512`.  
- Flood uses genesis classical fund + **Falcon-512** signed payments via `sign`/`submit` RPC.  
- Submit path is sequential; parallel submit would raise raw intake but not change ~3 s ledger cadence.

### Falcon DAG (research prototype)

| Metric | Value |
|--------|-------|
| In-process sim (Falcon-512 certs), 400 txs | **~500–900 TPS** (varies with load/CPU) |
| In-process sim (HMAC-dev certs), 400 txs | **~2×–3×** faster than Falcon-512 path when crypto dominates |
| Live 5-node regtest (Falcon-512), settle | **~10–180 TPS** depending on sequence probing / prior state |
| Cert size | **~655 B/sig**, **~2.6 KB** for 4-of-5 cert |
| Soft-final path | certify batch (UNL) then order |

---

## How to read this (important)

1. **Do not** claim Python Falcon DAG “beats” production Falcon TPS.  
   - DAG **sim** is a single-process algorithm harness.  
   - DAG **live** is multi-process Python + HTTP.  
   - Falcon **C++** is a real validator binary with full ledger machinery.

2. **What the comparison *does* show**  
   - Falcon C++ RPCA is healthy: **~3 s** ledger closes, multi-validator proposing.  
   - Falcon-512 **payment signing** on C++ is already a real cost (~96 submit TPS sequential in this harness).  
   - Falcon DAG can keep **real Falcon-512 certs** while still moving many txs per cert (~2.6 KB cert amortized over batch size).  
   - HMAC vs Falcon-512 in the DAG prototype quantifies **cert crypto overhead** (~2–3× in sim when cert work dominates).

3. **Architecture takeaway for the PQ goal**  

| Goal | Implication |
|------|-------------|
| Keep large Falcon-512 sigs | Both systems do |
| Raise throughput under PQ sigs | DAG aims to amortize **attestation** into fat batches; RPCA still closes **one ledger** of full txs |
| Near-term product | Falcon C++ RPCA remains the production path |
| Research spinoff | Falcon DAG continues as layer / alternate architecture |

Rough capacity sketch for Falcon C++ if ledgers fill to ~1000 txs @ 3 s close:

> theoretical ≈ **1000 / 3 ≈ 333 TPS** (config `target_txn_in_ledger=1000`)  

Actual flood here was lower because sequential sign RPC and account setup dominate the harness.

---

## Comparison table (headline)

| | Falcon C++ RPCA | Falcon DAG (Python) |
|--|-----------------|---------------------|
| Idle close / soft final cadence | ~3 s ledger | sub-second batch possible |
| Measured submit/flood TPS (this host) | **~96** sequential Falcon pay | **~500–900** sim / **~10–180** live settle |
| Implementation maturity | Production-shaped | Research |
| Risk to mainnet Falcon | N/A (isolated regtest) | None (separate repo) |

---

## Re-run

```bash
# Falcon C++ regtest (qXRP)
cd /home/scott/qXRP
./scripts/start-regtest.sh .build/xrpld data/h2h-regtest

# Head-to-head + flood
cd /home/scott/falcon-dag
source .venv/bin/activate
python scripts/head_to_head_tps.py --txs 400
python scripts/falcon_cpp_flood.py --accounts 12 --payments 300
```

JSON dump: `docs/HEAD_TO_HEAD_TPS.json`

---

## Verdict for “continue with DAG?”

**Yes, as research** — the head-to-head **does not** say replace Falcon C++.  
It **does** say:

- Falcon C++ baseline is **~3 s/ledger**, Falcon-512 pay path **measurable**.  
- Falcon DAG with **real Falcon-512 certificates** is operational and can be stress-tested side-by-side.  
- Next value is **C++ port / layer design**, not expecting Python to win raw TPS.
