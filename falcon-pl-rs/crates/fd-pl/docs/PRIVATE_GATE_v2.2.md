> **HISTORICAL.** This file is not the live Falcon PL **2300** dest-lock specification.
> Current: [BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md) and [README.md](README.md).
> Falcon Ledger **1001** lock-mint is shut down.

# Falcon PL v2.2 — Private testnet gate

Maps EXPANSION_PLAN §6.2 exit criteria. Updated by `pl-gate-private-v22.sh` and dual/triple soaks.

## Version line (locked)

| Version | Role |
|---------|------|
| **2.2.0** | **Private testnet baseline** — good to **start** private testnet |
| **2.2.1, 2.2.2, …** | Fixes + additions **while private testnet runs** (bridges, soak gaps, UX) |
| **2.3.0** | **Public testnet release** — only when private line is ready to go public |

Do **not** bump to 2.3 for private-only fixes. That is what 2.2.x is for.

## 2.2.0 private testnet gate

| Criterion | Status | Evidence |
|-----------|--------|----------|
| Multi-node tip agree under load | **PASS** (private) | dual-host soak same tip+hash; multi-host determinism fixes |
| Bond / unbond / slash | **PASS** | dual bond cycle + units; slash unit |
| Epoch + claim types (val/watcher/AMM/lend) | **PASS*** | units + emission matrix; multi-host payday harness still flaky |
| Emission math §4 | **PASS** | `EMISSION_MATRIX.md` + unit |
| Vault single-dest | **PASS** | unit + smoke |
| AMM lifecycle + LP claim | **PASS*** | units; dual payday may WARN |
| Lending lifecycle + LP claim | **PASS*** | units; dual payday may WARN |
| BTC rail framework deposit/withdraw | **PASS** (framework) | unit SPV shape + smoke; live multi-network finalisation → **2.2.x** |
| ETH/BNB rail slots | **Scaffold** | genesis rails present; real light-client/proofs → **2.2.x** |
| XRP / USDC as rails | **Not in 2.2.0 baseline** | design only → **2.2.x** then 2.3 if proven |
| Watcher presence×work + cap | **PASS*** | units; multi-host path polish → 2.2.x |
| Wallet create/pay/claim | **PASS** | pl-wallet-stage + ctl |
| History prune / snapshots | **PASS** | max_chain_ledgers + snapshots |
| Adversarial double-sign + standby | **PASS** | unit + adversarial local |
| Parameter deltas documented | **PASS** | this file + AMD; network_id **2200** |

\*Core logic present; keep improving under **2.2.x** during private soak — does not block calling **2.2.0 private testnet open**.

## network_id

| Env | network_id |
|-----|------------|
| Private v2.2.x | **2200** |
| Public v2.3.0 (planned) | e.g. **2300** (freeze at public release) |
| Old proto | 9101 (retired) |

## v2.3.0 public testnet (only after private success)

- [ ] Freeze parameter sheet from surviving **2.2.x** private run  
- [ ] Public network_id + genesis ceremony  
- [ ] Public RPC / faucet / wallet cutover  
- [ ] Rails you choose to expose (at least BTC proven on private)  
- [ ] Retire Falcon Ledger v1 public testnet

## How to run the gate

```bash
# Local unit-style smoke only (§1–8) — NOT full multi-host gate
bash crates/fd-pl/scripts/pl-gate-private-v22.sh

# Production-shaped: host1 + host2 + droplet (Tailscale mesh)
PL_GATE_TRIPLE=1 bash crates/fd-pl/scripts/pl-gate-private-v22.sh

# Or dual LAN only (host1 + host2)
PL_GATE_DUAL=1 bash crates/fd-pl/scripts/pl-gate-private-v22.sh

PL_GATE_FALCON512=1 bash crates/fd-pl/scripts/pl-gate-private-v22.sh
```

## Multi-host topology

See `scripts/pl-hosts.env`.

| Host | Role | Mesh IP (Tailscale) |
|------|------|---------------------|
| host1 | v1 :19101 | 100.64.0.1 |
| host2 | v2 :19102 (triple) / v3 :19103 (dual) | 100.64.0.3 |
| droplet | v3 :19103 (triple) | 100.64.0.5 |

```bash
bash crates/fd-pl/scripts/pl-triple-host-soak.sh preflight
bash crates/fd-pl/scripts/pl-triple-host-soak.sh all
```
