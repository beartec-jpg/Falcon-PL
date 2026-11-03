//! v2.10.4: on-ledger limit order book, hybrid with the constant-product AMM.
//!
//! Design (after the XRPL DEX + XLS-30 AMM, adapted to Falcon PL state):
//! * Every pair is `<ASSET>-FPL` (FPL is the quote, like XRP auto-bridging:
//!   asset↔asset swaps hop through FPL). A pair needs an `<ASSET>/FPL` pool.
//! * `PlaceLimitOrder` escrows funds (buy: FPL, sell: asset) inside the order
//!   itself; `CancelOrder` / expiry refunds the unfilled escrow.
//! * Matching runs inside tx apply on every seat: price-time priority
//!   (best price, then lowest order id = earliest), partial fills, integer
//!   maths only. At each step the taker takes the better of the best resting
//!   order and the AMM (XLS-30 "best of book vs pool"); the AMM is only
//!   pushed up to the next book price / the limit, so pool and book stay one
//!   market. Makers fill at their own price (taker gets the improvement).
//! * Resting orders the AMM price has crossed are filled against the pool
//!   in the per-ledger hook ([`ledger_hook`]), run on the pack, verify and
//!   commit drafts before txs (= end of the previous ledger), bounded.
//! * Prices are FPL raw units per one whole coin ([`pair_scale`] raw units).
//!
//! Conservation: every move is debit-one-side / credit-other between account
//! balances, order escrow and pool reserves; nothing is minted or burned
//! (AMM fee stays in the pool, rounding dust stays in escrow until refund).

use crate::defi::{AmmPool, DefiState};
use crate::WorldState;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

pub const QUOTE: &str = "FPL";
/// Open orders one account may hold (all pairs).
pub const MAX_OPEN_ORDERS_PER_ACCOUNT: usize = 32;
/// Open orders per pair side; keeps the per-tx sort cheap.
pub const MAX_OPEN_ORDERS_PER_SIDE: usize = 1_000;
/// Smallest order, as its FPL value at the limit price (raw FPL units).
pub const MIN_ORDER_QUOTE: u64 = 1_000;
/// Book/AMM steps one taker (order or swap) may consume.
pub const MAX_MATCH_STEPS_PER_TX: usize = 64;
/// AMM-cross trigger fills per ledger (all pairs).
pub const MAX_TRIGGER_FILLS_PER_LEDGER: usize = 64;
/// Expired orders refunded per ledger.
pub const MAX_EXPIRE_PER_LEDGER: usize = 256;
/// Recent fills kept on ledger for RPC.
pub const RECENT_FILLS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn parse(s: &str) -> Result<Side, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "buy" | "bid" => Ok(Side::Buy),
            "sell" | "ask" => Ok(Side::Sell),
            _ => Err("order: side must be buy or sell".into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub id: u64,
    pub owner: String,
    /// Base asset (pair is `<asset>-FPL`).
    pub asset: String,
    pub side: Side,
    /// FPL raw per whole coin.
    pub price: u64,
    /// Original base amount (raw).
    pub amount: u64,
    /// Unfilled base amount (raw).
    pub remaining: u64,
    /// Escrow held: FPL for buys, base asset for sells.
    pub escrow: u64,
    pub placed_height: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_height: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fill {
    pub height: u64,
    pub asset: String,
    /// Taker side.
    pub side: Side,
    /// FPL raw per whole coin (maker price; AMM: average).
    pub price: u64,
    pub base: u64,
    pub quote: u64,
    pub taker: String,
    /// Maker account, or `amm:<pool_id>`.
    pub maker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maker_order: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taker_order: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderBook {
    pub next_id: u64,
    pub orders: BTreeMap<u64, Order>,
    #[serde(default)]
    pub fills: VecDeque<Fill>,
}

impl OrderBook {
    pub fn is_empty(&self) -> bool {
        self.next_id == 0 && self.orders.is_empty() && self.fills.is_empty()
    }
    pub fn open_count(&self, owner: &str) -> usize {
        self.orders.values().filter(|o| o.owner == owner).count()
    }
    fn side_count(&self, asset: &str, side: Side) -> usize {
        self.orders
            .values()
            .filter(|o| o.asset == asset && o.side == side)
            .count()
    }
    /// Best-first ids of one side: buys high→low, sells low→high, then id.
    pub fn sorted_ids(&self, asset: &str, side: Side) -> Vec<u64> {
        let mut v: Vec<(u64, u64)> = self
            .orders
            .values()
            .filter(|o| o.asset == asset && o.side == side)
            .map(|o| (o.price, o.id))
            .collect();
        match side {
            Side::Buy => v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1))),
            Side::Sell => v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1))),
        }
        v.into_iter().map(|(_, id)| id).collect()
    }
    fn push_fill(&mut self, f: Fill) {
        self.fills.push_back(f);
        while self.fills.len() > RECENT_FILLS {
            self.fills.pop_front();
        }
    }
    /// Sum of escrow per asset (FPL key = buy escrow). For conservation checks.
    pub fn escrow_totals(&self) -> BTreeMap<String, u128> {
        let mut m = BTreeMap::new();
        for o in self.orders.values() {
            let k = match o.side {
                Side::Buy => QUOTE.to_string(),
                Side::Sell => o.asset.clone(),
            };
            *m.entry(k).or_insert(0u128) += o.escrow as u128;
        }
        m
    }
}

