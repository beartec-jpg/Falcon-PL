//! v2.10.4 on-ledger order book: matching, escrow conservation, partial
//! fills, cancel/expiry refunds, AMM-cross triggers, determinism across
//! seats, and pre-activation no-op.
use super::*;
use crate::orderbook::*;

const USERS: [&str; 4] = ["alice", "bob", "carol", "dave"];

/// Node with a USDC/FPL pool at spot 100 FPL raw per 1 USDC (1e6 raw).
fn node(gate: u64) -> Node {
    let accts: Vec<(String, u64)> = USERS.iter().map(|u| (u.to_string(), 1_000_000_000_000)).collect();
    let mut n = Node::new(
        "v1",
        ChainConfig { scheme: CryptoScheme::Falcon512, network_id: 0, pack_budget_ms: 0, min_fee: 0, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &accts,
    );
    n.orderbook_from_height = gate;
    n.tip_height = 100;
    n.create_pool("usdc-fpl", "USDC", "FPL").unwrap();
    let p = n.defi.pools.get_mut("usdc-fpl").unwrap();
    p.reserve_a = 1_000_000_000_000; // 1e6 USDC
    p.reserve_b = 100_000_000_000; // spot = 1e11 * 1e6 / 1e12 = 100_000
    for u in USERS {
        n.defi.credit_asset(u, "USDC", 10_000_000_000_000);
    }
    n
}

fn spot(n: &Node) -> u64 {
    pool_spot(&n.defi.pools["usdc-fpl"], "USDC").unwrap()
}

/// (total FPL, total USDC) over balances + escrow + pool.
fn totals(n: &Node) -> (u128, u128) {
    let mut f: u128 = n.state.accounts.values().map(|a| a.balance as u128).sum();
    let mut u: u128 = n.defi.assets.values().map(|m| *m.get("USDC").unwrap_or(&0) as u128).sum();
    for p in n.defi.pools.values() {
        f += p.reserve_b as u128;
        u += p.reserve_a as u128;
    }
    let e = n.defi.orderbook.escrow_totals();
    f += e.get("FPL").copied().unwrap_or(0);
    u += e.get("USDC").copied().unwrap_or(0);
    (f, u)
}

fn place(n: &mut Node, who: &str, side: &str, price: u64, amount: u64, exp: Option<u64>) -> Result<Option<u64>, String> {
    let h = n.tip_height + 1;
    let mut b = Books { state: &mut n.state, defi: &mut n.defi };
    place_limit(&mut b, who, "usdc-fpl", side, price, amount, exp, h)
}

fn fpl(n: &Node, who: &str) -> u64 {
    n.state.get(who).balance
}
fn usdc(n: &Node, who: &str) -> u64 {
    n.defi.asset_bal(who, "USDC")
}

#[test]
fn price_time_priority_and_partial_fill() {
    let mut n = node(0);
    let t0 = totals(&n);
    let a = place(&mut n, "alice", "sell", 100000, 100_000_000, None).unwrap().unwrap();
    let c = place(&mut n, "carol", "sell", 100000, 100_000_000, None).unwrap().unwrap();
    let d = place(&mut n, "dave", "sell", 101000, 100_000_000, None).unwrap().unwrap();
    assert_eq!(spot(&n), 100_000, "asks at/above spot do not touch the pool");
    let (fb, ub) = (fpl(&n, "bob"), usdc(&n, "bob"));
    // Buy 150 USDC at limit 101: alice (earlier) fills first, carol half.
    let r = place(&mut n, "bob", "buy", 101000, 150_000_000, None).unwrap();
    assert_eq!(r, None, "fully filled, nothing rests");
    assert!(!n.defi.orderbook.orders.contains_key(&a));
    assert_eq!(n.defi.orderbook.orders[&c].remaining, 50_000_000);
    assert_eq!(n.defi.orderbook.orders[&d].remaining, 100_000_000);
    assert_eq!(usdc(&n, "bob") - ub, 150_000_000);
    // Paid maker price 100, not the 101 limit; improvement refunded.
    assert_eq!(fb - fpl(&n, "bob"), 15_000_000);
    assert_eq!(fpl(&n, "alice"), 1_000_000_000_000 + 10_000_000);
    assert_eq!(spot(&n), 100_000);
    assert_eq!(totals(&n), t0);
}

#[test]
fn limit_buy_on_empty_book_uses_pool_up_to_limit_and_rests_the_rest() {
    let mut n = node(0);
    let t0 = totals(&n);
    let id = place(&mut n, "bob", "buy", 101000, 100_000_000_000, None).unwrap().expect("rests");
    let s = spot(&n);
    assert!(s <= 101_000 && s >= 100_000, "pool pushed to the limit, not past it: {s}");
    let o = &n.defi.orderbook.orders[&id];
    assert!(o.remaining < 100_000_000_000);
    assert!(o.escrow as u128 >= quote_of(o.remaining, o.price, pair_scale("USDC")) as u128);
    assert_eq!(totals(&n), t0);
}

#[test]
fn hybrid_buy_takes_best_of_pool_and_book() {
    let mut n = node(0);
    let t0 = totals(&n);
    let ask = place(&mut n, "alice", "sell", 105000, 1_000_000_000, None).unwrap().unwrap();
    let fills_before = n.defi.orderbook.fills.len();
    place(&mut n, "bob", "buy", 110000, 200_000_000_000, None).unwrap();
    // Pool first (cheaper than 105), then the ask, then pool towards 110.
    let f: Vec<_> = n.defi.orderbook.fills.iter().skip(fills_before).collect();
    assert!(f.len() >= 2);
    assert!(f[0].maker.starts_with("amm:"));
    let book_ix = f.iter().position(|x| x.maker_order == Some(ask)).expect("ask filled");
    assert!(book_ix >= 1);
    assert!(!n.defi.orderbook.orders.contains_key(&ask));
    assert!(spot(&n) <= 110_000);
    assert_eq!(totals(&n), t0);
}

#[test]
fn swap_with_empty_book_equals_legacy_pool_swap() {
    let mut a = node(0);
    let mut b = node(u64::MAX);
    let oa = a.swap_any("usdc-fpl", "bob", "FPL", 1_000_000_000, 1).unwrap();
    let ob = b.swap_any("usdc-fpl", "bob", "FPL", 1_000_000_000, 1).unwrap();
    assert_eq!(oa, ob);
    assert_eq!(a.defi.pools["usdc-fpl"].reserve_a, b.defi.pools["usdc-fpl"].reserve_a);
    assert_eq!(usdc(&a, "bob"), usdc(&b, "bob"));
    let oa = a.swap_any("usdc-fpl", "bob", "USDC", 5_000_000_000, 1).unwrap();
    let ob = b.swap_any("usdc-fpl", "bob", "USDC", 5_000_000_000, 1).unwrap();
    assert_eq!(oa, ob);
}

#[test]
fn swap_routes_through_resting_orders() {
    let mut n = node(0);
    let t0 = totals(&n);
    // A bid above spot can't rest unfilled vs the pool... place a sell below spot? It
    // would hit the pool. Instead: an ask at spot (100) beats the pool's 0.3% fee.
    let ask = place(&mut n, "alice", "sell", 100000, 10_000_000_000, None).unwrap().unwrap();
    let out = n.swap_any("usdc-fpl", "bob", "FPL", 500_000_000, 1).unwrap();
    // 500_000_000 FPL at 100_000 per USDC = 5_000 USDC = 5e9 raw, all from the book.
    assert_eq!(out, 5_000_000_000);
    assert_eq!(n.defi.orderbook.orders[&ask].remaining, 5_000_000_000);
    assert_eq!(spot(&n), 100_000);
    assert!(n.swap_any("usdc-fpl", "bob", "FPL", 1_000, 1_000_000_000).is_err(), "min_out still enforced");
    assert_eq!(totals(&n), t0);
}

#[test]
fn cancel_refunds_and_only_owner() {
    let mut n = node(0);
    let t0 = totals(&n);
    let before = fpl(&n, "bob");
    let id = place(&mut n, "bob", "buy", 50000, 1_000_000_000, None).unwrap().unwrap();
    assert_eq!(before - fpl(&n, "bob"), 50_000_000);
    let mut b = Books { state: &mut n.state, defi: &mut n.defi };
    assert!(cancel(&mut b, "alice", id).unwrap_err().contains("not owner"));
    cancel(&mut b, "bob", id).unwrap();
    assert!(cancel(&mut b, "bob", id).is_err());
    assert_eq!(fpl(&n, "bob"), before);
    assert_eq!(totals(&n), t0);
}

#[test]
fn expiry_refunds_in_ledger_hook() {
    let mut n = node(0);
    let t0 = totals(&n);
    let before = usdc(&n, "alice");
    let id = place(&mut n, "alice", "sell", 200000, 1_000_000_000, Some(105)).unwrap().unwrap();
    n.orderbook_ledger_hook_for_test(105);
    assert!(n.defi.orderbook.orders.contains_key(&id), "alive through its expiry height");
    n.orderbook_ledger_hook_for_test(106);
    assert!(!n.defi.orderbook.orders.contains_key(&id));
    assert_eq!(usdc(&n, "alice"), before);
    assert_eq!(totals(&n), t0);
    assert!(place(&mut n, "alice", "sell", 200000, 1_000_000_000, Some(50)).unwrap_err().contains("expiry"));
}

#[test]
fn amm_cross_triggers_fill_resting_orders_against_pool() {
    let mut n = node(0);
    let t0 = totals(&n);
    let bid = place(&mut n, "bob", "buy", 95000, 100_000_000_000, None).unwrap().unwrap();
    let ask = place(&mut n, "carol", "sell", 105000, 100_000_000_000, None).unwrap().unwrap();
    // Off-ratio liquidity (or any non-book move) drops spot under the bid.
    {
        let p = n.defi.pools.get_mut("usdc-fpl").unwrap();
        p.reserve_a += 200_000_000_000; // donate: totals change by this
    }
    let t1 = (t0.0, t0.1 + 200_000_000_000);
    assert!(spot(&n) < 95_000);
    let ub = usdc(&n, "bob");
    n.orderbook_ledger_hook_for_test(101);
    assert!(usdc(&n, "bob") > ub, "bid filled from the pool");
    let s = spot(&n);
    assert!(s >= 94_000 && s <= 95_000, "pool pulled back to the bid: {s}");
    assert!(n.defi.orderbook.orders.get(&bid).map_or(true, |o| o.remaining < 100_000_000_000));
    assert_eq!(n.defi.orderbook.orders[&ask].remaining, 100_000_000_000, "ask untouched");
    assert_eq!(totals(&n), t1);
}

#[test]
fn bounds_min_size_and_max_open() {
    let mut n = node(0);
    assert!(place(&mut n, "bob", "buy", 50000, 10, None).unwrap_err().contains("min size"));
    for i in 0..MAX_OPEN_ORDERS_PER_ACCOUNT as u64 {
        place(&mut n, "bob", "buy", (50 - (i % 10)) * 1000, 1_000_000_000, None).unwrap().unwrap();
    }
    assert!(place(&mut n, "bob", "buy", 40000, 1_000_000_000, None).unwrap_err().contains("max"));
    assert!(place(&mut n, "bob", "buy", 0, 1, None).is_err());
    let mut b = Books { state: &mut n.state, defi: &mut n.defi };
    assert!(place_limit(&mut b, "bob", "nope-fpl", "buy", 1, 1, None, 1).unwrap_err().contains("pool"));
    assert!(place_limit(&mut b, "bob", "usdc-fpl", "hold", 1, 1, None, 1).unwrap_err().contains("side"));
}

#[test]
fn isqrt_exact() {
    for v in [0u128, 1, 2, 3, 4, 15, 16, 17, 1 << 64, u64::MAX as u128, u128::MAX] {
        let r = isqrt(v);
        assert!(r * r <= v);
        assert!((r + 1).checked_mul(r + 1).map_or(true, |x| x > v));
    }
}

/// Property: random place/cancel/swap/hook sequences conserve value, keep
/// buy escrow covering the rest, never leave a cross between different
/// owners, and replay byte-identically.
#[test]
fn property_random_ops_conserve_and_replay() {
    for seed in 1..=40u64 {
        let run = |seed: u64| -> (Node, String) {
            let mut n = node(0);
            let t0 = totals(&n);
            let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut rnd = move |m: u64| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x % m
            };
            let mut ids: Vec<u64> = Vec::new();
            for step in 0..120u64 {
                let who = USERS[rnd(4) as usize];
                match rnd(10) {
                    0..=4 => {
                        let side = if rnd(2) == 0 { "buy" } else { "sell" };
                        let price = (80 + rnd(40)) * 1000;
                        let amt = 10_000_000 + rnd(50_000_000_000);
                        let exp = if rnd(3) == 0 { Some(n.tip_height + 1 + rnd(5)) } else { None };
                        if let Ok(Some(id)) = place(&mut n, who, side, price, amt, exp) {
                            ids.push(id);
                        }
                    }
                    5 => {
                        if !ids.is_empty() {
                            let id = ids[rnd(ids.len() as u64) as usize];
                            let mut b = Books { state: &mut n.state, defi: &mut n.defi };
                            let _ = cancel(&mut b, who, id);
                        }
                    }
                    6 | 7 => {
                        let (tin, amt) = if rnd(2) == 0 { ("FPL", 1_000_000 + rnd(2_000_000_000)) } else { ("USDC", 1_000_000 + rnd(20_000_000_000)) };
                        let _ = n.swap_any("usdc-fpl", who, tin, amt, 1);
                    }
                    _ => {
                        n.tip_height += 1;
                        let h = n.tip_height + 1;
                        n.orderbook_ledger_hook_for_test(h);
                    }
                }
                assert_eq!(totals(&n), t0, "seed {seed} step {step}: conservation");
                for o in n.defi.orderbook.orders.values() {
                    assert!(o.remaining > 0);
                    if o.side == Side::Buy {
                        assert!(o.escrow >= quote_of(o.remaining, o.price, pair_scale("USDC")), "seed {seed}: escrow short");
                    } else {
                        assert_eq!(o.escrow, o.remaining);
                    }
                }
                let bids = n.defi.orderbook.sorted_ids("USDC", Side::Buy);
                let asks = n.defi.orderbook.sorted_ids("USDC", Side::Sell);
                for b in &bids {
                    for a in &asks {
                        let (ob, oa) = (&n.defi.orderbook.orders[b], &n.defi.orderbook.orders[a]);
                        if ob.owner != oa.owner {
                            assert!(ob.price < oa.price, "seed {seed}: crossed book {ob:?} {oa:?}");
                        }
                    }
                }
            }
            let j = serde_json::to_string(&n.defi).unwrap();
            (n, j)
        };
        let (_, a) = run(seed);
        let (_, b) = run(seed);
        assert_eq!(a, b, "seed {seed}: replay must be byte-identical");
    }
}

