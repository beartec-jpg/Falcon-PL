//! Multi-asset account state, fees, token ops, emissions, state root, account guards, SPV rails.

mod bridge;

pub use bridge::{
    default_rail_configs, genesis_lock_id, is_protocol_rail, merkle_root_bitcoin,
    merkle_root_from_proof, merkle_root_protocol, sorted_withdrawals, synth_hash32,
    btc_txid_to_internal_leaf, deposit_proof_leaf, empty_claim_root, empty_evm_deposit_root,
    empty_xrp_deposit_root, evm_bind_root, evm_deposit_leaf, merkle_root_binary, note_id_bytes32,
    xrp_bind_root, xrp_deposit_leaf,
    synthetic_inclusion, synthetic_inclusion_bitcoin, verify_claim_proof, verify_deposit,
    withdrawal_claim_leaf, withdrawal_claim_proof, withdrawal_claim_root, RailConfig, RailHeader,
    WithdrawalNote, DEFAULT_MIN_CONFIRMATIONS, HEADER_HISTORY,
};

use fd_types::{
    bridge_account, merkle_root, sha256_hex, AccountId, AccountType, AssetId, EconomicConfig,
    HashHex, Transaction, TxKind, BRIDGE_ISSUER, GENESIS_RAILS, NATIVE_ASSET, TREASURY_ACCOUNT,
    VAULT_MAX_DESTINATIONS,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetMeta {
    pub issuer: AccountId,
    pub total_supply: u64,
    pub max_supply: Option<u64>,
}

/// Protocol-enforced account policy (Guard / Vault / Pool / Lending).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountPolicy {
    pub account_type: AccountType,
    /// Allowed outbound destinations (Vault/Pool/Timed/Lending).
    pub allowlist: Vec<AccountId>,
    /// Vault hard lock: permanent, irreversible. Also finalizes Pool/Lending allowlist.
    pub hard_locked: bool,
    /// Timed: max native-equivalent amount per outbound value tx (0 = unlimited).
    pub max_per_tx: u64,
    /// Timed: max amount per rolling window (0 = unlimited). Window tracking simplified for v1.
    pub max_per_window: u64,
    /// Cumulative outbound value in current window (native units, simplified).
    pub window_spent: u64,
}

impl Default for AccountPolicy {
    fn default() -> Self {
        Self {
            account_type: AccountType::Hot,
            allowlist: vec![],
            hard_locked: false,
            max_per_tx: 0,
            max_per_window: 0,
            window_spent: 0,
        }
    }
}

impl AccountPolicy {
    /// True if this policy forbids unrestricted outbound value transfers.
    pub fn is_restricted(&self) -> bool {
        !matches!(self.account_type, AccountType::Hot)
    }

    pub fn can_send_to(&self, dest: &str) -> bool {
        match self.account_type {
            AccountType::Hot => true,
            AccountType::Timed => {
                if self.allowlist.is_empty() {
                    // empty allowlist on Timed = receive-only for value
                    false
                } else {
                    self.allowlist.iter().any(|d| d == dest)
                }
            }
            AccountType::Vault | AccountType::Pool | AccountType::LendingMarket => {
                // Unlocked vault with empty list: no sends.
                // Locked (or pool/lending): only allowlist.
                if self.allowlist.is_empty() {
                    false
                } else {
                    self.allowlist.iter().any(|d| d == dest)
                }
            }
        }
    }
}

/// Constant-product AMM pool (reserves live on the Pool account balances).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PoolConfig {
    pub asset_a: AssetId,
    pub asset_b: AssetId,
    pub lp_asset: AssetId,
    /// Swap fee in bps (e.g. 30 = 0.30%).
    pub fee_bps: u16,
    /// Share of the swap fee skimmed to sealed treasury (bps of the fee, e.g. 1667 ≈ 1/6).
    pub protocol_fee_share_bps: u16,
}

/// Lending market (borrow asset reserves on LendingMarket account).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MarketConfig {
    pub asset: AssetId,
    /// Max LTV in bps of collateral value (oracle-priced).
    pub ltv_bps: u16,
    /// Liquidation bonus for liquidator in bps of collateral seized.
    pub liq_bonus_bps: u16,
    /// Supply index (Aave-style); INDEX_ONE = 1.0.
    pub supply_index: u64,
    /// Borrow index; INDEX_ONE = 1.0.
    pub borrow_index: u64,
    /// `applied_user_txs` at last accrual.
    pub last_accrual_txs: u64,
    /// Base borrow rate in bps per `ACCRUAL_TX_WINDOW` applied txs at 0 utilization.
    pub base_rate_bps: u16,
    /// Additional rate at 100% utilization (bps per window).
    pub slope_bps: u16,
    /// Assets accepted as collateral (empty = native only).
    pub allowed_collateral: Vec<AssetId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountState {
    /// balances[account][asset] = amount
    pub balances: BTreeMap<AccountId, BTreeMap<AssetId, u64>>,
    pub sequences: BTreeMap<AccountId, u64>,
    pub assets: BTreeMap<AssetId, AssetMeta>,
    /// Per-account protocol policy (defaults to Hot if missing).
    pub policies: BTreeMap<AccountId, AccountPolicy>,
    /// Registered AMM pools (key = pool account id).
    pub pools: BTreeMap<AccountId, PoolConfig>,
    /// Registered lending markets (key = market account id).
    pub markets: BTreeMap<AccountId, MarketConfig>,
    /// supplied[user][market] = supply *shares* (underlying = shares * supply_index / INDEX_ONE)
    pub supplied: BTreeMap<AccountId, BTreeMap<AccountId, u64>>,
    /// borrowed[user][market] = debt *shares*
    pub borrowed: BTreeMap<AccountId, BTreeMap<AccountId, u64>>,
    /// collateral[user][market][asset] = locked amount
    pub collateral: BTreeMap<AccountId, BTreeMap<AccountId, BTreeMap<AssetId, u64>>>,
    /// oracle_votes[asset][voter] = price (1e6 native per unit)
    pub oracle_votes: BTreeMap<AssetId, BTreeMap<AccountId, u64>>,
    /// Effective (median) oracle prices
    pub oracle_prices: BTreeMap<AssetId, u64>,
    /// Cumulative protocol AMM fee skims (native-equivalent tracking of input asset units skimmed)
    pub protocol_fees_skimmed: u64,
    /// SPV rail configs (BTC/ETH/BNB/XRP at genesis).
    pub rails: BTreeMap<AssetId, RailConfig>,
    /// headers[rail][height] = RailHeader
    pub rail_headers: BTreeMap<AssetId, BTreeMap<u64, RailHeader>>,
    /// spent deposit external txids per rail (prevent double mint)
    pub rail_spent_deposits: BTreeMap<AssetId, BTreeSet<String>>,
    /// open withdrawal notes (burn receipts for foreign-chain claim)
    pub rail_withdrawals: BTreeMap<HashHex, WithdrawalNote>,
    pub burned_native: u64,
    pub treasury_fees: u64,
    pub emissions_minted: u64,
    /// Count of successfully applied user txs since genesis.
    pub applied_user_txs: u64,
    /// Last non-genesis ordered batch count for which emissions were applied.
    pub last_emission_height: u64,
    pub economics: EconomicConfig,
}

impl Default for AccountState {
    fn default() -> Self {
        Self {
            balances: BTreeMap::new(),
            sequences: BTreeMap::new(),
            assets: BTreeMap::new(),
            policies: BTreeMap::new(),
            pools: BTreeMap::new(),
            markets: BTreeMap::new(),
            supplied: BTreeMap::new(),
            borrowed: BTreeMap::new(),
            collateral: BTreeMap::new(),
            oracle_votes: BTreeMap::new(),
            oracle_prices: BTreeMap::new(),
            protocol_fees_skimmed: 0,
            rails: BTreeMap::new(),
            rail_headers: BTreeMap::new(),
            rail_spent_deposits: BTreeMap::new(),
            rail_withdrawals: BTreeMap::new(),
            burned_native: 0,
            treasury_fees: 0,
            emissions_minted: 0,
            applied_user_txs: 0,
            last_emission_height: 0,
            economics: EconomicConfig::default(),
        }
    }
}

/// Integer square root (floor).
fn isqrt(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = x.div_ceil(2).max(1);
    while y < x {
        x = y;
        y = x.saturating_add(n / x) / 2;
    }
    x
}

/// Constant-product swap out given fee_bps on input.
fn swap_out(amount_in: u64, reserve_in: u64, reserve_out: u64, fee_bps: u16) -> u64 {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return 0;
    }
    let fee_bps = fee_bps.min(9_999) as u64;
    let amount_in_with_fee = (amount_in as u128) * (10_000 - fee_bps) as u128;
    let num = amount_in_with_fee * reserve_out as u128;
    let den = (reserve_in as u128) * 10_000 + amount_in_with_fee;
    if den == 0 {
        return 0;
    }
    (num / den) as u64
}

pub const AMM_DEFAULT_FEE_BPS: u16 = 30;
/// ~1/6 of the swap fee is skimmed to sealed treasury (optics: protocol, not pool key).
pub const AMM_PROTOCOL_FEE_SHARE_BPS: u16 = 1_667;
pub const LENDING_DEFAULT_LTV_BPS: u16 = 5_000; // 50%
pub const LENDING_LIQ_BONUS_BPS: u16 = 1_000; // 10%
/// Index scale: INDEX_ONE means 1.0.
pub const INDEX_ONE: u64 = 1_000_000_000;
/// Oracle price scale: 1_000_000 = 1.0 native per 1 asset unit.
pub const ORACLE_PRICE_SCALE: u64 = 1_000_000;
/// Accrue interest every this many applied user txs (rate is "per window").
pub const ACCRUAL_TX_WINDOW: u64 = 100;
pub const LENDING_BASE_RATE_BPS: u16 = 50; // 0.50% per window at 0 util
pub const LENDING_SLOPE_BPS: u16 = 200; // +2.00% at full util
/// Minimum distinct oracle voters before median replaces 1:1 fallback (1 = any vote wins).
pub const ORACLE_MIN_VOTES: usize = 1;

impl AccountState {
    pub fn with_economics(economics: EconomicConfig) -> Self {
        Self {
            economics,
            ..Default::default()
        }
    }

    pub fn balance(&self, account: &str, asset: &str) -> u64 {
        self.balances
            .get(account)
            .and_then(|m| m.get(asset))
            .copied()
            .unwrap_or(0)
    }

    pub fn native_balance(&self, account: &str) -> u64 {
        self.balance(account, &self.economics.native_asset)
    }

    /// Oracle price in ORACLE_PRICE_SCALE units (native per asset). Native always 1.0.
    pub fn oracle_price(&self, asset: &str) -> u64 {
        let native = &self.economics.native_asset;
        if asset == native {
            return ORACLE_PRICE_SCALE;
        }
        self.oracle_prices
            .get(asset)
            .copied()
            .unwrap_or(ORACLE_PRICE_SCALE) // 1:1 fallback until voted
    }

    /// Value in native units (floor): amount * price / ORACLE_PRICE_SCALE.
    pub fn asset_value_native(&self, asset: &str, amount: u64) -> u64 {
        ((amount as u128) * (self.oracle_price(asset) as u128) / (ORACLE_PRICE_SCALE as u128)) as u64
    }