/// Raw units per whole coin for book prices. BTC4 uses sats (1e8), as the
/// 2.10.4 lend-market fix does; everything else follows `coin_scale`.
pub fn pair_scale(asset: &str) -> u128 {
    if asset == crate::bridge::RAIL_BTC4 {
        100_000_000
    } else {
        crate::defi::coin_scale(asset).max(1)
    }
}

/// Parse `"btc4-fpl"`, `"BTC4/FPL"` or `"BTC4"` → `"BTC4"`.
pub fn parse_pair(pair: &str) -> Result<String, String> {
    let p = pair.trim().to_ascii_uppercase();
    let base = p
        .strip_suffix("-FPL")
        .or_else(|| p.strip_suffix("/FPL"))
        .or_else(|| p.strip_suffix("_FPL"))
        .unwrap_or(&p)
        .to_string();
    if base.is_empty() || base == QUOTE || !base.chars().all(|c| c.is_ascii_alphanumeric()) || base.len() > 16 {
        return Err("order: bad pair (want <ASSET>-FPL)".into());
    }
    Ok(base)
}

/// FPL value of `base` raw units at `price` (floor).
pub fn quote_of(base: u64, price: u64, scale: u128) -> u64 {
    ((base as u128 * price as u128) / scale).min(u64::MAX as u128) as u64
}

fn quote_of_ceil(base: u64, price: u64, scale: u128) -> u64 {
    let n = base as u128 * price as u128;
    ((n + scale - 1) / scale).min(u64::MAX as u128) as u64
}

pub fn isqrt(n: u128) -> u128 {
    if n < 2 {
        return n;
    }
    // Integer Newton from a power-of-two seed >= sqrt(n); no floats.
    let bits = 128 - n.leading_zeros();
    let mut x: u128 = 1u128 << ((bits + 1) / 2).min(127);
    loop {
        let y = (x + n / x) / 2;
        if y >= x {
            break;
        }
        x = y;
    }
    while x.checked_mul(x).map_or(true, |v| v > n) {
        x -= 1;
    }
    while (x + 1).checked_mul(x + 1).map_or(false, |v| v <= n) {
        x += 1;
    }
    x
}

/// Mutable view the engine moves value through.
pub struct Books<'a> {
    pub state: &'a mut WorldState,
    pub defi: &'a mut DefiState,
}

