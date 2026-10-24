//! Helpers for `falcon-pl-ctl web`: bind policy, request parsing, and the
//! in-memory history the local server samples. The page itself is
//! `health_web.html`, embedded with `include_str!` (no CDN, no external fetch).

use serde_json::{json, Value};
use std::collections::VecDeque;
use std::net::SocketAddr;

pub const INDEX_HTML: &str = include_str!("health_web.html");
/// Default: one sample every 10 s, 24 h kept.
pub const SAMPLE_SECS: u64 = 10;
pub const HISTORY_CAP: usize = 8640;

/// Loopback-only unless the operator passed `--allow-remote`.
pub fn check_bind(listen: &str, allow_remote: bool) -> Result<SocketAddr, String> {
    let a: SocketAddr = listen
        .parse()
        .map_err(|e| format!("--listen {listen}: {e} (use IP:PORT, e.g. 127.0.0.1:19480)"))?;
    if !a.ip().is_loopback() && !allow_remote {
        return Err(format!(
            "refusing to bind {a}: not loopback. Use 127.0.0.1 and `ssh -L 19480:127.0.0.1:19480`, or pass --allow-remote"
        ));
    }
    Ok(a)
}

/// DNS-rebinding guard: in loopback mode only accept a loopback Host header.
pub fn host_ok(host: Option<&str>, allow_remote: bool) -> bool {
    if allow_remote {
        return true;
    }
    let Some(h) = host else { return false };
    let h = h.trim();
    let name = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        h.rsplit_once(':').map(|(n, p)| if p.chars().all(|c| c.is_ascii_digit()) { n } else { h }).unwrap_or(h)
    };
    name.eq_ignore_ascii_case("localhost")
        || name.parse::<std::net::IpAddr>().map(|i| i.is_loopback()).unwrap_or(false)
}

#[derive(Debug, PartialEq)]
pub struct Req {
    pub method: String,
    pub path: String,
    pub host: Option<String>,
}

/// Parse the request head (everything before the blank line).
pub fn parse_head(head: &str) -> Option<Req> {
    let mut lines = head.split("\r\n").flat_map(|l| l.split('\n'));
    let first = lines.next()?;
    let mut p = first.split_whitespace();
    let method = p.next()?.to_string();
    let target = p.next()?;
    if !p.next()?.starts_with("HTTP/1.") {
        return None;
    }
    let path = target.split('?').next().unwrap_or("/").to_string();
    let mut host = None;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("host") {
                host = Some(v.trim().to_string());
            }
        }
    }
    Some(Req { method, path, host })
}

pub fn response(status: &str, ctype: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// One history point taken from a `falcon.health.v1` body.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub t_ms: u64,
    pub height: u64,
    pub tip: u64,
    pub lag: u64,
    pub peers: u64,
    pub rss: u64,
    pub cpu: f64,
    pub epoch: u64,
    pub proposals: u64,
    pub votes: u64,
}

impl Sample {
    pub fn from_health(t_ms: u64, h: &Value) -> Self {
        let u = |p: &str| h.pointer(p).and_then(|v| v.as_u64()).unwrap_or(0);
        Sample {
            t_ms,
            height: u("/chain/local_height"),
            tip: u("/chain/network_tip"),
            lag: u("/chain/lag"),
            peers: u("/peers/count"),
            rss: u("/system/process/rss_bytes"),
            cpu: h.pointer("/system/process/cpu_pct").and_then(|v| v.as_f64()).unwrap_or(0.0),
            epoch: u("/consensus/epoch"),
            proposals: u("/consensus/proposals_this_epoch"),
            votes: u("/consensus/votes_this_epoch"),
        }
    }
}

pub struct History {
    pub cap: usize,
    pub s: VecDeque<Sample>,
}

