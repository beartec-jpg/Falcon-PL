//! Minimal AMM + lending markets for Falcon PL v2.2 private fleet.
//!
//! Constant-product AMM (x*y=k) and fixed-LTV lending with external price.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const NATIVE: &str = "FPL";
/// LTV in bps (5000 = 50%).
pub const DEFAULT_LTV_BPS: u64 = 5_000;
/// Legacy name. Live prices are FPL per one whole coin, divided by `coin_scale`.
pub const PRICE_SCALE: u64 = 1_000_000;

/// Raw units in one whole coin. ETH must use 1e18 or a pool price of ~1e5 FPL stores as 0.
pub fn coin_scale(asset: &str) -> u128 {
    match asset {
        "ETH" => 1_000_000_000_000_000_000,
        "BTC" => 100_000_000,
        "USDC" => 1_000_000,
        _ => PRICE_SCALE as u128,
    }
}

/// FPL for one whole coin, from pool reserves. None when the quote rounds to zero.
pub fn fpl_per_coin(reserve_fpl: u64, reserve_asset: u64, asset: &str) -> Option<u64> {
    if reserve_fpl == 0 || reserve_asset == 0 {
        return None;
    }
    let v = (reserve_fpl as u128).saturating_mul(coin_scale(asset)) / reserve_asset as u128;
    if v == 0 {
        None
    } else {
        Some(v.min(u64::MAX as u128) as u64)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmmPool {
    pub id: String,
    pub asset_a: String,
    pub asset_b: String,
    pub reserve_a: u64,
    pub reserve_b: u64,
    /// LP token balances by account.
    pub lp: BTreeMap<String, u64>,
    pub lp_supply: u64,
}

impl AmmPool {
    pub fn new(id: impl Into<String>, asset_a: impl Into<String>, asset_b: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            asset_a: asset_a.into(),
            asset_b: asset_b.into(),
            reserve_a: 0,
            reserve_b: 0,
            lp: BTreeMap::new(),
            lp_supply: 0,
        }
    }

    /// Bootstrap or add liquidity; mints LP to `provider`.
    pub fn add_liquidity(
        &mut self,
        provider: &str,
        amt_a: u64,
        amt_b: u64,
    ) -> Result<u64, String> {
        if amt_a == 0 || amt_b == 0 {
            return Err("add_liquidity: amounts must be > 0".into());
        }
        let minted = if self.lp_supply == 0 {
            // geometric mean approx via f64 (proto)
            let g = ((amt_a as f64) * (amt_b as f64)).sqrt() as u64;
            g.max(1)
        } else {
            let s1 = (amt_a as u128)
                .saturating_mul(self.lp_supply as u128)
                / self.reserve_a.max(1) as u128;
            let s2 = (amt_b as u128)
                .saturating_mul(self.lp_supply as u128)
                / self.reserve_b.max(1) as u128;
            s1.min(s2) as u64
        };
        if minted == 0 {
            return Err("add_liquidity: minted 0".into());
        }
        self.reserve_a = self.reserve_a.saturating_add(amt_a);
        self.reserve_b = self.reserve_b.saturating_add(amt_b);
        self.lp_supply = self.lp_supply.saturating_add(minted);
        *self.lp.entry(provider.to_string()).or_insert(0) += minted;
        Ok(minted)
    }

    pub fn remove_liquidity(&mut self, provider: &str, lp_burn: u64) -> Result<(u64, u64), String> {
        let have = *self.lp.get(provider).unwrap_or(&0);
        if lp_burn == 0 || lp_burn > have || self.lp_supply == 0 {
            return Err("remove_liquidity: bad lp amount".into());
        }
        let out_a = (self.reserve_a as u128)
            .saturating_mul(lp_burn as u128)
            / self.lp_supply as u128;
        let out_b = (self.reserve_b as u128)
            .saturating_mul(lp_burn as u128)
            / self.lp_supply as u128;
        let out_a = out_a as u64;
        let out_b = out_b as u64;
        self.reserve_a = self.reserve_a.saturating_sub(out_a);
        self.reserve_b = self.reserve_b.saturating_sub(out_b);
        self.lp_supply = self.lp_supply.saturating_sub(lp_burn);
        *self.lp.get_mut(provider).unwrap() -= lp_burn;
        if self.lp[provider] == 0 {
            self.lp.remove(provider);
        }
        Ok((out_a, out_b))
    }

    /// Swap `amount_in` of `token_in` for the other side; returns amount_out.
    pub fn swap(&mut self, token_in: &str, amount_in: u64, min_out: u64) -> Result<u64, String> {
        if amount_in == 0 {
            return Err("swap: zero in".into());
        }
        let (rin, rout, a_side) = if token_in == self.asset_a {
            (self.reserve_a, self.reserve_b, true)
        } else if token_in == self.asset_b {
            (self.reserve_b, self.reserve_a, false)
        } else {
            return Err("swap: unknown asset".into());
        };
        if rin == 0 || rout == 0 {
            return Err("swap: empty pool".into());
        }
        // 0.3% fee: effective in = amount_in * 997/1000
        let amount_in_with_fee = (amount_in as u128).saturating_mul(997);
        let numer = amount_in_with_fee.saturating_mul(rout as u128);
        let denom = (rin as u128).saturating_mul(1000).saturating_add(amount_in_with_fee);
        let out = (numer / denom) as u64;
        if out < min_out {
            return Err(format!("swap: slippage out {out} < min {min_out}"));
        }
        if a_side {
            self.reserve_a = self.reserve_a.saturating_add(amount_in);
            self.reserve_b = self.reserve_b.saturating_sub(out);
        } else {
            self.reserve_b = self.reserve_b.saturating_add(amount_in);
            self.reserve_a = self.reserve_a.saturating_sub(out);
        }
        Ok(out)
    }

    /// LP weights for emission (lp balance).
    pub fn lp_weights(&self) -> BTreeMap<String, u64> {
        self.lp.clone()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LendingMarket {
    pub id: String,
    pub asset: String,
    /// Total supplied of asset.
    pub total_supply: u64,
    /// Total borrowed of asset.
    pub total_borrow: u64,
    /// Supplier shares (m-tokens).
    pub supply_shares: BTreeMap<String, u64>,
    pub share_supply: u64,
    /// Borrower debt of asset.
    pub debt: BTreeMap<String, u64>,
    /// Collateral in native FPL by borrower.
    pub collateral: BTreeMap<String, u64>,
    /// FPL per one whole coin (1 ETH, 1 BTC, 1 USDC). Not per million raw units.
    pub price_native_per_scale: u64,
    pub ltv_bps: u64,
}

impl LendingMarket {
    pub fn new(id: impl Into<String>, asset: impl Into<String>) -> Self {
        let asset = asset.into();
        // 1 raw unit = 1 FPL until a pool price is written. ETH's unit is 1e18 and fits in u64.
        let price = coin_scale(&asset).min(u64::MAX as u128) as u64;
        Self {
            id: id.into(),
            asset,
            total_supply: 0,
            total_borrow: 0,
            supply_shares: BTreeMap::new(),
            share_supply: 0,
            debt: BTreeMap::new(),
            collateral: BTreeMap::new(),
            price_native_per_scale: price.max(1),
            ltv_bps: DEFAULT_LTV_BPS,
        }
    }

    pub fn set_price(&mut self, price_fpl_per_coin: u64) {
        self.price_native_per_scale = price_fpl_per_coin.max(1);
    }

    pub fn supply(&mut self, who: &str, amount: u64) -> Result<u64, String> {
        if amount == 0 {
            return Err("supply zero".into());
        }
        let shares = if self.share_supply == 0 || self.total_supply == 0 {
            amount
        } else {
            ((amount as u128)
                .saturating_mul(self.share_supply as u128)
                / self.total_supply as u128) as u64
        };
        if shares == 0 {
            return Err("supply shares 0".into());
        }
        self.total_supply = self.total_supply.saturating_add(amount);
        self.share_supply = self.share_supply.saturating_add(shares);
        *self.supply_shares.entry(who.to_string()).or_insert(0) += shares;
        Ok(shares)
    }

    pub fn withdraw_supply(&mut self, who: &str, shares: u64) -> Result<u64, String> {
        let have = *self.supply_shares.get(who).unwrap_or(&0);
        if shares == 0 || shares > have || self.share_supply == 0 {
            return Err("withdraw bad shares".into());
        }
        let amount = (self.total_supply as u128)
            .saturating_mul(shares as u128)
            / self.share_supply as u128;
        let amount = amount as u64;
        let available = self.total_supply.saturating_sub(self.total_borrow);
        if amount > available {
            return Err("withdraw: utilization too high".into());
        }
        self.total_supply = self.total_supply.saturating_sub(amount);
        self.share_supply = self.share_supply.saturating_sub(shares);
        *self.supply_shares.get_mut(who).unwrap() -= shares;
        if self.supply_shares[who] == 0 {
            self.supply_shares.remove(who);
        }
        Ok(amount)
    }

    fn debt_value_native(&self, debt_asset: u64) -> u64 {
        let scale = coin_scale(&self.asset).max(1);
        ((debt_asset as u128)
            .saturating_mul(self.price_native_per_scale as u128)
            / scale) as u64
    }

    pub fn max_borrow_asset(&self, collateral_native: u64) -> u64 {
        let max_debt_native = collateral_native.saturating_mul(self.ltv_bps) / 10_000;
        let scale = coin_scale(&self.asset).max(1);
        ((max_debt_native as u128).saturating_mul(scale)
            / self.price_native_per_scale.max(1) as u128) as u64
    }

    pub fn borrow(
        &mut self,
        who: &str,
        amount: u64,
        collateral_native: u64,
    ) -> Result<(), String> {
        if amount == 0 {
            return Err("borrow zero".into());
        }
        let available = self.total_supply.saturating_sub(self.total_borrow);
        if amount > available {
            return Err("borrow: insufficient liquidity".into());
        }
        let col = self
            .collateral
            .get(who)
            .copied()
            .unwrap_or(0)
            .saturating_add(collateral_native);
        let new_debt = self.debt.get(who).copied().unwrap_or(0).saturating_add(amount);
        let max = self.max_borrow_asset(col) as u64;
        if new_debt > max {
            return Err(format!("borrow: LTV exceeded debt {new_debt} max {max}"));
        }
        self.total_borrow = self.total_borrow.saturating_add(amount);
        *self.debt.entry(who.to_string()).or_insert(0) = new_debt;
        *self.collateral.entry(who.to_string()).or_insert(0) = col;
        Ok(())
    }

    pub fn repay(&mut self, who: &str, amount: u64) -> Result<u64, String> {
        let d = *self.debt.get(who).unwrap_or(&0);
        if amount == 0 || d == 0 {
            return Err("repay nothing".into());
        }
        let pay = amount.min(d);
        *self.debt.get_mut(who).unwrap() -= pay;
        if self.debt[who] == 0 {
            self.debt.remove(who);
        }
        self.total_borrow = self.total_borrow.saturating_sub(pay);
        Ok(pay)
    }

    /// Liquidate: repay `repay_asset` of debt, seize collateral proportionally.
    pub fn liquidate(
        &mut self,
        borrower: &str,
        repay_asset: u64,
    ) -> Result<u64, String> {
        let d = *self.debt.get(borrower).unwrap_or(&0);
        let col = *self.collateral.get(borrower).unwrap_or(&0);
        if d == 0 || repay_asset == 0 {
            return Err("liquidate: no debt".into());
        }
        // Only if underwater: debt value > LTV * collateral
        let debt_v = self.debt_value_native(d);
        let max_v = col.saturating_mul(self.ltv_bps) / 10_000;
        if debt_v <= max_v {
            return Err("liquidate: position healthy".into());
        }
        let pay = repay_asset.min(d);
        let seize = if d == 0 {
            0
        } else {
            (col as u128).saturating_mul(pay as u128) / d as u128
        } as u64;
        *self.debt.get_mut(borrower).unwrap() -= pay;
        if self.debt[borrower] == 0 {
            self.debt.remove(borrower);
        }
        *self.collateral.get_mut(borrower).unwrap() =
            col.saturating_sub(seize);
        if self.collateral.get(borrower).copied().unwrap_or(0) == 0 {
            self.collateral.remove(borrower);
        }
        self.total_borrow = self.total_borrow.saturating_sub(pay);
        // repaid asset returns to pool supply side liquidity
        Ok(seize)
    }

    pub fn supply_weights(&self) -> BTreeMap<String, u64> {
        self.supply_shares.clone()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DefiState {
    pub pools: BTreeMap<String, AmmPool>,
    pub markets: BTreeMap<String, LendingMarket>,
    /// Secondary balances: account -> asset -> amount (non-native).
    pub assets: BTreeMap<String, BTreeMap<String, u64>>,
    /// v2.9.62 A10: validator admission approvals (id -> key, expiry). Kept
    /// here so it is in the commitment root and in every pack/verify/commit
    /// draft without new plumbing. Empty (omitted) before activation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub validator_approvals: BTreeMap<String, crate::ValidatorApproval>,
}

impl DefiState {
    pub fn asset_bal(&self, account: &str, asset: &str) -> u64 {
        if asset == NATIVE {
            return 0; // native lives in WorldState
        }
        self.assets
            .get(account)
            .and_then(|m| m.get(asset))
            .copied()
            .unwrap_or(0)
    }

    pub fn credit_asset(&mut self, account: &str, asset: &str, amount: u64) {
        if asset == NATIVE || amount == 0 {
            return;
        }
        *self
            .assets
            .entry(account.to_string())
            .or_default()
            .entry(asset.to_string())
            .or_insert(0) += amount;
    }

    pub fn debit_asset(&mut self, account: &str, asset: &str, amount: u64) -> Result<(), String> {
        if asset == NATIVE {
            return Err("use native balance path".into());
        }
        let bal = self.asset_bal(account, asset);
        if bal < amount {
            return Err("insufficient asset balance".into());
        }
        *self
            .assets
            .get_mut(account)
            .unwrap()
            .get_mut(asset)
            .unwrap() -= amount;
        Ok(())
    }

    pub fn aggregate_amm_lp_weights(&self) -> BTreeMap<String, u64> {
        let mut out = BTreeMap::new();
        for p in self.pools.values() {
            for (k, v) in p.lp_weights() {
                *out.entry(k).or_insert(0) += v;
            }
        }
        out
    }

    pub fn aggregate_lend_lp_weights(&self) -> BTreeMap<String, u64> {
        let mut out = BTreeMap::new();
        for m in self.markets.values() {
            for (k, v) in m.supply_weights() {
                *out.entry(k).or_insert(0) += v;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amm_roundtrip() {
        let mut p = AmmPool::new("p1", "FPL", "BTC");
        // Use non-native both for pure pool math (reserves only)
        p.asset_a = "AAA".into();
        p.asset_b = "BBB".into();
        p.add_liquidity("alice", 1000, 1000).unwrap();
        let out = p.swap("AAA", 100, 1).unwrap();
        assert!(out > 0 && out < 1000);
        p.remove_liquidity("alice", p.lp["alice"] / 2).unwrap();
    }

    #[test]
    fn lend_ltv() {
        let mut m = LendingMarket::new("m1", "BTC");
        m.supply("lp1", 1000).unwrap();
        m.borrow("bob", 100, 500).unwrap(); // 50% LTV of 500 native @ 1:1 price => max 250
        assert!(m.borrow("bob", 200, 0).is_err());
        m.repay("bob", 50).unwrap();
    }

    #[test]
    fn eth_price_is_fpl_per_whole_coin() {
        // 141_204 FPL per ETH. The old scale (FPL per 1e6 wei) stored this as 0.
        assert_eq!(fpl_per_coin(141, 964_389_647_971_760, "ETH").unwrap() > 100_000, true);
        let mut m = LendingMarket::new("lend-eth", "ETH");
        m.set_price(141_204);
        m.supply("lp", 10_000_000_000_000_000).unwrap();
        // 0.001 ETH is worth 141 FPL. 400 FPL collateral at 50% LTV covers it.
        m.borrow("bob", 1_000_000_000_000_000, 400).unwrap();
        assert!(m.borrow("bob", 1_000_000_000_000_000, 0).is_err());
    }
}
