//! v2.9.62 A13: consensus keys come from a deterministic registry from `H_act`.
//!
//! Regression for the devnet halt (installer worker, 9 Oct 2026): a validator
//! bonded the legacy way and connected to ONE seat. That seat learned its key
//! from the Hello, counted its OK vote and formed a cert with it; every other
//! seat had no key, rejected the cert (`cert bad sig`) and the chain halted.

use super::*;

fn users() -> Vec<(String, u64)> {
    vec![("alice".into(), 1_000_000), ("bob".into(), 1_000_000)]
}

fn net6() -> Network {
    let cfg = ChainConfig {
        pack_budget_ms: 0,
        max_txs_per_ledger: 8,
        progress_timeout_ms: 0,
        scheme: CryptoScheme::Falcon512,
        ..Default::default()
    };
    let mut net = Network::bootstrap(&["v1", "v2", "v3", "v4", "v5", "v6"], 1000, &users(), cfg);
    net.pong_all();
    net.payment("alice", "bob", 1, 2).unwrap();
    net.seal_next_height().unwrap();
    for n in net.nodes.values_mut() {
        n.cfg.require_vote_sigs = true; // the harness seals without vote sigs
    }
    net
}

/// Cert for the current tip signed by v1, v2, v3 and the outsider `vx`.
fn cert_with_outsider(net: &Network, vx_keys: &KeyRing) -> LedgerCert {
    let n = &net.nodes["v1"];
    let (h, hash) = (n.tip_height, n.tip_hash.clone());
    let nid = n.cfg.network_id;
    let voters: Vec<String> = ["v1", "v2", "v3", "vx"].iter().map(|s| s.to_string()).collect();
    let signatures = voters
        .iter()
        .map(|v| {
            let p = Node::vote_ok_payload(nid, h, &hash, v);
            if v == "vx" { vx_keys.sign(v, &p).unwrap() } else { n.keys.sign(v, &p).unwrap() }
        })
        .collect();
    LedgerCert {
        height: h,
        ledger_hash: hash,
        parent_hash: n.chain.last().unwrap().parent_hash.clone(),
        committee: ["v1", "v2", "v3", "v4", "v5", "vx"].iter().map(|s| s.to_string()).collect(),
        voters,
        signatures,
        round: 0,
    }
}

fn add_outsider(net: &mut Network) -> KeyRing {
    let mut vx = KeyRing::new(CryptoScheme::Falcon512);
    vx.generate_id("vx");
    let pk = vx.public_hex("vx").unwrap();
    for n in net.nodes.values_mut() {
        n.register_validator("vx", 1000); // legacy bond before H_act
    }
    // Only v1 hears vx's Hello.
    net.nodes.get_mut("v1").unwrap().keys.insert_public("vx", &pk).unwrap();
    vx
}

#[test]
fn a13_repro_unarmed_hello_key_splits_cert_verdicts() {
    let mut net = net6();
    let vx = add_outsider(&mut net);
    let cert = cert_with_outsider(&net, &vx);
    // Pre-fix behaviour (also live 2.9.61): v1 accepts, v2 rejects → halt.
    assert!(net.nodes["v1"].verify_cert(&cert).is_ok());
    let e = net.nodes["v2"].verify_cert(&cert).unwrap_err();
    assert!(e.contains("cert bad sig"), "{e}");
    // ...and v1 would count vx's vote toward a cert.
    let n1 = &net.nodes["v1"];
    let p = Node::vote_ok_payload(n1.cfg.network_id, n1.tip_height + 1, "aa", "vx");
    assert!(n1.accept_vote_sig("vx", &p, &vx.sign("vx", &p).unwrap()));
}

fn arm(net: &mut Network) {
    for n in net.nodes.values_mut() {
        n.rewards_v2_from_height = 1;
        let ids: Vec<String> = ["v1", "v2", "v3", "v4", "v5", "v6"].iter().map(|s| s.to_string()).collect();
        n.init_seat_key_registry_from_genesis(&ids);
    }
}

