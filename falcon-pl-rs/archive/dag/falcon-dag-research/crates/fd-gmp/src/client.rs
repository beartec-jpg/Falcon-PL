//! Async HTTP client for falcon-gmpd / falcon-gcpd / node JSON-RPC (no external HTTP crate).

use crate::{CapacityAdvert, GmpTx};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::net::ToSocketAddrs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Clone, Debug)]
pub struct GmpClient {
    host: String,
    port: u16,
}

impl GmpClient {
    /// Parse `http://host:port` or `host:port`.
    pub fn parse(url: &str) -> Result<Self> {
        let s = url
            .trim()
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/');
        let (host, port_s) = if let Some((h, p)) = s.rsplit_once(':') {
            (h.to_string(), p)
        } else {
            (s.to_string(), "18500")
        };
        let port: u16 = port_s.parse().context("gmp port")?;
        if host.is_empty() {
            bail!("empty gmp host");
        }
        Ok(Self { host, port })
    }

    pub fn base(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    async fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        let addr = format!("{}:{}", self.host, self.port);
        let mut addrs = addr
            .to_socket_addrs()
            .with_context(|| format!("resolve {addr}"))?;
        let sock_addr = addrs.next().context("no addr")?;
        let mut stream = TcpStream::connect(sock_addr)
            .await
            .with_context(|| format!("connect {addr}"))?;
        let payload = serde_json::to_vec(body)?;
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.host,
            payload.len()
        );
        stream.write_all(req.as_bytes()).await?;
        stream.write_all(&payload).await?;
        stream.flush().await?;

        let mut buf = Vec::with_capacity(8192);
        let mut tmp = [0u8; 4096];
        loop {
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.len() > 4 * 1024 * 1024 {
                bail!("response too large");
            }
        }
        let text = String::from_utf8_lossy(&buf);
        let body = text
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or("")
            .trim();
        if body.is_empty() {
            bail!("empty response body");
        }
        Ok(serde_json::from_str(body).context("parse response json")?)
    }

    async fn get_json(&self, path: &str) -> Result<Value> {
        let addr = format!("{}:{}", self.host, self.port);
        let mut addrs = addr.to_socket_addrs()?;
        let sock_addr = addrs.next().context("no addr")?;
        let mut stream = TcpStream::connect(sock_addr).await?;
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.host
        );
        stream.write_all(req.as_bytes()).await?;
        stream.flush().await?;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let text = String::from_utf8_lossy(&buf);
        let body = text.split("\r\n\r\n").nth(1).unwrap_or("").trim();
        Ok(serde_json::from_str(body).unwrap_or(serde_json::json!({})))
    }

    pub async fn status(&self) -> Result<Value> {
        self.get_json("/v1/status").await
    }

    pub async fn submit(&self, tx: &GmpTx) -> Result<Value> {
        self.post_json("/v1/submit", &serde_json::to_value(tx)?).await
    }

    pub async fn capacity(&self, adv: &CapacityAdvert) -> Result<Value> {
        self.post_json("/v1/capacity", &serde_json::to_value(adv)?).await
    }

    pub async fn lease(
        &self,
        validator_id: &str,
        max_n: usize,
        affinity_mod: Option<usize>,
        affinity_idx: Option<usize>,
    ) -> Result<Value> {
        let mut body = serde_json::json!({
            "validator_id": validator_id,
            "max_n": max_n,
        });
        if let (Some(m), Some(i)) = (affinity_mod, affinity_idx) {
            body["affinity_mod"] = serde_json::json!(m);
            body["affinity_idx"] = serde_json::json!(i);
        }
        self.post_json("/v1/lease", &body).await
    }

    pub async fn complete(&self, lease_id: &str) -> Result<Value> {
        self.post_json(
            "/v1/complete",
            &serde_json::json!({ "lease_id": lease_id }),
        )
        .await
    }

    pub async fn nack(&self, lease_id: &str, reason: &str) -> Result<Value> {
        self.post_json(
            "/v1/nack",
            &serde_json::json!({ "lease_id": lease_id, "reason": reason }),
        )
        .await
    }
}