    pub fn supply_underlying(&self, user: &str, market: &str) -> u64 {
        let shares = self
            .supplied
            .get(user)
            .and_then(|m| m.get(market))
            .copied()
            .unwrap_or(0);
        let idx = self
            .markets
            .get(market)
            .map(|m| m.supply_index)
            .unwrap_or(INDEX_ONE)
            .max(1);
        ((shares as u128) * (idx as u128) / (INDEX_ONE as u128)) as u64
    }

    pub fn borrow_underlying(&self, user: &str, market: &str) -> u64 {
        let shares = self
            .borrowed
            .get(user)
            .and_then(|m| m.get(market))
            .copied()
            .unwrap_or(0);
        let idx = self
            .markets
            .get(market)
            .map(|m| m.borrow_index)
            .unwrap_or(INDEX_ONE)
            .max(1);
        ((shares as u128) * (idx as u128) / (INDEX_ONE as u128)) as u64
    }

    pub fn total_supplied_underlying(&self, market: &str) -> u64 {
        let idx = self
            .markets
            .get(market)
            .map(|m| m.supply_index)
            .unwrap_or(INDEX_ONE)
            .max(1);
        let mut shares = 0u64;
        for m in self.supplied.values() {
            shares = shares.saturating_add(m.get(market).copied().unwrap_or(0));
        }
        ((shares as u128) * (idx as u128) / (INDEX_ONE as u128)) as u64
    }

    pub fn total_borrowed_underlying(&self, market: &str) -> u64 {
        let idx = self
            .markets
            .get(market)
            .map(|m| m.borrow_index)
            .unwrap_or(INDEX_ONE)
            .max(1);
        let mut shares = 0u64;
        for m in self.borrowed.values() {
            shares = shares.saturating_add(m.get(market).copied().unwrap_or(0));
        }
        ((shares as u128) * (idx as u128) / (INDEX_ONE as u128)) as u64
    }

    /// Sum of oracle-valued collateral for user on market (native units).
    pub fn collateral_value_native(&self, user: &str, market: &str) -> u64 {
        let Some(by_asset) = self
            .collateral
            .get(user)
            .and_then(|m| m.get(market))
        else {
            return 0;
        };
        let mut total = 0u64;
        for (asset, amt) in by_asset {
            total = total.saturating_add(self.asset_value_native(asset, *amt));
        }
        total
    }

    pub fn collateral_amount(&self, user: &str, market: &str, asset: &str) -> u64 {
        self.collateral
            .get(user)
            .and_then(|m| m.get(market))
            .and_then(|a| a.get(asset))
            .copied()
            .unwrap_or(0)
    }

    /// Recompute median price for an asset from votes.
    pub fn recompute_oracle_median(&mut self, asset: &str) {
        let Some(votes) = self.oracle_votes.get(asset) else {
            return;
        };
        if votes.len() < ORACLE_MIN_VOTES {
            return;
        }
        let mut prices: Vec<u64> = votes.values().copied().collect();
        prices.sort_unstable();
        let mid = prices.len() / 2;
        let median = if prices.len() % 2 == 0 {
            (prices[mid - 1] + prices[mid]) / 2
        } else {
            prices[mid]
        };
        self.oracle_prices.insert(asset.to_string(), median);
    }

    /// Recompute and store the withdrawal claim merkle root for a rail.
    /// Deterministic from open notes — agents/EVM can recompute and must match.
    pub fn commit_claim_root(&mut self, rail: &str) {
        let notes = sorted_withdrawals(&self.rail_withdrawals, Some(rail));
        let root = withdrawal_claim_root(&notes);
        let count = notes.len() as u64;
        let height = self.applied_user_txs;
        if let Some(r) = self.rails.get_mut(rail) {
            r.claim_root = root;
            r.claim_note_count = count;
            r.claim_height = height;
        }
    }

    /// Accrue interest indices for a market based on applied_user_txs delta.
    /// Only compounds when at least one full `ACCRUAL_TX_WINDOW` has elapsed (avoids dust on same-flow repay).
    pub fn accrue_market(&mut self, market: &str) {
        let Some(mut m) = self.markets.get(market).cloned() else {
            return;
        };
        let delta = self.applied_user_txs.saturating_sub(m.last_accrual_txs);
        if delta < ACCRUAL_TX_WINDOW {
            return;
        }
        // Use whole windows only; leave remainder for next accrue.
        let windows = delta / ACCRUAL_TX_WINDOW;
        let delta = windows * ACCRUAL_TX_WINDOW;
        let total_bor = {
            let idx = m.borrow_index.max(1);
            let mut shares = 0u64;
            for map in self.borrowed.values() {
                shares = shares.saturating_add(map.get(market).copied().unwrap_or(0));
            }
            ((shares as u128) * (idx as u128) / (INDEX_ONE as u128)) as u64
        };
        let total_sup = {
            let idx = m.supply_index.max(1);
            let mut shares = 0u64;
            for map in self.supplied.values() {
                shares = shares.saturating_add(map.get(market).copied().unwrap_or(0));
            }
            ((shares as u128) * (idx as u128) / (INDEX_ONE as u128)) as u64
        };
        if total_bor == 0 {
            m.last_accrual_txs = m.last_accrual_txs.saturating_add(delta);
            self.markets.insert(market.to_string(), m);
            return;
        }
        let util_bps = if total_sup == 0 {
            10_000u64
        } else {
            ((total_bor as u128) * 10_000 / (total_sup as u128)).min(10_000) as u64
        };
        // rate per ACCRUAL_TX_WINDOW applied txs
        let rate_bps = m.base_rate_bps as u64
            + (m.slope_bps as u64).saturating_mul(util_bps) / 10_000;
        // interest factor growth: index *= 1 + rate * delta / (window * 10000)
        let growth = (INDEX_ONE as u128)
            .saturating_mul(rate_bps as u128)
            .saturating_mul(delta as u128)
            / ((ACCRUAL_TX_WINDOW as u128) * 10_000);
        if growth > 0 {
            let new_bor_idx = ((m.borrow_index as u128)
                .saturating_mul((INDEX_ONE as u128) + growth)
                / (INDEX_ONE as u128)) as u64;
            m.borrow_index = new_bor_idx.max(m.borrow_index);
            // suppliers earn the interest (index rises so same shares redeem more)
            if total_sup > 0 {
                let interest = ((total_bor as u128) * growth / (INDEX_ONE as u128)) as u64;
                if interest > 0 {
                    let new_sup_idx = ((m.supply_index as u128)
                        .saturating_mul((total_sup as u128) + interest as u128)
                        / (total_sup as u128)) as u64;
                    m.supply_index = new_sup_idx.max(m.supply_index);
                }
            }
        }
        m.last_accrual_txs = m.last_accrual_txs.saturating_add(delta);
        self.markets.insert(market.to_string(), m);
    }

    pub fn set_balance(&mut self, account: &str, asset: &str, amount: u64) {
        self.balances
            .entry(account.to_string())
            .or_default()
            .insert(asset.to_string(), amount);
    }

    pub fn credit(&mut self, account: &str, asset: &str, amount: u64) {
        let b = self.balance(account, asset).saturating_add(amount);
        self.set_balance(account, asset, b);
    }

    pub fn debit(&mut self, account: &str, asset: &str, amount: u64) -> bool {
        let b = self.balance(account, asset);
        if b < amount {
            return false;
        }
        self.set_balance(account, asset, b - amount);
        true
    }

    pub fn ensure_account(&mut self, account: &str) {
        self.sequences.entry(account.to_string()).or_insert(1);
        self.balances.entry(account.to_string()).or_default();
        self.policies
            .entry(account.to_string())
            .or_insert_with(AccountPolicy::default);
    }

    pub fn policy(&self, account: &str) -> AccountPolicy {
        self.policies
            .get(account)
            .cloned()
            .unwrap_or_default()
    }

    /// Value-moving kinds restricted by allowlist / vault lock.
    fn is_value_out_kind(kind: TxKind) -> bool {
        matches!(
            kind,
            TxKind::Payment | TxKind::Transfer | TxKind::Mint | TxKind::Issue
        )
    }

    fn guard_allows_value_out(&self, tx: &Transaction) -> bool {
        let pol = self.policy(&tx.account);
        match pol.account_type {
            AccountType::Hot => true,
            AccountType::Timed => {
                if !pol.can_send_to(&tx.destination) {
                    return false;
                }
                if pol.max_per_tx > 0 && tx.amount > pol.max_per_tx {
                    return false;
                }
                if pol.max_per_window > 0
                    && pol.window_spent.saturating_add(tx.amount) > pol.max_per_window
                {
                    return false;
                }
                true
            }
            AccountType::Vault => {
                // User vault: key can only pay allowlisted addresses after setup.
                pol.can_send_to(&tx.destination)
            }
            // Protocol-owned economic accounts: NO free payment/transfer/mint/issue.
            // Value may only move via dedicated protocol TxKinds (Swap, Supply, …).
            // Holding a "pool key" must never enable draining reserves — optics + safety.
            AccountType::Pool | AccountType::LendingMarket => false,
        }
    }

    /// True if this account is system/protocol economic infrastructure (not user-spendable).
    pub fn is_protocol_account(&self, account: &str) -> bool {
        matches!(
            self.policy(account).account_type,
            AccountType::Pool | AccountType::LendingMarket
        )
    }

    /// Genesis: native faucet + treasury account + native asset meta.
    pub fn genesis_faucet(economics: EconomicConfig) -> Self {
        let mut s = Self::with_economics(economics.clone());
        let native = economics.native_asset.clone();
        let mut total = 0u64;
        for i in 1..=economics.genesis_accounts {
            let acct = format!("acct{i}");
            s.ensure_account(&acct);
            s.set_balance(&acct, &native, economics.genesis_native_per_account);
            total = total.saturating_add(economics.genesis_native_per_account);
        }
        s.ensure_account(&economics.treasury);
        s.ensure_account(TREASURY_ACCOUNT);
        // Fee sink: vault hard-locked with empty allowlist = pure receive-only forever.
        // No operator key can withdraw treasury via payment (optics + safety).
        s.policies.insert(
            economics.treasury.clone(),
            AccountPolicy {
                account_type: AccountType::Vault,
                allowlist: vec![],
                hard_locked: true,
                max_per_tx: 0,
                max_per_window: 0,
                window_spent: 0,
            },
        );
        if economics.treasury != TREASURY_ACCOUNT {
            s.policies.insert(
                TREASURY_ACCOUNT.into(),
                AccountPolicy {
                    account_type: AccountType::Vault,
                    allowlist: vec![],
                    hard_locked: true,
                    max_per_tx: 0,
                    max_per_window: 0,
                    window_spent: 0,
                },
            );
        }
        s.assets.insert(
            native.clone(),
            AssetMeta {
                issuer: "genesis".into(),
                total_supply: total,
                max_supply: None,
            },
        );
        // Protocol SPV rails: assets + sealed bridge accounts (no key drain).
        s.rails = default_rail_configs();
        for &rail in GENESIS_RAILS {
            let ba = bridge_account(rail);
            s.ensure_account(&ba);
            s.policies.insert(
                ba,
                AccountPolicy {
                    account_type: AccountType::Vault,
                    allowlist: vec![],
                    hard_locked: true,
                    max_per_tx: 0,
                    max_per_window: 0,
                    window_spent: 0,
                },
            );
            s.assets.insert(
                rail.to_string(),
                AssetMeta {
                    issuer: BRIDGE_ISSUER.into(),
                    total_supply: 0,
                    max_supply: None,
                },
            );
            s.rail_headers.insert(rail.to_string(), BTreeMap::new());
            s.rail_spent_deposits
                .insert(rail.to_string(), BTreeSet::new());
        }
        s
    }

