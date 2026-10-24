# ETH / USDC — Sepolia V3 Bridge (`FalconQcBridgeV3`, No Admin Withdraw)

> **Live Status (`v2.9.81`, Height `692,000+`):** Active on Sepolia **`FalconQcBridgeV3`** (`0xEd4eE497F21a9255a59e87535FF2A6097592Cfd4`) since height **`414,000`**, backed by the on-chain Ethereum Gloas sync-committee light client (active since **`409,134`**) and 4× `OPEN=1` Falcon-512 Groth16 proofs (`Groth16Verifier` at `0xA03005921E1807587B942ab00cC4212650670aA6`).
> See **[BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md)** and **[ETH_V3_DEPOSITS.md](ETH_V3_DEPOSITS.md)** for full operational details.

Core security properties:

- **Peg-in (`EthV3Deposit`):** User calls `depositEth(bytes20 dest20)` or `depositUsdc(bytes20 dest20, uint256 amount)` on `FalconQcBridgeV3`; mint proof verifies the Ethereum receipt MPT + execution header chain against the on-chain Sepolia Gloas sync-committee light client (50,400-header window, ~7 days). Unresolved deposits automatically become refundable after 3 days (`ETH_V3_DEPOSIT_EXPIRY_SECS`).
- **Peg-out:** Burn on Falcon PL → withdrawal leaf in `claim_root` → **4× Falcon-512 Groth16 proofs** (`submitHeaderWithProofs`, header jump `≤ 50,000`) → `openClaim` SHA-256 Merkle verification → **only `dest` can `take()`**.
- **No owner, no guardian, no pause, and no admin withdraw.**

## Live Sepolia Contracts (`FalconQcBridgeV3`)

| Component | Sepolia Address |
|--|--|
| **`FalconQcBridgeV3` (live)** | `0xEd4eE497F21a9255a59e87535FF2A6097592Cfd4` |
| **`Groth16Verifier` (current V3)** | `0xA03005921E1807587B942ab00cC4212650670aA6` (initial V2/V3 verifier: `0x2Cb70e9f082F2DF91E9A5e6E7C9DF6b8b11B4F80`) |
| `FalconQcBridgeV3Checks` | `0xAe89ce2794Ab9B6fd7877720Df1028b73304dD7a` |
| `FalconQcBridgeV3AnchorSet` | `0x974287d6D5363123Db8C264C656cA79555c53409` |
| `USDC` (Sepolia) | `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` |
| `FalconQcBridgeV2` (historical anchor `353,130`) | `0x811854827627024B38926Ea9DCc0f88ACd5fB23e` |
| `FalconQcBridge` (V1, historical) | `0xf8F1471643792eb1cD5d0C31061629777E55bc48` (verifier `0x9992cD8e45A2b7983E2b7f8fC725308a0E4845EC`) |

## Operator Scripts

| Script | Role |
|--------|------|
| `pl-eth-lc-relayer.py` | Sepolia Gloas beacon sync-committee LC bootstrap/update/daemon |
| `pl-eth-auto-mint.py --v3` | Scans V3 `Deposit` logs and submits `EthV3Deposit` (mint or 3-day refund) |
| `pl-falcon-qc-v3-prover.py` | Collects ≥4 Falcon-512 QC sigs → generates 4× `OPEN=1` Groth16 proofs → `submitHeaderWithProofs` on V3 |
| `pl-eth-claim-proof.py` | Builds SHA-256 Merkle proof for `openClaim` |
| `pl-eth-sepolia-e2e.py` | End-to-end Sepolia verification |

## Deprecated / Legacy Contracts

- **`FalconBridge.sol`** (`0x7eB72974…`) — Retired BabyJubJub LC peg-out.
- **`FalconLock.sol`** — Historical test harness contract; never deploy.