#[derive(Clone, Debug)]
pub struct GcpClient {
    inner: GmpClient,
}

impl GcpClient {
    pub fn parse(url: &str) -> Result<Self> {
        Ok(Self {
            inner: GmpClient::parse(url)?,
        })
    }

    pub async fn put_batch(
        &self,
        batch_hash: &str,
        creator: &str,
        ntx: u64,
        parents: &[String],
    ) -> Result<Value> {
        self.inner
            .post_json(
                "/v1/batch",
                &serde_json::json!({
                    "batch_hash": batch_hash,
                    "creator": creator,
                    "ntx": ntx,
                    "parents": parents,
                    "ts": crate::wall_now(),
                }),
            )
            .await
    }

    pub async fn put_sig(
        &self,
        batch_hash: &str,
        validator_id: &str,
        signature: &str,
    ) -> Result<Value> {
        self.inner
            .post_json(
                "/v1/sig",
                &serde_json::json!({
                    "batch_hash": batch_hash,
                    "validator_id": validator_id,
                    "signature": signature,
                }),
            )
            .await
    }

    pub async fn put_cert(&self, batch_hash: &str) -> Result<Value> {
        self.inner
            .post_json(
                "/v1/cert",
                &serde_json::json!({ "batch_hash": batch_hash }),
            )
            .await
    }

    pub async fn status(&self) -> Result<Value> {
        self.inner.get_json("/v1/status").await
    }

    pub async fn list_open(&self) -> Result<Value> {
        self.inner.get_json("/v1/open").await
    }
}

/// Minimal JSON-RPC client for falcon-dagd (account_info / submit).
#[derive(Clone, Debug)]
pub struct NodeRpcClient {
    inner: GmpClient,
}

impl NodeRpcClient {
    pub fn parse(url: &str) -> Result<Self> {
        Ok(Self {
            inner: GmpClient::parse(url)?,
        })
    }

    pub fn base(&self) -> String {
        self.inner.base()
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": [params],
        });
        let resp = self.inner.post_json("/", &body).await?;
        if let Some(err) = resp.get("error") {
            bail!("rpc error: {err}");
        }
        Ok(resp.get("result").cloned().unwrap_or(serde_json::json!({})))
    }

    /// Next sequence to use for `account` (engine account_info.sequence).
    pub async fn next_sequence(&self, account: &str) -> Result<u64> {
        let r = self
            .call("account_info", serde_json::json!({ "account": account }))
            .await?;
        // result may be nested under result or flat
        let seq = r
            .get("sequence")
            .or_else(|| r.pointer("/result/sequence"))
            .and_then(|v| v.as_u64())
            .unwrap_or(1);
        Ok(seq.max(1))
    }

    /// Best-effort native balance from account_info (lab monitoring).
    pub async fn account_balance(&self, account: &str) -> Result<u64> {
        let r = self
            .call("account_info", serde_json::json!({ "account": account }))
            .await?;
        if let Some(b) = r.get("balance").and_then(|v| v.as_u64()) {
            return Ok(b);
        }
        if let Some(b) = r
            .get("balances")
            .and_then(|m| m.get("FDAG").or_else(|| m.get("fdag")))
            .and_then(|v| v.as_u64())
        {
            return Ok(b);
        }
        if let Some(b) = r
            .pointer("/balances/FDAG")
            .or_else(|| r.pointer("/result/balance"))
            .and_then(|v| v.as_u64())
        {
            return Ok(b);
        }
        Ok(0)
    }

    pub async fn submit_payment(
        &self,
        account: &str,
        sequence: u64,
        destination: &str,
        amount: u64,
        fee: u64,
    ) -> Result<Value> {
        self.call(
            "submit",
            serde_json::json!({
                "account": account,
                "sequence": sequence,
                "destination": destination,
                "amount": amount,
                "fee": fee,
            }),
        )
        .await
    }
}
