# Falcon PL 2300 — Live Bridges Status (`v2.9.81`, Height `692,000+`)

**Live product version:** **`2.9.81`** on public testnet **2300** (height **`692,000+`**). Testnet — auditable in-repo, not an external audit report.

Each height, the deterministic committee lottery selects 6 bonded validator seats; the tip advances on **4-of-6 Falcon-512 Quorum Certificates (QC)**.

---

## 1. ETH / USDC Bridge (Ethereum Sepolia — V3 Live)

- **Ethereum Light Client (Gloas fork):** Active on-chain since height **`409,134`** (`ETH_EXEC_HEADER_WINDOW = 50,400` blocks, ~7 days).
- **V3 Deposits (Mint-or-Refund):** Active on-chain since height **`414,000`** ([ETH_V3_DEPOSITS.md](ETH_V3_DEPOSITS.md)).
- **No admin withdraw / no owner / no pause:** `FalconQcBridgeV3` is immutable and non-custodial once headers are proven; peg-outs require a 4-of-6 Falcon-512 Groth16 proof (`submitHeaderWithProofs`) and dest-locked `openClaim` / `take`.

| Contract | Sepolia Address | Role | Status |
|--|--|--|--|
| **`FalconQcBridgeV3`** | `0xEd4eE497F21a9255a59e87535FF2A6097592Cfd4` | **Live V3 Bridge** (V3 deposits + 3-day refund; peg-out via header jump `≤ 50,000`) | **Active since height `414,000`**. No owner, guardian, pause, or rescue. |
| **`Groth16Verifier` (current V3)** | `0xA03005921E1807587B942ab00cC4212650670aA6` | **Live Groth16 verifier** for 4× `OPEN=1` Falcon-512 QC proofs | **Active** (earlier V2/initial V3 verifier: `0x2Cb70e9f082F2DF91E9A5e6E7C9DF6b8b11B4F80`). |
| `FalconQcBridgeV3Checks` | `0xAe89ce2794Ab9B6fd7877720Df1028b73304dD7a` | Proof/header checks library (code-hash pinned by V3) | Active |
| `FalconQcBridgeV3AnchorSet` | `0x974287d6D5363123Db8C264C656cA79555c53409` | Commits the 9 legacy V2 anchor notes (refused on V3 to prevent double-pay) | Frozen at anchor `353,130` |
| `USDC` (Sepolia) | `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` | Sepolia Circle USDC token | Active |
| `FalconQcBridgeV2` (legacy) | `0x811854827627024B38926Ea9DCc0f88ACd5fB23e` | Historical V2 bridge (anchor `353,130`) | Prover disabled; all 9 anchor notes paid |
| `FalconQcBridge` (V1, legacy) | `0xf8F1471643792eb1cD5d0C31061629777E55bc48` | Historical V1 bridge (`fplTip` `327,507`) | Retired for new deposits (`eth_v1_deposit_cutoff` armed at `414,000`) |

### ETH / USDC Data Flow

- **Peg-in (`FalconQcBridgeV3`):** User calls `depositEth(bytes20 dest20)` or `depositUsdc(bytes20 dest20, uint256 amount)` on `0xEd4eE497F21a9255a59e87535FF2A6097592Cfd4`, where `dest20 = sha256(lowercase PL account)[:20]`.
  - A watcher relays Sepolia Gloas sync-committee updates (BLS 2/3) and execution headers via `pl-eth-lc-relayer.py`.
  - Any funded account submits `EthV3Deposit` (`pl-eth-auto-mint.py --v3`) with an MPT receipt proof against the verified execution header chain.
  - If resolved within 3 days (`ETH_V3_DEPOSIT_EXPIRY_SECS`) to a creditable account matching `dest20`, FPL mints wrapped ETH/USDC. If unresolved after 3 days (still within the 50,400-header window), `EthV3Deposit` records a `v3_refund` withdrawal leaf back to the original Sepolia `sender`.
- **Peg-out (`FalconQcBridgeV3`):** User burns ETH/USDC on Falcon PL → withdrawal leaf added to `claim_root` → V3 prover (`pl-falcon-qc-v3-prover.py`) collects ≥4 Falcon-512 committee signatures, generates four `OPEN=1` Groth16 proofs, and calls `submitHeaderWithProofs` → `openClaim` verifies the SHA-256 Merkle proof → `take()` transfers funds solely to `dest`.

