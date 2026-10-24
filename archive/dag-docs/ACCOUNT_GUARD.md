# Falcon DAG — Protocol Account Types & Guards

**Build:** `falcon-dagd/0.8.0-rust`  
**Enforcement:** ledger `can_apply` / `apply` — not wallet software.

## Account types

| Type | Default outbound | Destinations | Lock |
|------|------------------|--------------|------|
| **Hot** | Anywhere | n/a | n/a |
| **Timed** | Allowlist only (empty = receive-only) | editable until hard-lock | optional hard lock; velocity limits |
| **Vault** | None until allowlist | 1–5 addresses, then permanent | **irreversible** hard lock |
| **Pool** | None until allowlist + lock | 1–5 (LP / swap recipients / parent vault) | **irreversible** (same lock tx) |
| **LendingMarket** | None until allowlist + lock | protocol paths only | **irreversible** |

### Why Vault / Pool / LendingMarket

Even if a private key is stolen:

- Attacker **cannot** send to a random address.
- Worst case: send only to pre-chosen destinations (owner’s hot wallet, next vault layer, LP owners).

Layering (UX, not special protocol magic):

```
Vault2 ──only──► Vault1 ──only──► Hot
```

Air-gap vault signing keys offline; protocol still enforces destinations on-chain.

### Pool & lending fit (protocol-sealed — no key drain)

AMM / lending reserve accounts are **`Pool` / `LendingMarket`**, not Hot and not user-Vault:

| Risk | Mitigation |
|------|------------|
| “Admin” key empties pool | **No free payment/transfer/burn** from these types — ever |
| Operator optics / theft | Same rules for everyone; no admin override |
| Malicious wallet | Cannot invent a spend path |

On `open_account_type(pool|lending_market)` the account is **immediately hard_locked** with empty allowlist.  
Value leaves **only** via future protocol TxKinds (`Swap`, `Supply`, `Borrow`, …) that apply fixed math.

User **Vault** remains key-signed but destination-locked (personal cold storage).  
See [TRUST_AND_PROTOCOL_CONTROL.md](./TRUST_AND_PROTOCOL_CONTROL.md).

## Flow (Vault)

1. Fund a Hot account (or use faucet `acctN`).
2. `open_account_type` → `vault` (receive-only, empty allowlist).
3. `set_allowlist` with 1–5 withdraw addresses (still changeable).
4. `vault_lock` — **permanent**.
5. Outbound `payment` / `transfer` only to allowlisted addresses.

## RPC examples

```json
{"method":"submit","params":[{"kind":"open_account_type","account":"acct1","sequence":1,"account_type":"vault","fee":10}]}
{"method":"submit","params":[{"kind":"set_allowlist","account":"acct1","sequence":2,"destinations":["acct2"],"fee":10}]}
{"method":"submit","params":[{"kind":"vault_lock","account":"acct1","sequence":3,"fee":10}]}
{"method":"account_info","params":[{"account":"acct1"}]}
```

`account_info` returns `account_type`, `hard_locked`, `allowlist`.

## Tx kinds (guard)

| kind | Purpose |
|------|---------|
| `open_account_type` | Hot → Vault / Timed / Pool / LendingMarket |
| `set_allowlist` | Set destinations (max 5); blocked if hard_locked |
| `vault_lock` | Permanent hard lock (Vault/Pool/Lending/Timed) |
| `set_limits` | Timed only: max_per_tx / max_per_window |

## Defaults

- Genesis faucet accounts start as **Hot** (frictionless demos).
- New product wallets should **create Vault** by default for high-value cold storage.
- Pool/Lending accounts always created as those types at deploy.

## Not yet (Timed soft path)

- Real time-lock delay on unlock (block height delay queue).
- Secondary guardian multi-sig.
- Rolling 24h windows with wall-clock (v1 uses cumulative window counter).

## DeFi protocol hooks (implemented)

1. **Typed accounts** for pool/market isolation.  
2. **Multi-asset balances** under guarded accounts.  
3. **State root** including policies (light clients see guard status).  
4. **Protocol TxKinds** (`Swap`, `AddLiquidity`, `Supply`, `Borrow`, …) debit/credit Pool/Lending under fixed math — free `payment` from those types remains **rejected**.

See: [DEFI_PROTOCOL.md](./DEFI_PROTOCOL.md), [ECONOMICS.md](./ECONOMICS.md).
