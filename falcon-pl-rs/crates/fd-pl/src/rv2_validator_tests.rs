//! v2.9.62 workstream A validator-set rules (A3, A6–A11), gated on `H_act`.

use super::*;
use crate::params_v2::{pop_message, MIN_BOND_V2};

const ADMIN: &str = "scott";

fn users() -> Vec<(String, u64)> {
    vec![
        ("alice".into(), 1_000_000),
        ("bob".into(), 1_000_000),
        (ADMIN.into(), 1_000_000),
    ]
}

fn ring() -> KeyRing {
    let mut k = KeyRing::new(CryptoScheme::HmacDev);
    k.generate_many(["alice", "bob", ADMIN, "v1", "v9", "v10"]);
    k
}

fn set_admission_env(k: &KeyRing) {
    // Same value in every test, read only by the admission check.
    std::env::set_var("FPL_ADMISSION_PUBKEY", k.public_hex(ADMIN).unwrap());
}

fn node(armed: bool) -> Node {
    let mut n = Node::new("v1", ChainConfig { min_fee: 0, ..Default::default() }, ring(), &users());
    n.register_validator("v1", 1000);
    if armed {
        arm(&mut n);
    }
    n
}

fn arm(n: &mut Node) {
    n.rewards_v2_from_height = n.tip_height + 1;
    n.rewards_v2_epoch = 0;
}

fn tx(n: &Node, k: &KeyRing, acct: &str, amount: u64, body: TxBody) -> Tx {
    let seq = n.state.get(acct).sequence;
    Tx::sign_body(k, acct, seq, "", amount, 0, n.cfg.network_id, body).unwrap()
}

fn approve(n: &mut Node, k: &KeyRing, id: &str, pk: &str, exp: u64) -> Result<(), String> {
    let t = tx(n, k, ADMIN, 0, TxBody::ApproveValidator { id: id.into(), public_key: pk.into(), expires_height: exp });
    n.apply_protocol_tx(&t)
}

fn bond_v2(n: &mut Node, k: &KeyRing, from: &str, id: &str, amount: u64, sig_override: Option<&str>) -> Result<(), String> {
    let pk = k.public_hex(id).unwrap();
    let sig = match sig_override {
        Some(s) => s.to_string(),
        None => k.sign(id, &pop_message(n.cfg.network_id, id, from)).unwrap(),
    };
    let t = tx(n, k, from, amount, TxBody::BondV2 { validator: id.into(), public_key: pk, pop_sig: sig });
    n.apply_protocol_tx(&t)
}

#[test]
fn unarmed_rejects_v2_txs_and_keeps_legacy_bond() {
    let k = ring();
    set_admission_env(&k);
    let mut n = node(false);
    assert!(!n.rv2_open());
    let e = bond_v2(&mut n, &k, "alice", "v9", MIN_BOND_V2, None).unwrap_err();
    assert!(e.contains("rv2 not active"), "{e}");
    let e = approve(&mut n, &k, "v9", "x", 100).unwrap_err();
    assert!(e.contains("rv2 not active"), "{e}");
    // Legacy: anyone can create a 1,000 seat and top up others (unchanged).
    let t = tx(&n, &k, "alice", 1000, TxBody::Bond { validator: "v9".into() });
    n.apply_protocol_tx(&t).unwrap();
    let t = tx(&n, &k, "bob", 5, TxBody::Bond { validator: "v9".into() });
    n.apply_protocol_tx(&t).unwrap();
    assert_eq!(n.validators["v9"].bond, 1005);
    // Mempool refuses v2 kinds before H_act (2.9.61 seats can't decode them).
    let t = tx(&n, &k, ADMIN, 0, TxBody::Reactivate { validator: "v9".into() });
    assert!(n.rv2_tx_pre_activation(&t));
    assert_eq!(n.submit_tx(t).unwrap_err(), RV2_NOT_ACTIVE);
    // Root bytes: no v2 extension while every v2 field is default.
    assert!(Node::val_v2_slim(&n.validators).is_empty());
    assert_eq!(n.commitment_root(), n.commitment_root_fresh());
}