impl History {
    pub fn new(cap: usize) -> Self {
        History { cap: cap.max(1), s: VecDeque::new() }
    }
    pub fn push(&mut self, x: Sample) {
        if self.s.len() >= self.cap {
            self.s.pop_front();
        }
        self.s.push_back(x);
    }
    /// Column arrays (compact for the page).
    pub fn to_json(&self, sample_secs: u64) -> Value {
        let col = |f: &dyn Fn(&Sample) -> Value| Value::Array(self.s.iter().map(f).collect());
        json!({
            "schema": "falcon.health.history.v1",
            "sample_secs": sample_secs,
            "cap": self.cap,
            "t_ms": col(&|x| x.t_ms.into()),
            "height": col(&|x| x.height.into()),
            "tip": col(&|x| x.tip.into()),
            "lag": col(&|x| x.lag.into()),
            "peers": col(&|x| x.peers.into()),
            "rss_bytes": col(&|x| x.rss.into()),
            "cpu_pct": col(&|x| x.cpu.into()),
            "epoch": col(&|x| x.epoch.into()),
            "proposals_this_epoch": col(&|x| x.proposals.into()),
            "votes_this_epoch": col(&|x| x.votes.into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_policy() {
        assert!(check_bind("127.0.0.1:19480", false).is_ok());
        assert!(check_bind("[::1]:19480", false).is_ok());
        let e = check_bind("0.0.0.0:19480", false).unwrap_err();
        assert!(e.contains("--allow-remote") && e.contains("ssh -L"));
        assert!(check_bind("100.64.1.2:19480", false).is_err());
        assert!(check_bind("0.0.0.0:19480", true).is_ok());
        assert!(check_bind("localhost:19480", false).is_err(), "names not resolved");
    }

    #[test]
    fn host_guard() {
        assert!(host_ok(Some("127.0.0.1:19480"), false));
        assert!(host_ok(Some("localhost:19480"), false));
        assert!(host_ok(Some("[::1]:19480"), false));
        assert!(host_ok(Some("localhost"), false));
        assert!(!host_ok(Some("evil.example:19480"), false));
        assert!(!host_ok(Some("localhost.evil.example"), false));
        assert!(!host_ok(None, false));
        assert!(host_ok(Some("node.tailnet.ts.net"), true));
    }

    #[test]
    fn head_parse() {
        let r = parse_head("GET /health/v1?x=1 HTTP/1.1\r\nHost: 127.0.0.1:19480\r\nAccept: */*").unwrap();
        assert_eq!(r, Req { method: "GET".into(), path: "/health/v1".into(), host: Some("127.0.0.1:19480".into()) });
        assert!(parse_head("garbage").is_none());
        assert!(parse_head("GET / SPDY/3").is_none());
    }

    #[test]
    fn history_ring_and_json() {
        let mut h = History::new(3);
        for i in 0..5u64 {
            let body = json!({"chain": {"local_height": 100 + i, "network_tip": 102 + i, "lag": 2},
                "peers": {"count": 4}, "system": {"process": {"rss_bytes": 1000, "cpu_pct": 1.5}},
                "consensus": {"epoch": 3, "proposals_this_epoch": i, "votes_this_epoch": 10 * i}});
            h.push(Sample::from_health(i * 1000, &body));
        }
        assert_eq!(h.s.len(), 3);
        let j = h.to_json(10);
        assert_eq!(j["height"], json!([102, 103, 104]));
        assert_eq!(j["lag"], json!([2, 2, 2]));
        assert_eq!(j["votes_this_epoch"], json!([20, 30, 40]));
        assert_eq!(j["cpu_pct"][0], 1.5);
        // Sparse body never panics.
        assert_eq!(Sample::from_health(1, &json!({})).height, 0);
    }

    #[test]
    fn page_is_self_contained() {
        assert!(INDEX_HTML.contains("/health/v1") && INDEX_HTML.contains("/history/v1"));
        for bad in ["http://", "https://", "//cdn", "<link", "@import"] {
            assert!(!INDEX_HTML.contains(bad), "external ref {bad}");
        }
        let r = response("200 OK", "text/html", b"hi");
        let s = String::from_utf8(r).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n") && s.contains("Content-Length: 2\r\n") && s.ends_with("\r\n\r\nhi"));
        assert!(s.contains("default-src 'self'"));
    }
}
