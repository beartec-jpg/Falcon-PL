# Falcon PL — Product identity

| | |
|--|--|
| **Product** | **Falcon PL** |
| **Long name** | **Falcon Participation Ledger** |
| **Ticker** | **FPL** |
| **Consensus** | **Falcon Consensus** |
| **Signatures** | **Falcon-512** (NIST PQC) |

## One-line story

Anyone who **bonds in** can **join the packer lottery**. Each height elects a packer; they seal a **non-empty**, time-budgeted ledger; **everyone verifies**; soft/hard OK advances the tip; silence promotes the next ranked packer. **Falcon-512** signs txs and ledgers.

## Name split (do not collapse)

| Name | Means |
|------|--------|
| **Falcon-512** | Signature scheme (crypto) |
| **Falcon Consensus** | Agreement rules: lottery packer + soft/hard OK + ranked progress timeout + verify/slash |
| **Falcon PL / FPL** | Product / chain that runs Falcon Consensus |
| **Falcon Ledger (qXRP)** | Separate XRPL-fork product (RPCA + Falcon sigs) — **not** this codebase |

## Falcon Consensus (definition)

Bonded participants form the eligible set. For each height:

1. **Lottery** elects ranked packers (#1, #2, …) from tip + registry + skips (deterministic).
2. **#1 packs** applicable txs under `pack_budget_ms` (no empty seals).
3. **Full nodes verify** parent, height, packer, sig, state root.
4. **Soft / hard OK** votes pipeline and strengthen confirmation; depth confirms further.
5. **Progress timeout** with work pending → skip silent packer → next ranked packs.
6. **Equivocation** (same packer, same height, different hash) → jail/slash.

## Positioning

- **Not** an XRP Ledger / rippled fork.
- **Not** a DAG.
- **Not** “Falcon Consensus = Falcon signatures alone.”
- Greenfield **ordered-ledger** prototype with **participation lottery** packing.

## Pitch (reuse)

> **Falcon PL (FPL)** runs **Falcon Consensus**: a lottery-packer participation ledger with soft/hard confirmation and standby failover, secured by **Falcon-512**. Bond in to join the lottery; packers and checkers share rewards evenly on an epoch schedule; vaults, AMM/lending LPs, and SPV bridges (custodialess is the target; the live BTC bridge is custodial today) are the product surface — see [FEATURES_AND_TOKENOMICS.md](FEATURES_AND_TOKENOMICS.md).
