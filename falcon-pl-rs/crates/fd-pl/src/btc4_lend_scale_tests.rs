// Copyright (c) 2025-2026 BEARTEC. All rights reserved. See LICENSE.
//! v2.10.4 BTC4 lend price scale (`btc4_lend_scale_from_height`).
//!
//! Live 2300: pool btc4-fpl 406,760 FPL / 10,000,000 sats made `lend-set-price`
//! on lend-btc4 store 40,676 (FPL per 1e6 sats, the `coin_scale` fall-through)
//! instead of ~4,067,600 FPL per 1 BTC4. Before the height nothing changes;
//! from it the market pins 1e8 raw/coin and value-equivalent math holds.
use super::*;

fn node(gate: u64) -> Node {
    let mut n = Node::new(
        "v1",
        ChainConfig { scheme: CryptoScheme::Falcon512, network_id: 0, pack_budget_ms: 0, min_fee: 0, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &[("alice".into(), 1_000_000)],
    );
    n.btc4_lend_scale_from_height = gate;
    n.tip_height = 100;
    n.create_pool("btc4-fpl", "BTC4", "FPL").unwrap();
    let p = n.defi.pools.get_mut("btc4-fpl").unwrap();
    p.reserve_a = 10_000_000;
    p.reserve_b = 406_760;
    n.create_market("lend-btc4", "BTC4").unwrap();
    n
}

#[test]
fn pre_activation_keeps_legacy_per_1e6_price_and_bytes() {
    let mut n = node(u64::MAX);
    assert_eq!(n.sync_lend_price("lend-btc4").unwrap(), 40_676);
    let m = &n.defi.markets["lend-btc4"];
    assert!(m.raw_per_coin.is_none());
    assert!(!serde_json::to_string(m).unwrap().contains("raw_per_coin"));
}

#[test]
fn post_activation_prices_per_whole_btc4_value_equivalent() {
    let mut legacy = node(u64::MAX);
    legacy.sync_lend_price("lend-btc4").unwrap();
    let mut n = node(0);
    assert_eq!(n.sync_lend_price("lend-btc4").unwrap(), 4_067_600);
    let m = &n.defi.markets["lend-btc4"];
    assert_eq!(m.raw_per_coin, Some(100_000_000));
    let l = &legacy.defi.markets["lend-btc4"];
    // Same borrow capacity either way (within rounding).
    let (a, b) = (m.max_borrow_asset(1_000_000), l.max_borrow_asset(1_000_000));
    assert!(a.abs_diff(b) <= b / 10_000 + 1, "{a} vs {b}");
}
