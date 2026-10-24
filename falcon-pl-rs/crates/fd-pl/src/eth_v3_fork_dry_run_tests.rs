//! v2.9.61 V3 test plan, FPL side of the anvil Sepolia-fork dry run.
//!
//! `#[ignore]`d and inert unless `FPL_V3_FORK_CASE` names a case file written
//! by `scripts/v3-fork-dry-run.py`. Normal `cargo test` never runs it, and it
//! adds nothing to the baseline.
//!
//! What is real: the execution headers (RLP from the anvil fork, checked by
//! `verify_exec_header_rlp` through a signed `EthExecHeaders` tx), the deposit
//! receipts and their MPT proofs (built by the shipped relayer from the fork
//! block), the `submit-eth-v3-deposit` arguments auto-mint produced, and the
//! consensus code that packs and applies `EthV3Deposit` on three nodes.
//!
//! What is mocked: the ETH light client. Anvil blocks have no beacon chain or
//! sync committee, so `eth_lc` is set directly on every node to the fork block
//! the dry run treats as the finalized head (same shortcut as
//! `eth_v3_deposit_tests::install`). The node set is the in-process
//! `Network` harness, not the 2300 seats.

use super::*;
use crate::bridge::{
    V3Resolution, RAIL_ETH, RAIL_USDC, V3_DEPOSIT_ALREADY_RESOLVED, V3_DEPOSIT_DEST_UNRESOLVED,
    V3_DEPOSIT_NOT_IN_RECEIPT,
};
use crate::eth_exec::EthExecHeader;
use crate::eth_lc::EthLcState;
use serde_json::{json, Value};

fn cfg() -> ChainConfig {
    ChainConfig {
        fee_escalate: false,
        min_fee: 1,
        pack_budget_ms: 0,
        max_txs_per_ledger: 8,
        progress_timeout_ms: 0,
        ..Default::default()
    }
}