// ---- tx path / gating ------------------------------------------------------

fn order_tx(keys: &KeyRing, who: &str, seq: u64, nid: u32, body: TxBody) -> Tx {
    Tx::sign_body(keys, who, seq, "", 0, 1, nid, body).unwrap()
}

#[test]
fn pre_activation_order_txs_refused_and_state_bytes_identical() {
    let mut n = node(u64::MAX);
    assert!(!n.orderbook_open());
    let j = serde_json::to_string(&n.defi).unwrap();
    assert!(!j.contains("orderbook"), "empty book is not serialized");
    let mut keys = KeyRing::new(CryptoScheme::Falcon512);
    keys.generate_id("bob");
    let _ = n.keys.insert_public("bob", &keys.public_hex("bob").unwrap());
    let seq = n.state.get("bob").sequence;
    let tx = order_tx(&keys, "bob", seq, 0, TxBody::PlaceLimitOrder { pair: "USDC-FPL".into(), side: "buy".into(), price: 50, amount: 1_000_000_000, expiry_height: None });
    assert!(n.orderbook_tx_pre_activation(&tx));
    assert!(n.v3_deposit_pre_activation(&tx), "not kept / relayed / exported");
    let root = n.state.root();
    assert_eq!(n.submit_tx(tx.clone()).unwrap_err(), ORDERBOOK_NOT_ACTIVE);
    assert_eq!(n.apply_protocol_tx(&tx).unwrap_err(), ORDERBOOK_NOT_ACTIVE);
    assert!(!Node::is_permanent_tx_failure(ORDERBOOK_NOT_ACTIVE));
    assert_eq!(n.state.root(), root);
    n.orderbook_ledger_hook_for_test(101);
    assert_eq!(serde_json::to_string(&n.defi).unwrap(), j);
}

