//! `falcon-pl-ctl dash --once` / `health` against fake nodes: new node
//! (Health reply), older node (rejects or ignores HealthReq → status
//! fallback), and no node at all.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;

/// Fake node: for each accepted connection `i`, read one line and answer
/// `replies[i]` (`None` = read and stay silent until the client gives up).
fn fake_node(replies: Vec<Option<&'static str>>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for r in replies {
            let Ok((s, _)) = l.accept() else { return };
            std::thread::spawn(move || {
                let mut rd = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                let _ = rd.read_line(&mut line);
                let mut w = s;
                match r {
                    Some(reply) => {
                        let _ = w.write_all(reply.as_bytes());
                        let _ = w.write_all(b"\n");
                        let _ = w.flush();
                    }
                    None => std::thread::sleep(std::time::Duration::from_secs(10)),
                }
            });
        }
    });
    addr
}

fn ctl(args: &[&str]) -> (Option<i32>, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_falcon-pl-ctl"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run falcon-pl-ctl");
    (
        o.status.code(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

const HEALTH: &str = r#"{"type":"health","body":{"schema":"falcon.health.v1",
"node":{"id":"seat9","version":"2.9.79","network":"Falcon PL","network_id":2300,"role":"validator","uptime_secs":3700,"status":"active"},
"chain":{"local_height":671042,"network_tip":671044,"best_peer_tip":671044,"lag":2,"blocks_per_min":12.6,"blocks_per_min_spark":[10,12,13,12],"last_block":{"height":671042,"sealed_at_ms":1,"age_ms":4000,"hash":"9be1aaaaaaaaaaaa04aa","packer":"v1"}},
"consensus":{"epoch":10,"epoch_progress_pct":63,"proposals_this_epoch":41,"votes_this_epoch":3912,"missed_turns":1,"missed_turns_limit":20,"bond":250000,"last_proposal":{"height":671001,"sealed_at_ms":0}},
"mempool":{"size":3,"max":20000},
"peers":{"count":2,"outbound":1,"inbound":1,"seats":[{"id":"v1","tip":671044,"pong_age_ms":900},{"id":"v3","tip":671002,"pong_age_ms":null}]},
"system":{"process":{"cpu_pct":31.0,"rss_bytes":1073741824,"mem_total_bytes":17179869184,"loadavg":[0.84,0.91,0.88]},"disk":{"free_bytes":107374182400,"total_bytes":644245094400}},
"warnings":[{"at_ms":0,"level":"WARN","target":"t","msg":"peer v3 lag 42 blocks"}]}}"#;

const STATUS: &str = r#"{"type":"status","body":{"id":"old7","product_version":"2.9.70","network_id":2300,"tip_height":500,"best_peer_tip":510,"tip_lag":10,"mempool":0,"epoch":9,"validators":[{"id":"old7","bond":50000,"jailed":false,"lottery_ready":true,"v2":{"missed_turns":4}}]}}"#;

fn one_line(s: &str) -> &'static str {
    Box::leak(s.replace('\n', "").into_boxed_str())
}

#[test]
fn dash_once_renders_health() {
    let addr = fake_node(vec![Some(one_line(HEALTH))]);
    let (code, out, err) = ctl(&["--addr", &addr, "dash", "--once"]);
    assert_eq!(code, Some(0), "{out}{err}");
    for p in ["┤ Node ├", "┤ Chain ├", "┤ Consensus ├", "┤ Peers ├", "┤ System ├", "┤ Recent warnings ├"] {
        assert!(out.contains(p), "missing {p}\n{out}");
    }
    assert!(out.contains("seat9"));
    assert!(out.contains("● ACTIVE"));
    assert!(out.contains("Local height   671042"));
    assert!(out.contains("Missed turns   1/20"));
    assert!(out.contains("peer v3 lag 42 blocks"));
    assert!(!out.contains('\x1b'), "piped output has no ANSI");
}