fn users() -> Vec<(String, u64)> {
    vec![
        ("alice".into(), 1_000_000),
        ("bob".into(), 1_000_000),
        ("carol".into(), 1_000_000),
    ]
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn proof_of(sub: &Value) -> BridgeProof {
    BridgeProof {
        external_txid: s(&sub["external_txid"]),
        block_hash: s(&sub["block_hash"]),
        height: sub["height"].as_u64().unwrap(),
        merkle_path: sub["merkle_path"]
            .as_array()
            .unwrap()
            .iter()
            .map(s)
            .collect(),
        merkle_index: sub["merkle_index"].as_u64().unwrap() as u32,
        lock_id: String::new(),
        parent_hash: String::new(),
        merkle_root: String::new(),
        external_to: String::new(),
        raw_tx: s(&sub["raw_tx"]).trim_start_matches("0x").to_string(),
    }
}

fn v3_body(sub: &Value) -> TxBody {
    TxBody::EthV3Deposit {
        asset: s(&sub["asset"]),
        deposit_id: s(&sub["deposit_id"]),
        credit_to: s(&sub["credit_to"]),
        proof: proof_of(sub),
    }
}

struct Seq(u64);

impl Seq {
    fn next(&mut self) -> u64 {
        let n = self.0;
        self.0 += 1;
        n
    }
}

fn sign(net: &Network, seq: u64, body: TxBody) -> Tx {
    Tx::sign_body(&net.user_keys, "alice", seq, "", 0, 1, net.cfg.network_id, body).unwrap()
}

fn submit_all(net: &mut Network, tx: &Tx) -> Result<(), String> {
    for n in net.nodes.values_mut() {
        n.submit_tx(tx.clone())?;
    }
    Ok(())
}

/// MOCK: point every node's light client at the fork block treated as the
/// finalized head. Real seats get this from the beacon light client.
fn mock_lc(net: &mut Network, head: &Value) {
    for n in net.nodes.values_mut() {
        n.bridge.eth_lc = Some(EthLcState {
            genesis_validators_root: crate::eth_lc::SEPOLIA_GENESIS_VALIDATORS_ROOT.into(),
            finalized_slot: 10,
            finalized_beacon_root: "ab".repeat(32),
            execution_block_hash: s(&head["hash"]),
            execution_number: head["number"].as_u64().unwrap(),
            receipts_root: s(&head["receipts_root"]),
            period: 3,
            current_agg_pk: String::new(),
            current_pks: vec![],
            next_agg_pk: String::new(),
            next_pks: vec![],
        });
    }
}

fn roots_agree(net: &Network) -> String {
    let first = net.nodes.values().next().unwrap();
    let root = first.commitment_root();
    let bridge = serde_json::to_vec(&first.bridge).unwrap();
    for n in net.nodes.values() {
        assert_eq!(n.tip_hash, first.tip_hash, "{}", n.id);
        assert_eq!(n.commitment_root(), root, "{}", n.id);
        assert_eq!(serde_json::to_vec(&n.bridge).unwrap(), bridge, "{}", n.id);
    }
    format!("{root:?}")
}

fn resolution_json(r: Option<&V3Resolution>) -> Value {
    match r {
        None => Value::Null,
        Some(V3Resolution::Mint {
            credit_to, amount, ..
        }) => json!({"kind": "mint", "credit_to": credit_to, "amount": amount}),
        Some(V3Resolution::Refund {
            asset,
            amount,
            refund_to,
            note_id,
            eth_block,
        }) => json!({
            "kind": "refund",
            "asset": asset,
            "amount": amount,
            "refund_to": refund_to,
            "note_id": note_id,
            "eth_block": eth_block,
        }),
    }
}

#[test]
#[ignore = "driven by scripts/v3-fork-dry-run.py (FPL_V3_FORK_CASE)"]
fn v3_fork_dry_run_case() {
    // Run only by the driver. `--ignored` without a case must not pass having tested nothing.
    let path = std::env::var("FPL_V3_FORK_CASE")
        .expect("FPL_V3_FORK_CASE unset: run scripts/v3-fork-dry-run.py (this test only replays its case file)");
    let out_path = std::env::var("FPL_V3_FORK_RESULT").expect("FPL_V3_FORK_RESULT");
    let case: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let bridge20 = crate::evm_spv::parse_eth_addr(&s(&case["bridge"])).unwrap();

    let mut net = Network::bootstrap(&["v1", "v2", "v3"], 1000, &users(), cfg());
    for n in net.nodes.values_mut() {
        n.ensure_faucet();
        n.eth_v3_from_height = 1;
        n.eth_v3_bridge = Some(bridge20);
    }
    net.pong_all();
    let mut seq = Seq(1);
    let mut log: Vec<Value> = Vec::new();
    let mut expect: Vec<(String, String, String, u64)> = Vec::new();

    for phase in ["pre", "post"] {
        let head = &case["lc"][phase];
        mock_lc(&mut net, head);
        // Real: fork headers go in through consensus (rlp hash, parent links,
        // contiguous numbers, chain ends at the LC hash).
        let headers: Vec<EthExecHeader> =
            serde_json::from_value(head["headers"].clone()).unwrap();
        let (lo, hi) = (headers[0].number, headers.last().unwrap().number);
        let tx = sign(
            &net,
            seq.next(),
            TxBody::EthExecHeaders {
                headers_json: serde_json::to_string(&headers).unwrap(),
            },
        );
        submit_all(&mut net, &tx).unwrap();
        let sealed = net.seal_next_height().unwrap();
        assert_eq!(sealed.n_txs, 1, "{phase}: exec headers tx did not pack");
        for n in net.nodes.values() {
            assert!(n.bridge.eth_exec_headers.get(&lo).is_some(), "{phase} {lo}");
            assert!(n.bridge.eth_exec_headers.get(&hi).is_some(), "{phase} {hi}");
        }
        log.push(json!({"phase": phase, "exec_headers": [lo, hi], "ledger": sealed.height}));

        // Inside E an unresolved dest is refused, not permanent, and records nothing.
        for probe in case["unresolved_probes"].as_array().into_iter().flatten() {
            if s(&probe["phase"]) != phase {
                continue;
            }
            let v1 = net.nodes.get_mut("v1").unwrap();
            let before = serde_json::to_vec(&v1.bridge).unwrap();
            let err = v1
                .eth_v3_deposit(
                    &s(&probe["asset"]),
                    &s(&probe["deposit_id"]),
                    "",
                    &proof_of(probe),
                )
                .unwrap_err();
            assert_eq!(err, V3_DEPOSIT_DEST_UNRESOLVED);
            assert!(!Node::is_permanent_tx_failure(&err));
            assert_eq!(serde_json::to_vec(&v1.bridge).unwrap(), before);
            log.push(json!({"phase": phase, "unresolved_probe": s(&probe["deposit_id"]), "err": err}));
        }

        for sub in case["submissions"].as_array().unwrap() {
            if s(&sub["phase"]) != phase {
                continue;
            }
            let tx = sign(&net, seq.next(), v3_body(sub));
            submit_all(&mut net, &tx).unwrap();
            let sealed = net.seal_next_height().unwrap();
            assert_eq!(sealed.n_txs, 1, "{phase}: V3 tx {} did not pack", s(&sub["deposit_id"]));
            let id = s(&sub["deposit_id"]);
            let res = net.nodes["v1"].bridge.eth_v3_resolution(&id).cloned();
            log.push(json!({
                "phase": phase,
                "deposit_id": id,
                "credit_to": s(&sub["credit_to"]),
                "ledger": sealed.height,
                "resolution": resolution_json(res.as_ref()),
            }));
            expect.push((id, s(&sub["expect"]), s(&sub["asset"]), sub["amount"].as_u64().unwrap()));
        }
        roots_agree(&net);
    }

    // Each id resolved exactly as planned.
    let v1 = &net.nodes["v1"];
    let mut minted: std::collections::BTreeMap<String, u64> = Default::default();
    for (id, want, asset, amount) in &expect {
        let r = v1.bridge.eth_v3_resolution(id).unwrap_or_else(|| panic!("{id} unresolved"));
        match (want.as_str(), r) {
            ("mint", V3Resolution::Mint { amount: a, .. }) => {
                assert_eq!(a, amount);
                *minted.entry(asset.clone()).or_default() += amount;
            }
            ("refund", V3Resolution::Refund { amount: a, asset: x, .. }) => {
                assert_eq!(a, amount);
                assert_eq!(x, asset);
            }
            _ => panic!("{id}: wanted {want}, got {r:?}"),
        }
    }
    for (asset, amt) in &minted {
        assert_eq!(v1.defi.asset_bal("sally", asset), *amt, "sally {asset}");
    }

    // Replay: admission refuses every resolved id on every node, with or
    // without credit_to, and apply refuses it too. Nothing changes.
    let mut replays = Vec::new();
    let root_before = net.nodes["v1"].commitment_root();
    for sub in case["submissions"].as_array().unwrap() {
        for credit in [s(&sub["credit_to"]), String::new(), "sally".to_string()] {
            let mut body = v3_body(sub);
            if let TxBody::EthV3Deposit { credit_to, .. } = &mut body {
                *credit_to = credit.clone();
            }
            let tx = sign(&net, seq.0, body);
            for n in net.nodes.values_mut() {
                let err = n.submit_tx(tx.clone()).unwrap_err();
                assert_eq!(err, V3_DEPOSIT_ALREADY_RESOLVED, "admission {}", n.id);
            }
            let v1 = net.nodes.get_mut("v1").unwrap();
            let err = v1
                .eth_v3_deposit(&s(&sub["asset"]), &s(&sub["deposit_id"]), &credit, &proof_of(sub))
                .unwrap_err();
            assert_eq!(err, V3_DEPOSIT_ALREADY_RESOLVED);
            assert!(Node::is_permanent_tx_failure(&err));
            replays.push(json!({"deposit_id": s(&sub["deposit_id"]), "credit_to": credit, "admission": V3_DEPOSIT_ALREADY_RESOLVED, "apply": err}));
        }
    }
    assert_eq!(net.nodes["v1"].commitment_root(), root_before);

    // fundPool receipt: no V3 Deposit, so nothing to mint or refund.
    let mut fund_pool_err = Value::Null;
    if let Some(fp) = case.get("fund_pool").filter(|v| !v.is_null()) {
        let v1 = net.nodes.get_mut("v1").unwrap();
        let before = serde_json::to_vec(&v1.bridge).unwrap();
        let err = v1
            .eth_v3_deposit("ETH", &s(&fp["deposit_id"]), "sally", &proof_of(fp))
            .unwrap_err();
        assert_eq!(err, V3_DEPOSIT_NOT_IN_RECEIPT);
        assert!(Node::is_permanent_tx_failure(&err));
        assert_eq!(serde_json::to_vec(&v1.bridge).unwrap(), before);
        fund_pool_err = json!(err);
    }

    // Claim leaves and the header claim root FPL would commit, with a proof
    // for each refund leaf (same tree as withdrawals: evm_spv::build_tree).
    let v1 = &net.nodes["v1"];
    let leaves = crate::lc::evm_claim_leaves(&v1.bridge);
    let claim_root = hex::encode(crate::lc::evm_header_claim_root(&v1.bridge));
    let mut refunds = Vec::new();
    for (id, want, asset, amount) in &expect {
        if want != "refund" {
            continue;
        }
        let Some(V3Resolution::Refund {
            note_id, refund_to, ..
        }) = v1.bridge.eth_v3_resolution(id)
        else {
            unreachable!()
        };
        let nb = hex::decode(note_id).unwrap();
        let mut note = [0u8; 32];
        note.copy_from_slice(&nb);
        let dest = crate::evm_spv::parse_eth_addr(refund_to).unwrap();
        let leaf = crate::evm_spv::withdraw_leaf(&note, &dest, *amount, asset == "USDC");
        let idx = leaves.iter().position(|l| *l == leaf).expect("refund leaf in claim tree");
        let (root, path) = crate::evm_spv::build_tree(&leaves, idx).unwrap();
        assert_eq!(root, claim_root);
        refunds.push(json!({
            "deposit_id": id,
            "note_id": note_id,
            "refund_to": refund_to,
            "asset": asset,
            "amount": amount,
            "leaf": hex::encode(leaf),
            "index": idx,
            "path": path,
        }));
    }
    let mut status = v1.status_json();
    if let Some(lc) = status.get_mut("lc").and_then(|v| v.as_object_mut()) {
        lc.insert("claim_root".into(), json!(claim_root));
        lc.insert(
            "claim_leaves".into(),
            json!(leaves.iter().map(hex::encode).collect::<Vec<_>>()),
        );
    }
    let rails: Vec<Value> = status["rails"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| matches!(r["asset"].as_str(), Some("ETH") | Some("USDC")))
        .map(|r| {
            json!({
                "asset": r["asset"],
                "claim_root": r["claim_root"],
                "claim_leaves": r["claim_leaves"],
                "withdrawals": r["withdrawals"],
                "total_minted": r["total_minted"],
            })
        })
        .collect();
    let out = json!({
        "mocked": ["eth light client (eth_lc set directly to the fork head)", "in-process 3-node Network harness, not the 2300 seats"],
        "nodes": net.nodes.len(),
        "tip_height": v1.tip_height,
        "roots_agree": roots_agree(&net),
        "log": log,
        "balances": {
            "sally": {"ETH": v1.defi.asset_bal("sally", "ETH"), "USDC": v1.defi.asset_bal("sally", "USDC")},
        },
        "total_minted": {
            "ETH": v1.bridge.rail(RAIL_ETH).unwrap().total_minted,
            "USDC": v1.bridge.rail(RAIL_USDC).unwrap().total_minted,
        },
        "resolved_total": status["eth_v3_resolved_total"],
        "eth_v1_deposit_cutoff": status["eth_v1_deposit_cutoff"],
        "replays": replays,
        "fund_pool_err": fund_pool_err,
        "claim_leaves": leaves.iter().map(hex::encode).collect::<Vec<_>>(),
        "claim_root": claim_root,
        "refunds": refunds,
        "status": {"lc": status["lc"], "rails": rails},
    });
    std::fs::write(&out_path, serde_json::to_vec_pretty(&out).unwrap()).unwrap();
    eprintln!("v3 fork dry run: wrote {out_path}");
}
