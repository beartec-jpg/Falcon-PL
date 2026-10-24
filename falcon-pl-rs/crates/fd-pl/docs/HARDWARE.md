# Falcon PL — validator hardware floor

Measured on the private 2200 soak (2.9.26, Falcon-512, 25 tps, ~6 ledgers/s).

| Seat | Iron | Vals | Result |
|------|------|------|--------|
| host1 | i5-12400 6c/12t 15 GB | 5 | Head. ~45% CPU each, ~2.4 GB RSS after 20k ledgers |
| host2 | i5-8400 6c 15 GB | 4 | Stays lag 0–4 under soak |
| droplet | **2-core / 4 GB VPS**, Tailscale DERP | **2** | Could not verify live ledgers; snap treadmill, lag hundreds |
| droplet | same box | **1** | Held tip (lag 10–30) on residual after the other val was stopped |

## Binaries (2.9.30)

| argv0 | role | chain RAM |
|-------|------|-----------|
| `falcon-pl-archive` | hub + disk bodies + join-snap | 20_000 |
| `falcon-pl-hub` | pack intake | 20_000 |
| `falcon-pl-val` | vote / pack only | **128** |

Same crate. Do not run `falcon-pl-node` as a public seat without `--role`.

## Rules for public 2300

1. **One `falcon-pl-node` per 2-core / 4 GB machine.** Two Falcon-512 verifiers share the cores and lose to a loaded head.
2. A voting/packing seat must verify **live** ledgers at pack rate. If it cannot, it is a snap-joiner, not a head-chaser.
3. Remote / small boxes **must** use an archive join-snap (2.9.24–26). Do not expect residual of thousands of soak ledgers over WAN.
4. Archive hub (`--archive`) needs disk for bodies plus RAM for the 20k tail (~2+ GB after a long soak).
5. Prefer a direct path. DERP/relay works for 2 MB join-snaps; it is a bad residual path.

Falcon-512 verify on this iron is ~38 ms/tx. A 128-tx ledger is seconds on one core. Size the seat for that, not for HMAC-dev.
