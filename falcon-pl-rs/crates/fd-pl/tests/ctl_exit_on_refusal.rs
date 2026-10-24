//! #58: `falcon-pl-ctl` must exit non-zero when the node refuses a request,
//! and still print the reply on stdout.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;

/// One-shot fake node: read one request line, answer with `reply`.
fn fake_node(reply: &'static str) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        if let Ok((s, _)) = l.accept() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            let _ = r.read_line(&mut line);
            let mut w = s;
            let _ = w.write_all(reply.as_bytes());
            let _ = w.write_all(b"\n");
            let _ = w.flush();
        }
    });
    addr
}

fn ctl_tick(addr: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_falcon-pl-ctl"))
        .args(["--addr", addr, "tick", "--dt-ms", "1"])
        .output()
        .expect("run falcon-pl-ctl")
}

#[test]
fn refused_reply_exits_3_and_prints_reply() {
    let addr = fake_node(r#"{"type":"err","msg":"refused: test"}"#);
    let out = ctl_tick(&addr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(3), "stdout={stdout}");
    assert!(stdout.contains("refused: test"), "stdout={stdout}");
    assert!(stdout.contains("\"err\""), "stdout={stdout}");
}

#[test]
fn ok_reply_exits_0() {
    let addr = fake_node(r#"{"type":"ok","msg":"ticked"}"#);
    let out = ctl_tick(&addr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "stdout={stdout}");
    assert!(stdout.contains("ticked"), "stdout={stdout}");
}
