//! v2.9.62 workstream B (rewards v2), gated on the epoch gate `E_v2`.

use super::*;
use crate::economy::{fee_split_v2, proposer_block_credit, settle_epoch_v2, EpochWork};

fn users() -> Vec<(String, u64)> {
    vec![
        ("alice".into(), 10_000_000),
        ("bob".into(), 10_000_000),
        ("carol".into(), 10_000_000),
        ("scott".into(), 1_000_000),
    ]
}

#[test]
fn b1_fee_split_golden() {
    assert_eq!(fee_split_v2(128), (64, 38, 26));
    assert_eq!(fee_split_v2(0), (0, 0, 0));
    let (b, p, r) = fee_split_v2(1_000_003);
    assert_eq!(b + p + r, 1_000_003);
}

#[test]
fn b3_proposer_credit_cap() {
    assert_eq!(proposer_block_credit(0), 64);
    assert_eq!(proposer_block_credit(10), 74);
    assert_eq!(proposer_block_credit(200), 128, "a full block is worth at most 2x an empty one");
}

fn bmap(v: &[(&str, u64)]) -> BTreeMap<String, u64> {
    v.iter().map(|(k, w)| (k.to_string(), *w)).collect()
}

#[test]
fn b2_b5_split_matrix() {
    let seats = ["v1", "v3", "v6", "v10", "v11", "v12", "v13"];
    let mut w = EpochWork::default();
    for s in seats {
        w.proposer_credit.insert(s.into(), 1_000);
        w.vote_credit.insert(s.into(), 500);
    }
    w.fee_reward_pool = 0;
    let mut floor: BTreeMap<String, u64> = seats.iter().map(|s| (s.to_string(), 100)).collect();
    floor.insert("v1".into(), 105);
    let treasury = 190_771_061_468u64;
    let st = settle_epoch_v2(treasury, 30, true, &w, &BTreeMap::new(), &BTreeMap::new(), &floor);
    assert_eq!(st.epoch_emit, 572_313_184);
    // No LP/watcher weights: their buckets fall into the validator pot.
    assert!(st.epoch_emit - st.validator_pot <= 4, "bucket rounding only");
    let prop_pool = st.validator_pot / 2;
    let vote_pool = st.validator_pot * 40 / 100;
    let floor_pool = st.validator_pot - prop_pool - vote_pool;
    assert_eq!(st.pack_pays["v12"], prop_pool / 7);
    assert_eq!(st.check_pays["v13"], vote_pool / 7);
    assert_eq!(st.floor_pays["v1"], floor_pool * 105 / 705);
    assert_eq!(st.floor_pays["v3"], floor_pool * 100 / 705);
    assert!(st.floor_pays["v1"] > st.floor_pays["v3"]);
    let paid: u64 = st.pack_pays.values().chain(st.check_pays.values()).chain(st.floor_pays.values()).sum();
    assert!(paid <= st.epoch_emit && st.epoch_emit - paid < 100, "dust only");
    // Per-seat total ≈ emit / 7 (≈ 81.8M here; the plan's 80.2M also pays LP/watchers).
    let v3: u64 = st.pack_pays["v3"] + st.check_pays["v3"] + st.floor_pays["v3"];
    assert!((81_000_000..82_500_000).contains(&v3), "{v3}");
    // A seat with no credits gets nothing from the proposer/vote pools.
    let mut w2 = w.clone();
    w2.proposer_credit.remove("v10");
    w2.vote_credit.remove("v10");
    let st2 = settle_epoch_v2(treasury, 30, true, &w2, &BTreeMap::new(), &BTreeMap::new(), &floor);
    assert!(!st2.pack_pays.contains_key("v10") && !st2.check_pays.contains_key("v10"));
}

fn armed_net(ids: &[&str]) -> Network {
    let cfg = ChainConfig { pack_budget_ms: 0, max_txs_per_ledger: 64, progress_timeout_ms: 0, ..Default::default() };
    let mut net = Network::bootstrap(ids, 1000, &users(), cfg);
    net.pong_all();
    for n in net.nodes.values_mut() {
        n.rewards_v2_from_height = 1;
        n.rewards_v2_epoch = 0;
    }
    net
}

