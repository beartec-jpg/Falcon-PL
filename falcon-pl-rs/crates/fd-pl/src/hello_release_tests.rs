// Copyright (c) 2025-2026 BEARTEC. All rights reserved. See LICENSE.
//! v2.10.3: Hello `release_version` (sent; enforcement off until
//! `hello_min_release_from_height`, u64::MAX on 2300).
use super::*;

fn node() -> Node {
    let cfg = ChainConfig::default();
    let mut net = Network::bootstrap(&["v1", "v2", "v3", "v4"], 1000, &[], cfg);
    net.nodes.remove("v1").unwrap()
}

#[test]
fn enforcement_off_on_2300() {
    assert_eq!(crate::params_v2::TESTNET_2300_HELLO_MIN_RELEASE_HEIGHT, u64::MAX);
    assert_eq!(
        crate::params_v2::hello_min_release_from_height(crate::NETWORK_ID_PUBLIC_TESTNET_2300),
        u64::MAX
    );
    assert_eq!(HELLO_MIN_RELEASE, "2.10.3");
}

#[test]
fn legacy_2_10_2_hello_accepted_by_default() {
    let mut n = node();
    n.hello_min_release_from_height = u64::MAX;
    n.tip_height = 1_000_000;
    let mut s = HelloSession::default();
    assert_eq!(s.check(&n, &hello("")), HelloGate::Pass);
}

#[test]
fn hello_release_detail() {
    assert!(hello_release_reject_detail("2.10.3", "2.10.3").is_none());
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