#[test]
fn a7_owner_only_top_up_and_a9_grandfathered() {
    let k = ring();
    let mut n = node(false);
    let t = tx(&n, &k, "alice", 1000, TxBody::Bond { validator: "v9".into() });
    n.apply_protocol_tx(&t).unwrap();
    arm(&mut n);
    let t = tx(&n, &k, "bob", 5, TxBody::Bond { validator: "v9".into() });
    let e = n.apply_protocol_tx(&t).unwrap_err();
    assert!(e.contains("not bond owner"), "{e}");
    assert!(Node::is_permanent_tx_failure(&e));
    assert_eq!(n.validators["v9"].bond, 1000);
    // Owner top-up of 1 on a grandfathered seat is fine.
    let t = tx(&n, &k, "alice", 1, TxBody::Bond { validator: "v9".into() });
    n.apply_protocol_tx(&t).unwrap();
    assert_eq!(n.validators["v9"].bond, 1001);
    assert!(!n.validators["v9"].v2_seat);
    assert!(n.validators["v9"].lottery_weight() > 0.0, "grandfathered below 50k stays eligible");
    // Legacy Bond can no longer create a seat.
    let t = tx(&n, &k, "alice", MIN_BOND_V2, TxBody::Bond { validator: "v10".into() });
    let e = n.apply_protocol_tx(&t).unwrap_err();
    assert!(e.contains("BondV2"), "{e}");
}

#[test]
fn a6_a9_a10_bond_v2_needs_approval_min_and_pop() {
    let k = ring();
    set_admission_env(&k);
    let mut n = node(true);
    let pk = k.public_hex("v9").unwrap();
    let e = bond_v2(&mut n, &k, "alice", "v9", MIN_BOND_V2, None).unwrap_err();
    assert!(e.contains("not approved"), "{e}");
    // Only the admission key may approve.
    let t = tx(&n, &k, "alice", 0, TxBody::ApproveValidator { id: "v9".into(), public_key: pk.clone(), expires_height: 1000 });
    let e = n.apply_protocol_tx(&t).unwrap_err();
    assert!(e.contains("not the admission key"), "{e}");
    let root0 = n.commitment_root();
    approve(&mut n, &k, "v9", &pk, 1000).unwrap();
    assert_ne!(n.commitment_root(), root0, "approval is in the root");
    assert_eq!(n.commitment_root(), n.commitment_root_fresh());
    let e = bond_v2(&mut n, &k, "alice", "v9", MIN_BOND_V2 - 1, None).unwrap_err();
    assert!(e.contains("bond below min"), "{e}");
    let bad = k.sign("v9", "something else").unwrap();
    let e = bond_v2(&mut n, &k, "alice", "v9", MIN_BOND_V2, Some(&bad)).unwrap_err();
    assert!(e.contains("proof of possession"), "{e}");
    let bal = n.state.get("alice").balance;
    bond_v2(&mut n, &k, "alice", "v9", MIN_BOND_V2, None).unwrap();
    let v = &n.validators["v9"];
    assert!(v.v2_seat && v.bond == MIN_BOND_V2 && v.bond_account == "alice");
    assert!(v.public_key.eq_ignore_ascii_case(&pk));
    assert_eq!(n.state.get("alice").balance, bal - MIN_BOND_V2);
    assert!(!n.defi.validator_approvals.contains_key("v9"), "approval consumed");
    let e = bond_v2(&mut n, &k, "alice", "v9", MIN_BOND_V2, None).unwrap_err();
    assert!(e.contains("exists"), "{e}");
    assert!(!Node::val_v2_slim(&n.validators).is_empty());
    assert_eq!(n.commitment_root(), n.commitment_root_fresh());
    // A v2 seat that unbonds below 50k leaves the lottery.
    n.request_unbond("v9", 1).unwrap();
    assert_eq!(n.validators["v9"].lottery_weight(), 0.0);
}