#[test]
fn b1_b3_b4_commit_hook_credits_on_every_seat() {
    let mut net = armed_net(&["v1", "v2", "v3"]);
    let mut prop_total: BTreeMap<String, u64> = BTreeMap::new();
    for _ in 0..6 {
        for _ in 0..5 {
            net.payment("alice", "bob", 1, 10).unwrap();
        }
        let before: BTreeMap<String, u64> = net.nodes["v1"].claimable.clone();
        net.seal_next_height().unwrap();
        let led = net.nodes["v1"].chain.last().unwrap().clone();
        let fee_total: u64 = led.txs.iter().map(|t| t.fee).sum();
        let gained = net.nodes["v1"].claimable.get(&led.packer).copied().unwrap_or(0)
            - before.get(&led.packer).copied().unwrap_or(0);
        assert_eq!(gained, fee_total * 3 / 10, "proposer gets 30% of fees at commit");
        *prop_total.entry(led.packer.clone()).or_insert(0) += proposer_block_credit(led.txs.len() as u64);
    }
    for n in net.nodes.values() {
        assert_eq!(n.epoch_work.proposer_credit, prop_total);
        // Voters of each parent cert get +1 (harness certs carry all voters).
        let vc: u64 = n.epoch_work.vote_credit.values().sum();
        assert!(vc > 0, "vote credits recorded: {:?}", n.epoch_work.vote_credit);
    }
    let roots: BTreeSet<String> = net.nodes.values().map(|n| n.commitment_root()).collect();
    assert_eq!(roots.len(), 1);
}

#[test]
fn b_unarmed_commit_is_v1() {
    let cfg = ChainConfig { pack_budget_ms: 0, max_txs_per_ledger: 64, progress_timeout_ms: 0, ..Default::default() };
    let mut net = Network::bootstrap(&["v1", "v2", "v3"], 1000, &users(), cfg);
    net.pong_all();
    net.payment("alice", "bob", 1, 10).unwrap();
    net.seal_next_height().unwrap();
    for n in net.nodes.values() {
        assert!(n.epoch_work.proposer_credit.is_empty());
        assert!(n.epoch_work.vote_credit.is_empty());
        assert_eq!(n.epoch_work.fee_reward_pool, 5, "v1: 50% of fee to the pot");
        assert!(n.claimable.is_empty());
    }
}

fn lp_node() -> Node {
    let mut n = Node::new("v1", ChainConfig { epoch_ms: 100_000, emission_bps: 30, first_claim_epoch: 1, ..Default::default() }, KeyRing::new(CryptoScheme::HmacDev), &users());
    for v in ["v1", "v2", "v3", "v4", "v5", "v6", "v7"] {
        n.register_validator(v, 1000);
    }
    n.genesis_ms = n.now_ms;
    n.rewards_v2_from_height = 1;
    n.rewards_v2_epoch = 1;
    n
}

#[test]
fn b7_lp_value_weighting_equal_fpl_value_equal_weight() {
    let mut n = lp_node();
    for (pool, asset, units) in [("p-btc", "BTC", 1_000u64), ("p-eth", "ETH", 1_000_000_000_000), ("p-usdc", "USDC", 5_000)] {
        n.create_pool(pool, "FPL", asset).unwrap();
        let who = match asset { "BTC" => "alice", "ETH" => "bob", _ => "carol" };
        n.credit_asset_admin(who, asset, units).unwrap();
        n.add_liquidity(pool, who, 100_000, units).unwrap();
    }
    let (raw_amm, _) = (n.defi.aggregate_amm_lp_weights(), ());
    assert!(raw_amm["bob"] > raw_amm["alice"] * 100, "raw units are skewed by decimals: {raw_amm:?}");
    let (amm, _) = n.lp_value_weights_v2(&EpochWork::default());
    assert_eq!(amm["alice"], 200_000);
    assert_eq!(amm["bob"], 200_000);
    assert_eq!(amm["carol"], 200_000);
    // Hold rule: a position added after the epoch start counts only what was held.
    let mut w = EpochWork::default();
    w.lp_start_taken = true;
    w.lp_start = n.lp_units_now();
    let half = n.defi.pools["p-btc"].lp["alice"] / 2;
    w.lp_start.insert("amm|p-btc|alice".into(), half);
    w.lp_start.remove("amm|p-usdc|carol");
    let (amm2, _) = n.lp_value_weights_v2(&w);
    assert_eq!(amm2["alice"], 100_000);
    assert!(!amm2.contains_key("carol"), "just-in-time LP earns nothing this epoch");
    assert_eq!(amm2["bob"], 200_000);
}

