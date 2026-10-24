//! `falcon-pl-ctl web` against a fake node: page, health proxy (body
//! unchanged), history sampling, Host guard, read-only, loopback-only bind.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BODY: &str = r#"{"schema":"falcon.health.v1","node":{"id":"seat9","status":"active"},"chain":{"local_height":42,"network_tip":44,"lag":2},"peers":{"count":3},"system":{"process":{"rss_bytes":1000,"cpu_pct":2.5}},"consensus":{"epoch":1,"proposals_this_epoch":5,"votes_this_epoch":9},"warnings":[]}"#;

/// Fake node answering every connection with a Health reply.
fn fake_node() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut rd = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                let _ = rd.read_line(&mut line);
                let mut w = s;
                let reply = if line.contains("health_req") {
                    format!(r#"{{"type":"health","body":{BODY}}}"#)
                } else {
                    r#"{"type":"err","msg":"unexpected"}"#.to_string()
                };
                let _ = w.write_all(reply.as_bytes());
                let _ = w.write_all(b"\n");
            });
        }
    });
    addr
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(node: &str, listen: &str) -> Kill {
    let c = Command::new(env!("CARGO_BIN_EXE_falcon-pl-ctl"))
        .args(["--addr", node, "web", "--listen", listen, "--sample-secs", "1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while TcpStream::connect(listen).is_err() {
        assert!(t0.elapsed() < Duration::from_secs(10), "web did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    Kill(c)
}

fn http(addr: &str, req: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let code = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = out.split_once("\r\n\r\n").map(|x| x.1.to_string()).unwrap_or_default();
    (code, body)
}

fn get(addr: &str, path: &str) -> (u16, String) {
    http(addr, &format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n"))
}

#[test]
fn web_serves_page_health_history() {
    let node = fake_node();
    let listen = format!("127.0.0.1:{}", free_port());
    let _w = start(&node, &listen);

    let (c, page) = get(&listen, "/");
    assert_eq!(c, 200);
    assert!(page.contains("Falcon") && page.contains("/health/v1"));
    // 2.9.79 Balance card (reads the health `balance` block only).
    for id in ["id=\"bal\"", "b-liq", "b-bond", "b-rwd", "b-unb", "pending_rewards", "Liquid (withdrawable)"] {
        assert!(page.contains(id), "page missing {id}");
    }

    let (c, h) = get(&listen, "/health/v1");
    assert_eq!(c, 200);
    let got: serde_json::Value = serde_json::from_str(&h).unwrap();
    let want: serde_json::Value = serde_json::from_str(BODY).unwrap();
    assert_eq!(got, want, "health body proxied unchanged");

    let t0 = Instant::now();
    loop {
        let (c, j) = get(&listen, "/history/v1");
        assert_eq!(c, 200);
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        if v["height"].as_array().map(|a| a.len() >= 2).unwrap_or(false) {
            assert_eq!(v["height"][0], 42);
            assert_eq!(v["lag"][0], 2);
            assert_eq!(v["votes_this_epoch"][0], 9);
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "no history samples: {j}");
        std::thread::sleep(Duration::from_millis(300));
    }

    assert_eq!(get(&listen, "/nope").0, 404);
    let (c, _) = http(&listen, "GET / HTTP/1.1\r\nHost: evil.example\r\n\r\n");
    assert_eq!(c, 421, "DNS-rebinding Host refused");
    let (c, _) = http(&listen, &format!("POST /health/v1 HTTP/1.1\r\nHost: {listen}\r\nContent-Length: 0\r\n\r\n"));
    assert_eq!(c, 405);
}

#[test]
fn web_health_502_when_node_down() {
    let dead = format!("127.0.0.1:{}", free_port());
    let listen = format!("127.0.0.1:{}", free_port());
    let _w = start(&dead, &listen);
    let (c, b) = get(&listen, "/health/v1");
    assert_eq!(c, 502);
    assert!(b.contains("error"));
    assert_eq!(get(&listen, "/").0, 200, "page still served");
}

#[test]
fn web_refuses_non_loopback_without_flag() {
    let o = Command::new(env!("CARGO_BIN_EXE_falcon-pl-ctl"))
        .args(["web", "--listen", "0.0.0.0:0"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("--allow-remote"), "{err}");
}
