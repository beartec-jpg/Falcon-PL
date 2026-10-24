//! v2.9.60 tests: the Gloas ETH light-client rules are height-gated at the
//! ledger level (pack + verify + commit on every node agree), and a hash-only
//! LC head resolves its number/receipts root from RLP-verified exec headers.

use super::*;
use crate::eth_lc::tests::{gloas_fixture, state_period_1378};

const H_1379: &str = "9a8acc058dffdb10c5a143b937608aff8eb8d61ae94771a95b319529dbe7543d";
const N_1379: u64 = 11_856_686;

fn users() -> Vec<(String, u64)> {
    vec![
        ("alice".into(), 1_000_000),
        ("bob".into(), 1_000_000),
        ("carol".into(), 1_000_000),
    ]
}

fn net(gloas_from: u64) -> Network {
    let cfg = ChainConfig {
        fee_escalate: false,
        min_fee: 1,
        pack_budget_ms: 0,
        max_txs_per_ledger: 0,
        ..Default::default()
    };
    let mut net = Network::bootstrap(&["v1", "v2", "v3", "v4", "v5"], 1000, &users(), cfg);
    let mut live = state_period_1378();
    live.finalized_slot = 11_296_672; // live 2300 LC (frozen at the fork)
    for n in net.nodes.values_mut() {
        n.bridge.eth_lc = Some(live.clone());
        n.eth_lc_gloas_from_height = gloas_from;
    }
    net.pong_all();
    net
}

fn sign(net: &Network, acct: &str, seq: u64, body: TxBody) -> Tx {
    Tx::sign_body(&net.user_keys, acct, seq, "", 0, 1, net.cfg.network_id, body).unwrap()
}

fn submit_all(net: &mut Network, tx: &Tx) {
    for n in net.nodes.values_mut() {
        n.submit_tx(tx.clone()).unwrap();
    }
}

fn lc(net: &Network) -> crate::eth_lc::EthLcState {
    let first = net.nodes.values().next().unwrap();
    for n in net.nodes.values() {
        assert_eq!(n.tip_hash, first.tip_hash);
        assert_eq!(n.bridge.eth_lc, first.bridge.eth_lc, "every node has the same LC");
    }
    first.bridge.eth_lc.clone().unwrap()
}

fn exec_headers_1379() -> String {
    let all: Vec<crate::eth_exec::EthExecHeader> =
        serde_json::from_str(&gloas_fixture("exec-headers-amsterdam.json")).unwrap();
    let one: Vec<_> = all.into_iter().filter(|h| h.hash == H_1379).collect();
    assert_eq!(one.len(), 1);
    serde_json::to_string(&one).unwrap()
}

#[test]
fn gloas_lc_update_waits_for_activation_height_then_applies_on_every_node() {
    let mut net = net(3);
    assert!(!net.nodes["v1"].eth_lc_gloas_at(2));
    assert!(net.nodes["v1"].eth_lc_gloas_at(3));
    let upd = sign(
        &net,
        "alice",
        1,
        TxBody::EthLcUpdate {
            json: gloas_fixture("update-gloas-period-1379.json"),
        },
    );
    submit_all(&mut net, &upd);
    let before = lc(&net);
    // h1, h2: below activation — the Gloas update is not applicable (2.9.59 rules).
    for seq in 1..=2 {
        let pay = Tx::sign_with(&net.user_keys, "bob", seq, "carol", 1, 1, net.cfg.network_id)
            .unwrap();
        submit_all(&mut net, &pay);
        let r = net.seal_next_height().unwrap();
        assert_eq!(r.n_txs, 1, "only bob's pay packs at h{}", r.height);
        assert_eq!(lc(&net), before, "LC unchanged below activation");
    }
    assert_eq!(net.tip_height(), 2);
    // h3 = activation: the same tx now packs, verifies and commits everywhere.
    let r = net.seal_next_height().unwrap();
    assert_eq!((r.height, r.n_txs), (3, 1));
    let after = lc(&net);
    assert_eq!(after.period, 1379);
    assert_eq!(after.finalized_slot, 11_297_184);
    assert_eq!(after.execution_block_hash, H_1379);
    assert_eq!(after.execution_number, 0);

    // EthExecHeaders ending at the LC hash (post-Amsterdam RLP) resolves the head.
    let hdrs = sign(
        &net,
        "alice",
        2,
        TxBody::EthExecHeaders {
            headers_json: exec_headers_1379(),
        },
    );
    submit_all(&mut net, &hdrs);
    let r = net.seal_next_height().unwrap();
    assert_eq!(r.n_txs, 1);
    let resolved = lc(&net);
    assert_eq!(resolved.execution_number, N_1379);
    assert_eq!(
        resolved.receipts_root,
        "e7638a68331497ca52cbcd18ed9ff2868f10aa55d5401e5e069b859e83af8944"
    );
    assert_eq!(net.nodes["v1"].bridge.eth_exec_bounds(), (N_1379, N_1379));
    let st = net.nodes["v1"].status_json();
    assert_eq!(st["eth_lc"]["execution_number"], N_1379);
    assert_eq!(st["eth_lc_gloas_from_height"], 3);
}

