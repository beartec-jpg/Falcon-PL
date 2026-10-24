# Archived: Falcon DAG research (prior work)

**Status:** archived · not the active product path  
**Active product:** [Falcon PL (FPL)](../../crates/fd-pl/) — Participation Ledger + **Falcon Consensus**

This tree holds the **earlier Rust research** into Narwhal-style DA, Bullshark-style ordering, tip-BFT, GMP/rails, and multi-node DAG load tooling. It remains for history and reference. It is **not** Falcon PL and **not** the XRPL-based Falcon Ledger (`qXRP` / FalconLedger).

## What was here

| Area | Contents |
|------|----------|
| `crates/` | `fd-types`, `fd-crypto`, `fd-mempool`, `fd-dag`, `fd-order`, `fd-exec`, `fd-net`, `fd-store`, `fd-node` (`falcon-dagd`), `fd-gmp` |
| `scripts/` | DAG smokes, dig/soak, lane/TPS suites, rail indexers, reliability gates |
| `deploy/` | Docker / testnet-9003 layout for the DAG stack |

## Why archived

Product direction moved from **DAG + federated ordering** to a greenfield **ordered Participation Ledger** with **lottery-packer Falcon Consensus** (`crates/fd-pl`). Keeping both as “current” confused naming and roadmap.

## How to use (if needed)

These crates are **outside** the default workspace. To experiment:

1. Copy or path-patch a temporary workspace, or  
2. Open historical commits before the archive move.

Do **not** treat metrics from this tree as Falcon PL / FPL results.

## Related (separate repos / products)

| Name | What |
|------|------|
| **Falcon PL / FPL** | This monorepo’s active path (`crates/fd-pl`) |
| **Falcon Ledger (qXRP)** | XRPL fork + Falcon-512 + RPCA — separate codebase |
| **Falcon-512** | NIST PQ signature scheme (crypto), not consensus |
