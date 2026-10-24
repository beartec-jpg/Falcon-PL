//! falcon-gmp-facade — multi-shard GMP front door + cross-shard bridge (lab).
//!
//! Same-shard: proxy to shard GMP.
//! Cross-shard: debit → wait exec → credit; on credit failure after debit exec → refund.
//! Idempotency: optional `client_id` or stable key account:seq:dest:amount.
//!
//!   --shard 'id=0,name=s0,gmp=http://127.0.0.1:18500,rpc=http://127.0.0.1:18001,bridge=acct20,lo=1,hi=20'

use anyhow::{bail, Context, Result};
use clap::Parser;
use fd_gmp::http_util::{serve, HandlerFn};
use fd_gmp::{wall_now, AccountRouter, GmpClient, GmpTx, NodeRpcClient, ShardAssign, ShardEndpoint};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "falcon-gmp-facade")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:18499")]
    listen: String,
    #[arg(long = "shard", required = true)]
    shards: Vec<String>,
    #[arg(long, default_value_t = 45.0)]
    xshard_timeout_s: f64,
}

#[derive(Debug, Clone)]
struct ShardRuntime {
    ep: ShardEndpoint,
    gmp: GmpClient,
    rpc: NodeRpcClient,
}

#[derive(Debug, Clone, Default)]
struct FacadeStats {
    same_shard_ok: u64,
    same_shard_err: u64,
    xshard_started: u64,
    xshard_credit_ok: u64,
    xshard_credit_err: u64,
    xshard_timeout: u64,
    xshard_refund_ok: u64,
    xshard_refund_err: u64,
    xshard_idempotent_hits: u64,
}

#[derive(Debug, Deserialize)]
struct SubmitBody {
    account: String,
    sequence: u64,
    destination: String,
    #[serde(default = "one")]
    amount: u64,
    #[serde(default = "ten")]
    fee: u64,
    /// Client-supplied idempotency key (optional).
    #[serde(default)]
    client_id: Option<String>,
}

fn one() -> u64 {
    1
}
fn ten() -> u64 {
    10
}

fn parse_shards(lines: &[String]) -> Result<(AccountRouter, Vec<(u32, u32, usize)>)> {
    let mut endpoints = Vec::new();
    let mut ranges = Vec::new();
    for line in lines {
        let mut id = None::<usize>;
        let mut name = None::<String>;
        let mut gmp = None::<String>;
        let mut rpc = None::<String>;
        let mut bridge = None::<String>;
        let mut lo = None::<u32>;
        let mut hi = None::<u32>;
        for part in line.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (k, v) = part
                .split_once('=')
                .with_context(|| format!("bad shard kv in {line}"))?;
            match k.trim() {
                "id" => id = Some(v.trim().parse()?),
                "name" => name = Some(v.trim().into()),
                "gmp" => gmp = Some(v.trim().into()),
                "rpc" => rpc = Some(v.trim().into()),
                "bridge" => bridge = Some(v.trim().into()),
                "lo" => lo = Some(v.trim().parse()?),
                "hi" => hi = Some(v.trim().parse()?),
                other => bail!("unknown shard key {other}"),
            }
        }
        let id = id.context("id=")?;
        let lo = lo.context("lo=")?;
        let hi = hi.context("hi=")?;
        ranges.push((lo, hi, id));
        endpoints.push(ShardEndpoint {
            id,
            name: name.unwrap_or_else(|| format!("s{id}")),
            gmp_url: gmp.context("gmp=")?,
            rpc_url: rpc.context("rpc=")?,
            bridge_account: bridge.unwrap_or_else(|| format!("acct{hi}")),
        });
    }
    endpoints.sort_by_key(|e| e.id);
    let router = AccountRouter::new(
        ShardAssign::AccountRanges {
            ranges: ranges.clone(),
        },
        endpoints,
    );
    Ok((router, ranges))
}

fn idem_key(body: &SubmitBody) -> String {
    if let Some(cid) = &body.client_id {
        if !cid.is_empty() {
            return format!("cid:{cid}");
        }
    }
    format!(
        "k:{}:{}:{}:{}:{}",
        body.account, body.sequence, body.destination, body.amount, body.fee
    )
}

