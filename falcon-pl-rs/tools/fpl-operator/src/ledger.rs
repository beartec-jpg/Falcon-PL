//! BRIDGE ledger ops: once the Falcon node reports `bridge_operators.active`
//! (the one combined bridge height), the kit puts its operator duties on the
//! ledger with `falcon-pl-ctl operator`:
//!  * `operator_heartbeat` every half hour (liveness slot + watch unit) with the
//!    free BTC capacity it already advertises on gossip (wallet - in-flight - reserve);
//!  * `operator_graph_bind` for every deposit graph it presigned (kick_off_2 +
//!    assert_final txids from final.json), so a Disprove of its graph is slashable;
//!  * `operator_slash` after a Disprove it broadcast is mined (via the slash hook,
//!    default bin/fpl-operator-slash.sh -> slash_evidence.py).
//! Inert before activation (no tx is sent; the ledger would refuse it anyway).
//! The kit's Falcon account key is used only by falcon-pl-ctl (keys dir); never printed.

use serde_json::{json, Value};

/// `bridge_operators.active` from `falcon-pl-ctl status` (wrapped in `body` or not).
pub fn bridge_active(status: &Value) -> bool {
    let b = status.get("body").unwrap_or(status);
    b["bridge_operators"]["active"].as_bool().unwrap_or(false)
}

/// Our registry record (by Falcon account) from status, if registered.
pub fn our_record<'a>(status: &'a Value, account: &str) -> Option<&'a Value> {
    let b = status.get("body").unwrap_or(status);
    b["bridge_operators"]["operators"].get(account)
}

/// txid (display hex) of a consensus-serialized tx (segwit or legacy).
pub fn txid_of_hex(tx_hex: &str) -> Option<String> {
    let raw = hex::decode(tx_hex.trim()).ok()?;
    crate::deposits::txid_and_out0(&raw).map(|(t, _)| t)
}

/// `operator_graph_bind` bodies for every peg-out graph in a final.json whose
/// operator key is ours (x-only match).
pub fn graph_bind_bodies(final_json: &Value, our_xonly: &str) -> Vec<Value> {
    let ours = our_xonly.trim().to_ascii_lowercase();
    let mut out = Vec::new();
    for g in final_json["peg_out_graphs"].as_array().into_iter().flatten() {
        let opk = g["operator_public_key"].as_str().unwrap_or("").to_ascii_lowercase();
        let x = if opk.len() == 66 { opk[2..].to_string() } else { opk.clone() };
        if x != ours {
            continue;
        }
        let (Some(dep), Some(k2), Some(af)) = (
            g["peg_in_confirm_txid"].as_str(),
            g["kick_off_2_transaction"]["tx"].as_str().and_then(txid_of_hex),
            g["assert_final_transaction"]["tx"].as_str().and_then(txid_of_hex),
        ) else {
            continue;
        };
        out.push(json!({"kind": "operator_graph_bind", "deposit_txid": dep.to_ascii_lowercase(), "kick_off_2_txid": k2, "assert_final_txid": af}));
    }
    out
}

/// Bind bodies for this seat: our own peg-out graphs, or — when this seat
/// only co-signed the n-of-n (no graph of ours) — `cosign` attestations of
/// every other operator's graph in the ceremony.
pub fn bind_or_cosign_bodies(final_json: &Value, our_xonly: &str) -> Vec<Value> {
    let own = graph_bind_bodies(final_json, our_xonly);
    if !own.is_empty() {
        return own;
    }
    let mut out = Vec::new();
    for g in final_json["peg_out_graphs"].as_array().into_iter().flatten() {
        let (Some(dep), Some(k2), Some(af)) = (
            g["peg_in_confirm_txid"].as_str(),
            g["kick_off_2_transaction"]["tx"].as_str().and_then(txid_of_hex),
            g["assert_final_transaction"]["tx"].as_str().and_then(txid_of_hex),
        ) else {
            continue;
        };
        out.push(json!({"kind": "operator_graph_bind", "deposit_txid": dep.to_ascii_lowercase(), "kick_off_2_txid": k2, "assert_final_txid": af, "cosign": true}));
    }
    out
}