    /// Backward-compatible helper used by older call sites.
    pub fn genesis_faucet_simple(n: u32, balance: u64) -> Self {
        let mut econ = EconomicConfig::default();
        econ.genesis_accounts = n;
        econ.genesis_native_per_account = balance;
        Self::genesis_faucet(econ)
    }

    pub fn total_supply(&self, asset: &str) -> u64 {
        self.assets
            .get(asset)
            .map(|a| a.total_supply)
            .unwrap_or(0)
    }

    fn pay_fee(&mut self, payer: &str, fee: u64) -> bool {
        if fee == 0 {
            return true;
        }
        let native = self.economics.native_asset.clone();
        if !self.debit(payer, &native, fee) {
            return false;
        }
        let burn_bps = self.economics.fee_burn_bps.min(10_000) as u64;
        let treas_bps = self.economics.fee_treasury_bps.min(10_000) as u64;
        let burn = fee.saturating_mul(burn_bps) / 10_000;
        let to_treas = fee.saturating_mul(treas_bps) / 10_000;
        // remainder stays burned if bps sum < 10000, or last-drop to treasury if over
        let mut assigned = burn.saturating_add(to_treas);
        let mut burn = burn;
        let mut to_treas = to_treas;
        if assigned > fee {
            // scale down (shouldn't happen if bps sum <= 10000)
            burn = fee.saturating_mul(burn_bps) / (burn_bps + treas_bps).max(1);
            to_treas = fee.saturating_sub(burn);
            assigned = fee;
        }
        let dust = fee.saturating_sub(assigned);
        burn = burn.saturating_add(dust); // unallocated → burn
        self.burned_native = self.burned_native.saturating_add(burn);
        if let Some(meta) = self.assets.get_mut(&native) {
            meta.total_supply = meta.total_supply.saturating_sub(burn);
        }
        if to_treas > 0 {
            let t = self.economics.treasury.clone();
            self.credit(&t, &native, to_treas);
            self.treasury_fees = self.treasury_fees.saturating_add(to_treas);
        }
        true
    }

    /// Next sequence this account must execute (starts at 1).
    pub fn next_sequence(&self, account: &str) -> u64 {
        *self.sequences.get(account).unwrap_or(&1)
    }

    /// Mempool admit: allow current next seq and a bounded window of future sequences.
    /// Does **not** apply state; execution still uses [`can_apply`] (exact sequence only).
    pub fn can_admit(&mut self, tx: &Transaction, window: u64) -> bool {
        self.ensure_account(&tx.account);
        if !tx.destination.is_empty() {
            self.ensure_account(&tx.destination);
        }
        let expected = self.next_sequence(&tx.account);
        let win = window.max(1);
        // past or already consumed
        if tx.sequence < expected {
            return false;
        }
        // too far ahead of open window
        if tx.sequence >= expected.saturating_add(win) {
            return false;
        }
        // Exact next: full economic checks. Future: structural / guard checks only
        // (optimistic balance — apply will re-check when the sequence is due).
        if tx.sequence == expected {
            return self.can_apply_body(tx);
        }
        self.can_admit_future(tx)
    }

    /// Full apply gate: exact next sequence + full economic checks.
    pub fn can_apply(&mut self, tx: &Transaction) -> bool {
        self.ensure_account(&tx.account);
        if !tx.destination.is_empty() {
            self.ensure_account(&tx.destination);
        }
        let seq = self.next_sequence(&tx.account);
        if seq != tx.sequence {
            return false;
        }
        self.can_apply_body(tx)
    }

    /// Future-sequence admit: sealed accounts, rail rules, etc. without exact balance path.
    fn can_admit_future(&self, tx: &Transaction) -> bool {
        let pol = self.policy(&tx.account);
        if Self::is_value_out_kind(tx.kind) && !self.guard_allows_value_out(tx) {
            return false;
        }
        if pol.hard_locked
            && matches!(
                tx.kind,
                TxKind::OpenAccountType
                    | TxKind::SetAllowlist
                    | TxKind::VaultLock
                    | TxKind::SetLimits
            )
        {
            return false;
        }
        // Protocol rails: still no free Issue/Mint/Burn via window
        match tx.kind {
            TxKind::Issue | TxKind::Mint | TxKind::Burn => {
                if is_protocol_rail(&tx.asset) {
                    return false;
                }
            }
            TxKind::Payment | TxKind::Transfer => {
                if is_protocol_rail(&tx.asset) && tx.kind == TxKind::Transfer {
                    // transfer of rail asset is fine if not sealed; payment uses native
                }
            }
            _ => {}
        }
        if Self::is_value_out_kind(tx.kind) && self.is_protocol_account(&tx.account) {
            return false;
        }
        // Must have some native for fee at least (optimistic)
        if tx.fee > 0 && self.native_balance(&tx.account) < tx.fee {
            return false;
        }
        true
    }

    fn can_apply_body(&mut self, tx: &Transaction) -> bool {
        let native = self.economics.native_asset.clone();
        let pol = self.policy(&tx.account);

        // Protocol-enforced destination guard on value-out ops
        if Self::is_value_out_kind(tx.kind) && !self.guard_allows_value_out(tx) {
            return false;
        }

        // Hard-locked vault/pool/lending: block policy mutations
        if pol.hard_locked
            && matches!(
                tx.kind,
                TxKind::OpenAccountType
                    | TxKind::SetAllowlist
                    | TxKind::VaultLock
                    | TxKind::SetLimits
            )
        {
            return false;
        }

        match tx.kind {
            TxKind::Payment => {
                let need = tx.amount.saturating_add(tx.fee);
                self.native_balance(&tx.account) >= need
            }
            TxKind::Transfer => {
                if self.native_balance(&tx.account) < tx.fee {
                    return false;
                }
                if tx.asset == native {
                    self.native_balance(&tx.account) >= tx.amount.saturating_add(tx.fee)
                } else {
                    self.balance(&tx.account, &tx.asset) >= tx.amount
                        && self.native_balance(&tx.account) >= tx.fee
                }
            }
            TxKind::Issue => {
                if tx.asset.is_empty() || tx.asset == native || tx.asset == NATIVE_ASSET {
                    return false;
                }
                // Protocol rails: only SPV deposit mints — no discretionary Issue.
                if is_protocol_rail(&tx.asset) {
                    return false;
                }
                if self.assets.contains_key(&tx.asset) {
                    return false;
                }
                // Issue mints to destination — also a value-out for guard
                self.native_balance(&tx.account) >= tx.fee
            }
            TxKind::Mint => {
                // Protocol rails: issuer is protocol:spv-bridge — no human Mint.
                if is_protocol_rail(&tx.asset) {
                    return false;
                }
                let Some(meta) = self.assets.get(&tx.asset) else {
                    return false;
                };
                if meta.issuer != tx.account {
                    return false;
                }
                if let Some(max) = meta.max_supply {
                    if meta.total_supply.saturating_add(tx.amount) > max {
                        return false;
                    }
                }
                self.native_balance(&tx.account) >= tx.fee
            }
            TxKind::Burn => {
                // Rail assets only burn via RailWithdraw (opens external claim note).
                if is_protocol_rail(&tx.asset) {
                    return false;
                }
                // Protocol accounts cannot burn reserves via key (no discretionary destruction).
                if matches!(
                    pol.account_type,
                    AccountType::Pool | AccountType::LendingMarket
                ) {
                    return false;
                }
                if self.native_balance(&tx.account) < tx.fee {
                    return false;
                }
                if tx.asset == native {
                    self.native_balance(&tx.account) >= tx.amount.saturating_add(tx.fee)
                } else {
                    self.balance(&tx.account, &tx.asset) >= tx.amount
                }
            }
            TxKind::OpenAccountType => {
                let Some(new_ty) = tx.account_type else {
                    return false;
                };
                // Only Hot can convert
                if pol.account_type != AccountType::Hot {
                    return false;
                }
                if matches!(new_ty, AccountType::Hot) {
                    return false;
                }
                self.native_balance(&tx.account) >= tx.fee
            }
            TxKind::SetAllowlist => {
                if tx.destinations.is_empty() || tx.destinations.len() > VAULT_MAX_DESTINATIONS {
                    return false;
                }
                let mut seen = std::collections::BTreeSet::new();
                for d in &tx.destinations {
                    if d.is_empty() || !seen.insert(d) {
                        return false;
                    }
                }
                // Pool/Lending: no user-managed allowlist for free transfers — protocol-only later.
                // Vault/Timed: user-managed until hard_locked.
                match pol.account_type {
                    AccountType::Vault | AccountType::Timed => {
                        if pol.hard_locked {
                            return false;
                        }
                    }
                    AccountType::Pool | AccountType::LendingMarket | AccountType::Hot => {
                        return false;
                    }
                }
                self.native_balance(&tx.account) >= tx.fee
            }
            TxKind::VaultLock => {
                // Only Vault/Timed hard-lock via user key. Pool/Lending are protocol-sealed at open.
                match pol.account_type {
                    AccountType::Vault | AccountType::Timed => {
                        if pol.hard_locked || pol.allowlist.is_empty() {
                            return false;
                        }
                    }
                    AccountType::Hot | AccountType::Pool | AccountType::LendingMarket => {
                        return false;
                    }
                }
                self.native_balance(&tx.account) >= tx.fee
            }
            TxKind::SetLimits => {
                if pol.account_type != AccountType::Timed || pol.hard_locked {
                    return false;
                }
                self.native_balance(&tx.account) >= tx.fee
            }
            // --- Protocol AMM / Lending (user pays fee; pool moves by rules only) ---
            TxKind::CreatePool => self.can_create_pool(tx),
            TxKind::AddLiquidity => self.can_add_liquidity(tx),
            TxKind::RemoveLiquidity => self.can_remove_liquidity(tx),
            TxKind::Swap => self.can_swap(tx),
            TxKind::CreateMarket => self.can_create_market(tx),
            TxKind::Supply => self.can_supply(tx),
            TxKind::WithdrawSupply => self.can_withdraw_supply(tx),
            TxKind::Borrow => self.can_borrow(tx),
            TxKind::Repay => self.can_repay(tx),
            TxKind::Liquidate => self.can_liquidate(tx),
            TxKind::OracleVote => self.can_oracle_vote(tx),
            TxKind::SubmitRailHeader => self.can_submit_rail_header(tx),
            TxKind::RailDeposit => self.can_rail_deposit(tx),
            TxKind::RailWithdraw => self.can_rail_withdraw(tx),
        }
    }

    fn can_submit_rail_header(&self, tx: &Transaction) -> bool {
        let Some(rail) = self.rails.get(&tx.asset) else {
            return false;
        };
        let Some(p) = tx.bridge.as_ref() else {
            return false;
        };
        if p.block_hash.is_empty() || p.merkle_root.is_empty() {
            return false;
        }
        let work = p.work_u128();
        if work == 0 {
            return false;
        }
        if rail.tip_hash.is_empty() {
            // genesis header for this rail: height may be any, often 1
            return self.native_balance(&tx.account) >= tx.fee;
        }
        // extension: parent must match tip, height tip+1, work non-decreasing
        p.parent_hash == rail.tip_hash
            && p.height == rail.tip_height.saturating_add(1)
            && work >= rail.tip_work
            && self.native_balance(&tx.account) >= tx.fee
    }

