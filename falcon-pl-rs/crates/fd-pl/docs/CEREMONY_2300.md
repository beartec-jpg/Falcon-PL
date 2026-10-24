> Live public 2300 mesh reports **product_version 2.9.39** (2026-08-26). Rows below that say 2.9.26/2.9.30 are ceremony/soak notes, not the running tip.

# Public testnet ceremony (network_id **2300**)

**Do not flip 2200.** Private soak stays on 2200 until operators stop it on purpose. Public is a **new genesis**.

## Proven on private 2200 (2026-08-15, 2.9.26)

| Gate | Result |
|------|--------|
| Multi-host agree under load | host1 + host2 lockstep ~8 h; ~560 ledgers / 90 s |
| Join-snap under soak | v4/v11 adopt 2 MB / 8–10 chunks in 2–13 s |
| Stale pin | 2.9.26 rotates when live−pin ≥ 256 |
| One val / 2-core | v11 alone held tip; two vals on droplet did not |
| **New val under load** | **v12** bonded from alice, started `--join` **without** a 154 MB copy. Offer 352565 / 8 chunks, adopt **0→352565** in 2 s, lag 3 and **self bonded** ~20 s later |
| Skip lagging packer | skip-QC of v12 at h=352627 / 352635 → v8 packed (joiner was still catching) |
| Payday | v1 and v3 same `state_root` + `last_settled_epoch` when lockstep. claimable `{}` (already settled). Lagging v11 one epoch behind until snap — expected |
| Version / nid | 2.9.26 / 2200 everywhere |

Still optional before a loud announce: 24 h on **public params** (7d epoch, first_claim=8), formal packer-kill drill, spike > 25 tps.

## Operator steps (2300)

1. `cargo build -p fd-pl --release` at **2.9.30** (crate = `PRODUCT_VERSION`).
2. New empty root, e.g. `$HOME/falcon-pl-public-testnet-2300`.
3. `CONFIRM=2300 bash crates/fd-pl/scripts/pl-start-public-2300.sh keys`  
   Writes Falcon-512 keys. **Never** copy 2200 `keys/` or snapshots.
4. Record sha256 of `falcon-pl-node`, genesis validator publics, faucet public.
5. `CONFIRM=2300 bash crates/fd-pl/scripts/pl-start-public-2300.sh start`  
   Ports **19301+** so a leftover 2200 mesh on 19101 is not overwritten.
6. Status: `falcon-pl-ctl --addr 127.0.0.1:19301 status` → `network_id=2300`, `product_version=2.9.26`, tip advancing.
7. Publish: parameter sheet, hardware floor, seed peers, RPC, faucet, “not mainnet”.

## Refuse list

- `network_id=2200` on a public advertise
- `--scheme hmac` without `--allow-dev-crypto`
- Admin WireMsg on the TCP listen port
- Loading a 2200 snapshot into 2300
- Two vals on a 2-core public seat
