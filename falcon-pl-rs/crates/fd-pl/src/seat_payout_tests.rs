//! Seat payout: validator rewards kept per seat (`ValidatorInfo::seat_claimable`),
//! `SetSeatPayout` / `ClaimSeat` signed with the seat key by `<id>#seat`,
//! one-off migration at the activation height, nothing changes before it.

use super::*;

const NID: u32 = crate::NETWORK_ID_PUBLIC_TESTNET_2300;
/// The production height: the same block as Fix B (on-chain wallet keys) and
/// the bridge, so the activation-block tests below also cover that overlap.
const H: u64 = crate::params_v2::TESTNET_2300_SEAT_PAYOUT_FROM_HEIGHT;

#[test]
fn activation_shares_the_fix_b_and_bridge_height() {
    use crate::params_v2::*;
    assert_eq!(H, 692_000);
    assert_eq!(H, TESTNET_2300_KEY_BIND_ONCHAIN_FROM_HEIGHT);
    assert_eq!(H, TESTNET_2300_BRIDGE_FROM_HEIGHT);
    assert_eq!(seat_payout_from_height(NID), H);
    assert!(ring_restore_from_height(NID) > H, "ring restore stays later");
}

fn users() -> Vec<(String, u64)> {
    vec![("alice".into(), 1_000_000), ("bob".into(), 1_000_000)]
}

