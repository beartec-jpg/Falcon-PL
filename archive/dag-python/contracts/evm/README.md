# Falcon EVM rail lock (sketch)

**Not a committee wallet.** `FalconRailLock.sol` holds ETH/BNB under fixed rules:

| Action | Who | Authority |
|--------|-----|-----------|
| `deposit` | User | Locks msg.value; emits event for Falcon agents |
| `submitFalconRoot` | Anyone | Sketch commitment; production must verify Falcon certs |
| `claim` | Anyone | Pays out only if note merkle-proves under Falcon root |

Falcon still mints **only** via SPV/header `RailDeposit` on the Falcon chain. This contract does not mint Falcon balances.

## Claim codec (must match Falcon)

```
leaf = sha256( noteId[32] || amount_be8 || height_be8 || sha256(external_to) || sha256(rail) )
parent = sha256( left[32] || right[32] )
empty = sha256("falcon-claim-root-empty-v1")
```

Falcon auto-commits `rail.claim_root` on every `RailWithdraw`. Agents:

```bash
python3 falcon-dag-rs/scripts/rail_indexers/evm_outbound_claim.py --rpc $RPC --rail ETH
# → submitFalconRoot(root, height) then claim(...)

# Offline golden vectors (no node) + live multi-note export
./falcon-dag-rs/scripts/claim_codec_smoke.sh

# Foundry (install foundryup → forge)
cd contracts/evm
forge install foundry-rs/forge-std --no-git --shallow
forge test -vv
```

### Lock rules (v0.33)

| Check | Behavior |
|-------|----------|
| Claim merkle | Must equal `falconStateRoot` |
| `external_to` is `0x`+40 hex | Payout `to` **must** equal that address |
| Contract balance | Must cover claim amount |
| `submitFalconRoot` height | Must be ≥ current `falconHeight` |
| Double claim | `AlreadyClaimed` |

Golden leaves/roots are pinned in:

- `fd-exec` test `claim_leaf_golden_matches_solidity_spec`
- `falcon-dag-rs/scripts/rail_indexers/claim_codec_vectors.py`
- `contracts/evm/test/FalconRailLock.t.sol`

CI: `.github/workflows/evm-ci.yml` runs `forge test` on `contracts/evm/**` changes.

## Deploy notes

1. `lockId` must match Falcon genesis `lock_id` for the rail (sha256 of `falcon-spv-lock-v1:ETH` hex as bytes32).
2. Deploy on Ethereum / BNB Chain; fund lock via `deposit`.
3. After Falcon burns, run `evm_outbound_claim.py` and submit root + claim.
4. Production: replace open `submitFalconRoot` with Falcon cert verification.

## Security (honest)

- Anyone may post a root; **wrong roots fail claims** (no free mint/withdraw).  
- Production should verify Falcon finality on-chain before accepting a new root.  
- Still **not** a committee mint path.
