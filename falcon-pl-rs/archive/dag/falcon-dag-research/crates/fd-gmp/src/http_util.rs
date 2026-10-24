//! Minimal HTTP/1.1 JSON request handler (no external HTTP framework).

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub type HandlerFn =
    dyn Fn(String, String, Option<serde_json::Value>) -> serde_json::Value + Send + Sync + 'static;

pub async fn serve(addr: &str, handler: std::sync::Arc<HandlerFn>) -> Result<()> {
    let listener = TcpListener::bind(addr).await.context("bind")?;
    tracing::info!(%addr, "http listen");
    loop {
        let (sock, _) = listener.accept().await?;
        let h = handler.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(sock, h).await {
                tracing::debug!("conn err: {e:#}");
            }
        });
    }
}

async fn handle_conn(mut sock: TcpStream, handler: std::sync::Arc<HandlerFn>) -> Result<()> {
    // Read until header terminator, then finish body via Content-Length.
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let header_end;
    loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(());
            }
            bail!("eof before headers complete");
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            header_end = pos;
            break;
        }
        if buf.len() > 1024 * 1024 {
            bail!("headers too large");
        }
    }

    let head = std::str::from_utf8(&buf[..header_end]).context("headers utf8")?;
    let (method, path, content_length) = parse_headers(head)?;

    let mut body_bytes = buf[header_end + 4..].to_vec(); // skip \r\n\r\n
    while body_bytes.len() < content_length {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            bail!(
                "eof before body complete have={} need={}",
                body_bytes.len(),
                content_length
            );
        }
        body_bytes.extend_from_slice(&tmp[..n]);
        if body_bytes.len() > 8 * 1024 * 1024 {
            bail!("body too large");
        }
    }
    body_bytes.truncate(content_length);

    let json_body = if body_bytes.is_empty() {
        None
    } else {
        let s = std::str::from_utf8(&body_bytes).unwrap_or("");
        Some(serde_json::from_str(s).unwrap_or(serde_json::json!({})))
    };
    let resp = handler(method, path, json_body);
    let body = serde_json::to_string(&resp)?;
    let out = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    sock.write_all(out.as_bytes()).await?;
    Ok(())
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_headers(head: &str) -> Result<(String, String, usize)> {
    let mut lines = head.lines();
    let start = lines.next().unwrap_or("");
    let mut sp = start.split_whitespace();
    let method = sp.next().unwrap_or("GET").to_string();
    let path = sp.next().unwrap_or("/").to_string();
    if method.is_empty() {
        bail!("bad request");
    }
    let mut content_length = 0usize;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }
    Ok((method, path, content_length))
}