/// Falcon node with seat `nodesafe` (seat key B registered) and a wallet
/// account `nodesafe` whose on-chain key is A. Returns (node, wallet A, seat B).
fn setup(tip: u64) -> (Node, KeyRing, KeyRing) {
    let mut n = Node::new(
        "v1",
        ChainConfig { scheme: CryptoScheme::Falcon512, network_id: NID, pack_budget_ms: 0, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &[("nodesafe".into(), 1_000_000), ("alice".into(), 1_000)],
    );
    let mut seat = KeyRing::new(CryptoScheme::Falcon512);
    seat.generate_id("nodesafe");
    let b = seat.public_hex("nodesafe").unwrap();
    let mut wallet = KeyRing::new(CryptoScheme::Falcon512);
    wallet.generate_id("nodesafe");
    let a = wallet.public_hex("nodesafe").unwrap();
    let mut acct = n.state.get("nodesafe");
    acct.meta.public_key = a;
    n.state.accounts.insert("nodesafe".into(), acct);
    n.seat_key_registry.insert("nodesafe".into(), b.clone());
    n.validators.insert("nodesafe".into(), ValidatorInfo::new("nodesafe", 50_000, b));
    n.tip_height = tip;
    n.ring_restore_from_height = crate::params_v2::ring_restore_from_height(NID);
    n.seat_payout_from_height = H;
    (n, wallet, seat)
}

fn seat_tx(keys: &KeyRing, n: &Node, fee: u64, body: TxBody) -> Tx {
    let seq = n.state.get(&seat_sender("nodesafe")).sequence;
    Tx::sign_seat_body(keys, "nodesafe", seq, fee, NID, body).unwrap()
}

fn set_payout(p: Option<&str>) -> TxBody {
    TxBody::SetSeatPayout { validator: "nodesafe".into(), payout_account: p.map(str::to_string) }
}

fn claim_seat(to: &str) -> TxBody {
    TxBody::ClaimSeat { validator: "nodesafe".into(), to: to.into() }
}

#[test]
fn pre_activation_seat_txs_rejected_and_state_unchanged() {
    let (mut n, _w, seat) = setup(H - 2); // applies at H - 1
    let root0 = n.commitment_root();
    for body in [set_payout(Some("alice")), claim_seat("alice")] {
        let tx = seat_tx(&seat, &n, 0, body.clone());
        assert!(n.seat_tx_pre_activation(&tx));
        assert_eq!(n.submit_tx(tx.clone()).unwrap_err(), SEAT_PAYOUT_NOT_ACTIVE, "{body:?}");
        assert_eq!(n.apply_protocol_tx(&tx).unwrap_err(), SEAT_PAYOUT_NOT_ACTIVE, "{body:?}");
        // Not relayed / exported either.
        assert!(n.v3_deposit_pre_activation(&tx));
    }
    assert_eq!(n.commitment_root(), root0, "rejected seat txs must not touch state");
    assert!(n.state.accounts.get(&seat_sender("nodesafe")).is_none());
    // Seat map empty -> root has no 9th element (byte-identical history).
    assert!(Node::seat_slim(&n.validators).is_empty());
}

#[test]
fn set_payout_signed_with_seat_key_from_activation() {
    let (mut n, wallet, seat) = setup(H - 1); // applies at H
    let tx = seat_tx(&seat, &n, 0, set_payout(Some("alice")));
    assert!(!n.seat_tx_pre_activation(&tx));
    n.apply_protocol_tx(&tx).unwrap();
    assert_eq!(n.validators["nodesafe"].seat_payout.as_deref(), Some("alice"));
    assert_eq!(n.state.get(&seat_sender("nodesafe")).sequence, 1);
    // Wallet key A cannot act for the seat, even with the right sender.
    let bad = seat_tx(&wallet, &n, 0, set_payout(Some("bob")));
    assert!(n.apply_protocol_tx(&bad).is_err());
    // Seat body from the wallet account itself is refused.
    let seq = n.state.get("nodesafe").sequence;
    let from_wallet = Tx::sign_body(&wallet, "nodesafe", seq, "", 0, 0, NID, set_payout(Some("bob"))).unwrap();
    assert!(n.apply_protocol_tx(&from_wallet).is_err());
    assert_eq!(n.validators["nodesafe"].seat_payout.as_deref(), Some("alice"));
    // Clear.
    let tx = seat_tx(&seat, &n, 0, set_payout(None));
    n.apply_protocol_tx(&tx).unwrap();
    assert_eq!(n.validators["nodesafe"].seat_payout, None);
}

#[test]
fn payout_set_credits_balance_else_bucket() {
    let (mut n, _w, _s) = setup(H);
    let alice0 = n.state.get("alice").balance;
    Node::credit_seat_reward(&mut n.validators, &mut n.state, &mut n.claimable, "nodesafe", 40);
    assert_eq!(n.validators["nodesafe"].seat_claimable, 40);
    n.validators.get_mut("nodesafe").unwrap().seat_payout = Some("alice".into());
    Node::credit_seat_reward(&mut n.validators, &mut n.state, &mut n.claimable, "nodesafe", 60);
    assert_eq!(n.state.get("alice").balance, alice0 + 60);
    assert_eq!(n.validators["nodesafe"].seat_claimable, 40);
    assert!(n.claimable.get("nodesafe").is_none(), "nothing reaches the wallet map");
    // Non-seat ids keep the legacy account credit.
    Node::credit_seat_reward(&mut n.validators, &mut n.state, &mut n.claimable, "watch-x", 5);
    assert_eq!(n.claimable.get("watch-x"), Some(&5));
}

#[test]
fn shared_name_wallet_claim_cannot_take_seat_rewards_and_claim_seat_can() {
    let (mut n, wallet, seat) = setup(H);
    n.validators.get_mut("nodesafe").unwrap().seat_claimable = 1_000;
    n.claimable.insert("nodesafe".into(), 30); // e.g. watcher / AMM pay
    let bal0 = n.state.get("nodesafe").balance;
    // Website passkey Claim of the wallet `nodesafe` (key A).
    let seq = n.state.get("nodesafe").sequence;
    let claim = Tx::sign_body(&wallet, "nodesafe", seq, "", 0, 2, NID, TxBody::Claim).unwrap();
    n.apply_protocol_tx(&claim).unwrap();
    assert_eq!(n.state.get("nodesafe").balance, bal0 - 2 + 30, "wallet gets only account rewards");
    assert_eq!(n.validators["nodesafe"].seat_claimable, 1_000, "seat bucket untouched");
    // Manual seat claim (seat key), fee 2 from the bucket.
    let alice0 = n.state.get("alice").balance;
    let tx = seat_tx(&seat, &n, 2, claim_seat("alice"));
    n.apply_protocol_tx(&tx).unwrap();
    assert_eq!(n.state.get("alice").balance, alice0 + 998);
    assert_eq!(n.validators["nodesafe"].seat_claimable, 0);
    // Empty bucket and no sender balance: fee cannot be paid.
    let tx = seat_tx(&seat, &n, 2, claim_seat("alice"));
    assert!(n.apply_protocol_tx(&tx).unwrap_err().contains("insufficient balance"));
    // Wallet key cannot claim the seat bucket.
    n.validators.get_mut("nodesafe").unwrap().seat_claimable = 10;
    let bad = seat_tx(&wallet, &n, 0, claim_seat("nodesafe"));
    assert!(n.apply_protocol_tx(&bad).is_err());
    assert_eq!(n.validators["nodesafe"].seat_claimable, 10);
}

#[test]
fn seat_tx_admitted_to_mempool_from_activation() {
    let (mut n, _w, seat) = setup(H - 1);
    n.validators.get_mut("nodesafe").unwrap().seat_claimable = 10;
    let fee = n.current_min_fee();
    let tx = seat_tx(&seat, &n, fee, set_payout(Some("alice")));
    let r = n.submit_tx(tx);
    assert!(r.is_ok(), "{r:?}");
}

// ---- network (consensus) tests ----

fn net(epoch_ms: u64) -> Network {
    net_with(epoch_ms, CryptoScheme::HmacDev)
}

fn net_with(epoch_ms: u64, scheme: CryptoScheme) -> Network {
    let cfg = ChainConfig {
        scheme,
        pack_budget_ms: 0,
        max_txs_per_ledger: 8,
        progress_timeout_ms: 0,
        epoch_ms,
        archive: true,
        ..Default::default()
    };
    let mut net = Network::bootstrap(&["v1", "v2", "v3", "v4", "v5", "v6"], 1000, &users(), cfg);
    net.pong_all();
    net
}

fn set_all(net: &mut Network, f: impl Fn(&mut Node)) {
    for n in net.nodes.values_mut() {
        f(n);
    }
}

fn same_everywhere(net: &Network) {
    let first = net.nodes.values().next().unwrap();
    for n in net.nodes.values() {
        assert_eq!(n.tip_hash, first.tip_hash, "tip hash {}", n.id);
        assert_eq!(n.commitment_root(), first.commitment_root(), "root {}", n.id);
    }
}

/// Payday with the seat rule active: validator pays land in the seat buckets
/// (or the payout account), never in `claimable[seat]`; all seats agree.
#[test]
fn settle_routes_validator_pays_to_seats_and_payout() {
    let epoch_ms = 1_000u64;
    let mut net = net(epoch_ms);
    set_all(&mut net, |n| {
        n.seat_payout_from_height = 1;
        n.validators.get_mut("v2").unwrap().seat_payout = Some("bob".into());
    });
    let bob0 = net.nodes["v1"].state.get("bob").balance;
    net.payment("alice", "bob", 1, 1).unwrap();
    net.seal_next_height().unwrap();
    let genesis = net.nodes.values().next().unwrap().genesis_ms;
    for (e, k) in [(7u64, 3), (8, 3)] {
        set_all(&mut net, |n| n.now_ms = genesis + e * epoch_ms + 10);
        for _ in 0..k {
            net.payment("alice", "bob", 1, 1).unwrap();
        }
        net.seal_next_height().unwrap();
    }
    same_everywhere(&net);
    let n = &net.nodes["v1"];
    assert!(n.last_settled_epoch >= 8);
    let seats: u64 = n.validators.values().map(|v| v.seat_claimable).sum();
    assert!(seats > 0, "seat buckets empty after payday");
    for id in ["v1", "v2", "v3", "v4", "v5", "v6"] {
        assert!(n.claimable.get(id).is_none(), "{id} validator pay leaked into claimable");
    }
    assert_eq!(n.validators["v2"].seat_claimable, 0, "v2 pays go to its payout account");
    assert!(n.state.get("bob").balance > bob0 + 7, "payout account credited");
}

/// Same chain with the rule never active: legacy `claimable[seat]` credit.
#[test]
fn settle_unchanged_before_activation() {
    let epoch_ms = 1_000u64;
    let mut net = net(epoch_ms);
    net.payment("alice", "bob", 1, 1).unwrap();
    net.seal_next_height().unwrap();
    let genesis = net.nodes.values().next().unwrap().genesis_ms;
    for e in [7u64, 8] {
        set_all(&mut net, |n| n.now_ms = genesis + e * epoch_ms + 10);
        net.payment("alice", "bob", 1, 1).unwrap();
        net.seal_next_height().unwrap();
    }
    same_everywhere(&net);
    let n = &net.nodes["v1"];
    assert!(n.claimable.keys().any(|k| k.starts_with('v')), "legacy validator credit");
    assert!(n.validators.values().all(|v| v.seat_claimable == 0 && v.seat_payout.is_none()));
    assert!(Node::seat_slim(&n.validators).is_empty());
}

/// Migration on the activation ledger: the whole `claimable[seat id]` moves
/// to the seat bucket; other accounts keep theirs; every seat agrees.
#[test]
fn migration_moves_seat_named_claimable_at_activation() {
    // Falcon like production: seat txs are signed with the seat key.
    let mut net = net_with(3_600_000, CryptoScheme::Falcon512);
    net.payment("alice", "bob", 1, 1).unwrap();
    net.seal_next_height().unwrap();
    let tip = net.nodes["v1"].tip_height;
    set_all(&mut net, |n| {
        n.seat_payout_from_height = tip + 2;
        n.claimable.insert("v1".into(), 500);
        n.claimable.insert("alice".into(), 7);
    });
    net.payment("alice", "bob", 1, 1).unwrap();
    net.seal_next_height().unwrap(); // tip + 1: before
    assert_eq!(net.nodes["v1"].claimable.get("v1"), Some(&500));
    assert_eq!(net.nodes["v1"].validators["v1"].seat_claimable, 0);
    net.payment("alice", "bob", 1, 1).unwrap();
    net.seal_next_height().unwrap(); // tip + 2: activation
    same_everywhere(&net);
    for n in net.nodes.values() {
        assert_eq!(n.claimable.get("v1"), None, "{}", n.id);
        assert!(n.validators["v1"].seat_claimable >= 500, "{}", n.id);
        assert_eq!(n.claimable.get("alice"), Some(&7), "{}", n.id);
    }
    // A seat tx right after activation, signed with v1's seat key, applies
    // the same on every seat (fee from the migrated bucket).
    let (seq, fee, keys) = {
        let n = &net.nodes["v1"];
        (n.state.get(&seat_sender("v1")).sequence, n.current_min_fee(), n.keys.clone())
    };
    let nid = net.nodes["v1"].cfg.network_id;
    // Production seats are BondV2 or in the on-chain key registry; the
    // bootstrap test net has neither, so register v1's key the same way.
    let v1_pk = keys.public_hex("v1").unwrap();
    set_all(&mut net, |n| {
        n.seat_key_registry.insert("v1".into(), v1_pk.clone());
    });
    let body = TxBody::SetSeatPayout { validator: "v1".into(), payout_account: Some("bob".into()) };
    let tx = Tx::sign_seat_body(&keys, "v1", seq, fee, nid, body).unwrap();
    for n in net.nodes.values_mut() {
        n.submit_tx(tx.clone()).unwrap();
    }
    net.seal_next_height().unwrap();
    same_everywhere(&net);
    for n in net.nodes.values() {
        assert_eq!(n.validators["v1"].seat_payout.as_deref(), Some("bob"), "{}", n.id);
    }
    // Later ledgers do not sweep again.
    set_all(&mut net, |n| {
        n.claimable.insert("v1".into(), 9);
    });
    net.payment("alice", "bob", 1, 1).unwrap();
    net.seal_next_height().unwrap();
    assert_eq!(net.nodes["v1"].claimable.get("v1"), Some(&9));
}
