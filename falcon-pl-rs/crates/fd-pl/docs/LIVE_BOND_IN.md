# Live bond-in (private testnet v2.2.x)

## Goal

Add a **new validator** to a **running** mesh without restarting existing nodes.

Users (wallets) never need this. **Operators** who want to pack/check bond FPL and run a node.

## Rules

| Rule | Why |
|------|-----|
| **Registry grows only via genesis + Bond ledger txs** | Hello must not invent bonded validators (free lottery seat) |
| **Hello only advertises keys (+ tip)** | Peers learn packer public keys; joiners learn tip for pull catch-up |
| **Join after bond is confirmed** | Snapshot / residual catch-up must include the Bond |
| **State-sync via snapshot on long chains** | Snapshots hold tip state **and** the pruned chain tail (v2.2.1+); residual `NeedLedgers` works after restart without a full-history archive |
| **Joiner must not pack while lagging** | Enforced (`tip lag` / `best_peer_tip`) |
| **Lottery requires readiness (Pong)** | Bond alone is not enough — packers must answer the mesh liveness channel (`Pong`/Hello, mesh-flooded). Offline bonded seats are not selected. |
| **Live-but-lagging packers are skippable** | Bonded joiner ponging tip 0 must not stall the mesh (progress timeout) |

## Operator flow

```text
1) gen keys for new id (e.g. v5)  →  merge into shared keys dir (all hosts)
2) fund bonder (faucet / pay)
3) bond on live RPC:  falcon-pl-ctl bond --id v5 --from alice --amount 1000
4) wait for tip to advance (bond packed + visible in primary status)
5) copy a live peer snapshot (e.g. v1.json) → joiner snapshot path
   (required when mesh chain_len ≪ tip after snapshot restarts)
6) start joiner:
     falcon-pl-node --join --id v5 --snapshot <copied> --snapshot-out <path> \
       --peers <seeds> --validators v1,v2,v3,v4
7) residual pull catch-up closes any tip gap since the snapshot was taken
8) v5 pongs at mesh tip → lottery / pack when elected
```

Script: `scripts/pl-join-validator.sh` (bond → snapshot rsync → `--join` start → wait).

## Why snapshot state-sync (not full history pull)

| Path | Works when |
|------|------------|
| `NeedLedgers from=1` | Archive node still holds full `chain` in RAM (`max_chain_ledgers=0`) |
| **Snapshot + residual catch-up** | Production private mesh (v2.2.1+): snapshots persist tip state **and** the pruned chain tail, so post-restart peers can serve residual heights within `max_chain_ledgers` |

Joiner probes `NeedLedgers` on outbound dials even without reverse Pong fanout
(mesh only fans to *configured* peers). Hello carries `tip_height` for a fast start.

## Node flags

| Flag | Meaning |
|------|---------|
| `--join` | Do **not** register **self** with a free genesis bond. Self appears via Bond (in snapshot or catch-up). Other `--validators` stay the genesis set. |
| `--snapshot` | **Required for long chains.** Load tip state from a live peer after bond is visible. |
| `--validators` | Genesis / known set (ids). Live joins appear via Bond txs on all caught-up nodes. |
| `--peers` | At least one seed (`host:port`) already in the mesh |

## Failure modes

| Symptom | Cause | Fix |
|---------|--------|-----|
| Bond accepted but not in status | Not sealed yet | Wait / light load so lottery packs |
| Joiner tip stuck at 0, catch-up `n>0 tip 0→0` | Peers only hold recent heights; joiner needs tip state | Copy live snapshot; restart with `--snapshot` |
| Joiner tip stuck at 0, `state root mismatch at h=1` | Hard-resync re-registered **live bonders** as genesis (commitment includes bonds) | **v2.2.5+**: `genesis_validators` seed set only; upgrade mesh + re-snapshot join |
| Mesh stalls after bond, winner=joiner | Joiner pongs at tip 0; old builds never skip live lagging packers | Upgrade mesh (lagging-skip); or stop joiner until caught up |
| Joiner packs divergent tip / private fork | Tip race after residual catch-up; live fork did not arm hard-resync | **v2.2.5+**: arm hard-resync on live fork; refuse pack while `need_hard_resync` |
| Peers reject joiner seals | Missing public key | Ensure Hello after connect; keys dir has joiner public |

## Versioning

- **2.2.0**: fixed genesis set + ops restart expand
- **2.2.x**: live bond-in for private testnet growth
- **2.2.1**: snapshot chain-tail restore (restart reliability; residual catch-up after fleet restart)
- **2.3**: same path for public operators with published seeds/RPC