impl<'a> Books<'a> {
    fn bal(&self, who: &str, asset: &str) -> u64 {
        if asset == QUOTE {
            self.state.accounts.get(who).map(|a| a.balance).unwrap_or(0)
        } else {
            self.defi.asset_bal(who, asset)
        }
    }
    fn debit(&mut self, who: &str, asset: &str, amt: u64) -> Result<(), String> {
        if amt == 0 {
            return Ok(());
        }
        if asset == QUOTE {
            let mut a = self.state.get(who);
            if a.balance < amt {
                return Err("insufficient balance for order".into());
            }
            a.balance -= amt;
            self.state.accounts.insert(who.to_string(), a);
            Ok(())
        } else {
            self.defi
                .debit_asset(who, asset, amt)
                .map_err(|_| "insufficient balance for order".to_string())
        }
    }
    fn credit(&mut self, who: &str, asset: &str, amt: u64) {
        if amt == 0 {
            return;
        }
        if asset == QUOTE {
            let mut a = self.state.get(who);
            a.balance = a.balance.saturating_add(amt);
            self.state.accounts.insert(who.to_string(), a);
        } else {
            self.defi.credit_asset(who, asset, amt);
        }
    }
}

/// Pool for `<asset>/FPL` (first by id, same rule as `Node::find_pool`).
pub fn pair_pool(defi: &DefiState, asset: &str) -> Option<String> {
    defi.pools.iter().find_map(|(id, p)| {
        let hit = (p.asset_a == asset && p.asset_b == QUOTE) || (p.asset_a == QUOTE && p.asset_b == asset);
        hit.then(|| id.clone())
    })
}

/// (base reserve, FPL reserve).
fn reserves(p: &AmmPool, asset: &str) -> (u64, u64) {
    if p.asset_a == asset {
        (p.reserve_a, p.reserve_b)
    } else {
        (p.reserve_b, p.reserve_a)
    }
}

/// Pool spot, FPL raw per whole coin.
pub fn pool_spot(p: &AmmPool, asset: &str) -> Option<u64> {
    let (x, y) = reserves(p, asset);
    if x == 0 || y == 0 {
        return None;
    }
    Some(((y as u128 * pair_scale(asset)) / x as u128).min(u64::MAX as u128) as u64)
}

/// Base reserve at which the pool spot equals `price` (x' = sqrt(k*scale/P)).
fn x_at_price(x: u64, y: u64, price: u64, scale: u128) -> u128 {
    let k = x as u128 * y as u128;
    let p = price.max(1) as u128;
    match k.checked_mul(scale) {
        Some(ks) => isqrt(ks / p),
        None => isqrt(k / p).saturating_mul(isqrt(scale)),
    }
}

/// Amount out the pool would give (no state change; same maths as `AmmPool::swap`).
pub fn amm_quote(p: &AmmPool, token_in: &str, amount_in: u64) -> u64 {
    let (rin, rout) = if token_in == p.asset_a {
        (p.reserve_a, p.reserve_b)
    } else if token_in == p.asset_b {
        (p.reserve_b, p.reserve_a)
    } else {
        return 0;
    };
    if amount_in == 0 || rin == 0 || rout == 0 {
        return 0;
    }
    let a = (amount_in as u128).saturating_mul(997);
    let numer = a.saturating_mul(rout as u128);
    let denom = (rin as u128).saturating_mul(1000).saturating_add(a);
    (numer / denom) as u64
}

/// AMM swap on `pool_id` through the pool's own maths. Returns amount out.
fn amm_swap(b: &mut Books, pool_id: &str, token_in: &str, amount_in: u64) -> Result<u64, String> {
    let p = b.defi.pools.get_mut(pool_id).ok_or("unknown pool")?;
    p.swap(token_in, amount_in, 0)
}