/// Is `deposit:xonly` bound on the ledger (status `bridge_operators.graphs`)?
pub fn is_bound(status: &Value, deposit_txid: &str, xonly: &str) -> bool {
    let b = status.get("body").unwrap_or(status);
    let k = format!("{}:{}", deposit_txid.to_ascii_lowercase(), xonly.to_ascii_lowercase());
    b["bridge_operators"]["graphs"].get(&k).is_some()
}

fn bo(status: &Value) -> &Value {
    &status.get("body").unwrap_or(status)["bridge_operators"]
}

/// Was the slash for `deposit:xonly` applied on the ledger (status `bridge_operators.slashed`)?
pub fn slash_applied(status: &Value, deposit_txid: &str, xonly: &str) -> bool {
    let k = format!("{}:{}", deposit_txid.to_ascii_lowercase(), xonly.to_ascii_lowercase());
    bo(status)["slashed"].as_array().map_or(false, |a| a.iter().any(|x| x.as_str() == Some(k.as_str())))
}

/// Ledger height from which a still-open claim must not be fronted any more:
/// half the refund window (refund cannot race an in-flight honest peg-out).
pub fn front_cutoff(status: &Value, open_height: u64) -> Option<u64> {
    let b = bo(status);
    let t = b["pegout_refund_ledgers"].as_u64()?;
    let from = b["from_height"].as_u64().unwrap_or(0);
    Some(open_height.max(from).saturating_add(t / 2))
}

/// DEPOSITOR / KIT REFUSAL RULE (bridge active): a deposit is acceptable only if
/// final.json has at least one peg-out graph, all for the same peg-in confirm
/// txid, and EVERY graph's operator is a registered Active operator whose
/// on-ledger binding for that deposit exists, was posted by the account that
/// registered that BTC key, and matches the graph's KickOff2 + AssertFinal
/// txids. Anything else: refuse (do not broadcast the peg-in confirm, do not
/// front against it). Returns the number of graphs checked.
pub fn check_deposit_graphs(final_json: &Value, status: &Value) -> Result<usize, String> {
    let b = bo(status);
    let graphs = final_json["peg_out_graphs"].as_array().cloned().unwrap_or_default();
    if graphs.is_empty() {
        return Err("no peg-out graphs in final.json".into());
    }
    let ops = b["operators"].as_object().cloned().unwrap_or_default();
    let mut dep0: Option<String> = None;
    for (i, g) in graphs.iter().enumerate() {
        let opk = g["operator_public_key"].as_str().unwrap_or("").to_ascii_lowercase();
        let x = if opk.len() == 66 { opk[2..].to_string() } else { opk };
        let dep = g["peg_in_confirm_txid"].as_str().unwrap_or("").to_ascii_lowercase();
        if x.len() != 64 || dep.len() != 64 {
            return Err(format!("graph {i}: missing operator key or peg_in_confirm_txid"));
        }
        if dep0.get_or_insert(dep.clone()) != &dep {
            return Err(format!("graph {i}: different peg-in confirm txid"));
        }
        let k2 = g["kick_off_2_transaction"]["tx"].as_str().and_then(txid_of_hex).ok_or(format!("graph {i}: bad kick_off_2 tx"))?;
        let af = g["assert_final_transaction"]["tx"].as_str().and_then(txid_of_hex).ok_or(format!("graph {i}: bad assert_final tx"))?;
        let Some((acct, rec)) = ops.iter().find(|(_, r)| r["btc_xonly_hex"].as_str().map(|s| s.eq_ignore_ascii_case(&x)) == Some(true)) else {
            return Err(format!("graph {i}: operator {x} not registered on the ledger"));
        };
        if rec["status"].as_str() != Some("Active") && rec["status"] != json!("active") {
            return Err(format!("graph {i}: operator {x} not active ({})", rec["status"]));
        }
        let key = format!("{dep}:{x}");
        let Some(bind) = b["graphs"].get(&key) else {
            return Err(format!("graph {i}: operator {x} has not bound this graph on the ledger"));
        };
        if bind["account"].as_str() != Some(acct.as_str()) {
            return Err(format!("graph {i}: binding posted by {} not by registered account {acct}", bind["account"]));
        }
        if bind["kick_off_2_txid"].as_str().map(|s| s.eq_ignore_ascii_case(&k2)) != Some(true)
            || bind["assert_final_txid"].as_str().map(|s| s.eq_ignore_ascii_case(&af)) != Some(true)
        {
            return Err(format!("graph {i}: on-ledger binding does not match final.json (kick_off_2 {k2}, assert_final {af})"));
        }
    }
    Ok(graphs.len())
}