    fn can_rail_deposit(&self, tx: &Transaction) -> bool {
        let Some(rail) = self.rails.get(&tx.asset) else {
            return false;
        };
        let Some(p) = tx.bridge.as_ref() else {
            return false;
        };
        if tx.amount == 0 || self.native_balance(&tx.account) < tx.fee {
            return false;
        }
        let headers = self.rail_headers.get(&tx.asset).cloned().unwrap_or_default();
        let spent = self
            .rail_spent_deposits
            .get(&tx.asset)
            .cloned()
            .unwrap_or_default();
        verify_deposit(rail, &headers, p, tx.amount, &spent)
    }

    fn can_rail_withdraw(&self, tx: &Transaction) -> bool {
        if !self.rails.contains_key(&tx.asset) {
            return false;
        }
        let Some(p) = tx.bridge.as_ref() else {
            return false;
        };
        tx.amount > 0
            && !p.external_to.is_empty()
            && self.balance(&tx.account, &tx.asset) >= tx.amount
            && self.native_balance(&tx.account) >= tx.fee
    }

    fn can_create_pool(&self, tx: &Transaction) -> bool {
        let pool = &tx.destination;
        if self.policy(pool).account_type != AccountType::Pool {
            return false;
        }
        if self.pools.contains_key(pool) {
            return false;
        }
        let Some(ref b) = tx.asset_b else {
            return false;
        };
        let amount_b = tx.amount_b.unwrap_or(0);
        if tx.asset == *b || tx.amount == 0 || amount_b == 0 {
            return false;
        }
        self.native_balance(&tx.account) >= tx.fee
            && self.balance(&tx.account, &tx.asset) >= tx.amount
            && self.balance(&tx.account, b) >= amount_b
    }

    fn can_add_liquidity(&self, tx: &Transaction) -> bool {
        let Some(p) = self.pools.get(&tx.destination) else {
            return false;
        };
        let amount_b = tx.amount_b.unwrap_or(0);
        if tx.amount == 0 || amount_b == 0 {
            return false;
        }
        let ra = self.balance(&tx.destination, &p.asset_a);
        let rb = self.balance(&tx.destination, &p.asset_b);
        if ra == 0 || rb == 0 {
            return false;
        }
        // proportional: amount_b / amount_a ≈ rb / ra
        let expect_b = (tx.amount as u128 * rb as u128) / ra as u128;
        if amount_b as u128 + 1 < expect_b && expect_b > 0 {
            // allow 1 unit slack the other way only if over-provided is ok — require >=
        }
        if (amount_b as u128) < expect_b.saturating_sub(1) {
            return false;
        }
        self.native_balance(&tx.account) >= tx.fee
            && self.balance(&tx.account, &p.asset_a) >= tx.amount
            && self.balance(&tx.account, &p.asset_b) >= amount_b
    }

    fn can_remove_liquidity(&self, tx: &Transaction) -> bool {
        let Some(p) = self.pools.get(&tx.destination) else {
            return false;
        };
        let lp = self.balance(&tx.account, &p.lp_asset);
        if tx.amount == 0 || lp < tx.amount {
            return false;
        }
        self.native_balance(&tx.account) >= tx.fee
    }

    fn can_swap(&self, tx: &Transaction) -> bool {
        let Some(p) = self.pools.get(&tx.destination) else {
            return false;
        };
        if tx.amount == 0 {
            return false;
        }
        if tx.asset != p.asset_a && tx.asset != p.asset_b {
            return false;
        }
        let (rin, rout) = if tx.asset == p.asset_a {
            (
                self.balance(&tx.destination, &p.asset_a),
                self.balance(&tx.destination, &p.asset_b),
            )
        } else {
            (
                self.balance(&tx.destination, &p.asset_b),
                self.balance(&tx.destination, &p.asset_a),
            )
        };
        if rin == 0 || rout == 0 {
            return false;
        }
        let skim = ((tx.amount as u128)
            * (p.fee_bps as u128)
            * (p.protocol_fee_share_bps as u128)
            / 100_000_000u128) as u64;
        let amount_for_curve = tx.amount.saturating_sub(skim);
        if amount_for_curve == 0 {
            return false;
        }
        let out = swap_out(amount_for_curve, rin, rout, p.fee_bps);
        let min_out = tx.min_out.unwrap_or(0);
        if out < min_out || out == 0 {
            return false;
        }
        self.native_balance(&tx.account) >= tx.fee
            && self.balance(&tx.account, &tx.asset) >= tx.amount
    }

    fn can_create_market(&self, tx: &Transaction) -> bool {
        let m = &tx.destination;
        if self.policy(m).account_type != AccountType::LendingMarket {
            return false;
        }
        if self.markets.contains_key(m) {
            return false;
        }
        !tx.asset.is_empty() && self.native_balance(&tx.account) >= tx.fee
    }

    fn can_oracle_vote(&self, tx: &Transaction) -> bool {
        tx.amount > 0
            && !tx.asset.is_empty()
            && self.native_balance(&tx.account) >= tx.fee
    }

    fn can_supply(&self, tx: &Transaction) -> bool {
        let Some(m) = self.markets.get(&tx.destination) else {
            return false;
        };
        tx.amount > 0
            && self.native_balance(&tx.account) >= tx.fee
            && self.balance(&tx.account, &m.asset) >= tx.amount
    }

    fn can_withdraw_supply(&self, tx: &Transaction) -> bool {
        let Some(m) = self.markets.get(&tx.destination) else {
            return false;
        };
        // amount is underlying to withdraw; check shares cover after index
        let underlying = self.supply_underlying(&tx.account, &tx.destination);
        tx.amount > 0
            && underlying >= tx.amount
            && self.balance(&tx.destination, &m.asset) >= tx.amount
            && self.native_balance(&tx.account) >= tx.fee
    }

    fn can_borrow(&self, tx: &Transaction) -> bool {
        let Some(m) = self.markets.get(&tx.destination) else {
            return false;
        };
        let coll_amt = tx.amount_b.unwrap_or(0);
        if tx.amount == 0 || coll_amt == 0 {
            return false;
        }
        let coll_asset = tx
            .asset_b
            .clone()
            .unwrap_or_else(|| self.economics.native_asset.clone());
        // allowed collateral list (empty → native only)
        let allowed = if m.allowed_collateral.is_empty() {
            vec![self.economics.native_asset.clone()]
        } else {
            m.allowed_collateral.clone()
        };
        if !allowed.iter().any(|a| a == &coll_asset) {
            return false;
        }
        // existing + new collateral value vs existing + new debt (oracle-priced)
        let new_coll_val = self.asset_value_native(&coll_asset, coll_amt);
        let coll_val = self
            .collateral_value_native(&tx.account, &tx.destination)
            .saturating_add(new_coll_val);
        let existing_debt = self.borrow_underlying(&tx.account, &tx.destination);
        let new_debt_val = self
            .asset_value_native(&m.asset, existing_debt.saturating_add(tx.amount));
        let max_debt = coll_val.saturating_mul(m.ltv_bps as u64) / 10_000;
        if new_debt_val > max_debt {
            return false;
        }
        let fee_ok = if coll_asset == self.economics.native_asset {
            self.native_balance(&tx.account) >= tx.fee.saturating_add(coll_amt)
        } else {
            self.native_balance(&tx.account) >= tx.fee
                && self.balance(&tx.account, &coll_asset) >= coll_amt
        };
        fee_ok && self.balance(&tx.destination, &m.asset) >= tx.amount
    }

    fn can_repay(&self, tx: &Transaction) -> bool {
        let Some(m) = self.markets.get(&tx.destination) else {
            return false;
        };
        let bor = self.borrow_underlying(&tx.account, &tx.destination);
        // Partial or full repay; amount may exceed debt (extra is not pulled — apply caps).
        tx.amount > 0
            && bor > 0
            && self.balance(&tx.account, &m.asset) >= tx.amount.min(bor)
            && self.native_balance(&tx.account) >= tx.fee
    }

    fn can_liquidate(&self, tx: &Transaction) -> bool {
        let Some(m) = self.markets.get(&tx.destination) else {
            return false;
        };
        let borrower = tx.destinations.first().map(|s| s.as_str()).unwrap_or("");
        if borrower.is_empty() {
            return false;
        }
        let bor = self.borrow_underlying(borrower, &tx.destination);
        let coll_val = self.collateral_value_native(borrower, &tx.destination);
        if bor == 0 || coll_val == 0 || tx.amount == 0 || tx.amount > bor {
            return false;
        }
        let debt_val = self.asset_value_native(&m.asset, bor);
        let max_debt = coll_val.saturating_mul(m.ltv_bps as u64) / 10_000;
        if debt_val <= max_debt {
            return false; // healthy
        }
        self.balance(&tx.account, &m.asset) >= tx.amount
            && self.native_balance(&tx.account) >= tx.fee
    }

