//! End-to-end: HTTP router -> gateway -> mock Falcon node (real TCP, newline JSON
//! wire protocol, gossip lines before the reply like the live mesh port).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fpl_gateway::auth::{self, KeyStore};
use fpl_gateway::config::{Config, Tier};
use fpl_gateway::limits::Limiter;
use fpl_gateway::server::{self, AppState};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tower::ServiceExt;

/// Mock node: records every received message and answers like fd-pl.
async fn mock_node() -> (String, Arc<Mutex<Vec<Value>>>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let seen = seen2.clone();
            tokio::spawn(async move {
                let (r, mut w) = s.into_split();
                let mut lines = BufReader::new(r).lines();
                let Ok(Some(line)) = lines.next_line().await else { return };
                let m: Value = serde_json::from_str(&line).unwrap();
                seen.lock().unwrap().push(m.clone());
                // gossip noise first, as the mesh port does
                w.write_all(b"{\"type\":\"pong\",\"nonce\":1}\n{\"type\":\"ledger_ok\",\"height\":7}\n").await.unwrap();
                let resp = match m["type"].as_str().unwrap() {
                    "status_req" => json!({"type":"status","body":{"height":4242,"network":"falcon-pl-testnet"}}),
                    "health_req" => json!({"type":"health","body":{"schema":"falcon.health.v1","ok":true}}),
                    "account_query" if m["account"] == "nobody" => json!({"type":"err","msg":"unknown account"}),
                    "account_query" => json!({"type":"account","body":{"account":m["account"],"balance":"10"}}),
                    "submit_tx" => json!({"type":"ok","msg":"queued"}),
                    _ => json!({"type":"err","msg":"admin message rejected"}),
                };
                w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
            });
        }
    });
    (addr, seen)
}

struct Fx {
    app: axum::Router,
    key_free: String,
    key_scale: String,
    seen: Arc<Mutex<Vec<Value>>>,
}

async fn fixture(tweak: impl FnOnce(&mut Config)) -> Fx {
    let (addr, seen) = mock_node().await;
    let mut cfg = Config::proposed_default();
    cfg.upstream = addr;
    cfg.cors_origins = vec!["https://falcon-fpl.com".into()];
    cfg.max_body_bytes = 4096;
    tweak(&mut cfg);
    let (kf, rf) = auth::generate("k_free", Tier::Free, "");
    let (ks, rs) = auth::generate("k_scale", Tier::Scale, "");
    let st = AppState::new(cfg, KeyStore::from_records(vec![rf, rs]).unwrap(), Limiter::new(HashMap::new()));
    Fx { app: server::router(st), key_free: kf, key_scale: ks, seen }
}

async fn call(app: &axum::Router, key: Option<&str>, body: Value) -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut rb = Request::post("/v1/rpc").header("content-type", "application/json").header("origin", "https://falcon-fpl.com");
    if let Some(k) = key {
        rb = rb.header("authorization", format!("Bearer {k}"));
    }
    let r = app.clone().oneshot(rb.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let st = r.status();
    let h = r.headers().clone();
    let b = r.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&b).unwrap_or(Value::Null), h)
}

fn rq(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
}

#[tokio::test]
async fn read_methods_and_submit_round_trip() {
    let fx = fixture(|_| {}).await;
    let (s, v, h) = call(&fx.app, Some(&fx.key_scale), rq("fpl_status", json!({}))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["result"]["height"], 4242);
    assert_eq!(h["access-control-allow-origin"], "https://falcon-fpl.com");
    let (_, v, _) = call(&fx.app, Some(&fx.key_scale), rq("fpl_getAccount", json!({"account":"alice"}))).await;
    assert_eq!(v["result"]["account"], "alice");
    let (_, v, _) = call(&fx.app, Some(&fx.key_scale), rq("fpl_getAccount", json!({"account":"nobody"}))).await;
    assert_eq!(v["error"]["code"], -32000);
    let (_, v, _) = call(&fx.app, Some(&fx.key_scale), rq("fpl_submitTx", json!({"tx":{"from":"alice","seq":1}}))).await;
    assert_eq!(v["result"]["msg"], "queued");
    let seen = fx.seen.lock().unwrap().clone();
    assert_eq!(seen[0], json!({"type":"status_req","include_accounts":false,"brief":true}));
    assert_eq!(seen[3]["type"], "submit_tx");
}