#[test]
fn a13_armed_every_seat_rejects_unregistered_voter_identically() {
    let mut net = net6();
    let vx = add_outsider(&mut net);
    arm(&mut net);
    let cert = cert_with_outsider(&net, &vx);
    for (id, n) in &net.nodes {
        let e = n.verify_cert(&cert).unwrap_err();
        assert!(e.contains("no registered key"), "{id}: {e}");
        assert_eq!(n.seat_key_registry.len(), 6);
    }
    // v1 knows the key from the Hello but does not count the vote.
    let n1 = &net.nodes["v1"];
    let p = Node::vote_ok_payload(n1.cfg.network_id, n1.tip_height + 1, "aa", "vx");
    assert!(!n1.accept_vote_sig("vx", &p, &vx.sign("vx", &p).unwrap()));
    // Registered seats still verify; a cert of 4 registered voters passes everywhere.
    let p2 = Node::vote_ok_payload(n1.cfg.network_id, n1.tip_height + 1, "aa", "v2");
    assert!(n1.accept_vote_sig("v2", &p2, &n1.keys.sign("v2", &p2).unwrap()));
}

#[test]
fn a13_registry_ignores_a_spoofed_hello_key_for_a_seat() {
    let mut net = net6();
    arm(&mut net);
    // A wrong key for v2 cannot replace the registry key for consensus.
    let mut evil = KeyRing::new(CryptoScheme::Falcon512);
    evil.generate_id("v2");
    let n1 = &net.nodes["v1"];
    let p = Node::vote_ok_payload(n1.cfg.network_id, n1.tip_height + 1, "aa", "v2");
    assert!(!n1.accept_vote_sig("v2", &p, &evil.sign("v2", &p).unwrap()));
}

#[test]
fn a13_unregistered_legacy_seat_goes_inactive_at_h_act_and_cannot_reactivate() {
    let mut net = net6();
    let _vx = add_outsider(&mut net);
    for n in net.nodes.values_mut() {
        let ids: Vec<String> = ["v1", "v2", "v3", "v4", "v5", "v6"].iter().map(|s| s.to_string()).collect();
        n.init_seat_key_registry_from_genesis(&ids);
        n.rewards_v2_from_height = n.tip_height + 1;
        n.cfg.require_vote_sigs = false;
    }
    net.payment("alice", "bob", 1, 2).unwrap();
    net.seal_next_height().unwrap();
    for (id, n) in &net.nodes {
        assert!(n.validators["vx"].inactive, "{id}: vx inactive at H_act");
        assert!(!n.validators["v3"].inactive);
        assert!(!n.lottery_order().iter().any(|x| x == "vx"), "{id}: vx not drawn");
    }
    let roots: BTreeSet<String> = net.nodes.values().map(|n| n.commitment_root()).collect();
    assert_eq!(roots.len(), 1);
    // Reactivate by the owner is refused (no registered key).
    let n = net.nodes.get_mut("v1").unwrap();
    let mut k = KeyRing::new(CryptoScheme::Falcon512);
    k.generate_id("vx");
    n.keys.set_public_from_chain("vx", &k.public_hex("vx").unwrap()).unwrap();
    let t = Tx::sign_body(&k, "vx", n.state.get("vx").sequence, "", 0, 0, n.cfg.network_id,
        TxBody::Reactivate { validator: "vx".into() }).unwrap();
    let e = n.apply_protocol_tx(&t).unwrap_err();
    assert!(e.contains("no registered key"), "{e}");
}

#[test]
fn a13_empty_registry_keeps_keyring_behaviour() {
    // Devnets / tests with no registry: unchanged (and 2300 refuses to start armed with it empty).
    let mut net = net6();
    for n in net.nodes.values_mut() {
        n.rewards_v2_from_height = 1;
    }
    assert!(!net.nodes["v1"].key_registry_at(5));
    // Arming PR: the 7 live seats, each a valid falcon-512 key.
    let ks = crate::params_v2::TESTNET_2300_SEAT_KEYS;
    let ids: Vec<&str> = ks.iter().map(|(i, _)| *i).collect();
    assert_eq!(ids, ["v1", "v3", "v6", "v10", "v11", "v12", "v13"]);
    for (_, pk) in ks {
        assert!(crate::crypto::PublicKey::from_hex(CryptoScheme::Falcon512, pk).is_ok());
    }
    assert_eq!(crate::params_v2::network_seat_keys(2300).len(), 7);
}

