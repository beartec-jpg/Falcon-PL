# Falcon DAG — Deployment program (PR11)

Staged path: **prototype → live private net → internal mainnet-grade → public participation**.

## Stages

| Stage | Nodes | Profiles | Go criteria |
|-------|-------|----------|-------------|
| Prototype | 3 | full lab | `reliability_gate_n3` pass; snapshot restart ok |
| Private live | 5 | validator + optional RPC | `reliability_gate_n5` pass; Mode B near zero |
| Internal mainnet-grade | 5–9 | validator / archival / RPC | boring ops, bounded storage, rollback drill |
| Public participation | 5+ service | as above + scoring | PR9 economics live; private gates green |

## Deploy profiles

### Validator

```bash
falcon-dagd \
  --id val1 \
  --node-profile validator --force-node-profile \
  --role full \
  --rpc 0.0.0.0:18001 \
  --unl val1,val2,val3,val4,val5
```

Expect: pack + sign + order + exec. Protect from public RPC load (use separate RPC profile).

### Archival

```bash
falcon-dagd \
  --id archive1 \
  --node-profile archival --force-node-profile \
  --role order-exec \
  --rpc 127.0.0.1:18090
```

Expect: deep `max_hard_final_history`, no pack duty.

### RPC

```bash
falcon-dagd \
  --id rpc1 \
  --node-profile rpc --force-node-profile \
  --role full \
  --rpc 0.0.0.0:18080
```

Expect: admit + owner-route; no multipack.

### Lab full (transition)

```bash
falcon-dagd --id val1 --role full --rpc 127.0.0.1:18001 --unl ...
```

Backward-compatible combined process.

## Upgrade and rollback

1. **Pre-upgrade:** snapshot all validators; save `snapshot.meta.json` ages.
2. **Upgrade:** stop process → replace binary → start same data dir.
3. **Verify:** `server_info` build_version; `architecture` contract; executed_txs advances.
4. **Rollback:** stop → restore previous binary + last known-good snapshot → start.
5. **Do not** force-push ledger state across incompatible snapshot versions without a migration note.

## Monitoring minimums

| Profile | Alerts |
|---------|--------|
| Validator | open_needs_body_seed high; peer_signed gaps; admit_exec_lag; longest_own_open_age |
| Archival | snapshot_age_s; disk free; restore_ok |
| RPC | terBUSY rates by busy_causes; directed_to_owner; p2p_peers |

## Hardware (summary)

See IMPLEMENTATION_GUIDE hardware phases. Prototype: modest multicore + SSD. Private live: ≥16GB RAM validators. Public: role-separated machines.

## Stage promotion checklist

- [ ] Reliability gate artifact (`dig_summary.json`) for target N
- [ ] Snapshot save/restore drill documented with meta age
- [ ] Role-separated smoke (validator + rpc) if stage ≥ private live
- [ ] Economics freeze JSON reviewed (`EconomicConfig::freeze_json`) before public
- [ ] Rollback dry-run signed off

## Public participation prerequisites

Only after private live + internal mainnet-grade gates pass:

1. FALC public model frozen (PR9)
2. N=5 gate green with dig evidence
3. Operator runbooks (this doc) used without tribal knowledge
4. No admin treasury spend path in binary