#[tokio::test]
async fn auth_required() {
    let fx = fixture(|_| {}).await;
    let (s, v, _) = call(&fx.app, None, rq("fpl_status", json!({}))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"]["code"], -32001);
    let (s, _, _) = call(&fx.app, Some("fplk_deadbeef"), rq("fpl_status", json!({}))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(fx.seen.lock().unwrap().is_empty(), "nothing forwarded");
}

#[tokio::test]
async fn admin_and_ctl_methods_never_reach_node() {
    let fx = fixture(|_| {}).await;
    for m in ["seal_now", "mempool_drop", "snapshot_save", "operator_hard_reset", "fpl_sealForce", "status_req", "rail_deposit"] {
        let (_, v, _) = call(&fx.app, Some(&fx.key_scale), rq(m, json!({}))).await;
        assert_eq!(v["error"]["code"], -32601, "{m}");
    }
    let (_, _, _) = call(&fx.app, Some(&fx.key_scale), rq("fpl_getAccount", json!({"account":"a","type":"seal_force"}))).await;
    let seen = fx.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["type"], "account_query");
}

#[tokio::test]
async fn rate_limit_and_quota_over_http() {
    let fx = fixture(|c| {
        c.tiers.get_mut(&Tier::Free).unwrap().burst = 3;
        c.tiers.get_mut(&Tier::Free).unwrap().rps = 0.001;
    })
    .await;
    for _ in 0..3 {
        assert_eq!(call(&fx.app, Some(&fx.key_free), rq("fpl_health", json!({}))).await.0, StatusCode::OK);
    }
    let (s, v, h) = call(&fx.app, Some(&fx.key_free), rq("fpl_health", json!({}))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], -32005);
    assert!(h.contains_key("retry-after"));

    let fx = fixture(|c| c.tiers.get_mut(&Tier::Free).unwrap().monthly_quota = 2).await;
    call(&fx.app, Some(&fx.key_free), rq("fpl_health", json!({}))).await;
    call(&fx.app, Some(&fx.key_free), rq("fpl_health", json!({}))).await;
    let (s, v, _) = call(&fx.app, Some(&fx.key_free), rq("fpl_health", json!({}))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert!(v["error"]["message"].as_str().unwrap().contains("monthly quota"));
    // usage endpoint reflects metering
    let r = fx.app.clone().oneshot(Request::get("/v1/usage").header("x-api-key", &fx.key_free).body(Body::empty()).unwrap()).await.unwrap();
    let u: Value = serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(u["units_used"], 2);
    assert_eq!(u["tier"], "free");
}

#[tokio::test]
async fn body_cap_cors_preflight_health_tiers() {
    let fx = fixture(|_| {}).await;
    let big = rq("fpl_submitTx", json!({"tx":{"pad":"x".repeat(8000)}}));
    let (s, _, _) = call(&fx.app, Some(&fx.key_scale), big).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);

    let r = fx.app.clone().oneshot(Request::options("/v1/rpc").header("origin", "https://evil.example").body(Body::empty()).unwrap()).await.unwrap();
    assert!(!r.headers().contains_key("access-control-allow-origin"));
    let r = fx.app.clone().oneshot(Request::options("/v1/rpc").header("origin", "https://falcon-fpl.com").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert!(r.headers()["access-control-allow-headers"].to_str().unwrap().contains("x-api-key"));

    let r = fx.app.clone().oneshot(Request::get("/v1/health").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let r = fx.app.clone().oneshot(Request::get("/v1/tiers").body(Body::empty()).unwrap()).await.unwrap();
    let t: Value = serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(t["currency"], "GBP");
    assert_eq!(t["tiers"][1]["price_gbp_per_month"], 29.0);
    assert_eq!(t["tiers"][2]["price_gbp_per_month"], 199.0);

    let r = fx.app.clone().oneshot(Request::post("/v1/billing/stripe/webhook").body(Body::from("{}")).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND, "billing stub off by default");
}

#[tokio::test]
async fn upstream_down_is_502_and_health_503() {
    let fx = fixture(|c| {
        c.upstream = "127.0.0.1:1".into();
        c.upstream_timeout_ms = 500;
    })
    .await;
    let (s, v, _) = call(&fx.app, Some(&fx.key_scale), rq("fpl_status", json!({}))).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    assert_eq!(v["error"]["code"], -32002);
    let r = fx.app.clone().oneshot(Request::get("/v1/health").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    let m = server::metrics_router(AppState::new(Config::proposed_default(), KeyStore::default(), Limiter::new(HashMap::new())));
    let r = m.oneshot(Request::get("/metrics").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}