/// 2.9.79-beta.3: after a fresh snapshot adopt the ring may lack (or hold a
/// stale key for) a registered seat; certs/votes must still verify against
/// the registry key, exactly as a restarted seat does.
#[test]
fn a13_registry_cert_verifies_without_ring_key_after_adopt() {
    let mut net = net6();
    arm(&mut net);
    let n1 = &net.nodes["v1"];
    let (h, hash, nid) = (n1.tip_height, n1.tip_hash.clone(), n1.cfg.network_id);
    let voters: Vec<String> = ["v1", "v2", "v3", "v4"].iter().map(|s| s.to_string()).collect();
    let signatures = voters.iter().map(|v| n1.keys.sign(v, &Node::vote_ok_payload(nid, h, &hash, v)).unwrap()).collect();
    let cert = LedgerCert {
        height: h,
        ledger_hash: hash.clone(),
        parent_hash: n1.chain.last().unwrap().parent_hash.clone(),
        committee: ["v1", "v2", "v3", "v4", "v5", "v6"].iter().map(|s| s.to_string()).collect(),
        voters,
        signatures,
        round: 0,
    };
    let p = Node::vote_ok_payload(nid, h + 1, "aa", "v3");
    let vsig = n1.keys.sign("v3", &p).unwrap();
    // Ring without v3 (snapshot-adopted joiner) and with a stale v3 key.
    let mut stale = KeyRing::new(CryptoScheme::Falcon512);
    stale.generate_id("v3");
    for with_stale in [false, true] {
        let n = net.nodes.get_mut("v2").unwrap();
        let mut ring = KeyRing::new(CryptoScheme::Falcon512);
        for (id, pk) in n.keys.export_publics() {
            if id != "v3" {
                ring.insert_public(&id, &pk).unwrap();
            }
        }
        if with_stale {
            ring.insert_public("v3", &stale.public_hex("v3").unwrap()).unwrap();
        }
        n.keys = ring;
        assert!(n.verify_cert(&cert).is_ok(), "stale={with_stale}: {:?}", n.verify_cert(&cert));
        assert!(n.accept_vote_sig("v3", &p, &vsig), "stale={with_stale}");
        assert!(!n.accept_vote_sig("v3", &p, &stale.sign("v3", &p).unwrap()));
    }
}

/// Review #146 (1): round slash evidence verifies with the key valid at the
/// evidence height (registry from H_act), not the ring, so a freshly adopted
/// node (ring absent/stale) agrees with every other seat.
#[test]
fn a13_round_evidence_uses_registry_key_with_absent_or_stale_ring() {
    let mut net = net6();
    arm(&mut net);
    let signer = net.nodes["v1"].keys.clone();
    let nid = net.nodes["v1"].cfg.network_id;
    let h = net.nodes["v1"].tip_height + 1;
    let sign = |hash: &str| signer.sign("v3", &Node::vote_ok_payload_v2(nid, h, 2, hash, "v3")).unwrap();
    let ev = SlashEvidence {
        offender: "v3".into(), height: h, hash_a: "aa".into(), sig_a: sign("aa"),
        hash_b: "bb".into(), sig_b: sign("bb"), kind: String::new(), round: 2,
    };
    let mut stale = KeyRing::new(CryptoScheme::Falcon512);
    stale.generate_id("v3");
    for with_stale in [false, true] {
        let n = net.nodes.get_mut("v2").unwrap();
        n.cfg.vote_rounds_from_height = 1;
        let mut ring = KeyRing::new(CryptoScheme::Falcon512);
        for (id, pk) in signer.export_publics() {
            if id != "v3" {
                ring.insert_public(&id, &pk).unwrap();
            }
        }
        if with_stale {
            ring.insert_public("v3", &stale.public_hex("v3").unwrap()).unwrap();
        }
        n.keys = ring;
        assert!(n.verify_round_evidence_item(&ev).is_ok(), "stale={with_stale}: {:?}", n.verify_round_evidence_item(&ev));
        let forged = SlashEvidence {
            sig_a: stale.sign("v3", &Node::vote_ok_payload_v2(nid, h, 2, "aa", "v3")).unwrap(),
            ..ev.clone()
        };
        assert!(n.verify_round_evidence_item(&forged).is_err());
    }
}

