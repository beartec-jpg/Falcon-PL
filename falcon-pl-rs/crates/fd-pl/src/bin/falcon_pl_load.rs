//! Load / flood / stability harness for Falcon PL (in-process).
//!
//!   cargo run -p fd-pl --release --bin falcon-pl-load
//!   cargo run -p fd-pl --release --bin falcon-pl-load -- --scenario all
//!   cargo run -p fd-pl --release --bin falcon-pl-load -- --scenario flood --txs 5000

use clap::{Parser, ValueEnum};
use fd_pl::crypto::CryptoScheme;
use fd_pl::{ChainConfig, Network, Tx};
use std::time::Instant;

#[derive(Clone, ValueEnum, Debug)]
enum Scenario {
    All,
    /// Burst submit then drain via seals
    Flood,
    /// Many heights under continuous load
    Soak,
    /// Duplicate + garbage txs mixed with good ones
    Adversarial,
    /// Idle ticks — must not advance tip without txs
    Idle,
    /// Rapid seal attempts under contention
    Thrash,
}

#[derive(Parser, Debug)]
#[command(name = "falcon-pl-load")]
struct Args {
    #[arg(long, value_enum, default_value_t = Scenario::All)]
    scenario: Scenario,
    #[arg(long, default_value_t = 5)]
    nodes: usize,
    #[arg(long, default_value_t = 2000)]
    txs: usize,
    #[arg(long, default_value_t = 32)]
    max_txs_per_ledger: usize,
    #[arg(long, default_value = "hmac")]
    scheme: String,
    #[arg(long, default_value_t = 200)]
    soak_heights: u64,
}

struct Report {
    name: String,
    ok: bool,
    detail: String,
    ms: u128,
}