#[test]
fn single_2_10_4_height_gates_every_new_rule_on_2300() {
    use crate::params_v2::*;
    let nid = NETWORK_ID_PUBLIC_TESTNET_2300;
    let h = TESTNET_2300_V2_10_4_FROM_HEIGHT;
    assert_eq!(h, u64::MAX, "placeholder until Scott sets it");
    assert_eq!(btc4_lend_scale_from_height(nid), h);
    assert_eq!(orderbook_from_height(nid), h);
    assert_eq!(network_asset_pay_btc4_from_height(nid), h);
    assert_eq!(TESTNET_2300_BTC4_LEND_SCALE_HEIGHT, h);
    assert_eq!(TESTNET_2300_ASSET_PAY_BTC4_FROM_HEIGHT, h);
}

/// Three seats: signed order txs packed by one seat are verified (root
/// re-derived) and committed by all; books and roots match everywhere.
#[test]
fn seats_agree_on_order_book_ledgers() {
    let cfg = ChainConfig { pack_budget_ms: 0, max_txs_per_ledger: 16, soft_ok_votes: 1, min_fee: 0, fee_escalate: false, ..Default::default() };
    let mut net = Network::bootstrap(&["v1", "v2", "v3"], 1_000, &[("alice".into(), 1_000_000_000), ("bob".into(), 1_000_000_000)], cfg);
    net.pong_all();
    let nid = net.cfg.network_id;
    for n in net.nodes.values_mut() {
        n.orderbook_from_height = 0;
        n.create_pool("usdc-fpl", "USDC", "FPL").unwrap();
        let p = n.defi.pools.get_mut("usdc-fpl").unwrap();
        p.reserve_a = 1_000_000_000_000;
        p.reserve_b = 100_000_000_000;
        n.defi.credit_asset("alice", "USDC", 1_000_000_000_000);
        for u in ["alice", "bob"] {
            if let Some(pk) = net.user_keys.public_hex(u) {
                let _ = n.keys.insert_public(u, &pk);
            }
        }
    }
    let uk = net.user_keys.clone();
    let seq = |net: &Network, u: &str| net.nodes.values().next().unwrap().state.get(u).sequence;
    let t1 = order_tx(&uk, "alice", seq(&net, "alice"), nid, TxBody::PlaceLimitOrder { pair: "usdc-fpl".into(), side: "sell".into(), price: 100_000, amount: 50_000_000_000, expiry_height: None });
    let t2 = order_tx(&uk, "bob", seq(&net, "bob"), nid, TxBody::PlaceLimitOrder { pair: "usdc-fpl".into(), side: "buy".into(), price: 102_000, amount: 8_000_000_000, expiry_height: Some(50) });
    for n in net.nodes.values_mut() {
        n.submit_tx(t1.clone()).unwrap();
    }
    net.seal_next_height().unwrap();
    for n in net.nodes.values_mut() {
        n.submit_tx(t2.clone()).unwrap();
    }
    net.seal_next_height().unwrap();
    let t3 = order_tx(&uk, "bob", seq(&net, "bob"), nid, TxBody::CancelOrder { order_id: 1 });
    for n in net.nodes.values_mut() {
        let _ = n.submit_tx(t3.clone());
    }
    let _ = net.seal_next_height();
    let books: Vec<String> = net.nodes.values().map(|n| serde_json::to_string(&n.defi.orderbook).unwrap()).collect();
    let roots: Vec<String> = net.nodes.values().map(|n| n.commitment_root()).collect();
    assert!(books.windows(2).all(|w| w[0] == w[1]), "books diverged");
    assert!(roots.windows(2).all(|w| w[0] == w[1]), "roots diverged");
    let n = net.nodes.values().next().unwrap();
    assert!(!n.defi.orderbook.fills.is_empty(), "the buy matched");
    assert!(n.tip_height >= 2);
}