#[test]
fn a10_expired_and_revoked_approvals() {
    let k = ring();
    set_admission_env(&k);
    let mut n = node(true);
    let pk = k.public_hex("v9").unwrap();
    approve(&mut n, &k, "v9", &pk, 5).unwrap();
    n.tip_height = 5; // open height 6 > expiry
    let e = bond_v2(&mut n, &k, "alice", "v9", MIN_BOND_V2, None).unwrap_err();
    assert!(e.contains("expired"), "{e}");
    let t = tx(&n, &k, ADMIN, 0, TxBody::RevokeApproval { id: "v9".into() });
    n.apply_protocol_tx(&t).unwrap();
    assert!(n.defi.validator_approvals.is_empty());
    // Wrong key for the approved id.
    approve(&mut n, &k, "v9", &k.public_hex("v10").unwrap(), 100).unwrap();
    let e = bond_v2(&mut n, &k, "alice", "v9", MIN_BOND_V2, None).unwrap_err();
    assert!(e.contains("approved key differs"), "{e}");
}

#[test]
fn a10_admission_key_armed_on_2300() {
    // Arming PR: the public admission key generated on host1 (path in HANDOVER §10).
    let k = crate::params_v2::ADMISSION_PUBKEY_2300;
    assert_eq!(k.len(), 1794);
    assert!(crate::crypto::PublicKey::from_hex(crate::crypto::CryptoScheme::Falcon512, k).is_ok());
    assert_eq!(crate::params_v2::network_admission_pubkey(NETWORK_ID_PUBLIC_TESTNET_2300), k);
}

fn seat(id: &str, bond: u64, jc: u32) -> ValidatorInfo {
    let mut v = ValidatorInfo::new(id, bond, String::new());
    v.jail_count = jc;
    v
}

#[test]
fn a8_migration_moves_unjail_payments_to_escrow() {
    let n = node(true);
    let mut vals: BTreeMap<String, ValidatorInfo> = BTreeMap::new();
    vals.insert("v1".into(), seat("v1", 8_000, 3));
    vals.insert("v12".into(), seat("v12", 16_000, 4));
    vals.insert("v3".into(), seat("v3", 1_000, 0));
    vals.insert("v6".into(), seat("v6", 1_500, 2)); // topped up less than it paid
    n.rv2_ledger_hook(&mut vals, n.rewards_v2_from_height, "v3", None, &[]);
    assert_eq!((vals["v1"].bond, vals["v1"].escrow), (1_000, 7_000));
    assert_eq!((vals["v12"].bond, vals["v12"].escrow), (1_000, 15_000));
    assert_eq!((vals["v3"].bond, vals["v3"].escrow), (1_000, 0));
    assert_eq!((vals["v6"].bond, vals["v6"].escrow), (1_000, 500));
    // Only once: the next height doesn't move more.
    n.rv2_ledger_hook(&mut vals, n.rewards_v2_from_height + 1, "v3", None, &[]);
    assert_eq!(vals["v1"].escrow, 7_000);
    // Unarmed: nothing happens.
    let u = node(false);
    let mut vals2 = BTreeMap::new();
    vals2.insert("v1".to_string(), seat("v1", 8_000, 3));
    u.rv2_ledger_hook(&mut vals2, 1, "v1", None, &[]);
    assert_eq!(vals2["v1"].escrow, 0);
}

