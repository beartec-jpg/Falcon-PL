//! In-process multi-validator mesh simulator (no network).
//!
//! Proves DA certification + tip-BFT ordering + execution across N engines with full gossip.

use fd_crypto::{Falcon512KeyRing, SharedCrypto};
use fd_node::{GossipEvents, NodeEngine};
use fd_types::{ProtocolConfig, Transaction};
use std::collections::HashMap;
use std::sync::Arc;

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let n_txs: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let scheme = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "hmac-sha256-dev".into());

    let unl: Vec<String> = (1..=n).map(|i| format!("val{i}")).collect();
    let mut cfg = ProtocolConfig::default();
    cfg.worker_sharding = false;
    cfg.target_batch_interval_s = 0.0;
    cfg.shard_orphan_age_s = 0.0;
    cfg.crypto_scheme = scheme.clone();

    let mut engines: Vec<NodeEngine> = if scheme.starts_with("falcon") {
        let ring: SharedCrypto =
            Arc::new(Falcon512KeyRing::generate_unl(unl.iter().cloned()).unwrap());
        unl.iter()
            .enumerate()
            .map(|(i, id)| {
                NodeEngine::with_crypto(
                    id.clone(),
                    unl.clone(),
                    cfg.clone(),
                    i,
                    n,
                    Arc::clone(&ring),
                )
            })
            .collect()
    } else {
        unl.iter()
            .enumerate()
            .map(|(i, id)| NodeEngine::new(id.clone(), unl.clone(), cfg.clone(), i, n))
            .collect()
    };

    println!(
        "fd_sim validators={n} txs={n_txs} scheme={} tip-bft=on",
        engines[0].dag.crypto.scheme()
    );

    for seq in 1..=n_txs {
        let tx = Transaction::create("acct1", seq as u64, "acct2", 1, 10, seq as f64);
        for eng in engines.iter_mut() {
            eng.submit_tx(tx.clone());
        }
    }

    let mut rounds = 0u64;
    for origin in 0..n {
        let now = 10.0 + rounds as f64;
        let Some((batch, _)) = engines[origin].create_batch(now, true) else {
            continue;
        };
        rounds += 1;
        gossip_batch(&mut engines, origin, batch, now);
    }

    for _ in 0..n * 4 {
        let mut progressed = false;
        for origin in 0..n {
            let now = 100.0 + rounds as f64;
            let Some((batch, _)) = engines[origin].create_batch(now, true) else {
                continue;
            };
            progressed = true;
            rounds += 1;
            gossip_batch(&mut engines, origin, batch, now);
        }
        if !progressed {
            break;
        }
    }

    // Extra tip-BFT rounds for any lagging commits.
    gossip_tip_bft(&mut engines, 16);

    let order0 = engines[0].ordering.ordered.clone();
    let exec0 = engines[0].execution.executed_txs.clone();
    let tips0 = engines[0].ordering.tip_bft.commits.len();
    for eng in &engines {
        assert_eq!(
            eng.ordering.ordered, order0,
            "order mismatch on {}",
            eng.validator_id
        );
        assert_eq!(
            eng.execution.executed_txs, exec0,
            "exec mismatch on {}",
            eng.validator_id
        );
        assert!(
            eng.ordering.tip_bft.commits.len() > 0,
            "{} has no tip-BFT commits",
            eng.validator_id
        );
    }

    println!(
        "ok batches_created={} certified={} ordered={} executed={} hard_final={} tip_commits={} bullshark_anchors={}",
        engines.iter().map(|e| e.batches_created).sum::<u64>(),
        engines[0].dag.certificates.len(),
        order0.len(),
        exec0.len(),
        engines[0].ordering.hard_final.len(),
        tips0,
        engines[0].ordering.bullshark.anchors.len(),
    );
    println!(
        "agreement: all {n} validators identical order+exec (bullshark+tip-bft)"
    );
}

fn gossip_batch(
    engines: &mut [NodeEngine],
    origin: usize,
    batch: fd_types::Batch,
    now: f64,
) {
    let hash = batch.batch_hash.clone();
    for (i, eng) in engines.iter_mut().enumerate() {
        if i != origin {
            eng.on_remote_batch(batch.clone(), now);
        }
    }
    let mut sigs: HashMap<String, String> = HashMap::new();
    for eng in engines.iter() {
        sigs.insert(
            eng.validator_id.clone(),
            eng.dag.crypto.sign_batch(&eng.validator_id, &hash).unwrap(),
        );
    }
    for eng in engines.iter_mut() {
        for (vid, sig) in &sigs {
            eng.on_remote_sig(&hash, vid, sig);
        }
    }
    gossip_tip_bft(engines, 6);
}

fn gossip_tip_bft(engines: &mut [NodeEngine], rounds: usize) {
    for _ in 0..rounds {
        let mut proposals = Vec::new();
        let mut votes = Vec::new();
        let mut commits = Vec::new();
        for eng in engines.iter_mut() {
            let ev = eng.advance_ledger();
            proposals.extend(ev.tip_proposals);
            votes.extend(ev.tip_votes);
            commits.extend(ev.tip_commits);
        }
        for eng in engines.iter_mut() {
            for p in &proposals {
                if p.proposer == eng.validator_id {
                    continue;
                }
                let ev = eng.on_tip_proposal(p.clone());
                votes.extend(ev.tip_votes);
                commits.extend(ev.tip_commits);
            }
        }
        for eng in engines.iter_mut() {
            for v in &votes {
                if v.voter == eng.validator_id {
                    continue;
                }
                let ev = eng.on_tip_vote(v.clone());
                commits.extend(ev.tip_commits);
            }
        }
        for eng in engines.iter_mut() {
            for c in &commits {
                let _ = eng.on_tip_commit(c.clone());
            }
        }
        let _ = GossipEvents::default();
    }
}