#[test]
fn b7_lend_value_weighting() {
    let mut n = lp_node();
    n.create_market("m-usdc", "USDC").unwrap();
    n.create_market("m-btc", "BTC").unwrap();
    n.credit_asset_admin("alice", "USDC", 1_000_000_000).unwrap();
    n.credit_asset_admin("bob", "BTC", 100_000_000).unwrap();
    n.lend_supply("m-usdc", "alice", 1_000_000_000).unwrap(); // 1,000 USDC
    n.lend_supply("m-btc", "bob", 100_000_000).unwrap(); // 1 BTC
    n.defi.markets.get_mut("m-usdc").unwrap().price_native_per_scale = 10; // 10 FPL / USDC
    n.defi.markets.get_mut("m-btc").unwrap().price_native_per_scale = 10_000; // 10k FPL / BTC
    let (_, lend) = n.lp_value_weights_v2(&EpochWork::default());
    assert_eq!(lend["alice"], 10_000);
    assert_eq!(lend["bob"], 10_000);
}

#[test]
fn b2_b5_node_settle_v2_pays_floor_to_active_only() {
    let mut n = lp_node();
    n.validators.get_mut("v7").unwrap().inactive = true;
    n.validators.get_mut("v2").unwrap().archive = true;
    n.epoch_work.proposer_credit.insert("v1".into(), 640);
    n.epoch_work.vote_credit.insert("v3".into(), 10);
    let t0 = n.treasury;
    n.force_settle_current_epoch();
    let c = |id: &str| n.claimable.get(id).copied().unwrap_or(0);
    assert!(c("v1") > c("v4"), "proposer pool");
    assert!(c("v3") > c("v4"), "voter pool");
    assert!(c("v2") > c("v4"), "archive floor 1.05");
    assert!(c("v4") > 0, "floor for an active seat");
    assert_eq!(c("v7"), 0, "inactive seat: no floor");
    let ls = n.last_settle.clone().unwrap();
    assert_eq!(ls["formula"], "v2");
    let emit = ls["emit"].as_u64().unwrap();
    assert!(emit > 0 && n.treasury <= t0);
    // Hold-rule snapshot taken for the next epoch.
    assert!(n.epoch_work.lp_start_taken);
}

#[test]
fn presence_locks_prior_jail_and_scales_floor() {
    let mut n = lp_node();
    n.presence_from_height = 5;
    n.validators.get_mut("v1").unwrap().jail_count = 3;
    n.validators.get_mut("v2").unwrap().archive = true;
    let mut vals = n.validators.clone();
    n.rv2_ledger_hook(&mut vals, 5, "v3", Some("v3"), &["v3".into(), "v4".into()]);
    assert!(vals["v1"].earn_locked, "prior jail locks earnings at the presence height");
    assert!(!vals["v4"].earn_locked);
    assert_eq!(vals["v3"].earn_streak, 0, "unlocked seats do not track a streak");
    // 19 certificate votes do not unlock; the 20th does. A miss in between resets.
    for h in 6..25 {
        n.rv2_ledger_hook(&mut vals, h, "v4", Some("v4"), &["v1".into()]);
    }
    assert_eq!(vals["v1"].earn_streak, 19);
    assert!(vals["v1"].earn_locked);
    n.rv2_ledger_hook(&mut vals, 25, "v4", Some("v1"), &[]);
    assert_eq!(vals["v1"].earn_streak, 0, "missed own turn resets the streak");
    for h in 26..46 {
        n.rv2_ledger_hook(&mut vals, h, "v4", Some("v4"), &["v1".into()]);
    }
    assert!(!vals["v1"].earn_locked, "20 votes with no miss unlocks pay");
    n.validators = vals;
    n.epoch_work.presence_heights = 10;
    n.epoch_work.present.insert("v2".into(), 8);
    n.epoch_work.present.insert("v4".into(), 4);
    let w = n.floor_weights_v2();
    assert_eq!(w["v2"], 105 * 8 / 10, "archive base 105 scaled by uptime");
    assert!(!w.contains_key("v4"), "under half present: no floor");
}