#[test]
fn gloas_lc_off_by_default_and_gate_is_per_height_not_per_node_clock() {
    assert_eq!(TESTNET_2300_ETH_LC_GLOAS_FROM_HEIGHT, 409134);
    assert_eq!(network_eth_lc_gloas_from_height(NETWORK_ID_PUBLIC_TESTNET_2300), 409134);
    assert_eq!(network_eth_lc_gloas_from_height(NETWORK_ID_PRIVATE_V22), u64::MAX);
    let n = Node::new("v1", ChainConfig::default(), KeyRing::new(CryptoScheme::HmacDev), &users());
    assert!(!n.eth_lc_gloas_at(u64::MAX - 1));

    // Direct apply: rejected below the gate, accepted at it.
    let mut n = Node::new(
        "v1",
        ChainConfig {
            fee_escalate: false,
            min_fee: 1,
            ..Default::default()
        },
        {
            let mut k = KeyRing::new(CryptoScheme::HmacDev);
            k.generate_id("alice");
            k
        },
        &users(),
    );
    n.bridge.eth_lc = Some(state_period_1378());
    n.eth_lc_gloas_from_height = 10;
    let tx = Tx::sign_body(
        &n.keys,
        "alice",
        1,
        "",
        0,
        1,
        n.cfg.network_id,
        TxBody::EthLcUpdate {
            json: gloas_fixture("update-gloas-period-1379.json"),
        },
    )
    .unwrap();
    n.tip_height = 8; // applies at 9
    let e = n.apply_protocol_tx(&tx).unwrap_err();
    assert_eq!(e, "fin exec");
    assert!(
        !Node::is_permanent_tx_failure(&e),
        "pre-activation Gloas update stays queued (not dropped) for the gate"
    );
    let mut m = Node::new(
        "v1",
        ChainConfig {
            fee_escalate: false,
            min_fee: 1,
            ..Default::default()
        },
        n.keys.clone(),
        &users(),
    );
    m.bridge.eth_lc = Some(state_period_1378());
    m.eth_lc_gloas_from_height = 10;
    m.tip_height = 9; // applies at 10
    m.apply_protocol_tx(&tx).unwrap();
    assert_eq!(m.bridge.eth_lc.as_ref().unwrap().period, 1379);
}

#[test]
fn exec_headers_below_gate_do_not_resolve_hash_only_head() {
    // A hash-only head can only come from Gloas rules, but the resolver itself
    // is also gated: below the height, EthExecHeaders behave exactly as 2.9.59.
    let mut net = net(u64::MAX);
    for n in net.nodes.values_mut() {
        let lc = n.bridge.eth_lc.as_mut().unwrap();
        lc.execution_block_hash = H_1379.into();
        lc.execution_number = 0;
        lc.receipts_root.clear();
    }
    let hdrs = sign(
        &net,
        "alice",
        1,
        TxBody::EthExecHeaders {
            headers_json: exec_headers_1379(),
        },
    );
    submit_all(&mut net, &hdrs);
    net.seal_next_height().unwrap();
    let st = lc(&net);
    assert_eq!(st.execution_number, 0, "no resolution below the gate");
    assert_eq!(net.nodes["v1"].bridge.eth_exec_bounds(), (N_1379, N_1379));
    // Bridge-level: the gated resolver fills it.
    let mut b = net.nodes["v1"].bridge.clone();
    b.eth_lc_resolve_exec_head();
    assert_eq!(b.eth_lc.as_ref().unwrap().execution_number, N_1379);
}
