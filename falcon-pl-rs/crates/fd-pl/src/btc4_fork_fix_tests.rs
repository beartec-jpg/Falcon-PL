//! v2.10.1 BTC4 header fork fix (`btc4_fork_fix_from_height`).
//!
//! Reproduces the live testnet4 stall: stored main chain up to a stale 157549
//! on top of the common ancestor 157548, a pending wrong fork at 157549, and the
//! real chain 157549..=157560. Before the height (2.10.0 rules) the real chain
//! can never be admitted; from the height it accumulates as a competing branch,
//! replaces the pending fork and reorgs the rail onto the real chain.
use crate::bridge::{RailHeader, RailState, RAIL_BTC4, BTC_FORK_STALE_N};
use crate::btc_spv;

const BITS: u32 = 0x207fffff;

fn rh(height: u64, p: &btc_spv::ParsedHeader) -> RailHeader {
    RailHeader { height, hash: String::new(), parent_hash: String::new(), merkle_root: String::new(), raw: p.raw_hex.clone() }
}

fn mine(prev: &str, tag: u8, t: u32) -> btc_spv::ParsedHeader {
    btc_spv::mine_header(prev, &format!("{tag:02x}").repeat(32), BITS, t).unwrap()
}

struct Stuck {
    r: RailState,
    ancestor: btc_spv::ParsedHeader,
    stale: btc_spv::ParsedHeader,
    wrong: btc_spv::ParsedHeader,
    real: Vec<btc_spv::ParsedHeader>, // 157549..=157560
}

/// Main 157540..=157548 (ancestor) + stale 157549; pending wrong fork at 157549.
fn stuck(fork_fix: bool) -> Stuck {
    let mut r = RailState::new(RAIL_BTC4);
    let mut prev = "00".repeat(32);
    let mut ancestor = None;
    for (i, h) in (157_540u64..=157_548).enumerate() {
        let p = mine(&prev, 0x10 + i as u8, 1_700_000_000 + i as u32);
        assert!(r.submit_header_ex2(rh(h, &p), true, fork_fix).unwrap());
        prev = p.hash_display.clone();
        ancestor = Some(p);
    }
    let ancestor = ancestor.unwrap();
    let stale = mine(&ancestor.hash_display, 0xa1, 1_700_000_100);
    assert!(r.submit_header_ex2(rh(157_549, &stale), true, fork_fix).unwrap());
    let wrong = mine(&ancestor.hash_display, 0xb1, 1_700_000_101);
    assert!(!r.submit_header_ex2(rh(157_549, &wrong), true, fork_fix).unwrap(), "equal work: pending only");
    assert_eq!(r.btc_fork.len(), 1);
    let mut real = vec![];
    let mut p = ancestor.hash_display.clone();
    for i in 0..12u32 {
        let h = mine(&p, 0xc0 + i as u8, 1_700_000_200 + i);
        p = h.hash_display.clone();
        real.push(h);
    }
    assert_eq!(r.tip_height, 157_549);
    assert_eq!(r.tip_hash, stale.hash_display);
    Stuck { r, ancestor, stale, wrong, real }
}

#[test]
fn pre_height_stuck_case_unchanged_real_chain_rejected() {
    let mut s = stuck(false);
    let e = s.r.submit_header_ex2(rh(157_549, &s.real[0]), true, false).unwrap_err();
    assert_eq!(e, "BTC fork has less work than the pending fork");
    // Same through the 2.10.0 entry point.
    let e = s.r.submit_header_ex(rh(157_549, &s.real[0]), true).unwrap_err();
    assert_eq!(e, "BTC fork has less work than the pending fork");
    for (i, h) in s.real.iter().enumerate().skip(1) {
        assert!(s.r.submit_header_ex2(rh(157_549 + i as u64, h), true, false).is_err());
    }
    assert_eq!(s.r.tip_height, 157_549);
    assert_eq!(s.r.tip_hash, s.stale.hash_display);
    assert_eq!(s.r.btc_fork.len(), 1);
    assert_eq!(s.r.btc_fork[0].hash, s.wrong.hash_display);
    assert!(s.r.btc_fork_alt.is_empty());
    // Commitment encoding identical to 2.10.0: no new key.
    let j = serde_json::to_string(&s.r).unwrap();
    assert!(!j.contains("btc_fork_alt"));
}