#[test]
fn a8_unjail_to_escrow_clean_exit_returns_slash_seizes() {
    let k = ring();
    let mut n = node(true);
    n.validators.get_mut("v1").unwrap().jailed = true;
    n.validators.get_mut("v1").unwrap().jail_count = 1;
    let t = tx(&n, &k, "alice", 2_000, TxBody::Unjail { validator: "v1".into() });
    n.apply_protocol_tx(&t).unwrap();
    let v = &n.validators["v1"];
    assert!(!v.jailed);
    assert_eq!((v.bond, v.escrow), (1_000, 2_000), "payment is escrow, not bond");
    // Clean full exit returns bond + escrow.
    n.request_unbond("v1", 1_000).unwrap();
    assert_eq!(n.validators["v1"].unbonding, 3_000);
    assert_eq!(n.validators["v1"].escrow, 0);
    // A slash seizes escrow too.
    let mut vals = BTreeMap::new();
    let mut s = seat("v2", 1_000, 1);
    s.escrow = 4_000;
    vals.insert("v2".to_string(), s);
    let ev = SlashEvidence { offender: "v2".into(), hash_a: "a".into(), hash_b: "b".into(), ..Default::default() };
    let seized = Node::apply_slash_evidence(&mut vals, &[ev]);
    assert_eq!(seized, 5_000);
    assert_eq!(vals["v2"].escrow, 0);
}

fn seven() -> BTreeMap<String, ValidatorInfo> {
    ["v1", "v3", "v6", "v10", "v11", "v12", "v13"]
        .iter()
        .map(|id| (id.to_string(), seat(id, 1_000, 0)))
        .collect()
}

#[test]
fn a11_missed_turns_inactive_floor_and_reactivate() {
    let k = ring();
    let mut n = node(true);
    let h = n.rewards_v2_from_height + 1;
    let mut vals = seven();
    for i in 0..19 {
        n.rv2_ledger_hook(&mut vals, h + i, "v1", Some("v10"), &[]);
    }
    assert_eq!(vals["v10"].missed_turns, 19);
    assert!(!vals["v10"].inactive);
    n.rv2_ledger_hook(&mut vals, h + 19, "v1", Some("v10"), &[]);
    assert!(vals["v10"].inactive, "20 missed → inactive");
    assert_eq!(vals["v10"].lottery_weight(), 0.0);
    // Never below 6 eligible: with 6 left, v11 can't be inactivated.
    for i in 0..40 {
        n.rv2_ledger_hook(&mut vals, h + 20 + i, "v1", Some("v11"), &[]);
    }
    assert!(!vals["v11"].inactive);
    assert!(vals["v11"].missed_turns >= 20);
    // Sealing its own turn resets the counter.
    n.rv2_ledger_hook(&mut vals, h + 100, "v11", Some("v11"), &[]);
    assert_eq!(vals["v11"].missed_turns, 0);
    // Reactivate: owner only.
    let mut v9 = seat("v9", 1_000, 0);
    v9.bond_account = "alice".into();
    v9.inactive = true;
    v9.missed_turns = 20;
    n.validators.insert("v9".into(), v9);
    let t = tx(&n, &k, "bob", 0, TxBody::Reactivate { validator: "v9".into() });
    assert!(n.apply_protocol_tx(&t).unwrap_err().contains("not bond owner"));
    let t = tx(&n, &k, "alice", 0, TxBody::Reactivate { validator: "v9".into() });
    n.apply_protocol_tx(&t).unwrap();
    assert!(!n.validators["v9"].inactive);
    assert_eq!(n.validators["v9"].missed_turns, 0);
}

#[test]
fn catchup_miss_is_not_charged_and_clears_when_the_seat_votes() {
    let mut n = node(true);
    let h0 = n.rewards_v2_from_height + 1;
    n.catchup_miss_from_height = h0;
    let mut vals = seven();
    let window = crate::params_v2::CATCHUP_MISS_WINDOW;
    // Seen recently: misses still count.
    vals.get_mut("v10").unwrap().last_active_height = h0;
    for i in 0..5 {
        n.rv2_ledger_hook(&mut vals, h0 + i, "v1", Some("v10"), &["v1".into()]);
    }
    assert_eq!(vals["v10"].missed_turns, 5);
    assert!(!vals["v10"].inactive);
    // Quiet for longer than the window: further misses do not count.
    let stale = h0 + window + 10;
    vals.get_mut("v10").unwrap().last_active_height = h0;
    let before = vals["v10"].missed_turns;
    for i in 0..30 {
        n.rv2_ledger_hook(&mut vals, stale + i, "v1", Some("v10"), &["v1".into()]);
    }
    assert_eq!(vals["v10"].missed_turns, before, "behind: misses stay put");
    assert!(!vals["v10"].inactive);
    // Force the inactive floor, then a vote clears it.
    vals.get_mut("v10").unwrap().inactive = true;
    vals.get_mut("v10").unwrap().missed_turns = 20;
    n.rv2_ledger_hook(&mut vals, stale + 40, "v1", Some("v3"), &["v10".into()]);
    assert!(!vals["v10"].inactive, "a vote means it has caught up");
    assert_eq!(vals["v10"].missed_turns, 0);
    assert_eq!(vals["v10"].last_active_height, stale + 40);
}

