//! Falcon-512 ledger fill / sign / verify timings.
//!
//!   cargo run -p fd-pl --release --bin falcon-pl-bench

use fd_pl::crypto::{CryptoScheme, KeyRing};
use fd_pl::{ChainConfig, Network, Tx};
use std::time::Instant;

fn main() {
    let scheme = CryptoScheme::Falcon512;
    println!("=== Falcon-512 ledger fill bench ===\n");

    // --- pure sign throughput ---
    let mut keys = KeyRing::new(scheme);
    keys.generate_id("alice");
    keys.generate_id("bob");
    let n_sign = 200usize;
    let t0 = Instant::now();
    for i in 0..n_sign {
        let _ = Tx::sign_with(&keys, "alice", (i + 1) as u64, "bob", 1, 1, 9101).unwrap();
    }
    let sign_ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!(
        "SIGN  {} Falcon txs in {:.1} ms  →  {:.1} sign/s  ({:.2} ms/tx)",
        n_sign,
        sign_ms,
        n_sign as f64 / (sign_ms / 1000.0),
        sign_ms / n_sign as f64
    );

    // --- verify throughput ---
    let mut txs = Vec::new();
    for i in 0..n_sign {
        txs.push(Tx::sign_with(&keys, "alice", (i + 1) as u64, "bob", 1, 1, 9101).unwrap());
    }
    let t0 = Instant::now();
    let mut ok = 0usize;
    for tx in &txs {
        if tx.verify(&keys).is_ok() {
            ok += 1;
        }
    }
    let ver_ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!(
        "VERIFY {} Falcon txs in {:.1} ms  →  {:.1} verify/s  ({:.2} ms/tx) ok={ok}",
        n_sign,
        ver_ms,
        n_sign as f64 / (ver_ms / 1000.0),
        ver_ms / n_sign as f64
    );

    // --- fill ledgers of various sizes ---
    let users: Vec<(String, u64)> = (0..16)
        .map(|i| (format!("u{i}"), 100_000_000u64))
        .collect();
    let sizes = [1usize, 4, 8, 16, 32, 48, 64, 96, 128];
    println!("\n--- pack+accept one ledger (3 nodes, Falcon-512) ---");
    println!(
        "{:>6}  {:>10}  {:>10}  {:>10}  {:>12}  {:>10}",
        "txs", "pack_ms", "accept_ms", "total_ms", "ledger_KB", "tx/s"
    );

    for &sz in &sizes {
        let cfg = ChainConfig {
            network_id: 9101,
            max_txs_per_ledger: sz,
            pack_budget_ms: 0,
            pong_window_ms: 60_000,
            confirm_depth: 3,
            slash_equivocation: 100,
            scheme,
            soft_ok_votes: 3,
            hard_ok_ratio: 0.6,
            progress_timeout_ms: 1_000,
            max_mempool_txs: 50_000,
            ..Default::default()
        };
        let mut net = Network::bootstrap(&["v1", "v2", "v3"], 1000, &users, cfg);
        for n in net.nodes.values_mut() {
            n.cfg.max_txs_per_ledger = sz;
        }
        // submit exactly sz applicable txs (round-robin)
        for i in 0..sz {
            let from = format!("u{}", i % 16);
            let to = format!("u{}", (i + 1) % 16);
            net.payment(&from, &to, 1, 1).unwrap();
        }
        net.pong_all();
        let packer = net.nodes.values().next().unwrap().lottery_packer().unwrap();

        let t_pack = Instant::now();
        let ledger = net
            .nodes
            .get_mut(&packer)
            .unwrap()
            .pack_as(&packer)
            .unwrap();
        let pack_ms = t_pack.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(ledger.txs.len(), sz);

        let json_bytes = serde_json::to_vec(&ledger).map(|v| v.len()).unwrap_or(0);

        let t_acc = Instant::now();
        for n in net.nodes.values_mut() {
            n.accept_ledger(&ledger).unwrap();
        }
        let acc_ms = t_acc.elapsed().as_secs_f64() * 1000.0;
        let total = pack_ms + acc_ms;
        let tps = if total > 0.0 {
            sz as f64 / (total / 1000.0)
        } else {
            0.0
        };
        println!(
            "{:>6}  {:>10.1}  {:>10.1}  {:>10.1}  {:>12.1}  {:>10.0}",
            sz,
            pack_ms,
            acc_ms,
            total,
            json_bytes as f64 / 1024.0,
            tps
        );
    }

    // --- continuous fill: how many full ledgers / s at max_txs=32 ---
    let max_txs = 32usize;
    let n_ledgers = 20usize;
    let cfg = ChainConfig {
        network_id: 9101,
        max_txs_per_ledger: max_txs,
        pack_budget_ms: 0,
        pong_window_ms: 60_000,
        confirm_depth: 3,
        slash_equivocation: 100,
        scheme,
        soft_ok_votes: 3,
        hard_ok_ratio: 0.6,
        progress_timeout_ms: 1_000,
        max_mempool_txs: 50_000,
        ..Default::default()
    };
    let mut net = Network::bootstrap(&["v1", "v2", "v3"], 1000, &users, cfg);
    for n in net.nodes.values_mut() {
        n.cfg.max_txs_per_ledger = max_txs;
        n.cfg.pack_budget_ms = 0;
    }
    let need = max_txs * n_ledgers;
    for i in 0..need {
        let from = format!("u{}", i % 16);
        let to = format!("u{}", (i + 1) % 16);
        net.payment(&from, &to, 1, 1).unwrap();
    }
    let t0 = Instant::now();
    let mut sealed = 0usize;
    for _ in 0..n_ledgers {
        net.seal_next_height().unwrap();
        sealed += 1;
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let txs = sealed * max_txs;
    println!("\n--- continuous {} ledgers × {} txs (Falcon) ---", n_ledgers, max_txs);
    println!(
        "sealed {} ledgers ({} txs) in {:.1} ms  →  {:.1} ledger/s  {:.0} tx/s",
        sealed,
        txs,
        ms,
        sealed as f64 / (ms / 1000.0),
        txs as f64 / (ms / 1000.0)
    );

    // --- time-budget pack: fill as many as fit in budget_ms ---
    println!("\n--- time-budget pack (Falcon, full mempool, hard cap 2000) ---");
    println!(
        "{:>10}  {:>8}  {:>10}  {:>12}",
        "budget_ms", "got_txs", "pack_ms", "tx/s_in_pack"
    );
    for budget in [50u64, 100, 250, 500, 1000] {
        let cfg = ChainConfig {
            network_id: 9101,
            max_txs_per_ledger: 2000,
            pack_budget_ms: budget,
            pong_window_ms: 60_000,
            confirm_depth: 3,
            slash_equivocation: 100,
            scheme,
            soft_ok_votes: 3,
            hard_ok_ratio: 0.6,
            progress_timeout_ms: 1_000,
            max_mempool_txs: 0, // unlimited for pack-capacity bench
            ..Default::default()
        };
        let mut net = Network::bootstrap(&["v1", "v2", "v3"], 1000, &users, cfg);
        // flood mempool
        for i in 0..2500 {
            let from = format!("u{}", i % 16);
            let to = format!("u{}", (i + 1) % 16);
            let _ = net.payment(&from, &to, 1, 1);
        }
        net.pong_all();
        let packer = net.nodes.values().next().unwrap().lottery_packer().unwrap();
        let t0 = Instant::now();
        let ledger = net
            .nodes
            .get_mut(&packer)
            .unwrap()
            .pack_as(&packer)
            .unwrap();
        let pack_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let n = ledger.txs.len();
        let rate = if pack_ms > 0.0 {
            n as f64 / (pack_ms / 1000.0)
        } else {
            0.0
        };
        println!(
            "{:>10}  {:>8}  {:>10.1}  {:>12.0}",
            budget, n, pack_ms, rate
        );
    }

    // sig size sample
    let sample = Tx::sign_with(&keys, "alice", 1, "bob", 1, 1, 9101).unwrap();
    println!(
        "\nsample Falcon sig hex len={} (~{} bytes)  tx_id={}",
        sample.signature.len(),
        sample.signature.len() / 2,
        &sample.tx_id[..16]
    );
    println!(
        "\nModel: pack_budget_ms=1000 → pack until 1s or max_txs cap.\nFaster machine applies more txs before deadline → bigger ledgers."
    );
}