pub fn heartbeat_body(capacity_sats: u64) -> Value {
    json!({"kind": "operator_heartbeat", "capacity_sats": capacity_sats})
}

pub fn register_body(btc_xonly: &str, reward_spk_hex: &str, gossip_url: &str, capacity_sats: u64) -> Value {
    json!({"kind": "operator_register", "btc_xonly": btc_xonly, "reward_spk_hex": reward_spk_hex, "gossip_url": gossip_url, "capacity_sats": capacity_sats})
}

/// A mined Disprove reported by fpl_challenge / fpl_watcher output
/// (`DISPROVE_TXID <txid>` or the watcher's `DISPROVE <txid> ... ok=true`).
pub fn disprove_txid_in(out: &str) -> Option<String> {
    for l in out.lines() {
        let l = l.trim();
        let rest = if let Some(r) = l.strip_prefix("DISPROVE_TXID ") {
            r
        } else if let Some(i) = l.find("DISPROVE ") {
            if !l.contains("ok=true") {
                continue;
            }
            &l[i + 9..]
        } else {
            continue;
        };
        let t: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        if t.len() == 64 {
            return Some(t.to_ascii_lowercase());
        }
    }
    None
}

/// testnet4 rail checkpoint (must match fd-pl `BTC4_CHECKPOINT_HEIGHT`).
pub const BTC4_CHECKPOINT_HEIGHT: u64 = 155_232;
/// Headers relayed per poll per operator.
pub const BTC4_HEADER_BURST: u64 = 24;

/// BTC4 rail in the status JSON: (tip height, minted peg-in txids).
pub fn btc4_rail(status: &Value) -> Option<(u64, Vec<String>)> {
    let b = if status["body"].is_object() { &status["body"] } else { status };
    let r = b["rails"].as_array()?.iter().find(|r| r["asset"].as_str() == Some("BTC4"))?;
    let minted = r["spent_deposits"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(|s| s.to_ascii_lowercase())).collect();
    Some((r["tip_height"].as_u64().unwrap_or(0), minted))
}

/// Next sequence from `falcon-pl-ctl account` output (any `sequence` field).
pub fn account_sequence(out: &str) -> Option<u64> {
    fn find(v: &Value) -> Option<u64> {
        match v {
            Value::Object(m) => m.get("sequence").and_then(|s| s.as_u64()).or_else(|| m.values().find_map(find)),
            Value::Array(a) => a.iter().find_map(find),
            _ => None,
        }
    }
    let start = out.find('{')?;
    find(&serde_json::from_str::<Value>(&out[start..]).ok()?)
}

