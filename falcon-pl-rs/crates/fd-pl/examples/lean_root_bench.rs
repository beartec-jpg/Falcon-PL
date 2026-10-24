//! v2.9.58: check and time the cached commitment root on a real snapshot.
//!
//! `cargo run --release --example lean_root_bench -- snapshots/vN.json`
//!
//! Read-only: loads the snapshot, compares the cached root with the plain
//! (pre-2.9.58) serialize+sha256 root and with the tip ledger's
//! `state_root_after`, and times root / snapshot encode / status paths.
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("snapshot path");
    let t = Instant::now();
    let keys = fd_pl::crypto::KeyRing::new(fd_pl::crypto::CryptoScheme::HmacDev);
    let n = fd_pl::Node::load_snapshot(&path, keys).expect("load snapshot");
    println!("load {:.2}s tip={} exec_headers={}", t.elapsed().as_secs_f64(), n.tip_height, n.bridge.eth_exec_headers.len());
    let t = Instant::now();
    let fresh = n.commitment_root_fresh();
    let t_fresh = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let c1 = n.commitment_root();
    let t_miss = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let c2 = n.commitment_root();
    let t_hit = t.elapsed().as_secs_f64();
    let tip_root = n.chain.last().filter(|l| l.ledger_hash == n.tip_hash).map(|l| l.state_root_after.clone());
    println!("fresh  {:.3}s {}", t_fresh, fresh);
    println!("cached {:.3}s (first: encode map + hash) {}", t_miss, c1);
    println!("cached {:.3}s (memo hit)              {}", t_hit, c2);
    println!("tip ledger state_root_after           {}", tip_root.clone().unwrap_or_default());
    assert_eq!(fresh, c1);
    assert_eq!(fresh, c2);
    if let Some(r) = tip_root {
        assert_eq!(fresh, r, "root of loaded state == tip ledger root");
    }
    let snap = n.to_snapshot();
    let t = Instant::now();
    let plain = serde_json::to_vec(&snap).unwrap();
    let t_plain = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let split = fd_pl::exec_map::to_vec_split(&snap).unwrap();
    let t_split = t.elapsed().as_secs_f64();
    assert!(plain == split, "snapshot bytes identical");
    println!("snapshot encode: plain {:.3}s split {:.3}s ({} bytes, identical)", t_plain, t_split, plain.len());
    let t = Instant::now();
    let _ = n.status_json_slim();
    let t_full = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let b = n.status_json_brief();
    let t_brief = t.elapsed().as_secs_f64();
    assert_eq!(b["state_root"].as_str(), Some(fresh.as_str()));
    println!("status: full {:.3}s brief {:.3}s", t_full, t_brief);
    println!("root_stats (full map hashes, memo hits) = {:?}", fd_pl::exec_map::root_stats());
    println!("OK");
}