| Operator Tool | Script / Service |
|---|---|
| Sepolia Gloas LC relayer | `pl-eth-lc-relayer.py`, `falcon-pl-eth-lc-relayer.service` |
| V3 auto-minter / refunder | `pl-eth-auto-mint.py --v3` |
| V3 Groth16 QC header prover | `pl-falcon-qc-v3-prover.py`, `falcon-pl-falcon-qc-v3-prover.service` (key via systemd `LoadCredential`) |
| Claim Merkle proof builder | `pl-eth-claim-proof.py` |

---

## 2. Bitcoin Testnet4 (`BTC4`) — BitVM2 Groth16 Bridge (Live Since Height `692,000`)

- **Activation Height:** **`692,000`** (`TESTNET_2300_BRIDGE_FROM_HEIGHT = 692_000`, `TESTNET_2300_BITVM2_M1_FROM_HEIGHT = 692_000`, `TESTNET_2300_OPERATOR_REWARDS_FROM_HEIGHT = 692_000` in `crates/fd-pl/src/params_v2.rs`).
- **Rail Asset:** **`BTC4`** (Bitcoin Testnet4).
- **Architecture:** Independent bonded operators (`fpl-operatord`, minimum bond **`5,000 FPL`**) front withdrawals from their own Bitcoin Testnet4 wallets and are reimbursed from presigned BitVM2 peg-out graphs (`KickOff1 → StartTime → KickOff2 → Take1`, or `Challenge → Assert → Disprove / Take2`). Falcon PL holds no BTC and no operator keys.
- **Full Documentation:**
  - [docs/bridge/BRIDGE_INTEGRATION.md](../../../../docs/bridge/BRIDGE_INTEGRATION.md) — On-chain ledger rules (`Bitvm2AttestV2`, operator registry, graph bindings, SPV disprove slashing, 48h refund timeout)
  - [docs/bridge/OPERATOR_GUIDE.md](../../../../docs/bridge/OPERATOR_GUIDE.md) — Running `fpl-operatord` on Bitcoin Testnet4
  - [docs/bridge/OPERATOR_REWARDS.md](../../../../docs/bridge/OPERATOR_REWARDS.md) — `5,000 FPL` bond, 5% top-carve epoch emission share, and 100% slash + 50% disprover bounty
  - [BITVM2_ENGINE.md](BITVM2_ENGINE.md) — `FplWithdrawValid` Groth16 circuit (`fd-pl-bitvm2`) and M1 checkpoint verification

| Feature | Live `BTC4` Rule (`692,000+`) |
|---|---|
| **Peg-in (`BTC4`)** | Depositor + registered operators presign the BitVM2 graph (`fpl-bridge`); graph bindings (`operator_graph_bind`) are verified on-chain before `PEG_IN_CONFIRM`; Bitcoin SPV header proof (≥6 confs) mints `BTC4`. |
| **Peg-out (`BTC4`)** | User burns `BTC4` on FPL (`AwaitingPegout`) → registered operator cross-checks withdrawal binding with peer quorum, pays user on Bitcoin Testnet4 (`PEG_OUT`), and submits `Bitvm2AttestV2` (SPV ≥6 confs + Groth16 proof under `BITVM2_WITHDRAW_VK_HEX`). |
| **Finality & Refund** | `ClaimPosted` finalizes to `PaidOptimistic` after `BITVM2_PAYOUT_FINALITY_LEDGERS` (`21,600` ledgers). Unfronted withdrawals refund burned `BTC4` after `BITVM2_PEGOUT_REFUND_LEDGERS` (`43,200` ledgers, ~48h). |
| **Challenge & Slash** | Any watcher can broadcast a presigned `Challenge` tx on Bitcoin Testnet4 and, if the operator posts an invalid `AssertFinal`, broadcast `Disprove` / `DisproveChain` and submit `operator_slash` (SPV ≥6 confs) on FPL to slash **100%** of the operator's `5,000 FPL` bond (**50% bounty** to the disprover, **50% burned**). |

---

## 3. Legacy Bitcoin Testnet3 (`BTC`) — Custodial Pool Frozen

- The legacy Bitcoin Testnet3 (`BTC`) NUMS/watch pool (`tb1pd6ltq2yu89h37zkwn9jsqcq0svf4pk2upnyf7sfw6rk2v59tkw8sfsdq34`) was **frozen on 16 Oct 2026** and is retired in favor of the Bitcoin Testnet4 (`BTC4`) BitVM2 rail.
- See **[BTC_POOL_FREEZE_2026-10-16.md](BTC_POOL_FREEZE_2026-10-16.md)** for the complete UTXO accounting and freeze record.
