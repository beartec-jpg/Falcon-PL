# Falcon PL 2300 — ETH / USDC dest-lock: implementation and passed e2e

**Date of report:** 2026-09-04  
**Pass recorded:** 2026-09-02 17:07 UTC (`ETH_USDC_SEPOLIA_E2E_OK` in `logs/eth-sepolia-e2e-full.log`)  
**Network:** Falcon PL public testnet 2300 + Ethereum Sepolia  
**Pass token:** `ETH_USDC_SEPOLIA_E2E_OK`  
**Mode:** `FPL_PRODUCTION=1` (public rails only; no admin mint/withdraw)  
**Scope:** ETH and USDC peg-in and peg-out on FalconQcBridge. Not an audit. Not a mainnet claim. BitVM2 / BTC exit is out of scope.

Host: host1 (`/home/falcon/falcon-pl-public-testnet-2300`). Log: `logs/eth-sepolia-e2e-full.log`. First start 2026-09-02 13:47 UTC; resumed after the 2300 deadlock 15:32 UTC; token printed 17:07 UTC.

---

## 1. What was implemented

Ethereum has no Falcon-512 precompile. Peg-out headers are a Groth16 proof over BN254 that the same Falcon-512 committee that moves 2300 attested `(height, parent, headerHash, claimRoot, networkId=2300)`.

Trust is protocol-controlled:

- **Peg-in:** Sepolia Altair sync-committee BLS light client on 2300. Watchers relay updates; they cannot mint. Mint is a receipt MPT whose execution ancestry ends at the LC head.
- **Peg-out:** 4× OPEN=1 Groth16 of Falcon-512 QC (same keys as consensus) → `openClaim` merkle check vs that header’s `claimRoot` → only `dest` can `take()`.
- **No owner, no ETH Safe, no BabyJub LC sidecar, no `--test-seed` toxic waste** on the live vk.

`--test-seed` is forgeable and is refused when `FPL_PRODUCTION=1` without a public ptau setup.

### Live Sepolia contracts