/// Depositor Falcon account recorded by the deposit intake (`request.json`).
pub fn request_depositor(dir: &std::path::Path) -> String {
    std::fs::read(dir.join("request.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["depositor_account"].as_str().map(|s| s.trim().to_string()))
        .unwrap_or_default()
}

/// `falcon-pl-ctl` args for one operator body.
pub fn ctl_args(rpc: &str, account: &str, body: &Value, amount: u64, scheme: &str, keys_dir: &str, network_id: &str) -> Vec<String> {
    let mut a = vec![
        "--addr".to_string(), rpc.to_string(), "operator".into(),
        "--account".into(), account.to_string(),
        "--body".into(), body.to_string(),
        "--amount".into(), amount.to_string(),
        "--scheme".into(), scheme.to_string(),
        "--network-id".into(), network_id.to_string(),
    ];
    if !keys_dir.is_empty() {
        a.push("--keys-dir".into());
        a.push(keys_dir.to_string());
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_flag_both_shapes() {
        assert!(!bridge_active(&json!({"body": {"bridge_operators": {"active": false}}})));
        assert!(bridge_active(&json!({"body": {"bridge_operators": {"active": true}}})));
        assert!(bridge_active(&json!({"bridge_operators": {"active": true}})));
        assert!(!bridge_active(&json!({"height": 1})), "old node: inert");
        let st = json!({"body": {"bridge_operators": {"graphs": {format!("{}:{}", "cd".repeat(32), "aa".repeat(32)): {}}}}});
        assert!(is_bound(&st, &"CD".repeat(32), &"aa".repeat(32)));
        assert!(!is_bound(&st, &"cd".repeat(32), &"bb".repeat(32)));
    }

    #[test]
    fn disprove_tags() {
        let w = "watcher log: DISPROVE ca7620a03b45452032957b0b2f1fd38fc5cca135180cd070cf22d601487c3bba weight=956229 ok=true [..]";
        assert_eq!(disprove_txid_in(w).as_deref(), Some("ca7620a03b45452032957b0b2f1fd38fc5cca135180cd070cf22d601487c3bba"));
        assert_eq!(disprove_txid_in("watcher log: DISPROVE ca7620a03b45452032957b0b2f1fd38fc5cca135180cd070cf22d601487c3bba ok=false"), None);
        assert_eq!(disprove_txid_in(&format!("DISPROVE_TXID {}", "ab".repeat(32))), Some("ab".repeat(32)));
    }

    #[test]
    fn graph_bind_only_our_graphs() {
        let tx = "0200000001".to_string() + &"00".repeat(36) + "00ffffffff01e803000000000000016a00000000";
        let f = json!({"peg_out_graphs": [
            {"operator_public_key": format!("02{}", "aa".repeat(32)), "peg_in_confirm_txid": "CD".repeat(32),
             "kick_off_2_transaction": {"tx": tx}, "assert_final_transaction": {"tx": tx.replace("e803", "e903")}},
            {"operator_public_key": format!("03{}", "bb".repeat(32)), "peg_in_confirm_txid": "ef".repeat(32),
             "kick_off_2_transaction": {"tx": tx}, "assert_final_transaction": {"tx": tx}}
        ]});
        let b = graph_bind_bodies(&f, &"aa".repeat(32));
        assert_eq!(b.len(), 1);
        assert_eq!(b[0]["deposit_txid"], "cd".repeat(32));
        assert_ne!(b[0]["kick_off_2_txid"], b[0]["assert_final_txid"]);
        assert_eq!(b[0]["kick_off_2_txid"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn cosign_when_no_graph_of_ours() {
        let tx = "0200000001".to_string() + &"00".repeat(36) + "00ffffffff01e803000000000000016a00000000";
        let f = json!({"peg_out_graphs": [
            {"operator_public_key": format!("02{}", "aa".repeat(32)), "peg_in_confirm_txid": "CD".repeat(32),
             "kick_off_2_transaction": {"tx": tx}, "assert_final_transaction": {"tx": tx}}]});
        assert!(bind_or_cosign_bodies(&f, &"aa".repeat(32))[0].get("cosign").is_none());
        let c = bind_or_cosign_bodies(&f, &"cc".repeat(32));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0]["cosign"], true);
        assert_eq!(c[0]["deposit_txid"], "cd".repeat(32));
    }

    fn tx(v: &str) -> String {
        "0200000001".to_string() + &"00".repeat(36) + "00ffffffff01" + v + "000000000000016a00000000"
    }

    fn fixture() -> (Value, Value) {
        let (a, b) = ("aa".repeat(32), "bb".repeat(32));
        let dep = "cd".repeat(32);
        let g = |x: &str, k2: &str, af: &str| json!({"operator_public_key": format!("02{x}"), "peg_in_confirm_txid": dep,
            "kick_off_2_transaction": {"tx": tx(k2)}, "assert_final_transaction": {"tx": tx(af)}});
        let f = json!({"peg_out_graphs": [g(&a, "e803", "e903"), g(&b, "ea03", "eb03")]});
        let t = |v: &str| txid_of_hex(&tx(v)).unwrap();
        let st = json!({"body": {"bridge_operators": {"active": true, "from_height": 1000, "pegout_refund_ledgers": 43200,
            "operators": {"op1": {"btc_xonly_hex": a, "status": "Active"}, "op2": {"btc_xonly_hex": b, "status": "Active"}},
            "graphs": {
                format!("{dep}:{a}"): {"account": "op1", "kick_off_2_txid": t("e803"), "assert_final_txid": t("e903")},
                format!("{dep}:{b}"): {"account": "op2", "kick_off_2_txid": t("ea03"), "assert_final_txid": t("eb03")}
            },
            "slashed": [format!("{dep}:{b}")]}}});
        (f, st)
    }

    #[test]
    fn deposit_refusal_rule() {
        let (f, st) = fixture();
        assert_eq!(check_deposit_graphs(&f, &st), Ok(2));
        // one operator unbound -> refuse
        let mut s2 = st.clone();
        let k = s2["body"]["bridge_operators"]["graphs"].as_object().unwrap().keys().nth(1).unwrap().clone();
        s2["body"]["bridge_operators"]["graphs"].as_object_mut().unwrap().remove(&k);
        assert!(check_deposit_graphs(&f, &s2).unwrap_err().contains("has not bound"));
        // mismatch -> refuse
        let mut s3 = st.clone();
        s3["body"]["bridge_operators"]["graphs"][&k]["assert_final_txid"] = json!("00".repeat(32));
        assert!(check_deposit_graphs(&f, &s3).unwrap_err().contains("does not match"));
        // bound by someone else's account -> refuse
        let mut s4 = st.clone();
        s4["body"]["bridge_operators"]["graphs"][&k]["account"] = json!("op1");
        assert!(check_deposit_graphs(&f, &s4).unwrap_err().contains("not by registered"));
        // unregistered / jailed -> refuse
        let mut s5 = st.clone();
        s5["body"]["bridge_operators"]["operators"]["op2"]["status"] = json!({"Jailed": {"until_epoch": 9}});
        assert!(check_deposit_graphs(&f, &s5).unwrap_err().contains("not active"));
        let mut s6 = st.clone();
        s6["body"]["bridge_operators"]["operators"].as_object_mut().unwrap().remove("op2");
        assert!(check_deposit_graphs(&f, &s6).unwrap_err().contains("not registered"));
        assert!(check_deposit_graphs(&json!({"peg_out_graphs": []}), &st).is_err());
    }

    #[test]
    fn slash_recheck_and_cutoff() {
        let (_, st) = fixture();
        assert!(slash_applied(&st, &"CD".repeat(32), &"bb".repeat(32)));
        assert!(!slash_applied(&st, &"cd".repeat(32), &"aa".repeat(32)));
        assert_eq!(front_cutoff(&st, 500), Some(1000 + 21600));
        assert_eq!(front_cutoff(&st, 5000), Some(5000 + 21600));
        assert_eq!(front_cutoff(&json!({}), 5), None);
    }

    /// Real testnet4 take1 graph (public final.json subset): kit txids match an
    /// independent witness-stripping computation; rule accepts the matching
    /// binding and refuses a stale one.
    #[test]
    fn real_tn4_graph_bind_and_check() {
        let f: Value = serde_json::from_str(include_str!("../testdata/tn4_final_take1_graph.json")).unwrap();
        let x: Value = serde_json::from_str(include_str!("../testdata/tn4_final_take1_expect.json")).unwrap();
        let xonly = x["xonly"].as_str().unwrap();
        let b = graph_bind_bodies(&f, xonly);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0]["kick_off_2_txid"], x["kick_off_2_txid"]);
        assert_eq!(b[0]["assert_final_txid"], x["assert_final_txid"]);
        let dep = b[0]["deposit_txid"].as_str().unwrap().to_string();
        let mut st = json!({"bridge_operators": {"active": true, "operators": {"opA": {"btc_xonly_hex": xonly, "status": "Active"}},
            "graphs": {format!("{dep}:{xonly}"): {"account": "opA", "kick_off_2_txid": x["kick_off_2_txid"], "assert_final_txid": x["assert_final_txid"]}}}});
        assert_eq!(check_deposit_graphs(&f, &st), Ok(1));
        st["bridge_operators"]["graphs"][format!("{dep}:{xonly}")]["kick_off_2_txid"] = json!("11".repeat(32));
        assert!(check_deposit_graphs(&f, &st).is_err());
    }
}