#[test]
fn pre_height_state_identical_to_2_10_0_entry_point() {
    let mut a = stuck(false);
    let mut b = stuck(false);
    for (i, h) in a.real.clone().iter().enumerate() {
        let x = a.r.submit_header_ex2(rh(157_549 + i as u64, h), true, false);
        let y = b.r.submit_header_ex(rh(157_549 + i as u64, h), true);
        assert_eq!(x, y);
    }
    assert_eq!(serde_json::to_string(&a.r).unwrap(), serde_json::to_string(&b.r).unwrap());
}

#[test]
fn post_height_real_chain_reorgs_past_pending_wrong_fork() {
    let mut s = stuck(true);
    // 157549 real: accepted as competing branch (no tip move).
    assert!(!s.r.submit_header_ex2(rh(157_549, &s.real[0]), true, true).unwrap());
    assert_eq!(s.r.btc_fork_alt.len(), 1);
    // Re-submit is a duplicate (submitters must move on to 157550).
    let e = s.r.submit_header_ex2(rh(157_549, &s.real[0]), true, true).unwrap_err();
    assert!(e.contains("competing branch"), "{e}");
    // 157550: branch now heavier than pending fork and main suffix -> reorg.
    assert!(s.r.submit_header_ex2(rh(157_550, &s.real[1]), true, true).unwrap());
    assert_eq!(s.r.tip_height, 157_550);
    assert_eq!(s.r.headers.get(&157_549).unwrap().hash, s.real[0].hash_display);
    assert_eq!(s.r.headers.get(&157_548).unwrap().hash, s.ancestor.hash_display);
    assert!(s.r.btc_fork.is_empty() && s.r.btc_fork_alt.is_empty());
    for (i, h) in s.real.iter().enumerate().skip(2) {
        assert!(s.r.submit_header_ex2(rh(157_549 + i as u64, h), true, true).unwrap());
    }
    assert_eq!(s.r.tip_height, 157_560);
    assert_eq!(s.r.tip_hash, s.real[11].hash_display);
    let j = serde_json::to_string(&s.r).unwrap();
    assert!(!j.contains("btc_fork"));
}

#[test]
fn post_height_fix_is_btc4_only() {
    // Legacy BTC rail keeps 2.10.0 behaviour even with the gate open.
    let mut r = RailState::new(crate::bridge::RAIL_BTC);
    let mut prev = "00".repeat(32);
    let mut anc = None;
    for (i, h) in (100u64..=102).enumerate() {
        let p = mine(&prev, 0x20 + i as u8, 1_700_001_000 + i as u32);
        assert!(r.submit_header_ex2(rh(h, &p), true, true).unwrap());
        prev = p.hash_display.clone();
        anc = Some(p);
    }
    let anc = anc.unwrap();
    let s = mine(&anc.hash_display, 0x31, 1_700_001_100);
    r.submit_header_ex2(rh(103, &s), true, true).unwrap();
    let w = mine(&anc.hash_display, 0x32, 1_700_001_101);
    assert!(!r.submit_header_ex2(rh(103, &w), true, true).unwrap());
    let x = mine(&anc.hash_display, 0x33, 1_700_001_102);
    assert!(r.submit_header_ex2(rh(103, &x), true, true).unwrap_err().contains("less work than the pending fork"));
}

#[test]
fn post_height_stale_pending_fork_dropped_after_n_main_headers() {
    for fork_fix in [false, true] {
        let mut s = stuck(fork_fix);
        let mut prev = s.stale.hash_display.clone();
        for i in 0..(BTC_FORK_STALE_N + 1) {
            let p = mine(&prev, 0xd0 + i as u8, 1_700_000_300 + i as u32);
            assert!(s.r.submit_header_ex2(rh(157_550 + i, &p), true, fork_fix).unwrap());
            prev = p.hash_display.clone();
        }
        assert_eq!(s.r.btc_fork.is_empty(), fork_fix, "fork_fix={fork_fix}");
    }
}

#[test]
fn activation_height_gate() {
    let h = crate::params_v2::TESTNET_2300_BTC4_FORK_FIX_HEIGHT;
    assert!(h > crate::params_v2::TESTNET_2300_V2_10_FROM_HEIGHT);
    assert_eq!(crate::params_v2::btc4_fork_fix_from_height(crate::NETWORK_ID_PUBLIC_TESTNET_2300), h);
}
