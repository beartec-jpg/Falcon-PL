//! falcon-gmpd — GMP-B sharded global mempool HTTP service.

use clap::Parser;
use fd_gmp::http_util::{serve, HandlerFn};
use fd_gmp::{CapacityAdvert, GlobalMempool, GmpTx};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "falcon-gmpd")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:18500")]
    listen: String,
    #[arg(long, default_value_t = 16)]
    shards: usize,
    #[arg(long, default_value_t = 50_000)]
    max_pending: usize,
    #[arg(long, default_value_t = 3.0)]
    lease_ttl_s: f64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();
    let args = Args::parse();
    let gmp = GlobalMempool::new(args.shards, args.max_pending, args.lease_ttl_s);
    let gmp2 = gmp.clone();
    let handler: Arc<HandlerFn> = Arc::new(move |method, path, body| {
        let body = body.unwrap_or(serde_json::json!({}));
        match (method.as_str(), path.as_str()) {
            ("GET", "/v1/status") | ("GET", "/status") => gmp2.status(),
            ("POST", "/v1/submit") | ("POST", "/submit") => {
                match serde_json::from_value::<GmpTx>(body) {
                    Ok(tx) => match gmp2.submit(tx) {
                        Ok(v) => v,
                        Err(e) => serde_json::json!({"error": e}),
                    },
                    Err(e) => serde_json::json!({"error": format!("bad tx: {e}")}),
                }
            }
            ("POST", "/v1/capacity") | ("POST", "/capacity") => {
                match serde_json::from_value::<CapacityAdvert>(body) {
                    Ok(adv) => gmp2.upsert_capacity(adv),
                    Err(e) => serde_json::json!({"error": format!("bad advert: {e}")}),
                }
            }
            ("POST", "/v1/lease") | ("POST", "/lease") => {
                let vid = body
                    .get("validator_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let max_n = body.get("max_n").and_then(|v| v.as_u64()).unwrap_or(16) as usize;
                let aff_mod = body
                    .get("affinity_mod")
                    .and_then(|v| v.as_u64())
                    .map(|x| x as usize);
                let aff_idx = body
                    .get("affinity_idx")
                    .and_then(|v| v.as_u64())
                    .map(|x| x as usize);
                if vid.is_empty() {
                    serde_json::json!({"error": "validator_id required"})
                } else {
                    match gmp2.lease(vid, max_n, aff_mod, aff_idx) {
                        Ok(v) => v,
                        Err(e) => serde_json::json!({"error": e}),
                    }
                }
            }
            ("POST", "/v1/complete") | ("POST", "/complete") => {
                let lid = body.get("lease_id").and_then(|v| v.as_str()).unwrap_or("");
                let bh = body.get("batch_hash").and_then(|v| v.as_str());
                match gmp2.complete(lid, bh) {
                    Ok(()) => serde_json::json!({"ok": true}),
                    Err(e) => serde_json::json!({"error": e}),
                }
            }
            ("POST", "/v1/nack") | ("POST", "/nack") => {
                let lid = body.get("lease_id").and_then(|v| v.as_str()).unwrap_or("");
                let reason = body.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                match gmp2.nack(lid, reason) {
                    Ok(()) => serde_json::json!({"ok": true}),
                    Err(e) => serde_json::json!({"error": e}),
                }
            }
            _ => serde_json::json!({"error": "not_found", "path": path}),
        }
    });
    tracing::info!("falcon-gmpd GMP-B shards={}", args.shards);
    serve(&args.listen, handler).await?;
    Ok(())
}