/// FPL in needed to buy at most `base_cap` and move spot up to `target`
/// (u64::MAX = no price target). Returns 0 when the pool is at/above target.
fn amm_buy_in(p: &AmmPool, asset: &str, target: u64, base_cap: u64, budget: u64) -> u64 {
    let (x, y) = reserves(p, asset);
    if x == 0 || y == 0 || budget == 0 || base_cap == 0 {
        return 0;
    }
    let scale = pair_scale(asset);
    let mut fpl_in = budget as u128;
    if target != u64::MAX {
        if pool_spot(p, asset).map_or(true, |s| s >= target) {
            return 0;
        }
        let x2 = x_at_price(x, y, target, scale);
        if x2 >= x as u128 || x2 == 0 {
            return 0;
        }
        let k = x as u128 * y as u128;
        let y2 = k / x2;
        let dy = y2.saturating_sub(y as u128);
        fpl_in = fpl_in.min(dy * 1000 / 997);
    }
    if base_cap < x {
        // Exact-out cap: in = rin*out*1000 / ((rout-out)*997), rounded down so out <= cap.
        let cap_in = (y as u128 * base_cap as u128 * 1000) / ((x - base_cap) as u128 * 997);
        fpl_in = fpl_in.min(cap_in);
    }
    fpl_in.min(u64::MAX as u128) as u64
}

/// Base in to sell at most `base_cap` and move spot down to `target` (0 = none).
fn amm_sell_in(p: &AmmPool, asset: &str, target: u64, base_cap: u64) -> u64 {
    let (x, y) = reserves(p, asset);
    if x == 0 || y == 0 || base_cap == 0 {
        return 0;
    }
    let scale = pair_scale(asset);
    let mut base_in = base_cap as u128;
    if target != 0 {
        if pool_spot(p, asset).map_or(true, |s| s <= target) {
            return 0;
        }
        let x2 = x_at_price(x, y, target, scale);
        if x2 <= x as u128 {
            return 0;
        }
        base_in = base_in.min((x2 - x as u128) * 1000 / 997);
    }
    base_in.min(u64::MAX as u128) as u64
}

/// Result of a taker pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TakeResult {
    /// Buy: base received. Sell: base spent.
    pub base: u64,
    /// Buy: FPL spent. Sell: FPL received.
    pub quote: u64,
}

/// Taker: buy up to `max_base` with at most `budget` FPL (already debited
/// from `who` into the caller's custody), limit `limit` (u64::MAX = market).
/// Credits base to `who`; the caller refunds/escrows `budget - quote`.
#[allow(clippy::too_many_arguments)]
pub fn take_buy(
    b: &mut Books,
    pool_id: Option<&str>,
    who: &str,
    asset: &str,
    limit: u64,
    max_base: u64,
    budget: u64,
    height: u64,
    taker_order: Option<u64>,
) -> Result<TakeResult, String> {
    let scale = pair_scale(asset);
    let mut r = TakeResult::default();
    let mut steps = 0;
    while steps < MAX_MATCH_STEPS_PER_TX && r.base < max_base && r.quote < budget {
        steps += 1;
        let want = max_base - r.base;
        let left = budget - r.quote;
        let best = b
            .defi
            .orderbook
            .sorted_ids(asset, Side::Sell)
            .into_iter()
            .map(|id| b.defi.orderbook.orders[&id].clone())
            .find(|o| o.owner != who && o.price <= limit);
        // AMM first while it is strictly cheaper than the best ask (book wins ties).
        if let Some(pid) = pool_id {
            let target = best.as_ref().map(|o| o.price).unwrap_or(u64::MAX).min(limit);
            let p = &b.defi.pools[pid];
            let fin = amm_buy_in(p, asset, target, want, left);
            if fin > 0 && amm_quote(p, QUOTE, fin) > 0 {
                let out = amm_swap(b, pid, QUOTE, fin)?;
                b.credit(who, asset, out);
                r.base += out;
                r.quote += fin;
                b.defi.orderbook.push_fill(Fill {
                    height,
                    asset: asset.into(),
                    side: Side::Buy,
                    price: ((fin as u128 * scale) / out as u128).min(u64::MAX as u128) as u64,
                    base: out,
                    quote: fin,
                    taker: who.into(),
                    maker: format!("amm:{pid}"),
                    maker_order: None,
                    taker_order,
                });
                continue;
            }
        }
        let Some(o) = best else { break };
        let afford = ((left as u128 * scale) / o.price.max(1) as u128).min(u64::MAX as u128) as u64;
        let f = want.min(o.remaining).min(afford);
        let cost = quote_of(f, o.price, scale);
        if f == 0 || cost == 0 {
            break;
        }
        let m = b.defi.orderbook.orders.get_mut(&o.id).unwrap();
        m.remaining -= f;
        m.escrow -= f; // sell escrow is base
        let done = m.remaining == 0;
        let refund = if done { m.escrow } else { 0 };
        if done {
            b.defi.orderbook.orders.remove(&o.id);
        }
        b.credit(&o.owner, QUOTE, cost);
        b.credit(&o.owner, asset, refund);
        b.credit(who, asset, f);
        r.base += f;
        r.quote += cost;
        b.defi.orderbook.push_fill(Fill {
            height,
            asset: asset.into(),
            side: Side::Buy,
            price: o.price,
            base: f,
            quote: cost,
            taker: who.into(),
            maker: o.owner.clone(),
            maker_order: Some(o.id),
            taker_order,
        });
    }
    Ok(r)
}