fn lottery_shares(armed: bool) -> BTreeMap<String, u64> {
    let mut n = node(false);
    n.validators.clear();
    for (id, bond) in [("v1", 1_000u64), ("v3", 1_000), ("v6", 1_000), ("v10", 1_000), ("v11", 1_000), ("v12", 16_000), ("v13", 1_000)] {
        n.validators.insert(id.into(), seat(id, bond, 0));
    }
    if armed {
        n.rewards_v2_from_height = 1;
    }
    let mut counts = BTreeMap::new();
    for i in 0..7_000u64 {
        n.tip_height = i + 1;
        n.tip_hash = sha256_hex(format!("tip{i}").as_bytes());
        let w = n.lottery_packer_with_skips(&BTreeSet::new()).unwrap();
        *counts.entry(w).or_insert(0u64) += 1;
    }
    counts
}

#[test]
fn a3_equal_lottery_odds_from_h_act() {
    let eq = lottery_shares(true);
    for (id, c) in &eq {
        let share = *c as f64 / 7_000.0;
        assert!((share - 1.0 / 7.0).abs() < 0.02, "{id} share {share}");
    }
    let legacy = lottery_shares(false);
    assert!(legacy["v12"] as f64 / 7_000.0 > 0.6, "legacy is bond-weighted: {legacy:?}");
}

#[test]
fn network_seals_across_h_act_with_identical_roots() {
    let cfg = ChainConfig { pack_budget_ms: 0, max_txs_per_ledger: 8, progress_timeout_ms: 0, ..Default::default() };
    let mut net = Network::bootstrap(&["v1", "v2", "v3"], 1000, &users(), cfg);
    net.pong_all();
    fn seal(net: &mut Network) {
        let seq = net.account_seqs["alice"];
        let t = Tx::sign_body(&net.user_keys, "alice", seq, "bob", 1, 2, net.cfg.network_id, TxBody::Pay).unwrap();
        *net.account_seqs.get_mut("alice").unwrap() += 1;
        for n in net.nodes.values_mut() {
            n.submit_tx(t.clone()).unwrap();
        }
        net.seal_next_height().unwrap();
    }
    for _ in 0..3 {
        seal(&mut net);
    }
    // v2 carries a past unjail (jail_count 2, bond 4,000 → escrow 3,000 at H_act).
    let h_act = net.tip_height() + 2;
    for n in net.nodes.values_mut() {
        let v = n.validators.get_mut("v2").unwrap();
        v.bond = 4_000;
        v.jail_count = 2;
        n.rewards_v2_from_height = h_act;
        n.rewards_v2_epoch = 0;
    }
    for _ in 0..6 {
        seal(&mut net);
    }
    assert!(net.tip_height() >= h_act + 4);
    let roots: BTreeSet<String> = net.nodes.values().map(|n| n.commitment_root()).collect();
    assert_eq!(roots.len(), 1, "all seats agree after H_act");
    for n in net.nodes.values() {
        assert_eq!((n.validators["v2"].bond, n.validators["v2"].escrow), (1_000, 3_000));
        assert_eq!(n.commitment_root(), n.commitment_root_fresh());
    }
}

