//! falcon-gcpd — GCP-B v0 conf registry HTTP service.

use clap::Parser;
use fd_gmp::http_util::{serve, HandlerFn};
use fd_gmp::{ConfBatchRec, ConfRegistry};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "falcon-gcpd")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:18501")]
    listen: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();
    let args = Args::parse();
    let reg = ConfRegistry::new();
    let reg2 = reg.clone();
    let handler: Arc<HandlerFn> = Arc::new(move |method, path, body| {
        let body = body.unwrap_or(serde_json::json!({}));
        match (method.as_str(), path.as_str()) {
            ("GET", "/v1/status") | ("GET", "/status") => reg2.status(),
            ("GET", "/v1/open") | ("GET", "/open") => {
                serde_json::json!({ "open": reg2.list_open() })
            }
            ("POST", "/v1/batch") | ("POST", "/batch") => {
                match serde_json::from_value::<ConfBatchRec>(body) {
                    Ok(rec) => reg2.put_batch(rec),
                    Err(e) => serde_json::json!({"error": format!("{e}")}),
                }
            }
            ("POST", "/v1/sig") | ("POST", "/sig") => {
                let bh = body.get("batch_hash").and_then(|v| v.as_str()).unwrap_or("");
                let vid = body
                    .get("validator_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let sig = body.get("signature").and_then(|v| v.as_str()).unwrap_or("");
                match reg2.put_sig(bh, vid, sig) {
                    Ok(()) => serde_json::json!({"ok": true}),
                    Err(e) => serde_json::json!({"error": e}),
                }
            }
            ("POST", "/v1/cert") | ("POST", "/cert") => {
                let bh = body.get("batch_hash").and_then(|v| v.as_str()).unwrap_or("");
                match reg2.put_cert(bh) {
                    Ok(()) => serde_json::json!({"ok": true}),
                    Err(e) => serde_json::json!({"error": e}),
                }
            }
            _ => serde_json::json!({"error": "not_found", "path": path}),
        }
    });
    tracing::info!("falcon-gcpd GCP-B v0");
    serve(&args.listen, handler).await?;
    Ok(())
}
