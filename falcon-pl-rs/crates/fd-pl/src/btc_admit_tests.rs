//! v2.9.54 tests: chained BTC header admission (mempool only).
//!
//! A submitter burst h+1..h+32 from one account must be admitted while h+1 is
//! still pending and pack into one block. Out-of-order, sequence-gapped and
//! forked headers are still rejected, and one account's pending headers never
//! extend another account's admission probe.

use super::*;
use crate::bridge::RAIL_BTC;

const BITS: u32 = 0x207f_ffff;
const T0: u32 = 1_700_000_000;

fn users() -> Vec<(String, u64)> {
    vec![
        ("alice".into(), 1_000_000),
        ("bob".into(), 1_000_000),
        ("carol".into(), 1_000_000),
    ]
}

fn net() -> Network {
    let cfg = ChainConfig {
        fee_escalate: false,
        min_fee: 1,
        pack_budget_ms: 0,
        max_txs_per_ledger: 0,
        ..Default::default()
    };
    let mut net = Network::bootstrap(&["v1", "v2", "v3", "v4", "v5"], 1000, &users(), cfg);
    net.pong_all();
    net
}

/// Mine `n` chained regtest-difficulty headers on top of `prev` (display hex).
fn chain(prev: &str, n: usize, salt: u8, t: u32) -> Vec<crate::btc_spv::ParsedHeader> {
    let mut out = Vec::with_capacity(n);
    let mut p = prev.to_string();
    for i in 0..n {
        let merkle = format!("{:02x}{:02x}", salt, (i % 256) as u8).repeat(16);
        let h = crate::btc_spv::mine_header(&p, &merkle, BITS, t + i as u32).unwrap();
        p = h.hash_display.clone();
        out.push(h);
    }
    out
}

fn hdr_tx(net: &Network, acct: &str, seq: u64, height: u64, raw: &str) -> Tx {
    Tx::sign_body(
        &net.user_keys,
        acct,
        seq,
        "",
        0,
        1,
        net.cfg.network_id,
        TxBody::RailHeader {
            asset: RAIL_BTC.into(),
            height,
            hash: String::new(),
            parent_hash: String::new(),
            merkle_root: String::new(),
            raw: raw.to_string(),
        },
    )
    .unwrap()
}

fn pay_tx(net: &Network, acct: &str, seq: u64) -> Tx {
    Tx::sign_with(&net.user_keys, acct, seq, "carol", 1, 1, net.cfg.network_id).unwrap()
}

/// Submit to every node; all must agree on the verdict. Returns it.
fn submit_all(net: &mut Network, tx: &Tx) -> Result<bool, String> {
    let mut verdict: Option<Result<bool, String>> = None;
    for n in net.nodes.values_mut() {
        let r = n.submit_tx(tx.clone());
        if let Some(v) = &verdict {
            assert_eq!(v.is_ok(), r.is_ok(), "nodes disagree on admission: {v:?} vs {r:?}");
        } else {
            verdict = Some(r);
        }
    }
    verdict.unwrap()
}

fn submit_hdr(net: &mut Network, acct: &str, seq: u64, height: u64, raw: &str) -> Result<bool, String> {
    let tx = hdr_tx(net, acct, seq, height, raw);
    submit_all(net, &tx)
}

fn submit_pay(net: &mut Network, acct: &str, seq: u64) -> Result<bool, String> {
    let tx = pay_tx(net, acct, seq);
    submit_all(net, &tx)
}

fn rail_tip(net: &Network) -> (u64, String) {
    let n = net.nodes.values().next().unwrap();
    let r = n.bridge.rail(RAIL_BTC).unwrap();
    (r.tip_height, r.tip_hash.clone())
}

/// Commit an anchor header at `height` from alice seq 1. Returns its hash.
fn anchor(net: &mut Network, height: u64) -> String {
    let a = chain(&"00".repeat(32), 1, 0xa0, T0);
    let tx = hdr_tx(net, "alice", 1, height, &a[0].raw_hex);
    assert!(submit_all(net, &tx).unwrap());
    let r = net.seal_next_height().unwrap();
    assert_eq!(r.n_txs, 1);
    assert_eq!(rail_tip(net), (height, a[0].hash_display.clone()));
    a[0].hash_display.clone()
}

