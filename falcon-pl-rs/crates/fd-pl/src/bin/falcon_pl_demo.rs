//! In-process multi-validator demo.
//!   cargo run -p fd-pl --bin falcon-pl-demo -- --scheme hmac
//!   cargo run -p fd-pl --bin falcon-pl-demo -- --scheme falcon-512 --txs 10

use clap::Parser;
use fd_pl::crypto::CryptoScheme;
use fd_pl::{ChainConfig, Network, NETWORK_NAME};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value_t = 5)]
    nodes: usize,
    #[arg(long, default_value_t = 40)]
    txs: usize,
    #[arg(long, default_value_t = 6)]
    heights: u64,
    #[arg(long, default_value_t = 8)]
    max_txs_per_ledger: usize,
    #[arg(long, default_value = "hmac")]
    scheme: String,
}

fn main() {
    let args = Args::parse();
    let scheme = CryptoScheme::parse(&args.scheme).expect("scheme hmac|falcon-512");
    let cfg = ChainConfig {
        network_id: 9101,
        max_txs_per_ledger: args.max_txs_per_ledger,
        pack_budget_ms: 0, // fixed size for demo
        pong_window_ms: 5_000,
        confirm_depth: 3,
        slash_equivocation: 100,
        scheme,
        soft_ok_votes: 3,
        hard_ok_ratio: 0.6,
        progress_timeout_ms: 1_000,
        max_mempool_txs: 50_000,
        ..Default::default()
    };

    let val_ids: Vec<String> = (1..=args.nodes).map(|i| format!("v{i}")).collect();
    let val_refs: Vec<&str> = val_ids.iter().map(|s| s.as_str()).collect();
    let users = vec![
        ("alice".into(), 10_000_000u64),
        ("bob".into(), 10_000_000u64),
        ("carol".into(), 10_000_000u64),
        ("dave".into(), 10_000_000u64),
    ];

    let mut net = Network::bootstrap(&val_refs, 1_000, &users, cfg);

    println!("=== {NETWORK_NAME} ===");
    println!(
        "scheme={} validators={} max_txs/ledger={}",
        scheme.as_str(),
        args.nodes,
        args.max_txs_per_ledger
    );

    for i in 0..args.txs {
        let (from, to) = match i % 4 {
            0 => ("alice", "bob"),
            1 => ("bob", "carol"),
            2 => ("carol", "dave"),
            _ => ("dave", "alice"),
        };
        if let Err(e) = net.payment(from, to, 10, 2) {
            eprintln!("submit: {e}");
        }
    }
    println!(
        "mempool={}",
        net.nodes.values().next().map(|n| n.mempool.len()).unwrap_or(0)
    );

    for _ in 0..args.heights {
        net.tick_all(100);
        match net.seal_next_height() {
            Ok(r) => {
                let c1 = net
                    .nodes
                    .values()
                    .next()
                    .map(|n| n.confirmations(1))
                    .unwrap_or(0);
                println!(
                    "h={} packer={} txs={} fees={} reward={} confs_h1={}",
                    r.height, r.packer, r.n_txs, r.fees, r.reward_paid, c1
                );
            }
            Err(e) => {
                println!("seal: {e}");
                break;
            }
        }
    }

    let agree = {
        let h: Vec<_> = net.nodes.values().map(|n| n.tip_hash.clone()).collect();
        h.iter().all(|x| x == &h[0])
    };
    println!("tip_height={} all_agree={agree}", net.tip_height());
    for a in ["alice", "bob", "carol", "dave"] {
        println!("  {a}: {}", net.balances(a).unwrap_or(0));
    }
    if let Some(n) = net.nodes.values().next() {
        for (id, pay) in &n.rewards {
            println!("  reward {id}: {pay}");
        }
    }
}