/// Taker: sell up to `base_in` (already in caller's custody), limit `limit`
/// (0 = market). Credits FPL to `who`; caller handles unsold base.
#[allow(clippy::too_many_arguments)]
pub fn take_sell(
    b: &mut Books,
    pool_id: Option<&str>,
    who: &str,
    asset: &str,
    limit: u64,
    base_in: u64,
    height: u64,
    taker_order: Option<u64>,
) -> Result<TakeResult, String> {
    let scale = pair_scale(asset);
    let mut r = TakeResult::default();
    let mut steps = 0;
    while steps < MAX_MATCH_STEPS_PER_TX && r.base < base_in {
        steps += 1;
        let want = base_in - r.base;
        let best = b
            .defi
            .orderbook
            .sorted_ids(asset, Side::Buy)
            .into_iter()
            .map(|id| b.defi.orderbook.orders[&id].clone())
            .find(|o| o.owner != who && o.price >= limit);
        if let Some(pid) = pool_id {
            let target = best.as_ref().map(|o| o.price).unwrap_or(0).max(limit);
            let p = &b.defi.pools[pid];
            let bin = amm_sell_in(p, asset, target, want);
            if bin > 0 && amm_quote(p, asset, bin) > 0 {
                let out = amm_swap(b, pid, asset, bin)?;
                b.credit(who, QUOTE, out);
                r.base += bin;
                r.quote += out;
                b.defi.orderbook.push_fill(Fill {
                    height,
                    asset: asset.into(),
                    side: Side::Sell,
                    price: ((out as u128 * scale) / bin as u128).min(u64::MAX as u128) as u64,
                    base: bin,
                    quote: out,
                    taker: who.into(),
                    maker: format!("amm:{pid}"),
                    maker_order: None,
                    taker_order,
                });
                continue;
            }
        }
        let Some(o) = best else { break };
        let f = want.min(o.remaining);
        let pay = quote_of(f, o.price, scale);
        if f == 0 || pay == 0 {
            break;
        }
        let m = b.defi.orderbook.orders.get_mut(&o.id).unwrap();
        let pay = pay.min(m.escrow);
        m.remaining -= f;
        m.escrow -= pay;
        let done = m.remaining == 0;
        let refund = if done { m.escrow } else { 0 };
        if done {
            b.defi.orderbook.orders.remove(&o.id);
        }
        b.credit(&o.owner, asset, f);
        b.credit(&o.owner, QUOTE, refund);
        b.credit(who, QUOTE, pay);
        r.base += f;
        r.quote += pay;
        b.defi.orderbook.push_fill(Fill {
            height,
            asset: asset.into(),
            side: Side::Sell,
            price: o.price,
            base: f,
            quote: pay,
            taker: who.into(),
            maker: o.owner.clone(),
            maker_order: Some(o.id),
            taker_order,
        });
    }
    Ok(r)
}