#[test]
fn btc_burst_of_32_chained_headers_admitted_and_packs_in_one_block() {
    let mut net = anchor_net();
    let tip0 = rail_tip(&net);
    let hs = chain(&tip0.1, 32, 0xb0, T0 + 600);
    for (i, h) in hs.iter().enumerate() {
        let tx = hdr_tx(&net, "alice", 2 + i as u64, tip0.0 + 1 + i as u64, &h.raw_hex);
        let r = submit_all(&mut net, &tx);
        assert_eq!(r, Ok(true), "header #{i} (h={}) must be admitted", tip0.0 + 1 + i as u64);
    }
    for n in net.nodes.values() {
        assert_eq!(n.mempool.len(), 32);
        // Committed rail is untouched by admission.
        assert_eq!(n.bridge.rail(RAIL_BTC).unwrap().tip_height, tip0.0);
    }
    let r = net.seal_next_height().unwrap();
    assert_eq!(r.n_txs, 32, "whole burst packs into one block");
    assert_eq!(rail_tip(&net), (tip0.0 + 32, hs[31].hash_display.clone()));
    let first = net.nodes.values().next().unwrap();
    for n in net.nodes.values() {
        assert_eq!(n.tip_hash, first.tip_hash, "every node accepted the same block");
        assert!(n.mempool.is_empty());
    }
}

fn anchor_net() -> Network {
    let mut net = net();
    anchor(&mut net, 100);
    net
}

#[test]
fn btc_out_of_order_gapped_forked_and_foreign_headers_still_rejected() {
    let mut net = anchor_net();
    let (h0, tip_hash) = rail_tip(&net);
    let main = chain(&tip_hash, 4, 0xc0, T0 + 600);
    let fork = chain(&tip_hash, 2, 0xd0, T0 + 900);

    // h+1 pending at alice seq 2.
    assert_eq!(submit_hdr(&mut net, "alice", 2, h0 + 1, &main[0].raw_hex), Ok(true));

    // Out of order: h+3 while h+2 is not pending.
    let e = submit_hdr(&mut net, "alice", 3, h0 + 3, &main[2].raw_hex).unwrap_err();
    assert!(e.contains("BTC header parent mismatch"), "{e}");

    // Sequence gap: correct h+2 but at seq 4 with seq 3 missing.
    let e = submit_hdr(&mut net, "alice", 4, h0 + 2, &main[1].raw_hex).unwrap_err();
    assert!(e.contains("BTC header parent mismatch"), "{e}");

    // Fork: h+2' whose parent is h+1' (not the pending h+1).
    let e = submit_hdr(&mut net, "alice", 3, h0 + 2, &fork[1].raw_hex).unwrap_err();
    assert!(e.contains("BTC header parent mismatch"), "{e}");

    // Another account cannot build on alice's pending h+1.
    let e = submit_hdr(&mut net, "bob", 1, h0 + 2, &main[1].raw_hex).unwrap_err();
    assert!(e.contains("BTC header parent mismatch"), "{e}");
    // ... and bob's other txs are unaffected.
    assert_eq!(submit_pay(&mut net, "bob", 1), Ok(true));

    // The correct chained successor is admitted, and a non-header tx inside
    // alice's sequence run does not break the chain.
    assert_eq!(submit_hdr(&mut net, "alice", 3, h0 + 2, &main[1].raw_hex), Ok(true));
    assert_eq!(submit_pay(&mut net, "alice", 4), Ok(true));
    assert_eq!(submit_hdr(&mut net, "alice", 5, h0 + 3, &main[2].raw_hex), Ok(true));

    // Nothing rejected above leaked into the pool.
    let n = net.nodes.values().next().unwrap();
    let mut got: Vec<(String, u64)> = n
        .mempool
        .iter_candidates()
        .map(|t| (t.account.clone(), t.sequence))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("alice".into(), 2),
            ("alice".into(), 3),
            ("alice".into(), 4),
            ("alice".into(), 5),
            ("bob".into(), 1),
        ]
    );
    let r = net.seal_next_height().unwrap();
    assert_eq!(r.n_txs, 5, "alice 4 + bob pay in one block");
    assert_eq!(rail_tip(&net), (h0 + 3, main[2].hash_display.clone()));
}

