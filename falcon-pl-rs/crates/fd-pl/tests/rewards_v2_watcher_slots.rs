//! v2.9.62 A1: watcher presence slots survive a restart (snapshot round-trip)
//! once rewards v2 is active; unchanged (and byte-identical) before.

use fd_pl::crypto::{CryptoScheme, KeyRing};
use fd_pl::economy::EpochWork;
use fd_pl::{ChainConfig, Node};

#[test]
fn v2_bitset_dedups_across_serde_roundtrip() {
    let mut w = EpochWork::default();
    w.record_watcher_slot_v2("watcher-browser", 133);
    w.record_watcher_slot_v2("watcher-browser", 133);
    assert_eq!(w.watcher_slots["watcher-browser"], 1);
    let json = serde_json::to_string(&w).unwrap();
    let mut back: EpochWork = serde_json::from_str(&json).unwrap();
    back.record_watcher_slot_v2("watcher-browser", 133);
    assert_eq!(back.watcher_slots["watcher-browser"], 1, "same slot after restart must not count twice");
    back.record_watcher_slot_v2("watcher-browser", 134);
    back.record_watcher_slot_v2("watcher-browser", 0);
    back.record_watcher_slot_v2("watcher-browser", 167);
    back.record_watcher_slot_v2("watcher-browser", 168); // out of range: ignored
    assert_eq!(back.watcher_slots["watcher-browser"], 4);
}

#[test]
fn v1_path_unchanged_and_serialization_identical() {
    let mut w = EpochWork::default();
    w.record_watcher_slot("a", 3);
    let json = serde_json::to_string(&w).unwrap();
    assert!(!json.contains("watcher_slot_bits"), "empty v2 field must not be serialized: {json}");
    // Old snapshots (no field) still load.
    let back: EpochWork = serde_json::from_str(&json).unwrap();
    assert!(back.watcher_slot_bits.is_empty());
}

fn node(armed: bool) -> Node {
    let cfg = ChainConfig { epoch_ms: 168 * 1_000, emission_bps: 30, ..Default::default() };
    let mut n = Node::new("v1", cfg, KeyRing::new(CryptoScheme::HmacDev), &[]);
    n.genesis_ms = n.now_ms;
    if armed {
        n.rewards_v2_from_height = 1;
        n.rewards_v2_epoch = 1;
    }
    n
}

#[test]
fn node_unarmed_uses_v1_path() {
    let mut n = node(false);
    assert!(!n.rv2_epoch(n.current_epoch()));
    n.watcher_credit("w", 1, true);
    assert!(n.epoch_work.watcher_slot_bits.is_empty());
    assert_eq!(n.epoch_work.watcher_slots["w"], 1);
}

#[test]
fn node_armed_restart_does_not_double_count() {
    let mut n = node(true);
    assert!(n.rv2_epoch(n.current_epoch()));
    n.watcher_credit("w", 1, true);
    n.watcher_credit("w", 1, true);
    assert_eq!(n.epoch_work.watcher_slots["w"], 1);
    // Simulate restart: EpochWork is what snapshots persist.
    let json = serde_json::to_string(&n.epoch_work).unwrap();
    n.epoch_work = serde_json::from_str(&json).unwrap();
    n.watcher_credit("w", 1, true);
    assert_eq!(n.epoch_work.watcher_slots["w"], 1);
    assert_eq!(n.epoch_work.watcher_work["w"], 3);
}

#[test]
fn armed_but_before_epoch_gate_stays_v1() {
    let mut n = node(true);
    n.rewards_v2_epoch = 50;
    assert!(!n.rv2_epoch(n.current_epoch()));
}