#[test]
fn dash_falls_back_when_node_rejects_health() {
    let addr = fake_node(vec![
        Some(r#"{"type":"err","msg":"unknown variant `health_req`"}"#),
        Some(STATUS),
    ]);
    let (code, out, err) = ctl(&["--addr", &addr, "dash", "--once"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("health endpoint unavailable"), "{out}");
    assert!(out.contains("old7") && out.contains("Local height   500"), "{out}");
    assert!(out.contains("Missed turns   4/20"), "{out}");
}

#[test]
fn dash_falls_back_when_node_ignores_health() {
    let addr = fake_node(vec![None, Some(STATUS)]);
    let t0 = std::time::Instant::now();
    let (code, out, err) = ctl(&["--addr", &addr, "dash", "--once"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("timeout"), "{out}");
    assert!(out.contains("Local height   500"), "{out}");
    assert!(t0.elapsed() < std::time::Duration::from_secs(9));
}

#[test]
fn dash_once_fails_when_no_node() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    drop(l);
    let (code, out, _) = ctl(&["--addr", &addr, "dash", "--once"]);
    assert_ne!(code, Some(0));
    assert!(out.contains("node RPC not answering"), "{out}");
}

#[test]
fn health_subcommand_prints_json() {
    let addr = fake_node(vec![Some(one_line(HEALTH))]);
    let (code, out, err) = ctl(&["--addr", &addr, "health"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["body"]["schema"], "falcon.health.v1");
}

#[test]
fn dash_grid_collapses_warnings_and_stale_peers() {
    let h = HEALTH
        .replace(
            r#"{"id":"v3","tip":671002,"pong_age_ms":null}"#,
            r#"{"id":"v3","tip":671002,"pong_age_ms":null},{"id":"old2","tip":0,"pong_age_ms":4079164000}"#,
        )
        .replace(
            r#""warnings":[{"at_ms":0,"level":"WARN","target":"t","msg":"peer v3 lag 42 blocks"}]"#,
            r#""warnings":[{"at_ms":1,"level":"WARN","target":"t","msg":"inbound reply dropped"},{"at_ms":2,"level":"WARN","target":"t","msg":"inbound reply dropped"},{"at_ms":3,"level":"WARN","target":"t","msg":"inbound reply dropped"}]"#,
        );
    let addr = fake_node(vec![Some(one_line(&h))]);
    let o = Command::new(env!("CARGO_BIN_EXE_falcon-pl-ctl"))
        .args(["--addr", &addr, "dash", "--once"])
        .env("NO_COLOR", "1")
        .env("COLUMNS", "160")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("inbound reply dropped ×3"), "{out}");
    assert_eq!(out.matches("inbound reply dropped").count(), 1, "{out}");
    assert!(out.contains("stale (1): old2"), "{out}");
    // 3 panels side by side at 160 columns
    assert!(out.lines().any(|l| l.contains("┤ Node ├") && l.contains("┤ Chain ├") && l.contains("┤ Consensus ├")), "{out}");
    assert!(out.lines().all(|l| l.chars().count() <= 160), "{out}");
}

const BAL: &str = r#""balance":{"account":"seat9","liquid":1234567,"pending_rewards":89,"bonded":250000,"unbonding":1000,"unbond_ready_ms":5,"bond_account":"seat9-bond","bond_account_liquid":42},"mempool""#;

fn health_with_balance() -> String {
    HEALTH.replace(r#""mempool""#, BAL)
}

#[test]
fn dash_node_panel_shows_balance() {
    let addr = fake_node(vec![Some(one_line(&health_with_balance()))]);
    let (code, out, err) = ctl(&["--addr", &addr, "dash", "--once"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("── Balance ──"), "{out}");
    assert!(out.contains("Liquid         1,234,567 FPL"), "{out}");
    assert!(out.contains("Bonded         250,000 FPL"), "{out}");
    assert!(out.contains("Pending rwd    89 FPL"), "{out}");
    assert!(out.contains("Unbonding      1,000 FPL"), "{out}");
    assert!(out.contains("seat9-bond 42 FPL"), "{out}");
}

#[test]
fn balance_command_from_health_block() {
    let addr = fake_node(vec![Some(one_line(&health_with_balance()))]);
    let (code, out, err) = ctl(&["--addr", &addr, "balance"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert_eq!(
        out.trim(),
        "seat9: liquid 1,234,567 FPL · bonded 250,000 FPL · pending rewards 89 FPL · unbonding 1,000 FPL · bond account seat9-bond 42 FPL"
    );
}

/// Pre-2.9.79 node: no health endpoint → status + existing account query.
#[test]
fn balance_command_on_old_node_uses_account_query() {
    let addr = fake_node(vec![
        Some(r#"{"type":"err","msg":"unknown variant `health_req`"}"#),
        Some(STATUS),
        Some(r#"{"type":"account","body":{"account":"old7","sequence":3,"balance":777,"claimable":12}}"#),
    ]);
    let (code, out, err) = ctl(&["--addr", &addr, "balance", "--json"]);
    assert_eq!(code, Some(0), "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(v["account"], "old7");
    assert_eq!(v["liquid"], 777);
    assert_eq!(v["pending_rewards"], 12);
    assert_eq!(v["bonded"], 50000);
}

#[test]
fn balance_command_reads_no_keys() {
    // HOME points at an empty dir and no --keys-dir exists: balance must not need one.
    let tmp = std::env::temp_dir().join(format!("fpl-bal-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let addr = fake_node(vec![Some(one_line(&health_with_balance()))]);
    let o = Command::new(env!("CARGO_BIN_EXE_falcon-pl-ctl"))
        .args(["--addr", &addr, "balance"])
        .env("HOME", &tmp)
        .current_dir(&tmp)
        .output()
        .unwrap();
    assert!(o.status.success());
    assert!(std::fs::read_dir(&tmp).unwrap().next().is_none(), "balance wrote files");
    let _ = std::fs::remove_dir_all(&tmp);
}