#[test]
fn b5_set_archive_owner_only_and_revoke_by_admission() {
    let mut k = KeyRing::new(CryptoScheme::HmacDev);
    k.generate_many(["alice", "bob", "scott"]);
    std::env::set_var("FPL_ADMISSION_PUBKEY", k.public_hex("scott").unwrap());
    let mut n = lp_node();
    n.validators.get_mut("v2").unwrap().bond_account = "alice".into();
    let mk = |n: &Node, acct: &str, body: TxBody| {
        Tx::sign_body(&k, acct, n.state.get(acct).sequence, "", 0, 0, n.cfg.network_id, body).unwrap()
    };
    let t = mk(&n, "bob", TxBody::SetArchive { validator: "v2".into(), on: true });
    assert!(n.apply_protocol_tx(&t).unwrap_err().contains("not bond owner"));
    let t = mk(&n, "alice", TxBody::SetArchive { validator: "v2".into(), on: true });
    n.apply_protocol_tx(&t).unwrap();
    assert!(n.validators["v2"].archive);
    assert_eq!(n.commitment_root(), n.commitment_root_fresh());
    let t = mk(&n, "bob", TxBody::RevokeArchive { id: "v2".into() });
    assert!(n.apply_protocol_tx(&t).unwrap_err().contains("admission"));
    let t = mk(&n, "scott", TxBody::RevokeArchive { id: "v2".into() });
    n.apply_protocol_tx(&t).unwrap();
    assert!(!n.validators["v2"].archive);
}

#[test]
fn b_unarmed_settle_is_v1_formula() {
    let mut n = lp_node();
    n.rewards_v2_from_height = u64::MAX;
    n.epoch_work.record_pack("v1", 10);
    n.force_settle_current_epoch();
    assert_eq!(n.last_settle.clone().unwrap()["formula"], "v1");
    assert!(!n.epoch_work.lp_start_taken);
}

/// Cross-language golden: the same inputs and numbers are asserted in
/// docs/ops/tools/test_pl_rewards_check.py (`test_v2_golden_matches_rust`).
#[test]
fn b2_cross_language_golden_with_check_script() {
    let mut w = EpochWork::default();
    w.fee_reward_pool = 12_345;
    w.proposer_credit = bmap(&[("v1", 6400), ("v3", 7000), ("v12", 128)]);
    w.vote_credit = bmap(&[("v1", 900), ("v3", 1000), ("v12", 10)]);
    w.watcher_work = bmap(&[("w1", 300), ("w2", 40)]);
    w.watcher_slots = bmap(&[("w1", 168), ("w2", 84)]);
    let floor = bmap(&[("v1", 105), ("v3", 100), ("v12", 100)]);
    let amm = bmap(&[("lpA", 3_000_000), ("lpB", 1)]);
    let st = settle_epoch_v2(190_771_061_468, 30, true, &w, &amm, &BTreeMap::new(), &floor);
    assert_eq!(st.epoch_emit, 572_313_184);
    assert_eq!(st.validator_pot, 568_891_612);
    assert_eq!(st.pack_pays, bmap(&[("v1", 134_569_275), ("v12", 2_691_385), ("v3", 147_185_145)]));
    assert_eq!(st.check_pays, bmap(&[("v1", 107_225_643), ("v12", 1_191_396), ("v3", 119_139_604)]));
    assert_eq!(st.floor_pays, bmap(&[("v1", 19_584_793), ("v12", 18_652_184), ("v3", 18_652_184)]));
    assert_eq!(st.watcher_pays, bmap(&[("w1", 286_156), ("w2", 286_156)]));
    assert_eq!(st.amm_pays, bmap(&[("lpA", 2_861_565), ("lpB", 38)]));
    assert!(st.lend_pays.is_empty());
}

/// A settle ledger with `fee_total == 0` takes the legacy (non-incremental)
/// path. In a v2 epoch it must still use the committed 20 % pot, not the v1
/// fold over chain bodies.
#[test]
fn b2_v2_settle_ignores_legacy_fold() {
    let mut n = lp_node();
    n.epoch_work.fee_reward_pool = 1_000;
    n.epoch_work.proposer_credit.insert("v1".into(), 64);
    let w = n.epoch_work_from_chain(1, false);
    assert_eq!(w.fee_reward_pool, 1_000);
    assert_eq!(w.proposer_credit["v1"], 64);
    n.rewards_v2_from_height = u64::MAX;
    let w1 = n.epoch_work_from_chain(1, false);
    assert_eq!(w1.fee_reward_pool, 0, "v1 legacy fold unchanged (empty chain)");
}
