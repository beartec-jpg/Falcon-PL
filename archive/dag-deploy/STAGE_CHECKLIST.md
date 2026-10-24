# Stage promotion checklists (PR11)

## Prototype (N=3)

- [ ] `cargo test -p fd-types -p fd-mempool -p fd-store -p fd-node --lib`
- [ ] `./falcon-dag-rs/scripts/reliability_gate_n3.sh`
- [ ] Snapshot write + restart one node
- [ ] Rollback: previous binary boots same data dir

## Private live (N=5)

- [ ] `./falcon-dag-rs/scripts/reliability_gate_n5.sh` with real dig attach
- [ ] Validator-only + RPC profile smoke
- [ ] Chaos restart (see chaos_variants.md)
- [ ] Operator can interpret `busy_causes` and conf-pool `phase`

## Internal mainnet-grade

- [ ] Archival node retains history per RetentionPolicy
- [ ] Upgrade + rollback drill signed
- [ ] Monitoring alerts defined for three profiles
- [ ] Storage growth measured over multi-day soak

## Public participation

- [ ] PR9 freeze JSON reviewed
- [ ] Private live gates remain green
- [ ] Public RPC on RPC profile only
- [ ] Bootstrap / participation docs linked from README