    pub fn apply(&mut self, tx: &Transaction) -> bool {
        if !self.can_apply(tx) {
            return false;
        }
        let native = self.economics.native_asset.clone();
        match tx.kind {
            TxKind::Payment => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                if !self.debit(&tx.account, &native, tx.amount) {
                    return false;
                }
                self.credit(&tx.destination, &native, tx.amount);
            }
            TxKind::Transfer => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                if !self.debit(&tx.account, &tx.asset, tx.amount) {
                    return false;
                }
                self.credit(&tx.destination, &tx.asset, tx.amount);
            }
            TxKind::Issue => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                self.assets.insert(
                    tx.asset.clone(),
                    AssetMeta {
                        issuer: tx.account.clone(),
                        total_supply: tx.amount,
                        max_supply: tx.max_supply,
                    },
                );
                if tx.amount > 0 {
                    self.credit(&tx.destination, &tx.asset, tx.amount);
                }
            }
            TxKind::Mint => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                {
                    let meta = self.assets.get_mut(&tx.asset).unwrap();
                    meta.total_supply = meta.total_supply.saturating_add(tx.amount);
                }
                self.credit(&tx.destination, &tx.asset, tx.amount);
            }
            TxKind::Burn => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                if !self.debit(&tx.account, &tx.asset, tx.amount) {
                    return false;
                }
                if let Some(meta) = self.assets.get_mut(&tx.asset) {
                    meta.total_supply = meta.total_supply.saturating_sub(tx.amount);
                }
                if tx.asset == native {
                    self.burned_native = self.burned_native.saturating_add(tx.amount);
                }
            }
            TxKind::OpenAccountType => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                let ty = tx.account_type.unwrap();
                // Pool/LendingMarket: sealed at open — no key-signed value-out ever.
                // Swap/Supply/Borrow move reserves by protocol rules only.
                let hard = matches!(ty, AccountType::Pool | AccountType::LendingMarket);
                let pol = AccountPolicy {
                    account_type: ty,
                    allowlist: vec![],
                    hard_locked: hard,
                    max_per_tx: 0,
                    max_per_window: 0,
                    window_spent: 0,
                };
                self.policies.insert(tx.account.clone(), pol);
            }
            TxKind::SetAllowlist => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                let mut pol = self.policy(&tx.account);
                pol.allowlist = tx.destinations.clone();
                self.policies.insert(tx.account.clone(), pol);
            }
            TxKind::VaultLock => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                let mut pol = self.policy(&tx.account);
                pol.hard_locked = true;
                self.policies.insert(tx.account.clone(), pol);
            }
            TxKind::SetLimits => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                let mut pol = self.policy(&tx.account);
                pol.max_per_tx = tx.amount;
                pol.max_per_window = tx.max_supply.unwrap_or(0);
                self.policies.insert(tx.account.clone(), pol);
            }
            TxKind::CreatePool => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                let asset_b = tx.asset_b.clone().unwrap();
                let amount_b = tx.amount_b.unwrap();
                let lp = format!("LP-{}", tx.destination);
                if !self.debit(&tx.account, &tx.asset, tx.amount)
                    || !self.debit(&tx.account, &asset_b, amount_b)
                {
                    return false;
                }
                // Protocol credit into sealed pool (not a free payment from pool key).
                self.credit(&tx.destination, &tx.asset, tx.amount);
                self.credit(&tx.destination, &asset_b, amount_b);
                let lp_amt = isqrt(tx.amount.saturating_mul(amount_b)).max(1);
                self.assets.insert(
                    lp.clone(),
                    AssetMeta {
                        issuer: format!("pool:{}", tx.destination),
                        total_supply: lp_amt,
                        max_supply: None,
                    },
                );
                self.credit(&tx.account, &lp, lp_amt);
                self.pools.insert(
                    tx.destination.clone(),
                    PoolConfig {
                        asset_a: tx.asset.clone(),
                        asset_b,
                        lp_asset: lp,
                        fee_bps: AMM_DEFAULT_FEE_BPS,
                        protocol_fee_share_bps: AMM_PROTOCOL_FEE_SHARE_BPS,
                    },
                );
            }
            TxKind::AddLiquidity => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                let p = self.pools.get(&tx.destination).unwrap().clone();
                let amount_b = tx.amount_b.unwrap();
                let ra = self.balance(&tx.destination, &p.asset_a);
                let rb = self.balance(&tx.destination, &p.asset_b);
                let lp_supply = self.total_supply(&p.lp_asset);
                let lp_mint = if lp_supply == 0 {
                    isqrt(tx.amount.saturating_mul(amount_b)).max(1)
                } else {
                    let la = (tx.amount as u128 * lp_supply as u128) / ra as u128;
                    let lb = (amount_b as u128 * lp_supply as u128) / rb as u128;
                    la.min(lb) as u64
                };
                if lp_mint == 0 {
                    return false;
                }
                if !self.debit(&tx.account, &p.asset_a, tx.amount)
                    || !self.debit(&tx.account, &p.asset_b, amount_b)
                {
                    return false;
                }
                self.credit(&tx.destination, &p.asset_a, tx.amount);
                self.credit(&tx.destination, &p.asset_b, amount_b);
                if let Some(meta) = self.assets.get_mut(&p.lp_asset) {
                    meta.total_supply = meta.total_supply.saturating_add(lp_mint);
                }
                self.credit(&tx.account, &p.lp_asset, lp_mint);
            }
            TxKind::RemoveLiquidity => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                let p = self.pools.get(&tx.destination).unwrap().clone();
                let lp_supply = self.total_supply(&p.lp_asset);
                let ra = self.balance(&tx.destination, &p.asset_a);
                let rb = self.balance(&tx.destination, &p.asset_b);
                let out_a = (tx.amount as u128 * ra as u128 / lp_supply as u128) as u64;
                let out_b = (tx.amount as u128 * rb as u128 / lp_supply as u128) as u64;
                let min_a = tx.min_out.unwrap_or(0);
                let min_b = tx.amount_b.unwrap_or(0);
                if out_a < min_a || out_b < min_b {
                    return false;
                }
                if !self.debit(&tx.account, &p.lp_asset, tx.amount) {
                    return false;
                }
                if let Some(meta) = self.assets.get_mut(&p.lp_asset) {
                    meta.total_supply = meta.total_supply.saturating_sub(tx.amount);
                }
                if !self.debit(&tx.destination, &p.asset_a, out_a)
                    || !self.debit(&tx.destination, &p.asset_b, out_b)
                {
                    return false;
                }
                self.credit(&tx.account, &p.asset_a, out_a);
                self.credit(&tx.account, &p.asset_b, out_b);
            }
            TxKind::Swap => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                let p = self.pools.get(&tx.destination).unwrap().clone();
                let (rin_asset, rout_asset) = if tx.asset == p.asset_a {
                    (p.asset_a.clone(), p.asset_b.clone())
                } else {
                    (p.asset_b.clone(), p.asset_a.clone())
                };
                let rin = self.balance(&tx.destination, &rin_asset);
                let rout = self.balance(&tx.destination, &rout_asset);
                // Protocol fee skim: share of the trading fee → sealed treasury (not pool key).
                // amount * fee_bps * share_bps / 1e8 (floor; 0 on tiny swaps is OK).
                let skim = ((tx.amount as u128)
                    * (p.fee_bps as u128)
                    * (p.protocol_fee_share_bps as u128)
                    / 100_000_000u128) as u64;
                let amount_for_curve = tx.amount.saturating_sub(skim);
                // Curve still applies full fee_bps on the LP-bound input (Uniswap-style).
                let out = swap_out(amount_for_curve, rin, rout, p.fee_bps);
                if !self.debit(&tx.account, &tx.asset, tx.amount) {
                    return false;
                }
                if skim > 0 {
                    let treas = self.economics.treasury.clone();
                    self.credit(&treas, &rin_asset, skim);
                    self.protocol_fees_skimmed = self.protocol_fees_skimmed.saturating_add(skim);
                }
                self.credit(&tx.destination, &rin_asset, amount_for_curve);
                if !self.debit(&tx.destination, &rout_asset, out) {
                    return false;
                }
                self.credit(&tx.account, &rout_asset, out);
            }
            TxKind::CreateMarket => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                let native = self.economics.native_asset.clone();
                // Optional collateral allowlist via destinations[]; empty → native only.
                let mut allowed = tx.destinations.clone();
                if allowed.is_empty() {
                    allowed.push(native);
                }
                self.markets.insert(
                    tx.destination.clone(),
                    MarketConfig {
                        asset: tx.asset.clone(),
                        ltv_bps: LENDING_DEFAULT_LTV_BPS,
                        liq_bonus_bps: LENDING_LIQ_BONUS_BPS,
                        supply_index: INDEX_ONE,
                        borrow_index: INDEX_ONE,
                        last_accrual_txs: self.applied_user_txs,
                        base_rate_bps: LENDING_BASE_RATE_BPS,
                        slope_bps: LENDING_SLOPE_BPS,
                        allowed_collateral: allowed,
                    },
                );
            }
            TxKind::OracleVote => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                self.oracle_votes
                    .entry(tx.asset.clone())
                    .or_default()
                    .insert(tx.account.clone(), tx.amount);
                self.recompute_oracle_median(&tx.asset);
            }
            TxKind::SubmitRailHeader => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                let p = tx.bridge.as_ref().unwrap().clone();
                let hdr = RailHeader {
                    height: p.height,
                    hash: p.block_hash.clone(),
                    parent_hash: p.parent_hash.clone(),
                    merkle_root: p.merkle_root.clone(),
                    tx_root: p.tx_root.clone(),
                    work: p.work_u128(),
                };
                let store = self.rail_headers.entry(tx.asset.clone()).or_default();
                store.insert(hdr.height, hdr.clone());
                // prune old headers
                if store.len() > HEADER_HISTORY {
                    let cutoff = hdr.height.saturating_sub(HEADER_HISTORY as u64);
                    store.retain(|h, _| *h > cutoff);
                }
                if let Some(rail) = self.rails.get_mut(&tx.asset) {
                    rail.tip_height = hdr.height;
                    rail.tip_hash = hdr.hash;
                    rail.tip_work = hdr.work;
                }
            }
            TxKind::RailDeposit => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                let p = tx.bridge.as_ref().unwrap().clone();
                // mint rail asset to destination (protocol issuer only path)
                self.credit(&tx.destination, &tx.asset, tx.amount);
                if let Some(meta) = self.assets.get_mut(&tx.asset) {
                    meta.total_supply = meta.total_supply.saturating_add(tx.amount);
                }
                self.rail_spent_deposits
                    .entry(tx.asset.clone())
                    .or_default()
                    .insert(p.external_txid.clone());
                if let Some(rail) = self.rails.get_mut(&tx.asset) {
                    rail.total_minted = rail.total_minted.saturating_add(tx.amount);
                }
                // mirror on sealed bridge account for accounting (not spendable via payment)
                let ba = bridge_account(&tx.asset);
                // track liability: bridge vault holds memo balance of outstanding claims
                self.credit(&ba, &tx.asset, 0); // ensure account
                let _ = ba;
            }
            TxKind::RailWithdraw => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                let external_to = tx
                    .bridge
                    .as_ref()
                    .map(|p| p.external_to.clone())
                    .unwrap_or_default();
                if !self.debit(&tx.account, &tx.asset, tx.amount) {
                    return false;
                }
                if let Some(meta) = self.assets.get_mut(&tx.asset) {
                    meta.total_supply = meta.total_supply.saturating_sub(tx.amount);
                }
                if let Some(rail) = self.rails.get_mut(&tx.asset) {
                    rail.total_burned = rail.total_burned.saturating_add(tx.amount);
                }
                let note_id = sha256_hex(format!(
                    "wd:{}:{}:{}:{}:{}",
                    tx.asset, tx.account, tx.amount, external_to, self.applied_user_txs
                ));
                self.rail_withdrawals.insert(
                    note_id.clone(),
                    WithdrawalNote {
                        note_id,
                        rail: tx.asset.clone(),
                        amount: tx.amount,
                        from: tx.account.clone(),
                        external_to,
                        falcon_height: self.applied_user_txs,
                    },
                );
                // Protocol auto-commit: claim root is pure function of open notes (no committee).
                self.commit_claim_root(&tx.asset);
            }
            TxKind::Supply => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                self.accrue_market(&tx.destination);
                let m = self.markets.get(&tx.destination).unwrap().clone();
                if !self.debit(&tx.account, &m.asset, tx.amount) {
                    return false;
                }
                self.credit(&tx.destination, &m.asset, tx.amount);
                let shares = ((tx.amount as u128) * (INDEX_ONE as u128)
                    / (m.supply_index.max(1) as u128)) as u64;
                let shares = shares.max(1);
                *self
                    .supplied
                    .entry(tx.account.clone())
                    .or_default()
                    .entry(tx.destination.clone())
                    .or_insert(0) += shares;
            }
            TxKind::WithdrawSupply => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                self.accrue_market(&tx.destination);
                let m = self.markets.get(&tx.destination).unwrap().clone();
                let shares_burn = ((tx.amount as u128) * (INDEX_ONE as u128)
                    / (m.supply_index.max(1) as u128)) as u64;
                let shares_burn = shares_burn.max(1);
                let e = self
                    .supplied
                    .entry(tx.account.clone())
                    .or_default()
                    .entry(tx.destination.clone())
                    .or_insert(0);
                if *e < shares_burn {
                    return false;
                }
                *e = e.saturating_sub(shares_burn);
                if !self.debit(&tx.destination, &m.asset, tx.amount) {
                    return false;
                }
                self.credit(&tx.account, &m.asset, tx.amount);
            }
            TxKind::Borrow => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                self.accrue_market(&tx.destination);
                let coll_amt = tx.amount_b.unwrap();
                let coll_asset = tx
                    .asset_b
                    .clone()
                    .unwrap_or_else(|| self.economics.native_asset.clone());
                let m = self.markets.get(&tx.destination).unwrap().clone();
                if !self.debit(&tx.account, &coll_asset, coll_amt) {
                    return false;
                }
                // collateral held on market account (protocol-tracked; no free spend)
                self.credit(&tx.destination, &coll_asset, coll_amt);
                *self
                    .collateral
                    .entry(tx.account.clone())
                    .or_default()
                    .entry(tx.destination.clone())
                    .or_default()
                    .entry(coll_asset)
                    .or_insert(0) += coll_amt;
                if !self.debit(&tx.destination, &m.asset, tx.amount) {
                    return false;
                }
                self.credit(&tx.account, &m.asset, tx.amount);
                let debt_shares = ((tx.amount as u128) * (INDEX_ONE as u128)
                    / (m.borrow_index.max(1) as u128)) as u64;
                let debt_shares = debt_shares.max(1);
                *self
                    .borrowed
                    .entry(tx.account.clone())
                    .or_default()
                    .entry(tx.destination.clone())
                    .or_insert(0) += debt_shares;
            }
            TxKind::Repay => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                self.accrue_market(&tx.destination);
                let m = self.markets.get(&tx.destination).unwrap().clone();
                let debt = self.borrow_underlying(&tx.account, &tx.destination);
                let repay_amt = tx.amount.min(debt);
                if repay_amt == 0 {
                    return false;
                }
                if !self.debit(&tx.account, &m.asset, repay_amt) {
                    return false;
                }
                self.credit(&tx.destination, &m.asset, repay_amt);
                let e = self
                    .borrowed
                    .entry(tx.account.clone())
                    .or_default()
                    .entry(tx.destination.clone())
                    .or_insert(0);
                if repay_amt >= debt {
                    // Full repay: clear shares (avoids index dust)
                    *e = 0;
                } else {
                    let debt_shares_burn = ((repay_amt as u128) * (INDEX_ONE as u128)
                        / (m.borrow_index.max(1) as u128)) as u64;
                    *e = e.saturating_sub(debt_shares_burn);
                }
                // release all collateral when debt fully cleared
                if *e == 0 {
                    if let Some(by_asset) = self
                        .collateral
                        .entry(tx.account.clone())
                        .or_default()
                        .remove(&tx.destination)
                    {
                        for (asset, amt) in by_asset {
                            if amt > 0 {
                                let _ = self.debit(&tx.destination, &asset, amt);
                                self.credit(&tx.account, &asset, amt);
                            }
                        }
                    }
                }
            }
            TxKind::Liquidate => {
                if !self.pay_fee(&tx.account, tx.fee) {
                    return false;
                }
                self.accrue_market(&tx.destination);
                let m = self.markets.get(&tx.destination).unwrap().clone();
                let borrower = tx.destinations[0].clone();
                let bor = self.borrow_underlying(&borrower, &tx.destination);
                let coll_map = self
                    .collateral
                    .get(&borrower)
                    .and_then(|x| x.get(&tx.destination))
                    .cloned()
                    .unwrap_or_default();
                if bor == 0 || coll_map.is_empty() {
                    return false;
                }
                // Seize proportional share of each collateral asset for repay/bor
                let frac_num = tx.amount as u128;
                let frac_den = bor as u128;
                if !self.debit(&tx.account, &m.asset, tx.amount) {
                    return false;
                }
                self.credit(&tx.destination, &m.asset, tx.amount);
                let debt_shares_burn = ((tx.amount as u128) * (INDEX_ONE as u128)
                    / (m.borrow_index.max(1) as u128)) as u64;
                let be = self
                    .borrowed
                    .entry(borrower.clone())
                    .or_default()
                    .entry(tx.destination.clone())
                    .or_insert(0);
                *be = be.saturating_sub(debt_shares_burn);
                for (asset, total_coll) in coll_map {
                    let base = (frac_num * total_coll as u128) / frac_den;
                    let bonus = base * m.liq_bonus_bps as u128 / 10_000;
                    let seize = ((base + bonus) as u64).min(total_coll);
                    if seize == 0 {
                        continue;
                    }
                    let ce = self
                        .collateral
                        .entry(borrower.clone())
                        .or_default()
                        .entry(tx.destination.clone())
                        .or_default()
                        .entry(asset.clone())
                        .or_insert(0);
                    *ce = ce.saturating_sub(seize);
                    let _ = self.debit(&tx.destination, &asset, seize);
                    self.credit(&tx.account, &asset, seize);
                }
            }
        }
        // Timed velocity bookkeeping for value-out
        if Self::is_value_out_kind(tx.kind) {
            let mut pol = self.policy(&tx.account);
            if pol.account_type == AccountType::Timed && pol.max_per_window > 0 {
                pol.window_spent = pol.window_spent.saturating_add(tx.amount);
                self.policies.insert(tx.account.clone(), pol);
            }
        }
        *self.sequences.get_mut(&tx.account).unwrap() = tx.sequence + 1;
        self.applied_user_txs = self.applied_user_txs.saturating_add(1);
        true
    }

    /// Apply emission schedule when ordered non-genesis batch count advances.
    pub fn apply_emissions_for_batch_count(&mut self, non_genesis_ordered: u64) {
        if non_genesis_ordered <= self.last_emission_height {
            return;
        }
        let native = self.economics.native_asset.clone();
        let schedule = self.economics.emission_schedule.clone();
        let prev = self.last_emission_height;
        for h in (prev + 1)..=non_genesis_ordered {
            for e in &schedule {
                if e.every_n_batches == 0 || e.amount == 0 {
                    continue;
                }
                if h % e.every_n_batches == 0 {
                    self.ensure_account(&e.to);
                    self.credit(&e.to, &native, e.amount);
                    self.emissions_minted = self.emissions_minted.saturating_add(e.amount);
                    if let Some(meta) = self.assets.get_mut(&native) {
                        meta.total_supply = meta.total_supply.saturating_add(e.amount);
                    }
                }
            }
        }
        self.last_emission_height = non_genesis_ordered;
    }

    /// Deterministic state root for light clients.
    pub fn state_root(&self) -> HashHex {
        let mut leaves = Vec::new();
        for (acct, assets) in &self.balances {
            for (asset, amt) in assets {
                if *amt == 0 {
                    continue;
                }
                leaves.push(sha256_hex(format!("bal:{acct}:{asset}:{amt}")));
            }
        }
        for (acct, seq) in &self.sequences {
            leaves.push(sha256_hex(format!("seq:{acct}:{seq}")));
        }
        for (code, meta) in &self.assets {
            let max = meta
                .max_supply
                .map(|m| m.to_string())
                .unwrap_or_else(|| "none".into());
            leaves.push(sha256_hex(format!(
                "asset:{code}:{}:{}:{max}",
                meta.issuer, meta.total_supply
            )));
        }
        for (acct, pol) in &self.policies {
            let mut al = pol.allowlist.clone();
            al.sort();
            leaves.push(sha256_hex(format!(
                "pol:{acct}:{:?}:{}:{}:{}:{}:{}",
                pol.account_type,
                pol.hard_locked,
                al.join(","),
                pol.max_per_tx,
                pol.max_per_window,
                pol.window_spent
            )));
        }
        for (pool, p) in &self.pools {
            leaves.push(sha256_hex(format!(
                "pool:{pool}:{}:{}:{}:{}:{}",
                p.asset_a, p.asset_b, p.lp_asset, p.fee_bps, p.protocol_fee_share_bps
            )));
        }
        for (mkt, m) in &self.markets {
            leaves.push(sha256_hex(format!(
                "mkt:{mkt}:{}:{}:{}:{}:{}:{}",
                m.asset, m.ltv_bps, m.liq_bonus_bps, m.supply_index, m.borrow_index, m.last_accrual_txs
            )));
        }
        for (asset, price) in &self.oracle_prices {
            leaves.push(sha256_hex(format!("oracle:{asset}:{price}")));
        }
        for (rail, cfg) in &self.rails {
            // claim_root is protocol-committed on RailWithdraw — light clients bind outbound claims here.
            leaves.push(sha256_hex(format!(
                "rail:{rail}:{}:{}:{}:{}:{}:{}:{}:{}",
                cfg.tip_height,
                cfg.tip_hash,
                cfg.tip_work,
                cfg.total_minted,
                cfg.total_burned,
                cfg.claim_root,
                cfg.claim_note_count,
                cfg.claim_height
            )));
        }
        // Open withdrawal notes (sorted) so state_root covers every claim leaf set.
        let mut note_ids: Vec<_> = self.rail_withdrawals.keys().cloned().collect();
        note_ids.sort();
        for nid in note_ids {
            if let Some(n) = self.rail_withdrawals.get(&nid) {
                leaves.push(sha256_hex(format!(
                    "wdnote:{}:{}:{}:{}:{}:{}",
                    n.note_id, n.rail, n.amount, n.from, n.external_to, n.falcon_height
                )));
            }
        }
        leaves.push(sha256_hex(format!(
            "meta:burn:{}:treas:{}:emit:{}:skim:{}",
            self.burned_native, self.treasury_fees, self.emissions_minted, self.protocol_fees_skimmed
        )));
        merkle_root(leaves)
    }

    /// Flat map for snapshots: "acct" -> native balance (compat) plus multi-asset export.
    pub fn export_native_balances(&self) -> BTreeMap<String, u64> {
        let native = &self.economics.native_asset;
        let mut m = BTreeMap::new();
        for (acct, assets) in &self.balances {
            if let Some(b) = assets.get(native) {
                m.insert(acct.clone(), *b);
            }
        }
        m
    }

    pub fn export_all_balances(&self) -> BTreeMap<String, BTreeMap<String, u64>> {
        self.balances.clone()
    }
}