#[test]
fn a4_seed_v2_ignores_packer_signature() {
    let msg = "header-msg";
    let a = seed_v2_hex("parent", 10, msg);
    assert_eq!(a, seed_v2_hex("parent", 10, msg), "re-sign gives the same seed");
    assert_ne!(a, seed_v2_hex("parent", 11, msg));
    assert_ne!(a, seed_v2_hex("parent2", 10, msg));
}

#[test]
fn a5_sealed_at_bound() {
    let now = 1_000_000u64;
    assert!(sealed_at_v2_ok(now + 5_000, now - 3_000, now));
    assert!(!sealed_at_v2_ok(now + 10_000, now, now), "+10 s packer refused");
    // Chain already 20 s ahead of wall time: small steps still certify.
    let parent = now + 20_000;
    assert!(sealed_at_v2_ok(parent + 1, parent, now));
    assert!(sealed_at_v2_ok(parent + 1_000, parent, now));
    assert!(!sealed_at_v2_ok(parent + 1_001, parent, now));
}

#[test]
fn network_seals_with_v2_seed_and_time_rules() {
    // Same as the H_act harness test but long enough that the v8 committee is
    // drawn from seed_v2 on every seat (verify == pack).
    let cfg = ChainConfig { pack_budget_ms: 0, max_txs_per_ledger: 8, progress_timeout_ms: 0, ..Default::default() };
    let mut net = Network::bootstrap(&["v1", "v2", "v3"], 1000, &users(), cfg);
    net.pong_all();
    for n in net.nodes.values_mut() {
        n.rewards_v2_from_height = 2;
    }
    for i in 0..12u64 {
        let seq = net.account_seqs["alice"];
        let t = Tx::sign_body(&net.user_keys, "alice", seq, "bob", 1, 2, net.cfg.network_id, TxBody::Pay).unwrap();
        *net.account_seqs.get_mut("alice").unwrap() += 1;
        for n in net.nodes.values_mut() {
            n.submit_tx(t.clone()).unwrap();
            n.tick(3_000);
        }
        net.seal_next_height().unwrap_or_else(|e| panic!("height {i}: {e}"));
    }
    let roots: BTreeSet<String> = net.nodes.values().map(|n| n.commitment_root()).collect();
    assert_eq!(roots.len(), 1);
    // Ledger::seed_v2 does not read packer_sig.
    let mut l = net.nodes["v1"].chain.last().unwrap().clone();
    let s1 = l.seed_v2();
    l.packer_sig = "bb".into();
    assert_eq!(s1, l.seed_v2());
}

#[test]
fn v4_network_seals_with_seed_v2_equal_lottery_across_h_act() {
    let ids = ["v1", "v2", "v3", "v4", "v5", "v6", "v7"];
    let mut net = Network::bootstrap(&ids, 1000, &users(), ChainConfig { pack_budget_ms: 0, ..Default::default() });
    net.pong_all();
    for n in net.nodes.values_mut() {
        n.qc_header_version = 4;
        n.qc_start_height = 1;
        n.rewards_v2_from_height = 4; // cross H_act mid-run
        n.rewards_v2_epoch = 0;
        let v = n.validators.get_mut("v3").unwrap();
        v.bond = 8_000;
        v.jail_count = 3;
    }
    for h in 1..=14u64 {
        net.payment("alice", "bob", 1, 1).unwrap();
        let r = net.seal_next_height().unwrap_or_else(|e| panic!("h={h}: {e}"));
        assert_eq!(r.height, h);
    }
    for n in net.nodes.values() {
        let led = n.chain.last().unwrap();
        assert!(led.is_v4());
        assert!(n.rv2_at(led.height));
        assert_eq!(n.lottery_tip_seed(), led.seed_v2());
        assert_eq!((n.validators["v3"].bond, n.validators["v3"].escrow), (1_000, 7_000));
    }
    let roots: BTreeSet<String> = net.nodes.values().map(|n| n.commitment_root()).collect();
    assert_eq!(roots.len(), 1, "all seats agree");
    let tips: BTreeSet<String> = net.nodes.values().map(|n| n.tip_hash.clone()).collect();
    assert_eq!(tips.len(), 1);
}
