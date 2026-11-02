//! v2.10.3 (TBD heights): lagging-proposer skip and Hello release floor.
use super::*;

fn node() -> Node {
    let cfg = ChainConfig::default();
    let mut net = Network::bootstrap(&["v1", "v2", "v3", "v4"], 1000, &[], cfg);
    net.nodes.remove("v1").unwrap()
}

/// Replace the in-memory chain with `LAGGING_WINDOW` synthetic ledgers ending
/// at `tip`. Each cert has committee = all 4 seats, voters = all but `lagger`
/// from `stall_from` on.
fn fill_chain(n: &mut Node, tip: u64, lagger: &str, stall_from: u64) {
    let tmpl: Ledger = serde_json::from_value(serde_json::json!({
        "height": 0, "parent_hash": "", "network_id": n.cfg.network_id,
        "packer": "", "packer_public_key": "", "tx_ids": [], "txs": [],
        "state_root_after": "", "sealed_at_ms": 0, "packer_sig": "", "ledger_hash": ""
    }))
    .unwrap();
    let seats = ["v1", "v2", "v3", "v4"];
    let w = Node::LAGGING_WINDOW as u64;
    n.chain.clear();
    for h in (tip + 1 - w)..=tip {
        let mut l = tmpl.clone();
        l.height = h;
        // Lagger never packs; rotate the others.
        let packers: Vec<&str> = seats.iter().copied().filter(|s| *s != lagger).collect();
        l.packer = packers[(h as usize) % packers.len()].to_string();
        let voters: Vec<String> = seats
            .iter()
            .filter(|s| h - 1 < stall_from || **s != lagger)
            .map(|s| s.to_string())
            .collect();
        l.parent_cert = Some(LedgerCert {
            height: h - 1,
            committee: seats.iter().map(|s| s.to_string()).collect(),
            voters,
            ..Default::default()
        });
        n.chain.push(l);
    }
    n.tip_height = tip;
}

#[test]
fn healthy_chain_has_no_lagging_seats() {
    let mut n = node();
    fill_chain(&mut n, 100, "v4", u64::MAX);
    assert!(n.lagging_seats().is_empty());
}

#[test]
fn seat_three_behind_is_kept_four_behind_is_skipped() {
    let mut n = node();
    // Last vote by v4 is in cert for h=97 (ledger 98) -> 3 behind tip 100.
    fill_chain(&mut n, 100, "v4", 98);
    assert!(n.lagging_seats().is_empty(), "exactly 3 behind is not lagging");
    let mut n = node();
    fill_chain(&mut n, 100, "v4", 97); // last attested 96 -> 4 behind
    assert_eq!(n.lagging_seats(), BTreeSet::from(["v4".to_string()]));
}

#[test]
fn short_chain_skips_nothing() {
    let mut n = node();
    fill_chain(&mut n, 100, "v4", 50);
    n.chain.drain(0..2);
    assert!(n.lagging_seats().is_empty());
}

#[test]
fn not_in_committee_is_not_lagging() {
    let mut n = node();
    fill_chain(&mut n, 100, "v4", u64::MAX);
    for l in n.chain.iter_mut() {
        if let Some(c) = l.parent_cert.as_mut() {
            c.committee.retain(|s| s != "v4");
            c.voters.retain(|s| s != "v4");
        }
    }
    assert!(n.lagging_seats().is_empty());
}

fn draws(n: &Node) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for skips in 0..4 {
        let mut ex = BTreeSet::new();
        for w in out.iter().take(skips) {
            ex.insert(w.clone());
        }
        if let Some(w) = n.lottery_packer_with_skips(&ex) {
            out.insert(w);
        }
    }
    out
}

#[test]
fn lottery_unchanged_before_activation() {
    let mut n = node();
    fill_chain(&mut n, 100, "v4", 50);
    assert_eq!(n.lagging_skip_from_height, u64::MAX);
    let before = n.draw_committee();
    n.lagging_skip_from_height = 10_000; // still in the future
    assert_eq!(n.draw_committee(), before);
    assert!(before.iter().any(|s| s == "v4"), "v4 still drawn pre-activation");
}