/// `PlaceLimitOrder` apply. Returns the resting order id (None = fully filled).
#[allow(clippy::too_many_arguments)]
pub fn place_limit(
    b: &mut Books,
    who: &str,
    pair: &str,
    side: &str,
    price: u64,
    amount: u64,
    expiry_height: Option<u64>,
    height: u64,
) -> Result<Option<u64>, String> {
    let asset = parse_pair(pair)?;
    let side = Side::parse(side)?;
    let pool = pair_pool(b.defi, &asset).ok_or("order: no <ASSET>/FPL pool for pair")?;
    if price == 0 || amount == 0 {
        return Err("order: zero price or amount".into());
    }
    if let Some(e) = expiry_height {
        if e < height {
            return Err("order: expiry in the past".into());
        }
    }
    let scale = pair_scale(&asset);
    if quote_of(amount, price, scale) < MIN_ORDER_QUOTE {
        return Err(format!("order: below min size ({MIN_ORDER_QUOTE} FPL raw)"));
    }
    if b.defi.orderbook.open_count(who) >= MAX_OPEN_ORDERS_PER_ACCOUNT {
        return Err(format!("order: max {MAX_OPEN_ORDERS_PER_ACCOUNT} open orders per account"));
    }
    if b.defi.orderbook.side_count(&asset, side) >= MAX_OPEN_ORDERS_PER_SIDE {
        return Err("order: book side full".into());
    }
    let id = b.defi.orderbook.next_id;
    let escrow_asset = if side == Side::Buy { QUOTE.to_string() } else { asset.clone() };
    let escrow = match side {
        Side::Buy => quote_of_ceil(amount, price, scale),
        Side::Sell => amount,
    };
    if b.bal(who, &escrow_asset) < escrow {
        return Err("insufficient balance for order".into());
    }
    b.debit(who, &escrow_asset, escrow)?;
    b.defi.orderbook.next_id = id + 1;
    let (remaining, left) = match side {
        Side::Buy => {
            let r = take_buy(b, Some(&pool), who, &asset, price, amount, escrow, height, Some(id))?;
            (amount - r.base.min(amount), escrow - r.quote)
        }
        Side::Sell => {
            let r = take_sell(b, Some(&pool), who, &asset, price, amount, height, Some(id))?;
            (amount - r.base, amount - r.base)
        }
    };
    if remaining == 0 || (side == Side::Buy && quote_of(remaining, price, scale) == 0) {
        b.credit(who, &escrow_asset, left);
        return Ok(None);
    }
    b.defi.orderbook.orders.insert(
        id,
        Order {
            id,
            owner: who.into(),
            asset,
            side,
            price,
            amount,
            remaining,
            escrow: left,
            placed_height: height,
            expiry_height,
        },
    );
    Ok(Some(id))
}

fn refund_order(b: &mut Books, o: &Order) {
    let a = if o.side == Side::Buy { QUOTE } else { o.asset.as_str() };
    b.credit(&o.owner, a, o.escrow);
}

/// `CancelOrder` apply.
pub fn cancel(b: &mut Books, who: &str, id: u64) -> Result<(), String> {
    let o = b.defi.orderbook.orders.get(&id).ok_or("order: unknown order id")?;
    if o.owner != who {
        return Err("order: not owner".into());
    }
    let o = b.defi.orderbook.orders.remove(&id).unwrap();
    refund_order(b, &o);
    Ok(())
}

/// Market swap through book + pool (`Swap` / `SwapRoute` leg after activation).
pub fn hybrid_swap(
    b: &mut Books,
    pool_id: &str,
    who: &str,
    token_in: &str,
    amount_in: u64,
    height: u64,
) -> Result<u64, String> {
    let p = b.defi.pools.get(pool_id).ok_or("unknown pool")?;
    let asset = if p.asset_a == QUOTE { p.asset_b.clone() } else { p.asset_a.clone() };
    b.debit(who, token_in, amount_in)
        .map_err(|_| "insufficient balance".to_string())?;
    if token_in == QUOTE {
        let r = take_buy(b, Some(pool_id), who, &asset, u64::MAX, u64::MAX, amount_in, height, None)?;
        b.credit(who, QUOTE, amount_in - r.quote);
        Ok(r.base)
    } else {
        let r = take_sell(b, Some(pool_id), who, &asset, 0, amount_in, height, None)?;
        b.credit(who, &asset, amount_in - r.base);
        Ok(r.quote)
    }
}