#[derive(Debug)]
pub struct ExecutionEngine {
    pub state: AccountState,
    pub genesis: AccountState,
    pub executed_txs: Vec<HashHex>,
    pub soft_final_txs: std::collections::BTreeSet<HashHex>,
}

impl ExecutionEngine {
    pub fn new(genesis: AccountState) -> Self {
        Self {
            state: genesis.clone(),
            genesis,
            executed_txs: Vec::new(),
            soft_final_txs: std::collections::BTreeSet::new(),
        }
    }

    /// Replay full ordered list of transactions (first valid wins).
    pub fn apply_ordered(&mut self, txs: impl IntoIterator<Item = Transaction>) {
        self.state = self.genesis.clone();
        self.executed_txs.clear();
        let mut seen = std::collections::BTreeSet::new();
        for tx in txs {
            if seen.contains(&tx.tx_id) {
                continue;
            }
            if self.state.apply(&tx) {
                self.executed_txs.push(tx.tx_id.clone());
                seen.insert(tx.tx_id);
            }
        }
    }

    pub fn state_root(&self) -> HashHex {
        self.state.state_root()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fd_types::EconomicConfig;

    fn gen() -> AccountState {
        let mut e = EconomicConfig::default();
        e.genesis_accounts = 3;
        e.genesis_native_per_account = 1_000_000;
        e.fee_burn_bps = 5_000;
        e.fee_treasury_bps = 5_000;
        e.emission_schedule.clear();
        AccountState::genesis_faucet(e)
    }

    #[test]
    fn apply_and_conflict() {
        let mut st = gen();
        let a = Transaction::create("acct1", 1, "acct2", 10, 2, 0.0);
        assert!(st.apply(&a));
        let b = Transaction::create("acct1", 1, "acct3", 10, 1, 1.0);
        assert!(!st.apply(&b));
        // fee 2 → burn 1, treasury 1
        assert_eq!(st.burned_native, 1);
        assert_eq!(st.treasury_fees, 1);
        assert_eq!(st.native_balance("acct2"), 1_000_000 + 10);
    }

    #[test]
    fn issue_transfer_mint_burn_and_supply() {
        let mut st = gen();
        let issue = Transaction::issue("acct1", 1, "USDC", 1_000, "acct1", 10, Some(10_000), 0.0);
        assert!(st.apply(&issue));
        assert_eq!(st.total_supply("USDC"), 1_000);
        assert_eq!(st.balance("acct1", "USDC"), 1_000);

        let xfer = Transaction::transfer("acct1", 2, "acct2", "USDC", 100, 5, 1.0);
        assert!(st.apply(&xfer));
        assert_eq!(st.balance("acct2", "USDC"), 100);

        let mint = Transaction::mint("acct1", 3, "USDC", "acct2", 50, 5, 2.0);
        assert!(st.apply(&mint));
        assert_eq!(st.total_supply("USDC"), 1_050);

        let burn = Transaction::burn("acct2", 1, "USDC", 20, 5, 3.0);
        assert!(st.apply(&burn));
        assert_eq!(st.total_supply("USDC"), 1_030);
        assert_eq!(st.balance("acct2", "USDC"), 130);

        let root1 = st.state_root();
        let root2 = st.state_root();
        assert_eq!(root1, root2);
        assert_eq!(root1.len(), 64);
    }

    #[test]
    fn mint_respects_max_supply() {
        let mut st = gen();
        let issue = Transaction::issue("acct1", 1, "CAP", 100, "acct1", 1, Some(100), 0.0);
        assert!(st.apply(&issue));
        let mint = Transaction::mint("acct1", 2, "CAP", "acct1", 1, 1, 1.0);
        assert!(!st.can_apply(&mint));
    }

    #[test]
    fn vault_permanent_destination_lock() {
        use fd_types::AccountType;
        let mut st = gen();
        // open vault
        let open = Transaction::open_account_type("acct1", 1, AccountType::Vault, 1, 0.0);
        assert!(st.apply(&open));
        assert_eq!(st.policy("acct1").account_type, AccountType::Vault);
        // cannot send while empty allowlist
        let pay = Transaction::payment("acct1", 2, "acct2", 10, 1, 1.0);
        assert!(!st.can_apply(&pay));
        // set allowlist: only acct3
        let set = Transaction::set_allowlist("acct1", 2, vec!["acct3"], 1, 2.0);
        assert!(st.apply(&set));
        // can send to acct3 before lock
        let pay3 = Transaction::payment("acct1", 3, "acct3", 10, 1, 3.0);
        assert!(st.can_apply(&pay3));
        // cannot send to acct2
        let pay2 = Transaction::payment("acct1", 3, "acct2", 10, 1, 3.0);
        assert!(!st.can_apply(&pay2));
        // permanent lock
        let lock = Transaction::vault_lock("acct1", 3, 1, 4.0);
        assert!(st.apply(&lock));
        assert!(st.policy("acct1").hard_locked);
        // still only acct3
        let pay3b = Transaction::payment("acct1", 4, "acct3", 5, 1, 5.0);
        assert!(st.apply(&pay3b));
        let pay2b = Transaction::payment("acct1", 5, "acct2", 5, 1, 6.0);
        assert!(!st.can_apply(&pay2b));
        // cannot change allowlist or unlock
        let set2 = Transaction::set_allowlist("acct1", 5, vec!["acct2"], 1, 7.0);
        assert!(!st.can_apply(&set2));
        let lock2 = Transaction::vault_lock("acct1", 5, 1, 8.0);
        assert!(!st.can_apply(&lock2));
    }

    #[test]
    fn pool_and_lending_no_key_drain() {
        use fd_types::AccountType;
        let mut st = gen();
        let open = Transaction::open_account_type("acct1", 1, AccountType::Pool, 1, 0.0);
        assert!(st.apply(&open));
        assert!(st.policy("acct1").hard_locked);
        // No free payment/transfer/burn/set_allowlist from pool key
        assert!(!st.can_apply(&Transaction::payment("acct1", 2, "acct2", 1, 1, 1.0)));
        assert!(!st.can_apply(&Transaction::transfer(
            "acct1", 2, "acct2", "FDAG", 1, 1, 1.0
        )));
        assert!(!st.can_apply(&Transaction::set_allowlist(
            "acct1",
            2,
            vec!["acct2"],
            1,
            2.0
        )));
        assert!(!st.can_apply(&Transaction::vault_lock("acct1", 2, 1, 2.0)));
        assert!(!st.can_apply(&Transaction::burn("acct1", 2, "FDAG", 1, 1, 2.0)));
        // Pool can still receive (open fee was 1 from acct1)
        assert!(st.apply(&Transaction::payment("acct2", 1, "acct1", 50, 1, 3.0)));
        assert_eq!(st.native_balance("acct1"), 1_000_000 - 1 + 50);

        let open_l = Transaction::open_account_type("acct3", 1, AccountType::LendingMarket, 1, 4.0);
        assert!(st.apply(&open_l));
        assert!(!st.can_apply(&Transaction::payment("acct3", 2, "acct2", 1, 1, 5.0)));
    }

    #[test]
    fn treasury_is_sealed_receive_only() {
        let st = gen();
        let t = st.economics.treasury.clone();
        let pol = st.policy(&t);
        assert_eq!(pol.account_type, AccountType::Vault);
        assert!(pol.hard_locked);
        assert!(pol.allowlist.is_empty());
    }

    #[test]
    fn amm_swap_invariant_and_no_pool_payment() {
        use fd_types::AccountType;
        let mut st = gen();
        // Fresh pool shell with only fee buffer (not a genesis faucet account)
        assert!(st.apply(&Transaction::payment("acct1", 1, "pool1", 100, 1, 0.0)));
        assert!(st.apply(&Transaction::open_account_type(
            "pool1",
            1,
            AccountType::Pool,
            10,
            1.0
        )));
        assert!(!st.can_apply(&Transaction::payment("pool1", 2, "acct1", 1, 1, 1.0)));
        assert!(st.apply(&Transaction::issue(
            "acct1", 2, "GOLD", 50_000, "acct1", 1, None, 2.0
        )));
        assert!(st.apply(&Transaction::create_pool(
            "acct1", 3, "pool1", "FDAG", "GOLD", 10_000, 20_000, 1, 3.0
        )));
        let ra = st.balance("pool1", "FDAG");
        let rb = st.balance("pool1", "GOLD");
        // residual fee buffer may remain on pool1 FDAG
        assert!(ra >= 10_000);
        assert_eq!(rb, 20_000);
        let ra_liq = 10_000u64; // liquidity portion for k check approx
        assert!(st.apply(&Transaction::payment("acct1", 4, "acct2", 5_000, 1, 4.0)));
        let before = st.balance("acct2", "GOLD");
        assert!(st.apply(&Transaction::swap(
            "acct2", 1, "pool1", "FDAG", 1_000, 1, 1, 5.0
        )));
        assert!(st.balance("acct2", "GOLD") > before);
        let ra2 = st.balance("pool1", "FDAG");
        let rb2 = st.balance("pool1", "GOLD");
        // product of reserves should not decrease below pre-swap product of liq legs
        assert!(ra2 as u128 * rb2 as u128 >= (ra_liq as u128) * (rb as u128));
        assert!(!st.can_apply(&Transaction::payment("pool1", 2, "acct1", 100, 1, 6.0)));
    }

    #[test]
    fn lending_supply_borrow_repay() {
        use fd_types::AccountType;
        let mut st = gen();
        assert!(st.apply(&Transaction::payment("acct1", 1, "mkt1", 50, 1, 0.0)));
        assert!(st.apply(&Transaction::open_account_type(
            "mkt1",
            1,
            AccountType::LendingMarket,
            10,
            1.0
        )));
        assert!(st.apply(&Transaction::create_market(
            "acct1", 2, "mkt1", "FDAG", 1, 2.0
        )));
        assert!(st.apply(&Transaction::supply("acct1", 3, "mkt1", 5_000, 1, 3.0)));
        // market holds residual fee dust + supply
        assert!(st.balance("mkt1", "FDAG") >= 5_000);
        assert!(st.apply(&Transaction::borrow(
            "acct2", 1, "mkt1", 1_000, 2_000, 1, 4.0
        )));
        assert!(st.apply(&Transaction::repay("acct2", 2, "mkt1", 1_000, 1, 5.0)));
        assert_eq!(st.borrow_underlying("acct2", "mkt1"), 0);
        assert!(!st.can_apply(&Transaction::payment("mkt1", 2, "acct1", 1, 1, 6.0)));
    }

    #[test]
    fn oracle_median_and_multi_collateral_borrow() {
        use fd_types::AccountType;
        let mut st = gen();
        // GOLD at 2.0 native (2e6)
        assert!(st.apply(&Transaction::oracle_vote(
            "acct1", 1, "GOLD", 2_000_000, 1, 0.0
        )));
        assert_eq!(st.oracle_price("GOLD"), 2_000_000);
        assert!(st.apply(&Transaction::oracle_vote(
            "acct2", 1, "GOLD", 2_200_000, 1, 1.0
        )));
        assert!(st.apply(&Transaction::oracle_vote(
            "acct3", 1, "GOLD", 1_800_000, 1, 2.0
        )));
        // median of 1.8e6, 2.0e6, 2.2e6 = 2.0e6
        assert_eq!(st.oracle_price("GOLD"), 2_000_000);

        assert!(st.apply(&Transaction::issue(
            "acct1", 2, "GOLD", 100_000, "acct1", 1, None, 3.0
        )));
        assert!(st.apply(&Transaction::transfer(
            "acct1", 3, "acct2", "GOLD", 10_000, 1, 4.0
        )));
        assert!(st.apply(&Transaction::payment("acct1", 4, "mkt1", 50, 1, 5.0)));
        assert!(st.apply(&Transaction::open_account_type(
            "mkt1",
            1,
            AccountType::LendingMarket,
            10,
            6.0
        )));
        // create market for SILVER? use FDAG market, allow GOLD collateral
        let mut cm = Transaction::create_market("acct1", 5, "mkt1", "FDAG", 1, 7.0);
        cm.destinations = vec!["FDAG".into(), "GOLD".into()];
        assert!(st.apply(&cm));
        assert!(st.apply(&Transaction::supply("acct1", 6, "mkt1", 20_000, 1, 8.0)));
        // 5000 GOLD * 2.0 = 10000 native value → 50% LTV → max borrow 5000 FDAG
        assert!(st.apply(&Transaction::borrow_with_collateral(
            "acct2", 2, "mkt1", 4_000, "GOLD", 5_000, 1, 9.0
        )));
        assert_eq!(st.collateral_amount("acct2", "mkt1", "GOLD"), 5_000);
        // over LTV rejected
        assert!(!st.can_apply(&Transaction::borrow_with_collateral(
            "acct2", 3, "mkt1", 2_000, "GOLD", 100, 1, 10.0
        )));
        assert!(!st.can_apply(&Transaction::payment("mkt1", 2, "acct1", 1, 1, 11.0)));
    }

    #[test]
    fn interest_accrual_grows_debt() {
        use fd_types::AccountType;
        let mut st = gen();
        assert!(st.apply(&Transaction::payment("acct1", 1, "mkt1", 50, 1, 0.0)));
        assert!(st.apply(&Transaction::open_account_type(
            "mkt1",
            1,
            AccountType::LendingMarket,
            10,
            1.0
        )));
        assert!(st.apply(&Transaction::create_market(
            "acct1", 2, "mkt1", "FDAG", 1, 2.0
        )));
        assert!(st.apply(&Transaction::supply("acct1", 3, "mkt1", 50_000, 1, 3.0)));
        assert!(st.apply(&Transaction::borrow(
            "acct2", 1, "mkt1", 10_000, 25_000, 1, 4.0
        )));
        let debt0 = st.borrow_underlying("acct2", "mkt1");
        // simulate many applied txs then force accrue
        st.applied_user_txs = st.applied_user_txs.saturating_add(1_000);
        st.accrue_market("mkt1");
        let debt1 = st.borrow_underlying("acct2", "mkt1");
        assert!(debt1 > debt0, "debt {debt1} should exceed {debt0}");
        let idx = st.markets.get("mkt1").unwrap().borrow_index;
        assert!(idx > INDEX_ONE);
    }

    #[test]
    fn sequence_window_admits_future_but_apply_is_exact() {
        let mut st = gen();
        // expected = 1: admit 1..5 with window 5, reject 6 and past
        assert!(st.can_admit(&Transaction::create("acct1", 1, "acct2", 1, 10, 0.0), 5));
        assert!(st.can_admit(&Transaction::create("acct1", 5, "acct2", 1, 10, 0.0), 5));
        assert!(!st.can_admit(&Transaction::create("acct1", 6, "acct2", 1, 10, 0.0), 5));
        // cannot apply seq 5 before 1
        assert!(!st.can_apply(&Transaction::create("acct1", 5, "acct2", 1, 10, 0.0)));
        assert!(st.apply(&Transaction::create("acct1", 1, "acct2", 1, 10, 0.0)));
        assert_eq!(st.next_sequence("acct1"), 2);
        // after applying 1, admit window is 2..6
        assert!(!st.can_admit(&Transaction::create("acct1", 1, "acct2", 1, 10, 1.0), 5)); // past
        assert!(st.can_admit(&Transaction::create("acct1", 2, "acct2", 1, 10, 1.0), 5));
        assert!(st.can_admit(&Transaction::create("acct1", 6, "acct2", 1, 10, 1.0), 5));
        assert!(!st.can_admit(&Transaction::create("acct1", 7, "acct2", 1, 10, 1.0), 5));
        // free Issue rail still blocked even as "future"
        assert!(!st.can_admit(
            &Transaction::issue("acct1", 2, "BTC", 1, "acct1", 1, None, 0.0),
            5
        ));
    }

    #[test]
    fn spv_rail_deposit_and_no_federated_mint() {
        use crate::bridge::{
            btc_txid_to_internal_leaf, genesis_lock_id, synth_hash32, synthetic_inclusion_bitcoin,
        };
        use fd_types::{BridgeProof, RAIL_BTC};
        let mut st = gen();
        // Cannot Issue/Mint/Burn BTC (protocol rail only via SPV deposit/withdraw)
        assert!(!st.can_apply(&Transaction::issue(
            "acct1", 1, RAIL_BTC, 1, "acct1", 1, None, 0.0
        )));
        assert!(!st.can_apply(&Transaction::mint(
            "acct1", 1, RAIL_BTC, "acct1", 1, 1, 0.0
        )));
        // BTC: merkle uses internal leaf; external_txid is display-order (byte-reversed)
        let leaf = synth_hash32("btc_lock_tx_001");
        let display_txid = btc_txid_to_internal_leaf(&leaf).unwrap();
        let s0 = synth_hash32("s0");
        let s1 = synth_hash32("s1");
        let (path, root) = synthetic_inclusion_bitcoin(&leaf, &[&s0, &s1], 0).unwrap();
        let lock = genesis_lock_id(RAIL_BTC);
        let mut parent = String::new();
        let mut seq = 1u64;
        for h in 1u64..=7 {
            let hash = format!("hdr{h}");
            let proof = BridgeProof {
                height: h,
                block_hash: hash.clone(),
                parent_hash: parent.clone(),
                merkle_root: if h == 1 {
                    root.clone()
                } else {
                    format!("root{h}")
                },
                work: (h * 1000).to_string(),
                ..Default::default()
            };
            assert!(
                st.apply(&Transaction::submit_rail_header(
                    "acct1", seq, RAIL_BTC, proof, 1, h as f64,
                )),
                "header {h}"
            );
            seq += 1;
            parent = hash;
        }
        let dep = BridgeProof {
            external_txid: display_txid.clone(),
            block_hash: "hdr1".into(),
            height: 1,
            merkle_path: path,
            merkle_index: 0,
            lock_id: lock,
            merkle_root: root,
            ..Default::default()
        };
        assert!(st.apply(&Transaction::rail_deposit(
            "acct1", seq, RAIL_BTC, "acct2", 50_000, dep, 1, 10.0
        )));
        assert_eq!(st.balance("acct2", RAIL_BTC), 50_000);
        assert_eq!(st.rails.get(RAIL_BTC).unwrap().total_minted, 50_000);
        let (path2, root2) = synthetic_inclusion_bitcoin(&leaf, &[&s0, &s1], 0).unwrap();
        let dep2 = BridgeProof {
            external_txid: display_txid,
            block_hash: "hdr1".into(),
            height: 1,
            merkle_path: path2,
            merkle_index: 0,
            lock_id: genesis_lock_id(RAIL_BTC),
            merkle_root: root2,
            ..Default::default()
        };
        assert!(!st.can_apply(&Transaction::rail_deposit(
            "acct1",
            seq + 1,
            RAIL_BTC,
            "acct2",
            1,
            dep2,
            1,
            11.0
        )));
        assert!(st.apply(&Transaction::rail_withdraw(
            "acct2", 1, RAIL_BTC, 10_000, "bc1q_test_out", 1, 12.0
        )));
        assert_eq!(st.balance("acct2", RAIL_BTC), 40_000);
        assert_eq!(st.rails.get(RAIL_BTC).unwrap().total_burned, 10_000);
        assert_eq!(st.rail_withdrawals.len(), 1);
        // claim root auto-committed and matches recompute
        let rail = st.rails.get(RAIL_BTC).unwrap();
        assert!(!rail.claim_root.is_empty());
        assert_eq!(rail.claim_note_count, 1);
        let notes = sorted_withdrawals(&st.rail_withdrawals, Some(RAIL_BTC));
        assert_eq!(rail.claim_root, withdrawal_claim_root(&notes));
        // claim_root is bound into state_root (light-client / foreign finality)
        let root1 = st.state_root();
        assert_eq!(root1, st.state_root());
        assert!(!st.can_apply(&Transaction::payment(
            "bridge:BTC", 1, "acct1", 1, 1, 13.0
        )));
        assert!(!st.can_apply(&Transaction::burn(
            "acct2", 2, RAIL_BTC, 1, 1, 14.0
        )));
    }

    #[test]
    fn claim_root_changes_state_root() {
        use crate::bridge::{
            btc_txid_to_internal_leaf, genesis_lock_id, synth_hash32, synthetic_inclusion_bitcoin,
        };
        use fd_types::{BridgeProof, RAIL_BTC};
        let mut st = gen();
        let leaf = synth_hash32("state_root_claim_tx");
        let display_txid = btc_txid_to_internal_leaf(&leaf).unwrap();
        let s0 = synth_hash32("s0");
        let s1 = synth_hash32("s1");
        let (path, root) = synthetic_inclusion_bitcoin(&leaf, &[&s0, &s1], 0).unwrap();
        let lock = genesis_lock_id(RAIL_BTC);
        let mut parent = String::new();
        let mut seq = 1u64;
        for h in 1u64..=7 {
            let hash = format!("hdr{h}");
            assert!(st.apply(&Transaction::submit_rail_header(
                "acct1",
                seq,
                RAIL_BTC,
                BridgeProof {
                    height: h,
                    block_hash: hash.clone(),
                    parent_hash: parent.clone(),
                    merkle_root: if h == 1 { root.clone() } else { format!("root{h}") },
                    work: (h * 1000).to_string(),
                    ..Default::default()
                },
                1,
                h as f64,
            )));
            seq += 1;
            parent = hash;
        }
        assert!(st.apply(&Transaction::rail_deposit(
            "acct1",
            seq,
            RAIL_BTC,
            "acct2",
            1000,
            BridgeProof {
                external_txid: display_txid,
                block_hash: "hdr1".into(),
                height: 1,
                merkle_path: path,
                merkle_index: 0,
                lock_id: lock,
                merkle_root: root,
                ..Default::default()
            },
            1,
            10.0,
        )));
        let before = st.state_root();
        assert!(st.apply(&Transaction::rail_withdraw(
            "acct2", 1, RAIL_BTC, 100, "0xabc", 1, 11.0
        )));
        let after = st.state_root();
        assert_ne!(before, after, "withdraw claim must move state_root");
    }

    #[test]
    fn swap_skims_protocol_fee_to_treasury() {
        use fd_types::AccountType;
        let mut st = gen();
        assert!(st.apply(&Transaction::payment("acct1", 1, "pool1", 100, 1, 0.0)));
        assert!(st.apply(&Transaction::open_account_type(
            "pool1",
            1,
            AccountType::Pool,
            10,
            1.0
        )));
        assert!(st.apply(&Transaction::issue(
            "acct1", 2, "GOLD", 50_000, "acct1", 1, None, 2.0
        )));
        assert!(st.apply(&Transaction::create_pool(
            "acct1", 3, "pool1", "FDAG", "GOLD", 10_000, 20_000, 1, 3.0
        )));
        let treas = st.economics.treasury.clone();
        let before = st.balance(&treas, "FDAG");
        assert!(st.apply(&Transaction::payment("acct1", 4, "acct2", 12_000, 1, 4.0)));
        assert!(st.apply(&Transaction::swap(
            "acct2", 1, "pool1", "FDAG", 10_000, 1, 1, 5.0
        )));
        // trade fee = 10000 * 30/10000 = 30; skim ≈ 30 * 1667/10000 = 5
        assert!(st.protocol_fees_skimmed > 0);
        assert!(st.balance(&treas, "FDAG") > before);
        assert!(!st.can_apply(&Transaction::payment("pool1", 2, "acct1", 1, 1, 6.0)));
    }
}