#[test]
fn btc_chain_admission_capped_then_falls_back_to_committed_tip() {
    let mut net = anchor_net();
    let (h0, tip_hash) = rail_tip(&net);
    let cap = Node::BTC_ADMIT_CHAIN_MAX as usize;
    let hs = chain(&tip_hash, cap + 2, 0xe0, T0 + 600);
    // seq 2 (head) + `cap` predecessors-replayed successors are admitted.
    for i in 0..=cap {
        let tx = hdr_tx(&net, "alice", 2 + i as u64, h0 + 1 + i as u64, &hs[i].raw_hex);
        assert_eq!(submit_all(&mut net, &tx), Ok(true), "#{i}");
    }
    // One deeper than the cap: no replay, checked against the committed tip.
    let deep = hdr_tx(&net, "alice", 2 + cap as u64 + 1, h0 + cap as u64 + 2, &hs[cap + 1].raw_hex);
    let e = submit_all(&mut net, &deep).unwrap_err();
    assert!(e.contains("BTC header parent mismatch"), "{e}");
    let r = net.seal_next_height().unwrap();
    assert_eq!(r.n_txs, cap + 1);
    assert_eq!(rail_tip(&net).0, h0 + cap as u64 + 1);
    // After the pack the same header is admissible.
    assert_eq!(submit_all(&mut net, &deep), Ok(true));
}

#[test]
fn btc_pending_head_that_no_longer_applies_stops_the_chain() {
    // alice: anchor h0 (seq 1), h0+1 committed (seq 2), h0+2 pending (seq 3).
    // bob then commits a same-height reorg h0+1'. alice's pending h0+2 no longer
    // links (the packer will drop it), so her chained h0+3 must NOT be admitted
    // on top of it: the replay stops at the first unappliable sequence.
    let mut net = anchor_net();
    let (h0, tip_hash) = rail_tip(&net);
    let main = chain(&tip_hash, 3, 0x51, T0 + 600);
    let fork = chain(&tip_hash, 1, 0x52, T0 + 900);
    assert_eq!(submit_hdr(&mut net, "alice", 2, h0 + 1, &main[0].raw_hex), Ok(true));
    net.seal_next_height().unwrap();
    assert_eq!(submit_hdr(&mut net, "bob", 1, h0 + 1, &fork[0].raw_hex), Ok(true));
    net.seal_next_height().unwrap();
    assert_eq!(rail_tip(&net), (h0 + 1, fork[0].hash_display.clone()));
    // alice's h0+2 arrives via hub sync (absorb path has no probe) after the reorg.
    let stale = hdr_tx(&net, "alice", 3, h0 + 2, &main[1].raw_hex);
    for n in net.nodes.values_mut() {
        assert_eq!(n.absorb_txs(&[stale.clone()]), 1);
    }
    let e = submit_hdr(&mut net, "alice", 4, h0 + 3, &main[2].raw_hex).unwrap_err();
    assert!(e.contains("BTC header parent mismatch"), "{e}");
    // A replacement at seq 3 that links to the new tip is admitted (evicts the
    // stale same-seq entry), and its successor chains on it.
    let fork2 = chain(&fork[0].hash_display, 2, 0x53, T0 + 1200);
    assert_eq!(submit_hdr(&mut net, "alice", 3, h0 + 2, &fork2[0].raw_hex), Ok(true));
    assert_eq!(submit_hdr(&mut net, "alice", 4, h0 + 3, &fork2[1].raw_hex), Ok(true));
    let r = net.seal_next_height().unwrap();
    assert_eq!(r.n_txs, 2);
    assert_eq!(rail_tip(&net), (h0 + 3, fork2[1].hash_display.clone()));
}