/// Per-ledger hook: expire orders, then fill resting orders the AMM crossed.
pub fn ledger_hook(b: &mut Books, height: u64) {
    if b.defi.orderbook.orders.is_empty() {
        return;
    }
    // 1. Expiry (order of expiry height, then id).
    let mut exp: Vec<(u64, u64)> = b
        .defi
        .orderbook
        .orders
        .values()
        .filter_map(|o| o.expiry_height.filter(|e| *e < height).map(|e| (e, o.id)))
        .collect();
    exp.sort();
    for (_, id) in exp.into_iter().take(MAX_EXPIRE_PER_LEDGER) {
        if let Some(o) = b.defi.orderbook.orders.remove(&id) {
            refund_order(b, &o);
        }
    }
    // 2. AMM-cross triggers, pairs in asset order, buys then sells, best first.
    let mut assets: Vec<String> = b.defi.orderbook.orders.values().map(|o| o.asset.clone()).collect();
    assets.sort();
    assets.dedup();
    let mut budget = MAX_TRIGGER_FILLS_PER_LEDGER;
    for asset in assets {
        let Some(pid) = pair_pool(b.defi, &asset) else { continue };
        for side in [Side::Buy, Side::Sell] {
            for id in b.defi.orderbook.sorted_ids(&asset, side) {
                if budget == 0 {
                    return;
                }
                let Some(o) = b.defi.orderbook.orders.get(&id).cloned() else { continue };
                let p = &b.defi.pools[&pid];
                let crossed = match (side, pool_spot(p, &asset)) {
                    (Side::Buy, Some(s)) => s < o.price,
                    (Side::Sell, Some(s)) => s > o.price,
                    _ => false,
                };
                if !crossed {
                    break; // best-first: later orders cross even less
                }
                budget -= 1;
                let filled = match side {
                    Side::Buy => {
                        let fin = amm_buy_in(p, &asset, o.price, o.remaining, o.escrow);
                        if fin == 0 || amm_quote(p, QUOTE, fin) == 0 {
                            continue;
                        }
                        match amm_swap(b, &pid, QUOTE, fin) {
                            Ok(out) if out > 0 => Some((out, fin)),
                            _ => None,
                        }
                    }
                    Side::Sell => {
                        let bin = amm_sell_in(p, &asset, o.price, o.remaining);
                        if bin == 0 || amm_quote(p, &asset, bin) == 0 {
                            continue;
                        }
                        match amm_swap(b, &pid, &asset, bin) {
                            Ok(out) if out > 0 => Some((bin, out)),
                            _ => None,
                        }
                    }
                };
                let Some((base, quote)) = filled else { continue };
                let m = b.defi.orderbook.orders.get_mut(&id).unwrap();
                m.remaining = m.remaining.saturating_sub(base);
                match side {
                    Side::Buy => m.escrow -= quote,
                    Side::Sell => m.escrow -= base,
                }
                let scale = pair_scale(&asset);
                let dust_left = side == Side::Buy && quote_of(m.remaining, m.price, scale) == 0;
                let done = m.remaining == 0 || dust_left;
                match side {
                    Side::Buy => b.credit(&o.owner, &asset, base),
                    Side::Sell => b.credit(&o.owner, QUOTE, quote),
                }
                if done {
                    let o2 = b.defi.orderbook.orders.remove(&id).unwrap();
                    refund_order(b, &o2);
                }
                b.defi.orderbook.push_fill(Fill {
                    height,
                    asset: asset.clone(),
                    side,
                    price: ((quote as u128 * scale) / base.max(1) as u128).min(u64::MAX as u128) as u64,
                    base,
                    quote,
                    taker: o.owner.clone(),
                    maker: format!("amm:{pid}"),
                    maker_order: None,
                    taker_order: Some(id),
                });
            }
        }
    }
}

