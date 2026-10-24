# Mainnet transition: testnet snapshot, genesis manifest, allocations

**Status: proposal for review (2026-10-14). Nothing here is promised. Every figure, date and rule is subject to change** until Falcon publishes the mainnet parameter sheet and the governance ADR (`falcon-pl-rs/crates/fd-pl/docs/AMENDMENTS.md` §3.3).

## 1. Allocations (from the 4B genesis circulating supply)

| Allocation | Amount | Bucket | Notes |
|---|---|---|---|
| Pre-mainnet airdrop | 1B | Community (2B) | Testnet activity snapshot → credited at mainnet genesis. Includes the operators pool (up to 100M, see §2a) |
| Release-phase giveaway | 1B | Community (2B) | Mainnet epochs 1–7 (no protocol emission then); per epoch for arcade, activity and similar; per-epoch and per-account caps |
| First-35 prize seats | 12M | Builder pot (1B) | 5 × 1,000,000 · 10 × 500,000 · 20 × 100,000. Separate from the airdrop |
| Mainnet faucet | drips | Faucet (1B) | Unchanged |
| Treasury | 196B | keyless | Untouched; emission from epoch 8 as today |

Users and operators are ranked separately (testnet activity board and operators board on falcon-fpl.com/scan). Their pools never mix.

## 2. Testnet snapshot

