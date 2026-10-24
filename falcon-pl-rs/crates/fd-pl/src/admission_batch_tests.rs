//! v2.9.76: ApproveValidator from a key-bound admission account.
//! 2.9.75 stall (2300 h=668627): the Merkle batch stripped the admission
//! account's known key, apply then compared an empty signer key with the
//! admission key and every proposal failed self-accept; nothing evicted it.

use super::*;

const ADMIN: &str = "scott";

fn users() -> Vec<(String, u64)> {
    vec![("alice".into(), 1_000_000), ("bob".into(), 1_000_000), (ADMIN.into(), 1_000_000)]
}

fn net() -> Network {
    let cfg = ChainConfig { min_fee: 0, pack_budget_ms: 0, max_txs_per_ledger: 8, progress_timeout_ms: 0, ..Default::default() };
    let mut net = Network::bootstrap(&["v1", "v2", "v3"], 1000, &users(), cfg);
    net.pong_all();
    // Same env value as rv2_validator_tests (HmacDev keys are derived from the id).
    let adm = net.user_keys.public_hex(ADMIN).unwrap();
    std::env::set_var("FPL_ADMISSION_PUBKEY", &adm);
    let h = net.tip_height() + 1;
    for n in net.nodes.values_mut() {
        n.rewards_v2_from_height = h;
        n.rewards_v2_epoch = 0;
        // Admission account already bound to its key (seq > 0 on 2300).
        n.state.accounts.entry(ADMIN.into()).or_default().meta.public_key = adm.clone();
    }
    net
}

fn submit_all(net: &mut Network, acct: &str, dest: &str, amount: u64, body: TxBody) -> Tx {
    let seq = net.account_seqs.get(acct).copied().unwrap_or(0);
    let t = Tx::sign_body(&net.user_keys, acct, seq, dest, amount, 2, net.cfg.network_id, body).unwrap();
    *net.account_seqs.entry(acct.into()).or_default() += 1;
    for n in net.nodes.values_mut() {
        n.submit_tx(t.clone()).unwrap();
    }
    t
}

fn approve_body() -> TxBody {
    TxBody::ApproveValidator { id: "nodesafe".into(), public_key: "ab".repeat(8), expires_height: 1_000_000 }
}

#[test]
fn batch_keeps_signer_key_for_admission_bodies_only() {
    let mut keys = KeyRing::new(CryptoScheme::HmacDev);
    keys.generate_many(["packer", ADMIN, "alice"]);
    let mk = |acct: &str, seq, body| Tx::sign_body(&keys, acct, seq, "bob", 1, 0, NETWORK_ID_PUBLIC_TESTNET_2300, body).unwrap();
    let txs = vec![mk(ADMIN, 3, approve_body()), mk("alice", 0, TxBody::Pay)];
    let mut l = Ledger::seal(700_000, "00".into(), NETWORK_ID_PUBLIC_TESTNET_2300, "packer", &keys, txs, "root".into(), 1).unwrap();
    let known = |id: &str| keys.public_hex(id);
    l.attach_merkle_batch_known(&keys, &known).unwrap();
    assert!(!l.txs[0].public_key.is_empty(), "admission body keeps its signer key");
    assert!(l.txs[1].public_key.is_empty(), "plain pay still stripped");
    l.verify_merkle_batch_known(&keys, &known).unwrap();
}

#[test]
fn approve_from_bound_admission_account_seals_on_every_seat() {
    let mut net = net();
    let before = net.nodes.values().next().unwrap().state.get(ADMIN).sequence;
    submit_all(&mut net, ADMIN, "", 0, approve_body());
    net.seal_next_height_batched(false).unwrap();
    for n in net.nodes.values() {
        assert!(n.defi.validator_approvals.contains_key("nodesafe"));
        assert_eq!(n.state.get(ADMIN).sequence, before + 1);
    }
    let roots: BTreeSet<String> = net.nodes.values().map(|n| n.commitment_root()).collect();
    assert_eq!(roots.len(), 1);
}

#[test]
fn legacy_stripped_body_reproduces_the_stall_then_eviction_unblocks() {
    let mut net = net();
    let bad = submit_all(&mut net, ADMIN, "", 0, approve_body());
    submit_all(&mut net, "alice", "bob", 5, TxBody::Pay);
    // 2.9.75 body: self-accept fails exactly like the live seats.
    let e = net.seal_next_height_batched(true).unwrap_err();
    assert!(e.contains("admission: signer is not the admission key"), "{e}");
    assert!(e.contains(&bad.tx_id), "evicted the failing tx: {e}");
    let tip = net.tip_height();
    // Next proposal from the same packer omits it and seals.
    net.seal_next_height_batched(true).unwrap();
    assert_eq!(net.tip_height(), tip + 1);
    let packer_view = net.nodes.values().map(|n| n.state.get("bob").balance).max().unwrap();
    assert_eq!(packer_view, 1_000_005, "pay sealed");
    for n in net.nodes.values() {
        assert!(!n.defi.validator_approvals.contains_key("nodesafe"));
    }
}

#[test]
fn eviction_drops_later_sequences_of_the_failing_account() {
    let mut net = net();
    let bad = submit_all(&mut net, ADMIN, "", 0, approve_body());
    let later = submit_all(&mut net, ADMIN, "bob", 1, TxBody::Pay);
    let _ = net.seal_next_height_batched(true).unwrap_err();
    let packer = net.nodes.values().next().unwrap().lottery_packer().unwrap();
    let p = &net.nodes[&packer];
    let ids: Vec<String> = p.mempool.iter_candidates().map(|t| t.tx_id.clone()).collect();
    assert!(!ids.contains(&bad.tx_id) && !ids.contains(&later.tx_id), "{ids:?}");
}