#[derive(Clone)]
struct FacadeState {
    router: AccountRouter,
    runtimes: HashMap<usize, ShardRuntime>,
    stats: Arc<Mutex<FacadeStats>>,
    xshard_timeout_s: f64,
    xshard_seq: Arc<AtomicU64>,
    xshard_status: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    /// idempotency key → xfer_id or same-shard response
    idem: Arc<Mutex<HashMap<String, serde_json::Value>>>,
}

impl FacadeState {
    fn runtime(&self, sid: usize) -> Result<&ShardRuntime> {
        self.runtimes
            .get(&sid)
            .with_context(|| format!("no runtime for shard {sid}"))
    }

    async fn bridge_balances(&self) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        for (id, rt) in &self.runtimes {
            let acct = &rt.ep.bridge_account;
            let seq = rt.rpc.next_sequence(acct).await.ok();
            let bal = rt.rpc.account_balance(acct).await.ok();
            out.push(json!({
                "shard": id,
                "bridge": acct,
                "next_sequence": seq,
                "balance": bal,
                "rpc": rt.ep.rpc_url,
            }));
        }
        out
    }

    async fn proxy_same_shard(&self, body: SubmitBody, ikey: &str) -> serde_json::Value {
        if let Some(prev) = self.idem.lock().get(ikey).cloned() {
            self.stats.lock().xshard_idempotent_hits += 1;
            let mut p = prev;
            if let Some(o) = p.as_object_mut() {
                o.insert("idempotent".into(), json!(true));
            }
            return p;
        }
        let sid = self.router.shard_id_for_account(&body.account);
        let rt = match self.runtime(sid) {
            Ok(r) => r,
            Err(e) => {
                self.stats.lock().same_shard_err += 1;
                return json!({"error": e.to_string()});
            }
        };
        let tx = GmpTx {
            account: body.account.clone(),
            sequence: body.sequence,
            destination: body.destination.clone(),
            amount: body.amount,
            fee: body.fee,
            tx_id: String::new(),
        };
        match rt.gmp.submit(&tx).await {
            Ok(v) => {
                if v.get("error").is_some() {
                    self.stats.lock().same_shard_err += 1;
                    return v;
                }
                self.stats.lock().same_shard_ok += 1;
                let mut out = v;
                if let Some(obj) = out.as_object_mut() {
                    obj.insert("shard".into(), json!(sid));
                    obj.insert("route".into(), json!("same_shard"));
                }
                self.idem.lock().insert(ikey.to_string(), out.clone());
                out
            }
            Err(e) => {
                self.stats.lock().same_shard_err += 1;
                json!({"error": e.to_string(), "shard": sid})
            }
        }
    }

    async fn start_cross_shard(&self, body: SubmitBody, ikey: &str) -> serde_json::Value {
        if let Some(prev) = self.idem.lock().get(ikey).cloned() {
            self.stats.lock().xshard_idempotent_hits += 1;
            let mut p = prev;
            if let Some(o) = p.as_object_mut() {
                o.insert("idempotent".into(), json!(true));
            }
            return p;
        }

        let src_id = self.router.shard_id_for_account(&body.account);
        let dst_id = self.router.shard_id_for_account(&body.destination);
        let src = match self.runtime(src_id) {
            Ok(r) => r.clone(),
            Err(e) => return json!({"error": e.to_string()}),
        };
        let dst = match self.runtime(dst_id) {
            Ok(r) => r.clone(),
            Err(e) => return json!({"error": e.to_string()}),
        };

        let xfer_id = format!(
            "xs-{}-{}",
            self.xshard_seq.fetch_add(1, Ordering::Relaxed),
            (wall_now() * 1000.0) as u64
        );

        let debit = GmpTx {
            account: body.account.clone(),
            sequence: body.sequence,
            destination: src.ep.bridge_account.clone(),
            amount: body.amount,
            fee: body.fee,
            tx_id: String::new(),
        };
        let debit_res = match src.gmp.submit(&debit).await {
            Ok(v) => v,
            Err(e) => {
                return json!({"error": format!("xshard debit gmp: {e:#}"), "xfer_id": xfer_id})
            }
        };
        if debit_res.get("error").is_some() {
            return json!({
                "error": "xshard_debit_rejected",
                "detail": debit_res,
                "xfer_id": xfer_id,
            });
        }

        self.stats.lock().xshard_started += 1;
        let src_bridge = src.ep.bridge_account.clone();
        let accept = json!({
            "status": "xshard_accepted",
            "route": "cross_shard",
            "xfer_id": xfer_id,
            "source_shard": src_id,
            "dest_shard": dst_id,
            "idem_key": ikey,
            "debit": {
                "account": body.account,
                "sequence": body.sequence,
                "destination": src_bridge,
                "amount": body.amount,
            },
            "credit_pending": true,
            "poll": format!("/v1/xshard/{xfer_id}"),
        });
        self.xshard_status.lock().insert(
            xfer_id.clone(),
            json!({
                "xfer_id": xfer_id,
                "status": "debit_pending_exec",
                "source_shard": src_id,
                "dest_shard": dst_id,
                "account": body.account,
                "destination": body.destination,
                "amount": body.amount,
                "fee": body.fee,
                "debit_sequence": body.sequence,
                "debit_gmp": debit_res,
                "idem_key": ikey,
            }),
        );
        self.idem.lock().insert(ikey.to_string(), accept.clone());

        let st = self.clone();
        let xfer = xfer_id.clone();
        let account = body.account.clone();
        let dest = body.destination.clone();
        let amount = body.amount;
        let fee = body.fee;
        let debit_seq = body.sequence;
        let timeout = self.xshard_timeout_s;
        let src_c = src.clone();
        let dst_c = dst.clone();
        tokio::spawn(async move {
            if let Err(e) = st
                .complete_cross_shard(
                    &xfer, &src_c, &dst_c, &account, debit_seq, &dest, amount, fee, timeout,
                )
                .await
            {
                warn!(%xfer, "xshard complete err: {e:#}");
            }
        });

        accept
    }

    async fn complete_cross_shard(
        &self,
        xfer_id: &str,
        src: &ShardRuntime,
        dst: &ShardRuntime,
        account: &str,
        debit_seq: u64,
        final_dest: &str,
        amount: u64,
        fee: u64,
        timeout_s: f64,
    ) -> Result<()> {
        let t0 = wall_now();
        let mut debit_executed = false;
        loop {
            if wall_now() - t0 > timeout_s {
                self.stats.lock().xshard_timeout += 1;
                if let Some(v) = self.xshard_status.lock().get_mut(xfer_id) {
                    v["status"] = json!("timeout_waiting_source_exec");
                }
                // If debit never executed, nothing to refund.
                if debit_executed {
                    let _ = self.try_refund(xfer_id, src, account, amount, fee).await;
                }
                bail!("timeout waiting source exec for {account} seq {debit_seq}");
            }
            let next = src.rpc.next_sequence(account).await.unwrap_or(1);
            if next > debit_seq {
                debit_executed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        if let Some(v) = self.xshard_status.lock().get_mut(xfer_id) {
            v["status"] = json!("crediting");
            v["debit_executed"] = json!(true);
        }

        let bridge = dst.ep.bridge_account.clone();
        let mut last_err = String::new();
        for attempt in 0..10u32 {
            let seq = match dst.rpc.next_sequence(&bridge).await {
                Ok(s) => s,
                Err(e) => {
                    last_err = format!("bridge seq: {e:#}");
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    continue;
                }
            };
            let gmp_tx = GmpTx {
                account: bridge.clone(),
                sequence: seq,
                destination: final_dest.to_string(),
                amount,
                fee,
                tx_id: String::new(),
            };
            match dst.gmp.submit(&gmp_tx).await {
                Ok(v) if v.get("error").is_none() => {
                    self.stats.lock().xshard_credit_ok += 1;
                    if let Some(st) = self.xshard_status.lock().get_mut(xfer_id) {
                        st["status"] = json!("credited");
                        st["credit"] = v;
                        st["credit_sequence"] = json!(seq);
                        st["credit_attempt"] = json!(attempt);
                    }
                    return Ok(());
                }
                Ok(v) => {
                    last_err = format!("gmp: {v}");
                    if let Ok(r) = dst
                        .rpc
                        .submit_payment(&bridge, seq, final_dest, amount, fee)
                        .await
                    {
                        let code = r
                            .get("engine_result")
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        if code == "tesSUCCESS" {
                            self.stats.lock().xshard_credit_ok += 1;
                            if let Some(st) = self.xshard_status.lock().get_mut(xfer_id) {
                                st["status"] = json!("credited_rpc");
                                st["credit"] = r;
                                st["credit_sequence"] = json!(seq);
                            }
                            return Ok(());
                        }
                        last_err = format!("rpc code={code} {r}");
                    }
                }
                Err(e) => last_err = format!("gmp err: {e:#}"),
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }

        // Credit failed after debit executed → refund source user from source bridge.
        self.stats.lock().xshard_credit_err += 1;
        if let Some(v) = self.xshard_status.lock().get_mut(xfer_id) {
            v["status"] = json!("credit_failed_refunding");
            v["credit_error"] = json!(last_err);
        }
        self.try_refund(xfer_id, src, account, amount, fee).await?;
        bail!("credit failed after debit; refund attempted")
    }

    /// Source bridge returns funds to original account (lab refund).
    async fn try_refund(
        &self,
        xfer_id: &str,
        src: &ShardRuntime,
        account: &str,
        amount: u64,
        fee: u64,
    ) -> Result<()> {
        let bridge = src.ep.bridge_account.clone();
        let mut last_err = String::new();
        for attempt in 0..10u32 {
            let seq = match src.rpc.next_sequence(&bridge).await {
                Ok(s) => s,
                Err(e) => {
                    last_err = e.to_string();
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    continue;
                }
            };
            let gmp_tx = GmpTx {
                account: bridge.clone(),
                sequence: seq,
                destination: account.to_string(),
                amount,
                fee,
                tx_id: String::new(),
            };
            match src.gmp.submit(&gmp_tx).await {
                Ok(v) if v.get("error").is_none() => {
                    self.stats.lock().xshard_refund_ok += 1;
                    if let Some(st) = self.xshard_status.lock().get_mut(xfer_id) {
                        st["status"] = json!("refunded");
                        st["refund"] = v;
                        st["refund_sequence"] = json!(seq);
                        st["refund_attempt"] = json!(attempt);
                    }
                    return Ok(());
                }
                Ok(v) => {
                    last_err = format!("gmp: {v}");
                    if let Ok(r) = src
                        .rpc
                        .submit_payment(&bridge, seq, account, amount, fee)
                        .await
                    {
                        if r.get("engine_result").and_then(|x| x.as_str()) == Some("tesSUCCESS") {
                            self.stats.lock().xshard_refund_ok += 1;
                            if let Some(st) = self.xshard_status.lock().get_mut(xfer_id) {
                                st["status"] = json!("refunded_rpc");
                                st["refund"] = r;
                            }
                            return Ok(());
                        }
                        last_err = format!("rpc: {r}");
                    }
                }
                Err(e) => last_err = e.to_string(),
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        self.stats.lock().xshard_refund_err += 1;
        if let Some(st) = self.xshard_status.lock().get_mut(xfer_id) {
            st["status"] = json!("refund_failed");
            st["refund_error"] = json!(last_err);
        }
        bail!("refund failed: {last_err}")
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();
    let args = Args::parse();
    let (router, ranges) = parse_shards(&args.shards)?;
    let mut runtimes = HashMap::new();
    for ep in &router.shards {
        let gmp = GmpClient::parse(&ep.gmp_url)?;
        let rpc = NodeRpcClient::parse(&ep.rpc_url)?;
        info!(
            shard = ep.id,
            gmp = %ep.gmp_url,
            rpc = %ep.rpc_url,
            bridge = %ep.bridge_account,
            "shard endpoint"
        );
        runtimes.insert(
            ep.id,
            ShardRuntime {
                ep: ep.clone(),
                gmp,
                rpc,
            },
        );
    }

    let state = Arc::new(FacadeState {
        router,
        runtimes,
        stats: Arc::new(Mutex::new(FacadeStats::default())),
        xshard_timeout_s: args.xshard_timeout_s,
        xshard_seq: Arc::new(AtomicU64::new(1)),
        xshard_status: Arc::new(Mutex::new(HashMap::new())),
        idem: Arc::new(Mutex::new(HashMap::new())),
    });

    info!(
        listen = %args.listen,
        ranges = ?ranges,
        "falcon-gmp-facade multi-shard + cross-shard + refunds"
    );

    let st = state.clone();
    let handler: Arc<HandlerFn> = Arc::new(move |method, path, body| {
        let st = st.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async move { dispatch(st.as_ref(), &method, &path, body).await })
        })
    });

    serve(&args.listen, handler).await?;
    Ok(())
}

async fn dispatch(
    st: &FacadeState,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> serde_json::Value {
    let path = path.split('?').next().unwrap_or(path);
    match (method, path) {
        ("GET", "/v1/status") | ("GET", "/status") => {
            let stats = st.stats.lock().clone();
            let mut shards = Vec::new();
            for (id, rt) in &st.runtimes {
                let gstat = rt
                    .gmp
                    .status()
                    .await
                    .unwrap_or_else(|_| json!({"error": "unreachable"}));
                shards.push(json!({
                    "id": id,
                    "name": rt.ep.name,
                    "gmp": rt.ep.gmp_url,
                    "rpc": rt.ep.rpc_url,
                    "bridge": rt.ep.bridge_account,
                    "gmp_status": gstat,
                }));
            }
            let bridges = st.bridge_balances().await;
            json!({
                "service": "falcon-gmp-facade",
                "stats": {
                    "same_shard_ok": stats.same_shard_ok,
                    "same_shard_err": stats.same_shard_err,
                    "xshard_started": stats.xshard_started,
                    "xshard_credit_ok": stats.xshard_credit_ok,
                    "xshard_credit_err": stats.xshard_credit_err,
                    "xshard_timeout": stats.xshard_timeout,
                    "xshard_refund_ok": stats.xshard_refund_ok,
                    "xshard_refund_err": stats.xshard_refund_err,
                    "xshard_idempotent_hits": stats.xshard_idempotent_hits,
                },
                "bridges": bridges,
                "shards": shards,
            })
        }
        ("GET", "/v1/bridges") => json!({"bridges": st.bridge_balances().await}),
        ("POST", "/v1/route") => {
            let account = body
                .as_ref()
                .and_then(|b| b.get("account"))
                .and_then(|a| a.as_str())
                .unwrap_or("");
            if account.is_empty() {
                return json!({"error": "account required"});
            }
            let sid = st.router.shard_id_for_account(account);
            json!({
                "account": account,
                "shard_id": sid,
                "endpoint": st.router.endpoint(sid),
            })
        }
        ("GET", p) if p.starts_with("/v1/xshard/") => {
            let id = p.trim_start_matches("/v1/xshard/");
            st.xshard_status
                .lock()
                .get(id)
                .cloned()
                .unwrap_or_else(|| json!({"error": "unknown_xfer", "xfer_id": id}))
        }
        ("POST", "/v1/submit") | ("POST", "/submit") => {
            let body = match body {
                Some(b) => b,
                None => return json!({"error": "empty body"}),
            };
            let sub: SubmitBody = match serde_json::from_value(body) {
                Ok(s) => s,
                Err(e) => return json!({"error": format!("bad submit: {e}")}),
            };
            if sub.account.is_empty() || sub.destination.is_empty() || sub.sequence == 0 {
                return json!({"error": "account, destination, sequence required"});
            }
            let ikey = idem_key(&sub);
            if st.router.is_cross_shard(&sub.account, &sub.destination) {
                st.start_cross_shard(sub, &ikey).await
            } else {
                st.proxy_same_shard(sub, &ikey).await
            }
        }
        _ => json!({"error": "not_found", "path": path}),
    }
}
