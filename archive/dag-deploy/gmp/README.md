# GMP / GCP live stack (PR-G1..G5)

Implements `docs/GLOBAL_MEMPOOL_CONF_POOL_DESIGN.md` through live full-stack tests.

## Components

| Binary | Port | Role |
|--------|------|------|
| `falcon-gmpd` | `:18500` | GMP-B sharded global mempool + capacity-weighted leases |
| `falcon-gcpd` | `:18501` | GCP-B conf registry (open batch / sig / cert) |
| `falcon-dagd --gmp-url --gcp-url` | existing | **G4** lease agent + **G5** pack/sig/cert publish |

Bins: `~/falcon-dag-mesh/bin/` (from `~/falcon-dag/falcon-dag-rs`).

## Boring ops (default day-to-day)

```bash
export SSH_F2=100.64.0.3
bash deploy/gmp/start-stack.sh --watchdog   # 2 shards + facade + watchdog
bash deploy/gmp/status.sh                   # one glance
python3 deploy/gmp/health.py                # 0 ok / 1 degraded / 2 down
python3 deploy/gmp/smoke.py                 # same-shard + xshard
bash deploy/gmp/stop-stack.sh --all         # full teardown
```

Watchdog: single-instance (`flock`), restarts only on health **DOWN**, cooldown + max 3/hour, periodic smoke.  
Full detail: `deploy/gmp/RUNBOOK.md`

## Preferred: PR-G5 full path

```bash
export SSH_F2=100.64.0.3
unset F1_IP F2_IP
export OUT=/tmp/gmp-gcp-$(date +%Y%m%d-%H%M%S)
python3 deploy/gmp/13-gmp-gcp-live.py
```

Clients → GMP only; packers lease in-process; **every pack/sig/cert** is published to GCP.  
Phase B pauses val3’s GMP agent (kill dig).

### Node flags

| Flag | Meaning |
|------|---------|
| `--gmp-url` | In-engine lease agent (PR-G4) |
| `--gmp-budget-mb` | Capacity advert budget |
| `--gmp-max-in-flight` | Cap concurrent leases |
| `--gmp-affinity` | Prefer owned-lane accounts on lease |
| `--gcp-url` | Publish batches/sigs/certs to registry (PR-G5) |

Mesh env (`07-scale-n-curve.py` `node_cmd`):

```bash
export GMP_URL=http://192.0.2.1:18500
export GCP_URL=http://192.0.2.1:18501
export GMP_BUDGET_val1=8192 GMP_BUDGET_val3=1024
```

RPC: `gmp_stats`, `gmp_control {"enabled": false}`, `gcp_stats`.

Capacity adverts use **live** mempool/open gauges + **clearance EMA** from executed delta (not only static demo numbers).

## Other harnesses

| Script | Scope |
|--------|--------|
| `13-gmp-gcp-live.py` | **Preferred** G4+G5 |
| `12-gmp-engine-live.py` | G4 engine leases only |
| `11-gmp-live-stack.py` | G3 external Python issuer |

## Pack workers dig (non-UNL pack + UNL cert)

```bash
export SSH_F2=100.64.0.3; unset F1_IP F2_IP
python3 deploy/gmp/15-pack-workers-dig.py
```

Node flag: `--unl val1,val2,val3 --packers val1,val2,val3,pack1,pack2,pack3`  
Only UNL votes for certs; pack* nodes publish bodies.

| Suite | Result |
|-------|--------|
| **Facade + xshard** | **PASS** · route, same-shard, cross-shard, bridges API · refunds/idempotency in facade · `FACADE_XSHARD_REPORT.*` |
| **Shard v0 (2)** | **HELPS** · single 38.6 vs dual sum **46.4** · `SHARD_V0_REPORT.*` |
| **Shard 3** | sum **~45** ≈ single **~45** at rate 90 (load splits evenly; not congested single) · `SHARD3_REPORT.*` |
| **Ops** | `start-stack.sh --watchdog` · `status.sh` · health/smoke/watchdog (flock+cooldown) · `RUNBOOK.md` |
| **Fat-batch** | TPS **flat ~38–40**; cert/s drops & exec/cert rises · `FAT_BATCH_REPORT.*` |
| **Pack workers A/B/C** | C pure works · A **26.3** · B **20.1** · C **20.2** · `PACK_WORKERS_REPORT.*` |
| **Scale-N GMP** | **SCALE_OUT_FAIL** · N3 **21.8** · N6 **14.66** · `SCALE_N_GMP_REPORT.*` |
| **PR-G5** GMP+GCP | **PASS** · ~15.7 TPS · GCP certified=158 · kill-agent · `GMP_GCP_REPORT.*` |
| PR-G4 engine | **PASS** · ~17.6 TPS · kill-agent · `GMP_ENGINE_REPORT.*` |
| PR-G3 issuer | **PASS** · ~22 TPS · `GMP_LIVE_REPORT.*` |