/// beta.3 fresh-install stall: a snapshot-adopted joiner whose ring lacks a
/// committee seat (learned only via Hello) rejected every catch-up batch with
/// "no public key for <seat>". The committee root now falls back to the
/// registered key and equals the full-ring root.
#[test]
fn a13_committee_root_falls_back_to_registry_after_adopt() {
    let mut net = net6();
    arm(&mut net);
    let ids: Vec<String> = ["v1", "v2", "v3", "v4", "v5", "v6"].iter().map(|s| s.to_string()).collect();
    let n = net.nodes.get_mut("v2").unwrap();
    let h = n.tip_height + 1;
    let want = crate::qc_header::committee_root_for_ids(&ids, &n.keys).unwrap();
    assert_eq!(n.committee_root_at(&ids, h).unwrap(), want);
    let mut ring = KeyRing::new(CryptoScheme::Falcon512);
    for (id, pk) in n.keys.export_publics() {
        if id != "v3" {
            ring.insert_public(&id, &pk).unwrap();
        }
    }
    n.keys = ring;
    assert!(crate::qc_header::committee_root_for_ids(&ids, &n.keys).is_err());
    assert_eq!(n.committee_root_at(&ids, h).unwrap(), want);
}

/// beta.3 fresh-install stall (2): catch-up ledgers packed by a seat missing
/// from the adopted ring failed "bad packer sig". From `H_act` the packer
/// signature verifies with the registered key (which must match the header's
/// `packer_public_key`).
#[test]
fn a13_packer_sig_falls_back_to_registry_after_adopt() {
    let mut net = net6();
    arm(&mut net);
    let n = net.nodes.get_mut("v2").unwrap();
    let led = n.chain.last().unwrap().clone();
    assert!(n.key_registry_at(led.height), "test needs a ledger at/after H_act");
    let p = led.packer.clone();
    assert!(n.packer_sig_ok(&led));
    let mut ring = KeyRing::new(CryptoScheme::Falcon512);
    for (id, pk) in n.keys.export_publics() {
        if id != p {
            ring.insert_public(&id, &pk).unwrap();
        }
    }
    n.keys = ring;
    assert!(!led.verify_packer(&n.keys));
    assert!(n.packer_sig_ok(&led));
    let mut bad = led.clone();
    let mut other = KeyRing::new(CryptoScheme::Falcon512);
    other.generate_id(&p);
    bad.packer_public_key = other.public_hex(&p).unwrap();
    assert!(!n.packer_sig_ok(&bad));
    let mut forged = led.clone();
    forged.packer_sig = other.sign(&p, "x").unwrap();
    assert!(!n.packer_sig_ok(&forged));
}

fn ring_with(n: &Node, drop: &str, stale: Option<&KeyRing>) -> KeyRing {
    let mut ring = KeyRing::new(CryptoScheme::Falcon512);
    for (id, pk) in n.keys.export_publics() {
        if id != drop {
            ring.insert_public(&id, &pk).unwrap();
        }
    }
    if let Some(s) = stale {
        ring.insert_public(drop, &s.public_hex(drop).unwrap()).unwrap();
    }
    ring
}

/// Review #148: from H_act the committee root uses ONLY the registry key, so a
/// stale or absent ring entry yields the identical root; a stale ring key can
/// no longer sign a packed ledger.
#[test]
fn a13_committee_root_registry_only_post_activation_stale_ring() {
    let mut net = net6();
    arm(&mut net);
    let ids: Vec<String> = ["v1", "v2", "v3", "v4", "v5", "v6"].iter().map(|s| s.to_string()).collect();
    let n = net.nodes.get_mut("v2").unwrap();
    let h = n.tip_height + 1;
    assert!(n.key_registry_at(h));
    let want = n.committee_root_at(&ids, h).unwrap();
    let mut stale = KeyRing::new(CryptoScheme::Falcon512);
    stale.generate_id("v3");
    n.keys = ring_with(n, "v3", Some(&stale));
    assert_ne!(crate::qc_header::committee_root_for_ids(&ids, &n.keys).unwrap(), want);
    assert_eq!(n.committee_root_at(&ids, h).unwrap(), want);
    let led = n.chain.last().unwrap().clone();
    let p = led.packer.clone();
    let mut stale_p = KeyRing::new(CryptoScheme::Falcon512);
    stale_p.generate_id(&p);
    n.keys = ring_with(n, &p, Some(&stale_p));
    assert!(n.packer_sig_ok(&led));
}