#[test]
fn lottery_skips_lagging_seat_after_activation() {
    let mut n = node();
    fill_chain(&mut n, 100, "v4", 50);
    n.lagging_skip_from_height = 0;
    let order = n.draw_committee();
    // Lagging seats only fill in once every in-sync seat is drawn: never the
    // proposer, at worst the last backup.
    assert_ne!(order.first().map(String::as_str), Some("v4"), "order {order:?}");
    if let Some(pos) = order.iter().position(|s| s == "v4") {
        assert_eq!(pos, 3, "v4 only at the tail: {order:?}");
    }
    assert_ne!(n.lottery_packer_with_skips(&BTreeSet::new()).as_deref(), Some("v4"));
    let _ = draws(&n);
}

#[test]
fn lottery_never_empties_when_all_lag() {
    let mut n = node();
    fill_chain(&mut n, 100, "v4", 50);
    n.lagging_skip_from_height = 0;
    // Only v4 left eligible: fallback must still draw it.
    let ex: BTreeSet<String> = ["v1", "v2", "v3"].iter().map(|s| s.to_string()).collect();
    assert_eq!(n.lottery_packer_with_skips(&ex).as_deref(), Some("v4"));
}

#[test]
fn activation_constant_is_placeholder() {
    assert_eq!(crate::params_v2::TESTNET_2300_LAGGING_PROPOSER_SKIP_HEIGHT, u64::MAX);
    assert_eq!(
        crate::params_v2::lagging_proposer_skip_from_height(crate::NETWORK_ID_PUBLIC_TESTNET_2300),
        u64::MAX
    );
    assert_eq!(crate::params_v2::TESTNET_2300_HELLO_MIN_RELEASE_HEIGHT, u64::MAX);
}

#[test]
fn hello_release_detail() {
    assert!(hello_release_reject_detail("2.10.2", "2.10.2").is_none());
    assert!(hello_release_reject_detail("2.10.10", "2.10.2").is_none());
    assert!(hello_release_reject_detail("3.0.0", "2.10.2").is_none());
    let r = hello_release_reject_detail("2.10.1", "2.10.2").unwrap();
    assert!(r.contains("2.10.1 < minimum 2.10.2"), "{r}");
    assert!(hello_release_reject_detail("", "2.10.2").unwrap().contains("no release_version"));
    assert!(hello_release_reject_detail("2.10", "2.10.2").unwrap().contains("unparseable"));
}

fn hello(release: &str) -> WireMsg {
    WireMsg::Hello {
        id: "v2".into(),
        public_key: String::new(),
        validators: BTreeMap::new(),
        publics: BTreeMap::new(),
        tip_height: 0,
        tip_hash: String::new(),
        is_hub: false,
        mempool_len: 0,
        product_version: HELLO_WIRE_VERSION.into(),
        hello_network_id: 0,
        archive: false,
        snap_height: 0,
        release_version: release.into(),
    }
}

#[test]
fn hello_session_release_gate_by_height() {
    let mut n = node();
    n.tip_height = 50;
    n.hello_min_release_from_height = 100;
    let mut s = HelloSession::default();
    assert_eq!(s.check(&n, &hello("")), HelloGate::Pass, "before activation: legacy ok");
    n.tip_height = 100;
    let mut s = HelloSession::default();
    match s.check(&n, &hello("")) {
        HelloGate::Close(r) => assert!(r.starts_with(HELLO_ERR_RELEASE_TOO_OLD), "{r}"),
        g => panic!("expected close, got {g:?}"),
    }
    let mut s = HelloSession::default();
    assert_eq!(s.check(&n, &hello(PRODUCT_VERSION)), HelloGate::Pass);
}

#[test]
fn hello_release_field_omitted_when_empty() {
    let s = serde_json::to_string(&hello("")).unwrap();
    assert!(!s.contains("release_version"));
    let s = serde_json::to_string(&hello("2.10.2")).unwrap();
    assert!(s.contains("\"release_version\":\"2.10.2\""));
}
