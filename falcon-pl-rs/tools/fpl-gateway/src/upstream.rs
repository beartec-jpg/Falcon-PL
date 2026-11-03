//! Node wire RPC client. Mirrors `rpc_on_split` in fd-pl/src/bin/falcon_pl_ctl.rs:
//! connect, write one JSON line, read lines until a directed reply, skipping
//! mesh gossip. We never send `Hello`, so the node treats us as an RPC session
//! (no piece framing, admin messages rejected on the public mesh port).

use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Reply types that end an RPC exchange.
pub const REPLY_TYPES: &[&str] = &["status", "health", "account", "order_book", "ok", "err"];

#[derive(Debug)]
pub enum UpstreamErr {
    Connect(String),
    Timeout,
    TooLarge,
    Protocol(String),
}

impl std::fmt::Display for UpstreamErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpstreamErr::Connect(e) => write!(f, "upstream connect: {e}"),
            UpstreamErr::Timeout => write!(f, "upstream timeout"),
            UpstreamErr::TooLarge => write!(f, "upstream reply too large"),
            UpstreamErr::Protocol(e) => write!(f, "upstream protocol: {e}"),
        }
    }
}

pub async fn call(addr: &str, msg: &Value, timeout: Duration, max_reply: usize) -> Result<Value, UpstreamErr> {
    tokio::time::timeout(timeout, call_inner(addr, msg, max_reply)).await.map_err(|_| UpstreamErr::Timeout)?
}

async fn call_inner(addr: &str, msg: &Value, max_reply: usize) -> Result<Value, UpstreamErr> {
    let stream = TcpStream::connect(addr).await.map_err(|e| UpstreamErr::Connect(e.kind().to_string()))?;
    let (r, mut w) = stream.into_split();
    let line = serde_json::to_string(msg).map_err(|e| UpstreamErr::Protocol(e.to_string()))? + "\n";
    w.write_all(line.as_bytes()).await.map_err(|e| UpstreamErr::Connect(e.kind().to_string()))?;
    let mut rd = BufReader::new(r);
    let mut buf = Vec::new();
    for _ in 0..64 {
        buf.clear();
        let n = (&mut rd).take(max_reply as u64 + 1).read_until(b'\n', &mut buf).await.map_err(|e| UpstreamErr::Protocol(e.kind().to_string()))?;
        if n == 0 {
            return Err(UpstreamErr::Protocol("no response".into()));
        }
        if n > max_reply {
            return Err(UpstreamErr::TooLarge);
        }
        let v: Value = serde_json::from_slice(&buf).map_err(|e| UpstreamErr::Protocol(e.to_string()))?;
        let t = v.get("type").and_then(Value::as_str).unwrap_or("");
        if REPLY_TYPES.contains(&t) {
            return Ok(v);
        }
        // gossip (hello, propose_*, ledger_ok, pong, ...) - skip
    }
    Err(UpstreamErr::Protocol("no directed reply in 64 lines".into()))
}

use tokio::io::AsyncReadExt as _;
