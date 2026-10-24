# Open validators, committees, shards — risks and Falcon path

## Are open validators a risk?

**Yes — open participation is a risk if “open” means “anyone’s vote always counts equally.”**  
That’s the Sybil problem: an attacker creates many identities and outvotes honest nodes.

| Risk | What it is | Mitigation |
|------|------------|------------|
| **Sybil** | Fake mass join | Stake, identity bond, reputation, invite, or costly entry |
| **Committee capture** | Corrupt the *sampled* k | Large honest stake/share; random beacons; k large enough; slash |
| **Nothing-at-stake / grinding** | Cheap to attack many committees | Stake lockups, slashing, long unbonding |
| **Churn / liveness** | Voters offline when sampled | Backup samples, view-change, inactivity leak |
| **Long-range / weak subjectivity** | New nodes don’t know who’s valid set | Checkpoints, social consensus on genesis registry |
| **Spam packing** | Open packers flood junk | Fees, leases, rate limits (GMP already helps) |

**Open validators are fine** if open means:

> Anyone can **register** as a validator (with stake/bond),  
> but **each decision** only needs a **bounded committee** (or a **shard-local** set).

**Open validators are unsafe** if open means:

> Every registered node signs every batch with equal weight, no stake, free identities.

So: **open registry ≠ full-N voting.** Risk is managed by **bond + sampling/shards**, not by banning openness.

---

## Workers vs committees vs shards (reminder)

| | Workers only | Committees | Shards |
|--|--------------|------------|--------|
| Open join | Service roles | Validator registry | Per-shard registry |
| Vote every batch | Small fixed set | Sampled k | Small set **per shard** |
| TPS vs N | Flat | ~f(k) | **Can grow with #shards** |
| Vision fit | Weak if you reject small UNL | Strong open membership | Strong capacity growth |

---

## Why shards for “grows with expansion”

One global voter set (even rotating) mostly keeps **one chain’s TPS ceiling**.  
**Shards** = parallel consensus domains → total exec capacity can add:

`global_tps ≈ sum(shard_i tps)` (for single-shard txs)

Cross-shard transfers cost extra (bridges); most load should be **intra-shard**.

---

## Falcon v0 shard plan (lab)

**Not full bridges yet.** First prove:

> Two independent N=3 multipack meshes + two GMPs + partitioned accounts  
> → combined `exec_tps_min` **higher** than one N=3 under the same total client rate.

That is **capacity expansion by parallelism**, without growing a single UNL.

Later: account router, cross-shard txs, shared light clients.

### Lab result (2026-08-10)

Harness: `deploy/gmp/17-shard-v0-dig.py` · report: `deploy/gmp/SHARD_V0_REPORT.md`

| Mode | exec_tps_min |
|------|--------------|
| Single N=3 (all accts, 80/s) | **38.6** |
| Shard0 (acct1–20, 40/s) | 25.4 |
| Shard1 (acct21–40, 40/s) | 21.0 |
| **Dual sum** | **46.4** |

**Verdict: SHARD_V0_HELPS** — parallel shards **add** capacity (~+20% combined vs one chain under same total offer rate), with lag 0 / agreement on both shards.

This is the first positive scale signal after UNL-growth and pack-worker digs failed to raise single-chain TPS.

---

## GMP facade + cross-shard (lab)

**Binary:** `falcon-gmp-facade` (`crates/fd-gmp`)

```
Clients ──► facade :18499
              │ route(account)
              ├─ same shard ──► shard GMP ──► packers
              └─ cross shard ──► debit on source GMP
                                   wait source exec
                                   credit from dest bridge GMP
```

| API | Purpose |
|-----|---------|
| `POST /v1/submit` | Same-shard proxy or start cross-shard |
| `POST /v1/route` | `{account}` → shard id + endpoint |
| `GET /v1/xshard/{id}` | Cross-shard transfer status |
| `GET /v1/status` | Aggregate stats + per-shard GMP |

**Cross-shard lab protocol (not full atomic bridge):**
1. Source: `account → bridge_src` (escrow)
2. Wait until source `account_info.sequence` advances past debit
3. Dest: `bridge_dst → destination` (mint from prefunded bridge faucet)

**Live dig** (`deploy/gmp/18-facade-xshard-dig.py`): **PASS**
- route acct5→s0, acct25→s1
- 20/20 same-shard via facade
- cross-shard acct5→acct25: debit exec + credit, bal 1e9→1e9+5
- Hardened: **idempotency** (`client_id` / stable key), **refund** on credit failure, **`GET /v1/bridges`** inventory

### 3-shard dig

`19-shard3-dig.py`: at total rate 90/s, single N=3 ≈ **45.4** TPS; three shards sum ≈ **45.1** (flat — load partitioned, single not saturated).  
Earlier **2-shard** dig at rate 80 showed **sum 46 > single 39** under more contention. Shards help when one chain is the bottleneck; they don’t invent free TPS if the offer is already fully handled by one N=3.

### Ops

- `deploy/gmp/start-stack.sh` / `stop-stack.sh` / `RUNBOOK.md`
- Committees (open validators): `docs/COMMITTEES.md` (design only)