/// Review #148: below H_act the exact legacy ring behaviour is kept, and every
/// committed ledger's stored committee roots re-verify (historical replay).
#[test]
fn a13_legacy_heights_keep_ring_roots_and_history_verifies() {
    // v4 headers from h=1 so every committed ledger carries committee roots
    // (review #149: the regression must not pass without checking a root).
    let ids = ["v1", "v2", "v3", "v4", "v5", "v6", "v7"];
    let cfg = ChainConfig { pack_budget_ms: 0, scheme: CryptoScheme::Falcon512, ..Default::default() };
    let mut net = Network::bootstrap(&ids, 1000, &users(), cfg);
    net.pong_all();
    let h_act = 5u64;
    for n in net.nodes.values_mut() {
        n.qc_header_version = 4;
        n.qc_start_height = 1;
        n.rewards_v2_from_height = h_act; // cross H_act mid-run
        n.rewards_v2_epoch = 0;
        let gid: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        n.init_seat_key_registry_from_genesis(&gid);
    }
    for h in 1..=10u64 {
        net.payment("alice", "bob", 1, 1).unwrap();
        net.seal_next_height().unwrap_or_else(|e| panic!("h={h}: {e}"));
    }
    let n = &net.nodes["v2"];
    let (mut legacy, mut post) = (0, 0);
    for l in &n.chain {
        assert!(l.is_v4(), "h={}", l.height);
        assert!(!l.current_committee.is_empty(), "h={}", l.height);
        assert!(n.packer_sig_ok(l), "h={}", l.height);
        if l.height < h_act {
            assert_eq!(
                crate::qc_header::committee_root_for_ids(&l.current_committee, &n.keys).unwrap(),
                l.current_committee_root,
                "legacy ring root h={}",
                l.height
            );
            assert_eq!(n.committee_root_at(&l.current_committee, l.height).unwrap(), l.current_committee_root);
            legacy += 1;
        } else {
            assert_eq!(n.committee_root_at(&l.current_committee, l.height).unwrap(), l.current_committee_root, "registry root h={}", l.height);
            post += 1;
        }
    }
    // Committee roots chain: each ledger's current root is the previous next root.
    for w in n.chain.windows(2) {
        assert_eq!(w[1].current_committee_root, w[0].next_committee_root, "h={}", w[1].height);
    }
    assert!(legacy > 0 && post > 0, "legacy={legacy} post={post}");
}

/// beta.3 fresh-install stall (3): Merkle-batch packer sig ("batch sig: bad tx
/// crypto: unknown key") verifies with the registry key from H_act only.
#[test]
fn a13_batch_sig_registry_only_post_activation() {
    let mut net = net6();
    arm(&mut net);
    let n = net.nodes.get_mut("v2").unwrap();
    let mut led = n.chain.last().unwrap().clone();
    let p = led.packer.clone();
    let msg = format!("pl-batch:v1|{}", led.batch_root);
    led.batch_sig = n.keys.sign(&p, &msg).unwrap_or_default();
    if led.batch_sig.is_empty() {
        let s = net.nodes[p.as_str()].keys.sign(&p, &msg).unwrap();
        led.batch_sig = s;
    }
    let n = net.nodes.get_mut("v2").unwrap();
    led.batch_paths = vec![Vec::new(); led.txs.len()];
    let rk = n.packer_registry_key(&led);
    assert!(rk.as_deref().is_some_and(|k| !k.is_empty()));
    let mut stale = KeyRing::new(CryptoScheme::Falcon512);
    stale.generate_id(&p);
    n.keys = ring_with(n, &p, None);
    // Only the packer-sig stage is exercised (no txs => paths trivially ok).
    let mut l0 = led.clone();
    l0.txs.clear();
    l0.batch_paths.clear();
    assert!(l0.verify_merkle_batch_onchain(&n.keys, &|_| None, true).is_err(), "ring lacks packer");
    assert!(l0.verify_merkle_batch_reg(&n.keys, &|_| None, true, rk.as_deref()).is_ok());
    n.keys = ring_with(n, &p, Some(&stale));
    assert!(l0.verify_merkle_batch_reg(&n.keys, &|_| None, true, rk.as_deref()).is_ok());
    let mut forged = l0.clone();
    forged.batch_sig = stale.sign(&p, &msg).unwrap();
    assert!(forged.verify_merkle_batch_reg(&n.keys, &|_| None, true, rk.as_deref()).is_err());
    assert!(l0.verify_merkle_batch_reg(&n.keys, &|_| None, true, Some("")).is_err());
}

