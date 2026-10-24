# Protocol SPV Bridge (non-custodial rails)

**Build:** `falcon-dagd/0.34.0-rust`  
**Model:** **Protocol-controlled light clients** — **not** a federated / UNL custody peg.  
**Agents:** humans and bots are equal clients; invalid txs return `tecPROTOCOL` (no override).  
**Indexers:** `scripts/rail_indexers/` feed the agent; see [RAIL_AGENT.md](./RAIL_AGENT.md).

Minting `BTC` / `ETH` / `BNB` / `XRP` on Falcon requires a **verifiable inclusion proof** against the on-chain header tip. There is **no** operator mint key and **no** multisig “federation credits your balance” path.

---

## What “protocol controlled” means here

| Path | Allowed? |
|------|----------|
| `Issue` / `Mint` of rail assets | **No** |
| Free `Burn` of rail assets | **No** (must use `RailWithdraw`) |
| `payment` from `bridge:BTC` etc. | **No** (sealed vault) |
| `SubmitRailHeader` (anyone, fee-paid) | **Yes** — extends light client if parent/work valid |
| `RailDeposit` + valid SPV proof | **Yes** — only mint path |
| `RailWithdraw` (burn + claim note) | **Yes** — only burn path |

Real coins never “live inside” Falcon. Falcon holds **claims**. The **external lock** (Bitcoin script / EVM contract / XRPL escrow) holds the coins and releases only when the foreign side verifies Falcon finality (or an equivalent challenge protocol).

```text
External chain                         Falcon protocol
──────────────                         ───────────────
User locks to protocol lock_id
       │
       ▼
Header + merkle proof ──SubmitRailHeader / RailDeposit──► mint rail asset
                                                              │
User wants coins out ◄── foreign lock verifies ── RailWithdraw burn note
```

---

## Genesis rails

| Asset | Family | Bridge account | Issuer |
|-------|--------|----------------|--------|
| `BTC` | Bitcoin | `bridge:BTC` | `protocol:spv-bridge` |
| `ETH` | Evm | `bridge:ETH` | `protocol:spv-bridge` |
| `BNB` | Evm | `bridge:BNB` | `protocol:spv-bridge` |
| `XRP` | XrpLedger | `bridge:XRP` | `protocol:spv-bridge` |

`lock_id` at genesis = `sha256("falcon-spv-lock-v1:{RAIL}")` (placeholder). Production replaces this with the hash of the **deployed** lock script / contract / escrow id so deposits only count against the real protocol vault.

Default **min confirmations:** 6.

---

## TxKinds

### `SubmitRailHeader`
- `asset` = rail code  
- `bridge`: `height`, `block_hash`, `parent_hash`, `merkle_root`, `work`  
- Rules: first header bootstraps tip; later headers must have `parent_hash == tip`, `height == tip+1`, `work >= tip_work`  
- Anyone may submit (pays fee). Consensus re-checks; invalid headers rejected.

### `RailDeposit`
- `amount` = units to mint  
- `destination` = Falcon recipient  
- `bridge`: `external_txid`, `block_hash`, `height`, `merkle_path`, `merkle_index`, `lock_id`  
- Rules:  
  - `lock_id` matches rail  
  - header at `height` exists and matches `block_hash`  
  - tip is ≥ `min_confirmations` deeper than deposit height  
  - merkle root from leaf+path equals header merkle root  
  - `external_txid` not already spent  

### `RailWithdraw`
- Burns `amount` of rail from user  
- Opens `WithdrawalNote` (`note_id`, `external_to`, `falcon_height`) for the **foreign** lock to redeem  
- Falcon does **not** sign a BTC/ETH multisig spend

---

## Linking networks (without custody)

| Layer | Role |
|-------|------|
| Falcon light client | Headers + inclusion proofs → mint/burn accounting |
| Foreign lock contract/script | Holds coins; releases on Falcon finality proof / challenge |
| Relayers | Submit headers/proofs (unprivileged; cannot mint without valid proof) |
| UNL | Orders Falcon txs only — **not** a BTC wallet committee |

### Chain-specific outbound (honest scope)

| Rail | Inbound SPV | Outbound non-custodial |
|------|-------------|-------------------------|
| **ETH / BNB** | Receipt/tx against block root (substrate uses generic merkle; upgrade to MPT) | EVM contract verifies Falcon cert/header batch |
| **BTC** | Tx merkle in block + header chain work | Harder: BitVM / optimistic exit / HTLC; substrate emits claim note |
| **XRP** | Ledger tx inclusion | Escrow finish conditioned on attested Falcon note |

v0.11 implements the **Falcon-side protocol substrate** (deterministic verify + rails). Family-specific binary codecs (BTC double-SHA256, EVM MPT, XRPL) plug into the same TxKinds without reopening mint authority.

---

## RPC

- `submit` kinds: `submit_rail_header`, `rail_deposit`, `rail_withdraw`  
- `rail_info` — tip, lock_id, minted/burned, `federated_mint: false`  
- `economy` includes `rails`

---

## Explicitly rejected design

**Federated / UNL peg** (committee sees BTC, posts mint without SPV) is **not** implemented and must not be added as a parallel path. If a proof is invalid, **no mint**.

---

## Rule-bound agent

```bash
# Local synthetic demo (not mainnet): headers + deposit, then prove Issue is rejected
python3 scripts/rail_agent.py --rpc http://127.0.0.1:18811 --rail BTC --dev-synthetic
```

Watch mode (no mint without external proofs): omit `--dev-synthetic`.

Submit path now **admits only** txs that pass `can_apply` → agents get `tecPROTOCOL` immediately for sealed-account drains, invalid SPV, or rail `Issue`/`Mint`.

Snapshot **v4** persists rails, headers, spent deposits, and withdrawal notes across restart.

## Next engineering (off Falcon, then tighten proofs)

1. Deploy lock contracts/scripts per rail with Falcon light-client verifier.  
2. Specialize merkle verify per `RailFamily`.  
3. Production indexer → same agent interface (replace `--dev-synthetic` data source).