/// RPC: book depth for one pair, aggregated by price level, plus pool spot.
/// Shape follows the site's `/api/market/orderbook` (asks/bids/amm) so the
/// QXRP-era `OrderBookPanel` can be pointed at it.
pub fn depth_json(defi: &DefiState, pair: &str, levels: usize) -> serde_json::Value {
    let Ok(asset) = parse_pair(pair) else {
        return serde_json::json!({"error": "bad pair"});
    };
    let scale = pair_scale(&asset);
    let side_json = |side: Side| -> Vec<serde_json::Value> {
        let mut out: Vec<(u64, u64, u64, u64)> = Vec::new(); // price, base, quote, count
        for id in defi.orderbook.sorted_ids(&asset, side) {
            let o = &defi.orderbook.orders[&id];
            match out.last_mut() {
                Some(l) if l.0 == o.price => {
                    l.1 += o.remaining;
                    l.2 += quote_of(o.remaining, o.price, scale);
                    l.3 += 1;
                }
                _ => {
                    if out.len() >= levels {
                        break;
                    }
                    out.push((o.price, o.remaining, quote_of(o.remaining, o.price, scale), 1))
                }
            }
        }
        out.into_iter()
            .map(|(p, b, q, c)| serde_json::json!({"price": p, "amountBase": b, "amountFpl": q, "orders": c}))
            .collect()
    };
    let offers = |side: Side| -> Vec<serde_json::Value> {
        defi.orderbook
            .sorted_ids(&asset, side)
            .into_iter()
            .take(levels)
            .map(|id| order_json(&defi.orderbook.orders[&id], scale))
            .collect()
    };
    let amm = pair_pool(defi, &asset).map(|pid| {
        let p = &defi.pools[&pid];
        let (x, y) = reserves(p, &asset);
        serde_json::json!({"pool_id": pid, "reserveBase": x, "reserveFpl": y, "spot": pool_spot(p, &asset)})
    });
    serde_json::json!({
        "pair": format!("{asset}-FPL"),
        "base": asset,
        "quote": QUOTE,
        "price_scale": scale.to_string(),
        "price_unit": "FPL raw per 1 whole coin",
        "asks": side_json(Side::Sell),
        "bids": side_json(Side::Buy),
        "ask_offers": offers(Side::Sell),
        "bid_offers": offers(Side::Buy),
        "amm": amm,
    })
}

pub fn order_json(o: &Order, scale: u128) -> serde_json::Value {
    serde_json::json!({
        "id": o.id, "seq": o.id, "owner": o.owner, "pair": format!("{}-FPL", o.asset),
        "side": o.side, "price": o.price, "amount": o.amount, "remaining": o.remaining,
        "amountBase": o.remaining, "amountFpl": quote_of(o.remaining, o.price, scale),
        "escrow": o.escrow, "placed_height": o.placed_height, "expiry_height": o.expiry_height,
    })
}

pub fn account_orders_json(defi: &DefiState, account: &str) -> serde_json::Value {
    let v: Vec<_> = defi
        .orderbook
        .orders
        .values()
        .filter(|o| o.owner == account)
        .map(|o| order_json(o, pair_scale(&o.asset)))
        .collect();
    serde_json::json!({"account": account, "orders": v, "max_open": MAX_OPEN_ORDERS_PER_ACCOUNT})
}

pub fn fills_json(defi: &DefiState, pair: Option<&str>, account: Option<&str>, limit: usize) -> serde_json::Value {
    let asset = pair.and_then(|p| parse_pair(p).ok());
    let v: Vec<_> = defi
        .orderbook
        .fills
        .iter()
        .rev()
        .filter(|f| asset.as_ref().map_or(true, |a| &f.asset == a))
        .filter(|f| account.map_or(true, |a| f.taker == a || f.maker == a))
        .take(limit.min(RECENT_FILLS))
        .cloned()
        .collect();
    serde_json::json!({"fills": v})
}