fn make_net(nodes: usize, max_txs: usize, scheme: CryptoScheme, users: usize, bal: u64) -> Network {
    let vals: Vec<String> = (1..=nodes).map(|i| format!("v{i}")).collect();
    let val_refs: Vec<&str> = vals.iter().map(|s| s.as_str()).collect();
    let accounts: Vec<(String, u64)> = (0..users)
        .map(|i| (format!("u{i}"), bal))
        .collect();
    let cfg = ChainConfig {
        network_id: 9101,
        max_txs_per_ledger: max_txs,
        pack_budget_ms: 0, // fixed size for load harness
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
    let mut net = Network::bootstrap(&val_refs, 1_000, &accounts, cfg);
    for n in net.nodes.values_mut() {
        n.cfg.max_txs_per_ledger = max_txs;
    }
    net
}

fn drain_until_empty(net: &mut Network, max_heights: u64) -> (u64, usize, Vec<String>) {
    let mut heights = 0u64;
    let mut sealed_txs = 0usize;
    let mut errs = Vec::new();
    for _ in 0..max_heights {
        let mp = net.nodes.values().next().map(|n| n.mempool.len()).unwrap_or(0);
        if mp == 0 {
            break;
        }
        net.tick_all(10);
        match net.seal_next_height() {
            Ok(r) => {
                heights += 1;
                sealed_txs += r.n_txs;
                if r.n_txs == 0 {
                    errs.push("empty ledger sealed".into());
                    break;
                }
            }
            Err(e) => {
                if e.contains("empty") {
                    break;
                }
                errs.push(e);
                break;
            }
        }
    }
    (heights, sealed_txs, errs)
}

fn tips_agree(net: &Network) -> bool {
    let tips: Vec<_> = net.nodes.values().map(|n| n.tip_hash.clone()).collect();
    !tips.is_empty() && tips.iter().all(|t| t == &tips[0])
}

fn run_flood(args: &Args, scheme: CryptoScheme) -> Report {
    let t0 = Instant::now();
    let users = 20usize;
    let mut net = make_net(args.nodes, args.max_txs_per_ledger, scheme, users, 50_000_000);

    // Flood: submit many txs as fast as possible (round-robin accounts)
    let mut ok_sub = 0usize;
    let mut fail_sub = 0usize;
    let submit_t0 = Instant::now();
    for i in 0..args.txs {
        let from = format!("u{}", i % users);
        let to = format!("u{}", (i + 1) % users);
        match net.payment(&from, &to, 1, 1) {
            Ok(_) => ok_sub += 1,
            Err(_) => fail_sub += 1,
        }
    }
    let submit_ms = submit_t0.elapsed().as_millis();
    let mp = net.nodes.values().next().map(|n| n.mempool.len()).unwrap_or(0);

    let drain_t0 = Instant::now();
    let max_h = (args.txs as u64 / args.max_txs_per_ledger as u64) + 50;
    let (heights, sealed_txs, mut errs) = drain_until_empty(&mut net, max_h);
    let drain_ms = drain_t0.elapsed().as_millis();

    let agree = tips_agree(&net);
    let empty_left = net.nodes.values().next().map(|n| n.mempool.len()).unwrap_or(0);
    // leftover = failed apply candidates (e.g. depleted account) ok if small
    if !agree {
        errs.push("tip disagreement".into());
    }
    if sealed_txs == 0 && ok_sub > 0 {
        errs.push("no txs sealed".into());
    }
    // Must not have sealed more than submitted
    if sealed_txs > ok_sub {
        errs.push(format!("sealed {sealed_txs} > submitted {ok_sub}"));
    }

    let submit_tps = if submit_ms > 0 {
        ok_sub as f64 * 1000.0 / submit_ms as f64
    } else {
        0.0
    };
    let seal_tps = if drain_ms > 0 {
        sealed_txs as f64 * 1000.0 / drain_ms as f64
    } else {
        0.0
    };

    let ok = errs.is_empty() && sealed_txs > 0 && agree;
    Report {
        name: "flood".into(),
        ok,
        detail: format!(
            "submit ok={ok_sub} fail={fail_sub} mempool_after_submit={mp} | \
             heights={heights} sealed_txs={sealed_txs} mempool_left={empty_left} | \
             submit_tps={submit_tps:.0} seal_apply_tps={seal_tps:.0} agree={agree} | errs={errs:?}"
        ),
        ms: t0.elapsed().as_millis(),
    }
}

fn run_soak(args: &Args, scheme: CryptoScheme) -> Report {
    let t0 = Instant::now();
    let users = 10usize;
    let mut net = make_net(args.nodes, args.max_txs_per_ledger, scheme, users, 100_000_000);
    let mut submitted = 0usize;
    let mut sealed = 0usize;
    let mut heights = 0u64;
    let mut empty_attempts = 0u64;
    let mut errs = Vec::new();

    // Continuous: each height submit a batch then seal
    for h in 0..args.soak_heights {
        let batch = args.max_txs_per_ledger;
        for i in 0..batch {
            let from = format!("u{}", (h as usize + i) % users);
            let to = format!("u{}", (h as usize + i + 3) % users);
            if net.payment(&from, &to, 1, 1).is_ok() {
                submitted += 1;
            }
        }
        net.tick_all(5);
        match net.seal_next_height() {
            Ok(r) => {
                if r.n_txs == 0 {
                    empty_attempts += 1;
                    errs.push(format!("empty seal at loop {h}"));
                } else {
                    heights += 1;
                    sealed += r.n_txs;
                }
            }
            Err(e) => {
                if e.contains("empty") {
                    empty_attempts += 1;
                } else {
                    errs.push(e);
                    break;
                }
            }
        }
        if !tips_agree(&net) {
            errs.push(format!("diverge at height {}", net.tip_height()));
            break;
        }
    }

    let agree = tips_agree(&net);
    let tps = sealed as f64 * 1000.0 / t0.elapsed().as_millis().max(1) as f64;
    let ok = errs.is_empty() && agree && heights > 0 && empty_attempts == 0;
    Report {
        name: "soak".into(),
        ok,
        detail: format!(
            "loops={} sealed_heights={heights} submitted={submitted} sealed_txs={sealed} \
             empty_seals={empty_attempts} tip={} agree={agree} tps≈{tps:.0} errs={errs:?}",
            args.soak_heights,
            net.tip_height()
        ),
        ms: t0.elapsed().as_millis(),
    }
}

fn run_adversarial(args: &Args, scheme: CryptoScheme) -> Report {
    let t0 = Instant::now();
    let users = 5usize;
    let mut net = make_net(args.nodes, args.max_txs_per_ledger, scheme, users, 10_000_000);
    let mut good = 0usize;
    let mut dup = 0usize;
    let mut junk = 0usize;

    // Good txs
    let mut good_txs = Vec::new();
    for _ in 0..100 {
        if let Ok(tx) = net.payment("u0", "u1", 1, 1) {
            good += 1;
            good_txs.push(tx);
        }
    }
    // Duplicate flood (same tx_id)
    if let Some(tx) = good_txs.first() {
        for _ in 0..500 {
            for n in net.nodes.values_mut() {
                let _ = n.submit_tx(tx.clone());
            }
            dup += 1;
        }
    }
    // Junk: bad signature
    for i in 0..50 {
        let mut tx = Tx::sign_with(
            &net.user_keys,
            "u0",
            99999 + i as u64,
            "u1",
            1,
            1,
            net.cfg.network_id,
        )
        .unwrap();
        tx.signature = "00".repeat(32);
        for n in net.nodes.values_mut() {
            if n.submit_tx(tx.clone()).is_err() {
                junk += 1;
                break;
            }
        }
    }
    let mp_before = net.nodes.values().next().map(|n| n.mempool.len()).unwrap_or(0);
    let (heights, sealed, mut errs) = drain_until_empty(&mut net, 50);
    let agree = tips_agree(&net);
    // Mempool should not explode from dups (dedupe)
    if mp_before > good + 10 {
        errs.push(format!("mempool bloated {mp_before} after dups (good={good})"));
    }
    if !agree {
        errs.push("tips disagree".into());
    }
    if sealed == 0 {
        errs.push("nothing sealed".into());
    }

    let ok = errs.is_empty() && sealed > 0 && agree;
    Report {
        name: "adversarial".into(),
        ok,
        detail: format!(
            "good={good} dup_submits={dup} junk_rejected≈{junk} mempool_before_drain={mp_before} \
             heights={heights} sealed={sealed} agree={agree} errs={errs:?}"
        ),
        ms: t0.elapsed().as_millis(),
    }
}

fn run_idle(args: &Args, scheme: CryptoScheme) -> Report {
    let t0 = Instant::now();
    let mut net = make_net(args.nodes, args.max_txs_per_ledger, scheme, 3, 1_000_000);
    let tip0 = net.tip_height();
    let mut empty_refused = 0u64;
    for _ in 0..50 {
        net.tick_all(100);
        net.pong_all();
        match net.seal_next_height() {
            Ok(r) if r.n_txs == 0 => {
                return Report {
                    name: "idle".into(),
                    ok: false,
                    detail: "empty ledger was sealed".into(),
                    ms: t0.elapsed().as_millis(),
                };
            }
            Ok(_) => {
                return Report {
                    name: "idle".into(),
                    ok: false,
                    detail: "progressed without txs".into(),
                    ms: t0.elapsed().as_millis(),
                };
            }
            Err(e) if e.contains("empty") => empty_refused += 1,
            Err(e) => {
                // also "no applicable" etc.
                if e.contains("empty") || e.contains("no applicable") {
                    empty_refused += 1;
                } else {
                    return Report {
                        name: "idle".into(),
                        ok: false,
                        detail: format!("unexpected: {e}"),
                        ms: t0.elapsed().as_millis(),
                    };
                }
            }
        }
    }
    let tip1 = net.tip_height();
    let ok = tip0 == tip1 && tip1 == 0 && empty_refused > 0;
    Report {
        name: "idle".into(),
        ok,
        detail: format!(
            "tip stayed {tip1}; empty_refused={empty_refused}/50 seal attempts; agree={}",
            tips_agree(&net)
        ),
        ms: t0.elapsed().as_millis(),
    }
}

fn run_thrash(args: &Args, scheme: CryptoScheme) -> Report {
    let t0 = Instant::now();
    let users = 8usize;
    let mut net = make_net(args.nodes, 16, scheme, users, 20_000_000);
    // Preload
    for i in 0..400 {
        let from = format!("u{}", i % users);
        let to = format!("u{}", (i + 1) % users);
        let _ = net.payment(&from, &to, 1, 1);
    }
    let mut seals = 0u64;
    let mut fails = 0u64;
    // Thrash: many seal attempts interleaved with small submits
    for i in 0..100 {
        if i % 3 == 0 {
            let from = format!("u{}", i % users);
            let to = format!("u{}", (i + 2) % users);
            let _ = net.payment(&from, &to, 1, 1);
        }
        net.tick_all(1);
        match net.seal_next_height() {
            Ok(r) => {
                if r.n_txs == 0 {
                    fails += 1;
                } else {
                    seals += 1;
                }
            }
            Err(_) => fails += 1,
        }
    }
    let agree = tips_agree(&net);
    let ok = agree && seals > 0 && net.tip_height() == seals;
    Report {
        name: "thrash".into(),
        ok,
        detail: format!(
            "successful_seals={seals} fail_or_empty={fails} tip={} mempool_left={} agree={agree}",
            net.tip_height(),
            net.nodes.values().next().map(|n| n.mempool.len()).unwrap_or(0)
        ),
        ms: t0.elapsed().as_millis(),
    }
}

fn main() {
    let args = Args::parse();
    let scheme = CryptoScheme::parse(&args.scheme).unwrap_or(CryptoScheme::HmacDev);

    println!("=== Falcon PL load harness ===");
    println!(
        "scheme={} nodes={} max_txs/ledger={}",
        scheme.as_str(),
        args.nodes,
        args.max_txs_per_ledger
    );
    println!();

    let scenarios: Vec<Scenario> = match &args.scenario {
        Scenario::All => vec![
            Scenario::Idle,
            Scenario::Flood,
            Scenario::Soak,
            Scenario::Adversarial,
            Scenario::Thrash,
        ],
        s => vec![s.clone()],
    };

    let mut reports = Vec::new();
    for s in scenarios {
        let r = match s {
            Scenario::All => unreachable!(),
            Scenario::Flood => run_flood(&args, scheme),
            Scenario::Soak => run_soak(&args, scheme),
            Scenario::Adversarial => run_adversarial(&args, scheme),
            Scenario::Idle => run_idle(&args, scheme),
            Scenario::Thrash => run_thrash(&args, scheme),
        };
        let mark = if r.ok { "PASS" } else { "FAIL" };
        println!("[{mark}] {} ({} ms)", r.name, r.ms);
        println!("       {}", r.detail);
        println!();
        reports.push(r);
    }

    let pass = reports.iter().filter(|r| r.ok).count();
    let total = reports.len();
    println!("=== summary {pass}/{total} passed ===");
    if pass != total {
        std::process::exit(1);
    }
}