| Role | Address |
|------|---------|
| FalconQcBridge | [`0xf8F1471643792eb1cD5d0C31061629777E55bc48`](https://sepolia.etherscan.io/address/0xf8F1471643792eb1cD5d0C31061629777E55bc48) |
| Groth16Verifier | [`0x9992cD8e45A2b7983E2b7f8fC725308a0E4845EC`](https://sepolia.etherscan.io/address/0x9992cD8e45A2b7983E2b7f8fC725308a0E4845EC) |
| Sepolia USDC | `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` |
| falconKeyRoot | `0x04a9ad1908569884ff307f63291c592af16fe345fb471c22104d0c0717bcfd8a` |
| Deployed | 2026-09-02 ~10:20 UTC |
| First header height | 326444 |

Retired (do not use): test-seed FalconQcBridge `0x933Fd0a03986CdF62C9abaBC781ABF1C6D1e1b5d`, BabyJub `FalconBridge` `0x7eB72974…`.

### Groth16 ceremony (one-shot)

Public Hermez `powersOfTau28_hez_final_23.ptau` → `falcon-setup-one-from-ptau` → `contribute` → `beacon`. Circuit is OPEN=1 Falcon-512 (~2.4M constraints, SHAKE HashToPoint). Four proofs share one vk (`submitHeaderWithProofs`). Prove RSS is ~6 GiB per `falcon-prove-one`; host1 runs 2 workers.

Ceremony does not shrink prove RAM. It removes operator-known toxic waste from `--test-seed`.

### Operators

| Role | Script / unit |
|------|----------------|
| Sepolia LC + mint | `pl-eth-lc-relayer.py` / `falcon-pl-eth-lc-relayer.service` |
| Peg-out headers | `pl-falcon-qc-prover.py` (SAMPLE_GAP=48 jumps allowed) |
| Claim merkle | `pl-eth-claim-proof.py` (`--fpl-height` = **on-chain** `fplTip`, not mesh tip) |
| E2e | `pl-eth-sepolia-e2e.py` |

`openClaim` requires `headers[H].finalized` and a merkle proof against **that** header’s `claimRoot`. The prover submits sampled/jump heights, not every FPL height. E2e waits for `eth_fpl_tip ≥ seal_height` and claims at `eth_fpl_tip`.

---

## 2. Test that passed

`FPL_PRODUCTION=1 FPL_E2E_ASSETS=ETH,USDC python3 scripts/pl-eth-sepolia-e2e.py`

Flow for each asset:

1. `depositEth` / `depositUsdc` on FalconQcBridge (dest20 = first 20 bytes of `sha256(lowercase PL account)`).
2. LC mint via public `submit-rail-deposit` (receipt MPT).
3. Public `submit-rail-withdraw` packed into `claim_root`.
4. Four Falcon-512 Groth16 proofs; `submitHeaderWithProofs` to a height ≥ seal height.
5. `openClaim` + dest-only `take`.

Accounts: sally (ETH), dave (USDC). Dest / take signer: deployer `0xDb52847EE70cEd3128f49309c3DC65b69d7466f4`.

### Amounts

| Step | Amount |
|------|--------|
| ETH deposit | 0.005 ETH |
| ETH withdraw / take | 0.001 ETH |
| USDC deposit | 10 USDC (6 decimals) |
| USDC withdraw / take | 2 USDC |

### Recorded transactions (Sepolia)

| Step | Tx |
|------|----|
| ETH deposit | [`0xdb54b0ea…`](https://sepolia.etherscan.io/tx/0xdb54b0eae8520e4ed7dfac2bf147f47aea66407e394d11783724996fd42022b7) |
| USDC deposit | [`0xe53478c3…`](https://sepolia.etherscan.io/tx/0xe53478c38a66c8fcafe7a57dc932e4f26d8c31a1f0a10971f96b04a006864fd9) |
| ETH `openClaim` | [`0xd7a11c0a…`](https://sepolia.etherscan.io/tx/0xd7a11c0aab21c7aa480a82ef9ca85b59cacac86a83be112f6324639d189fa0e7) |
| ETH `take` | [`0xc37a44c5…`](https://sepolia.etherscan.io/tx/0xc37a44c58a41e0d8c4a2dff155128eaf1a94761f25b3829c150b9b69f1e46f16) |
| USDC `openClaim` | [`0xfa8a6ca7…`](https://sepolia.etherscan.io/tx/0xfa8a6ca744d3c3cb56004eb751595f2abbc4d8806d2927ee988d4f4bd55badb8) |
| USDC `take` | [`0x69ba7604…`](https://sepolia.etherscan.io/tx/0x69ba7604b60d4427492632429666c71a05e9b2a7f968018a0beda433e0b862d4) |

Withdraws sealed at FPL heights **327326** (ETH) and **327327** (USDC). Claims used Groth16 header **327458** (`headers(327458).finalized = true`). Notes: ETH `0xfb13ddc0bd773488…`, USDC `0xc3d31eff0c5c610b…`.

After take: `ethPool` 0.018 ETH, `usdcPool` 8 USDC (10 in − 2 out). On-chain `fplTip` continued (327507 observed 2026-09-04).

An earlier take-only ETH path on 2026-09-02 (header 327135, `0xa814557e…` / `0xb2c47c48…`) proved claim/take on the new vk after a packed withdraw. The run that printed `ETH_USDC_SEPOLIA_E2E_OK` at 17:07 UTC is the **full** deposit→mint→withdraw→claim→take for both assets.

---

## 3. Incidents during the run (fixed or worked around)

1. **E2e claimed the withdraw seal height, not the submitted Groth16 header.** SAMPLE_GAP jumps never finalize `headers(326885)`. Fixed: wait `eth_fpl_tip ≥ seal`, `openClaim` at on-chain tip.
2. **Mint subprocess timeout was fatal.** LC lag killed the e2e with uncaught `TimeoutExpired` after 1800s of captured stdout. Fixed: short mint attempts, retry on LC lag.
3. **2300 unique-pending deadlock (2026-09-02 ~11:41–15:31 UTC).** v1 proposed height 327156; other seats rejected `state root mismatch`. Rule 2.9.4 will not drop a unique pending; header re-fans reset the view-change timer. LC updates sat in mempool. Recovery: restart v1 from snapshot (pending is not persisted). Not a force-seal. Tip moved 327155 → 327176+; LC execution jumped past the deposit block (11619969 → 11620401). Then mint succeeded.

---

## 4. What this does and does not prove

**Proved on Sepolia testnet, production rails:**

- Deposit events mint only after LC + receipt MPT.
- Public `submit-rail-withdraw` packs without admin sock.
- Ethereum verifies 4× Falcon-512 Groth16 under the ptau+contribute+beacon vk.
- `take` pays only the dest-lock address.

**Not proved / not claimed:**

- BitVM2 / BTC public peg-out (`BITVM2_E2E_OK`).
- Mainnet Groth16 (testnet ceremony, not a large public MPC).
- QC header lag SLA under `SAMPLE_GAP×2` without jumps (2 workers on 15 GiB cannot hold gap 48 at tip).
- Audit.

---

## 5. Reproduce

```bash
export PL_ROOT=/home/falcon/falcon-pl-public-testnet-2300
export FPL_PRODUCTION=1 FPL_E2E_ASSETS=ETH,USDC FPL_FEE_RESERVE=10000
# depositor key must hold Sepolia ETH and ≥10 USDC (e2e signs depositEth/depositUsdc)
python3 $PL_ROOT/scripts/pl-eth-sepolia-e2e.py
# pass: ETH_USDC_SEPOLIA_E2E_OK
```

Config: `crates/fd-pl/config/sepolia-qc.json`. Deploy: `scripts/deploy-eth-sepolia.sh`. Setup: `scripts/falcon-qc-setup-one-ptau.sh`.