1. **Freeze date** announced at least 4 weeks ahead. Snapshot height H_snap recorded with its ledger hash.
2. For each eligible account: `name`, the **bound Falcon-512 public key** (account `meta.public_key`, or the keyring key), testnet score, operators score if any, and the computed amount. An account qualifies only if it has a bound key, i.e. it signed at least one transaction.
3. Excluded: test, load-test, system and Falcon-operated accounts (rules in the site's `src/config/tracker.ts`). Linked accounts are scored as one (shared hashed faucet IP, later the funding graph).
4. Publish the full list (name, key hash, amount) and its **Merkle root** (leaf = `sha256(name | pubkey_hex | amount)`, sorted by name) on falcon-fpl.com and in this repo. The root goes into the genesis manifest.

## 2a. Operators pool (decided 2026-10-21; proposed, subject to change)

- **Size**: up to 10% of the 1B pre-mainnet airdrop (100M FPL). **Cap**: 1% of 1B (10M FPL) per operator.
- **Split of the 1B**: 100M operators pool + 50M buffer (5%, fixed) + 850M users. Operator FPL not paid out goes to users.
- **Score**: average epoch work share over chain epochs from the epoch current on 2026-10-21 to the snapshot. Epochs with no samples at all (our cron/node outage) are left out of the denominator; a seat missing from a sampled epoch scores 0 for it. Epoch work share = 0.5 × proposer-credit share + 0.4 × vote-credit share + 0.1 × floor (presence) weight share — the rewards v2 split, as on the operators board. An epoch where the seat is jailed or earn-locked counts 0.
- **Eligibility**: active (non-zero, not jailed / locked) in at least 50% of the window epochs.
- **Split**: N ≤ 10 eligible operators: linear between the lowest eligible score (0.7%, 7M) and the highest (1%, 10M); a single operator, or all-equal scores, get 1%. N > 10: 100M pro-rata by score, capped at 10M each, excess redistributed pro-rata.
- **Excluded**: Falcon-operated seats (v1–v13, falcon-node, scottspersonalnode) plus the tracker's test, system, bond and watcher rules.
- Implementation: falcon-fpl.com `src/config/operators.ts`, `src/lib/operator-score.ts`; per-epoch work sampled by the metrics cron into `operator_epoch_work`; leaderboard at `/airdrop#operators-pool`.

## 3. Anti-sybil review and appeals

- Automated flags (shared faucet IP hash, funding graph, wash or self-transfer patterns) produce a review list. Nothing is auto-banned.
- Reviewers decide; the outcome is recorded per account (reason code only, no personal data).
- **Appeals window: 14 days** after the draft list is published. An appeal is a message signed by the account key. The list is final after the window and only then hashed into the manifest.

## 4. Genesis manifest (preferred)

A new node input `--genesis-manifest <file.json>` for the mainnet genesis:

```json
{
  "network_id": 1,
  "merkle_root": "<hex>",
  "accounts": [
    { "name": "dave", "public_key": "<falcon-512 pub hex>", "balance": 123456, "source": "airdrop" },
    { "name": "seat-holder", "public_key": "<hex>", "balance": 1000000, "source": "first35" }
  ],
  "buckets": { "community": 2000000000, "faucet": 1000000000, "builder": 1000000000 }
}
```

- Each entry creates the account with `meta.public_key` preset and the name **activated** (no fee), so `key_bind_check` accepts the holder's first transaction.
- Balances are debited from the named bucket (`airdrop` → community, `first35` → builder). The genesis checks Σ ≤ bucket and that the manifest hashes to the published root.
- The release-phase 1B stays in a community distribution account for per-epoch payouts.

## 5. Wallet UX

- The site wallet keeps one passkey-wrapped Falcon seed. On the **mainnet** toggle it unlocks the same key and queries `account_query(name)` on mainnet.
- If the account was in the manifest, the balance simply appears. There is no claim step and no new key.
- If the name was reserved but not credited (fallback, §6), the wallet shows "reserved for you" and binds on the first signed transaction.
- Testnet signatures can't be replayed on mainnet: the network id is in every signed payload.

## 6. Fallback: PayToKey plus name reservation

If a genesis manifest is not available, use only existing primitives:

1. After genesis, the distribution account runs `reserve_name(name, public_key)` for each snapshot entry.
2. It sends `PayToKey` to each entry's key commitment.
3. The holder's first transaction with that key binds it (`key_bind_check` accepts a funder-committed key hash).

Costs: one tx per account, plus the operator time to run it.

## 7. Lost keys and unclaimed funds

- Keys live only in the user's browser or their backup. Before the snapshot, users are told to make a wallet backup.
- **Unclaimed sweep:** amounts for accounts with no signed mainnet transaction about **12 months** after genesis return to the community bucket.
  - With a genesis manifest, this needs a protocol rule (sweep at height X for untouched manifest accounts).
  - Alternatively, credit via an escrow that releases on first use.
- No key recovery by operators. A lost key means the allocation is swept.

## 8. Timeline (indicative)

| When | Step |
|---|---|
| T − 8 weeks | Publish rules, freeze-date announcement, start backup reminders |
| T − 4 weeks | Snapshot freeze (H_snap) |
| T − 4 → T − 3 weeks | Draft list + review flags |
| T − 3 → T − 1 weeks | 14-day appeals window |
| T − 1 week | Final list, Merkle root, manifest published |
| T (genesis) | Accounts credited; First-35 seats credited |
| Epochs 1–7 | Release-phase giveaway, per epoch |
| Epoch 8 | Protocol emission starts (unchanged) |
| T + ~12 months | Unclaimed sweep |

## 9. Node work for Falcon

1. `--genesis-manifest` loader: create accounts with preset `meta.public_key`, activated names and balances; debit named buckets; verify the Merkle root and Σ against the bucket caps; reject duplicates or reserved names.
2. Mainnet `genesis_accounts()` without the testnet harness accounts (alice, bob, carol, dave, load wallets).
3. Named bucket accounts at genesis: `community`, `faucet`, `builder` (2B / 1B / 1B), plus a community distribution account for the release phase.
4. Key-bind gate active from genesis on mainnet (`key_bind_from_height`).
5. Unclaimed-sweep rule (or the escrow alternative).
6. Read-only RPC for the tracker: `accounts_list` and `blocks_range` (see the site's tracker plan), so the snapshot covers all accounts and transaction history.
7. A snapshot export tool: name, bound key, sequence at H_snap.