/// Review #149 (1): the committee frozen by the last pre-H_act ledger keeps
/// its committed root at H_act even if a member has no registry key; no ring
/// fallback, and only on the boundary height.
#[test]
fn a13_frozen_committee_root_preserved_across_activation() {
    let mut net = net6();
    arm(&mut net);
    let ids: Vec<String> = ["v1", "v2", "v3", "v4", "v5", "v6"].iter().map(|s| s.to_string()).collect();
    let n = net.nodes.get_mut("v2").unwrap();
    let h = n.tip_height + 1;
    n.rewards_v2_from_height = h; // activation exactly at the next height
    assert!(n.key_registry_at(h) && !n.key_registry_at(h - 1));
    let frozen = n.committee_root_at(&ids, h - 1).unwrap(); // legacy ring root
    {
        let t = n.chain.last_mut().unwrap();
        t.inner_hash = "11".repeat(32);
        t.next_committee = ids.clone();
        t.next_committee_root = frozen.clone();
    }
    n.seat_key_registry.remove("v3"); // unregistered legacy seat in the frozen committee
    assert!(n.committee_root_at(&ids, h).is_err());
    assert_eq!(n.current_committee_root_at(&ids, h).unwrap(), frozen);
    // A different committee is not covered by the frozen root.
    let other: Vec<String> = ids.iter().rev().cloned().collect();
    assert!(n.current_committee_root_at(&other, h).is_err());
    // Past the boundary: registry only (no ring fallback).
    n.rewards_v2_from_height = 1;
    assert!(n.current_committee_root_at(&ids, h).is_err());
}

/// Review #149 (2): next-committee keys come from the post-tx draft registry,
/// so a BondV2 seat enrolled by this ledger and drawn into next-6 has a key.
#[test]
fn a13_next_committee_root_uses_draft_registry_on_enrollment() {
    let mut net = net6();
    arm(&mut net);
    let n = net.nodes.get_mut("v2").unwrap();
    let h = n.tip_height + 1;
    let mut kr = KeyRing::new(CryptoScheme::Falcon512);
    kr.generate_id("vnew");
    let pk = kr.public_hex("vnew").unwrap();
    let mut draft = n.validators.clone();
    let mut vi = draft["v1"].clone();
    vi.id = "vnew".into();
    vi.v2_seat = true;
    vi.public_key = pk.clone();
    draft.insert("vnew".into(), vi);
    let ids: Vec<String> = ["v1", "v2", "v3", "v4", "v5", "vnew"].iter().map(|s| s.to_string()).collect();
    assert!(n.committee_root_at(&ids, h).is_err(), "pre-state has no key for vnew");
    let got = n.next_committee_root_at(&ids, h, &draft).unwrap();
    let mut pks: Vec<String> = ids[..5].iter().map(|i| n.registered_key(i).unwrap()).collect();
    pks.push(pk);
    assert_eq!(got, crate::qc_header::committee_root_hex(&pks).unwrap());
    // Pre-state committees are unchanged by the draft path.
    let six: Vec<String> = ["v1", "v2", "v3", "v4", "v5", "v6"].iter().map(|s| s.to_string()).collect();
    assert_eq!(n.next_committee_root_at(&six, h, &n.validators.clone()).unwrap(), n.committee_root_at(&six, h).unwrap());
}

/// Review #149: header gossip (ProposeHeader) uses the same registry-only
/// policy from H_act: a stale ring entry for the packer no longer rejects a
/// valid registry-signed header; below H_act the legacy ring check stays.
#[test]
fn a13_header_sig_registry_only_post_activation_stale_ring() {
    let mut net = net6();
    let led = net.nodes["v2"].chain.last().unwrap().clone();
    let p = led.packer.clone();
    let hdr = led.header();
    {
        let n = net.nodes.get_mut("v3").unwrap();
        assert!(n.header_sig_ok(&hdr), "legacy ring accepts");
        let mut stale = KeyRing::new(CryptoScheme::Falcon512);
        stale.generate_id(&p);
        n.keys = ring_with(n, &p, Some(&stale));
        assert!(!n.header_sig_ok(&hdr), "below H_act: stale ring still rejects (legacy)");
    }
    net.nodes.remove("v3");
    arm(&mut net);
    let n = net.nodes.get_mut("v2").unwrap();
    let mut stale = KeyRing::new(CryptoScheme::Falcon512);
    stale.generate_id(&p);
    n.keys = ring_with(n, &p, Some(&stale));
    assert!(!hdr.verify_packer(&n.keys), "ring verifier rejects");
    assert!(n.header_sig_ok(&hdr), "registry verifier accepts");
    let mut bad = hdr.clone();
    bad.packer_public_key = stale.public_hex(&p).unwrap();
    assert!(!n.header_sig_ok(&bad), "non-registry key rejected");
}
