# Trust model: protocol-controlled by design

**Principle:** No privileged operator, admin key, or “foundation wallet” can discretionary drain user or pool funds.  
**Optics:** Even chain authors cannot “just take” reserves without breaking consensus rules everyone runs.

---

## What “protocol-controlled” means

| Layer | Who decides | Mutable after genesis? |
|-------|-------------|-------------------------|
| Consensus rules | Code / shared binary | Only by everyone upgrading (social, not a secret key) |
| Account policies | On-chain state + rules | Vault hard-lock irreversible; Pool/Lending sealed |
| Value movement | Tx types + `can_apply` | No skip path for “admin” |
| Pool / market funds | Future **protocol** ops only | Key cannot `payment` out |

There is **no** superuser account that can override guards.

---

## Account classes (trust)

### Hot
User-controlled, free spend. Risk = key risk. Appropriate for small balances.

### Vault (user cold / layered cold)
- User still holds a key (must sign).
- After hard lock: key can **only** send to 1–5 pre-chosen addresses.
- **Not** for holding AMM reserves (key can still move to allowlist).
- **Is** for personal treasury: “even if I’m phished, attacker only feeds my hardware wallet.”

### SPV rails BTC / ETH / BNB / XRP (**protocol mint only via proofs**)

- Assets registered at genesis with issuer `protocol:spv-bridge`.
- **No** `Issue` / `Mint` / free `Burn` of rail codes; **no** federated/UNL mint.
- Mint only via `RailDeposit` after light-client SPV verification; burn via `RailWithdraw` (claim note for foreign lock).
- Sealed `bridge:{RAIL}` vaults: no free payment. See [BRIDGE_SPV.md](./BRIDGE_SPV.md).

### Pool & LendingMarket (**protocol economic accounts**)
- Opened from Hot → immediately **hard_locked**.
- **No** free `payment` / `transfer` / `mint` / `issue` / `burn` from that key — **ever**.
- **No** user `set_allowlist` / `vault_lock` (nothing to unlock).
- Reserves can **only** move via protocol TxKinds (`Swap`, `AddLiquidity`, `Supply`, `Borrow`, …) that apply **fixed math** to both sides.

So: there is no “pool admin key” that can empty the pool to a personal address.  
If someone claims they hold the pool key, that key is **useless for draining**.

### Treasury (fees)
At genesis, `treasury` is a **Vault hard-locked with empty allowlist** = pure sink.  
Fees can enter; **no payment can leave**. Burns reduce supply; treasury share accumulates on-chain with no withdraw path unless a future **protocol** emission/spend module is added by consensus upgrade (visible to all).

---

## Trust boundaries (who can steal what)

| Scenario | Can they steal pool funds? | Can they steal your Vault? |
|----------|----------------------------|----------------------------|
| Attacker steals your Hot key | n/a | If Vault locked to Hot, only to that Hot |
| Attacker steals Vault key | n/a | Only to allowlisted destinations you set |
| Attacker steals “pool” key | **No** (no free outflows) | n/a |
| Chain operator / you | **No** (no admin override) | **No** (same rules) |
| Malicious wallet app | Rejected if dest not allowed / not protocol op | Same |
| Quantum breaks Falcon | Separate crypto risk; still no free pool drain path | Same policy engine |

---

## Genesis (only manual bootstrap)

Allowed at genesis / first network start:

- Genesis balances, UNL, economics constants (burn bps, native asset id).
- Sealed treasury policy.
- Optional pre-created Pool/LendingMarket shells with zero discretionary key path.

**Not** allowed as a hidden runtime privilege:

- Operator “emergency withdraw”.
- Upgrade key that rewrites allowlists for sealed pools.
- Pause that freezes users while operator moves funds.

Upgrades = **new software everyone runs**, not a signed “admin” message.

---

## Future DeFi (still protocol-controlled)

```text
User Hot ──Supply──► LendingMarket (protocol account)
User Hot ──Swap───► Pool (protocol account)
Pool ──RemoveLiquidity──► only LP owner address (encoded in tx, checked by rules)
```

Each op:

1. Validates math (invariant, LTV, oracle median from UNL votes if needed).
2. Debits/credits Pool/LendingMarket **without** a free `payment` from a human key.
3. Is fully determined by ordered txs + state (replayable, auditable).

---

## What we refuse (by design)

- Multisig “council” that can empty pools without rule-bound ops.
- Soft-unlock of Pool/LendingMarket via key.
- Empty allowlist Vault that can later be opened by operator (hard lock empty = forever sink).
- Off-chain “we’ll send you funds from treasury” without on-chain protocol spend rules.

---

## Summary one-liner

**Users can constrain their own Vaults; pools and markets have no spendable keys — only consensus rules move those balances.**
