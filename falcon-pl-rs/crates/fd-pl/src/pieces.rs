// Copyright (c) 2025-2026 BEARTEC. All rights reserved. See LICENSE.
//! v2.10.3: chunked ("piece") transport for large wire lines.
//!
//! A JSON line over [`PIECE_THRESHOLD`] bytes (ledger bodies, LedgersBatch,
//! TxsBatch replies to NeedTxs / NeedMempoolTxs, ...) is sent as a run of
//! `WireMsg::Piece` lines, each at most [`PIECE_MAX_LINE`] bytes, in order,
//! on the same queue. The receiver joins them and handles the original line
//! exactly as if it had arrived whole. Only used towards peers whose Hello
//! advertised `release_version >= 2.10.3` ([`peer_release_supports_pieces`]);
//! 2.10.2 peers keep getting the legacy single line. Transport only: no
//! consensus, hash or state effect.

use std::collections::BTreeMap;

use crate::WireMsg;

/// Lines larger than this are pieced (same as the node's large-line gate).
pub const PIECE_THRESHOLD: usize = 64 * 1024;
/// Raw bytes of the original line per piece. JSON string escaping at most
/// doubles it, so a serialized piece line stays under [`PIECE_MAX_LINE`].
pub const PIECE_RAW: usize = 24_000;
/// Upper bound for one serialized piece line (incl. newline).
pub const PIECE_MAX_LINE: usize = 48 * 1024;
/// Reassembly limits per connection.
pub const PIECE_MAX_COUNT: u32 = 2048;
pub const PIECE_MAX_TOTAL: usize = 48 * 1024 * 1024;
pub const PIECE_MAX_STREAMS: usize = 8;
pub const PIECE_TIMEOUT_MS: u64 = 30_000;
/// First release that understands `WireMsg::Piece`.
pub const PIECE_MIN_RELEASE: (u64, u64, u64) = (2, 10, 3);

/// Does a peer's Hello `release_version` ("" = pre-2.10.3) accept pieces?
pub fn peer_release_supports_pieces(release_version: &str) -> bool {
    crate::parse_release_pub(release_version).map_or(false, |v| v >= PIECE_MIN_RELEASE)
}

/// Split one wire line (with or without trailing newline) into piece lines
/// (each ending in '\n'). Returns `None` when the line is small enough to
/// go as is.
pub fn split_line(line: &str, pid: u64) -> Option<Vec<String>> {
    let body = line.strip_suffix('\n').unwrap_or(line);
    if body.len() + 1 <= PIECE_THRESHOLD {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    let mut start = 0;
    while start < body.len() {
        let mut end = (start + PIECE_RAW).min(body.len());
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        parts.push(&body[start..end]);
        start = end;
    }
    let n = parts.len() as u32;
    if n > PIECE_MAX_COUNT {
        return None;
    }
    let mut out = Vec::with_capacity(parts.len());
    for (i, d) in parts.into_iter().enumerate() {
        let m = WireMsg::Piece {
            pid,
            i: i as u32,
            n,
            d: d.to_string(),
        };
        out.push(serde_json::to_string(&m).ok()? + "\n");
    }
    Some(out)
}

struct Stream {
    n: u32,
    next: u32,
    buf: String,
    started_ms: u64,
}

/// Per-connection reassembler. Pieces of one `pid` must arrive in order.
#[derive(Default)]
pub struct PieceAssembler {
    streams: BTreeMap<u64, Stream>,
    pub dropped: u64,
}

impl PieceAssembler {
    fn expire(&mut self, now_ms: u64) {
        let before = self.streams.len();
        self.streams
            .retain(|_, s| now_ms.saturating_sub(s.started_ms) < PIECE_TIMEOUT_MS);
        self.dropped += (before - self.streams.len()) as u64;
    }

    /// Feed one piece. `Ok(Some(line))` = the original line is complete.
    pub fn feed(&mut self, now_ms: u64, pid: u64, i: u32, n: u32, d: &str) -> Result<Option<String>, &'static str> {
        self.expire(now_ms);
        if n == 0 || n > PIECE_MAX_COUNT || i >= n {
            return Err("piece: bad index/count");
        }
        if i == 0 {
            if self.streams.remove(&pid).is_some() {
                self.dropped += 1;
            }
            if self.streams.len() >= PIECE_MAX_STREAMS {
                // Drop the oldest stream.
                if let Some((&old, _)) = self.streams.iter().min_by_key(|(_, s)| s.started_ms) {
                    self.streams.remove(&old);
                    self.dropped += 1;
                }
            }
            self.streams.insert(pid, Stream { n, next: 0, buf: String::new(), started_ms: now_ms });
        }
        let Some(s) = self.streams.get_mut(&pid) else {
            return Err("piece: unknown stream");
        };
        if s.n != n || s.next != i {
            self.streams.remove(&pid);
            self.dropped += 1;
            return Err("piece: out of order");
        }
        if s.buf.len() + d.len() > PIECE_MAX_TOTAL {
            self.streams.remove(&pid);
            self.dropped += 1;
            return Err("piece: too large");
        }
        s.buf.push_str(d);
        s.next += 1;
        if s.next == s.n {
            let s = self.streams.remove(&pid).unwrap();
            return Ok(Some(s.buf));
        }
        Ok(None)
    }

    pub fn open_streams(&self) -> usize {
        self.streams.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_line(n_txs: usize) -> String {
        let m = WireMsg::NeedMempoolTxs {
            ids: (0..n_txs).map(|i| format!("tx-\"{i}\"-{}", "é".repeat(20))).collect(),
        };
        serde_json::to_string(&m).unwrap() + "\n"
    }

    fn reassemble(lines: &[String]) -> String {
        let mut a = PieceAssembler::default();
        let mut out = None;
        for l in lines {
            match serde_json::from_str::<WireMsg>(l.trim()).unwrap() {
                WireMsg::Piece { pid, i, n, d } => {
                    if let Some(x) = a.feed(1, pid, i, n, &d).unwrap() {
                        out = Some(x);
                    }
                }
                _ => panic!("not a piece"),
            }
        }
        out.unwrap()
    }

    #[test]
    fn small_line_not_pieced() {
        assert!(split_line(&big_line(3), 1).is_none());
    }

    #[test]
    fn roundtrip_and_size_bound() {
        let line = big_line(8000);
        assert!(line.len() > 300 * 1024);
        let ps = split_line(&line, 7).unwrap();
        assert!(ps.len() > 1);
        for p in &ps {
            assert!(p.len() <= PIECE_MAX_LINE, "piece {} bytes", p.len());
        }
        let back = reassemble(&ps);
        assert_eq!(back + "\n", line);
        let m: WireMsg = serde_json::from_str(&reassemble(&ps)).unwrap();
        assert!(matches!(m, WireMsg::NeedMempoolTxs { ids } if ids.len() == 8000));
    }

    #[test]
    fn worst_case_escaping_bound() {
        let line = "\"".repeat(200_000);
        for p in split_line(&line, 1).unwrap() {
            assert!(p.len() <= PIECE_MAX_LINE, "{}", p.len());
        }
    }

    #[test]
    fn interleaved_streams_and_out_of_order() {
        let a = split_line(&big_line(4000), 1).unwrap();
        let b = split_line(&big_line(5000), 2).unwrap();
        let mut asm = PieceAssembler::default();
        let mut done = 0;
        for (x, y) in a.iter().zip(b.iter()) {
            for l in [x, y] {
                if let WireMsg::Piece { pid, i, n, d } = serde_json::from_str(l.trim()).unwrap() {
                    if asm.feed(5, pid, i, n, &d).unwrap().is_some() {
                        done += 1;
                    }
                }
            }
        }
        // `a` is shorter: it completed; `b` is still open.
        assert_eq!(done, 1);
        assert_eq!(asm.open_streams(), 1);
        // Skipping a piece kills the stream.
        let mut asm = PieceAssembler::default();
        asm.feed(1, 9, 0, 3, "a").unwrap();
        assert!(asm.feed(1, 9, 2, 3, "c").is_err());
        assert_eq!(asm.open_streams(), 0);
        assert!(asm.feed(1, 9, 1, 3, "b").is_err());
    }

    #[test]
    fn limits_and_timeout() {
        let mut asm = PieceAssembler::default();
        assert!(asm.feed(1, 1, 0, PIECE_MAX_COUNT + 1, "x").is_err());
        assert!(asm.feed(1, 1, 3, 2, "x").is_err());
        asm.feed(1, 1, 0, 2, "x").unwrap();
        // Timed out: the continuation is refused.
        assert!(asm.feed(1 + PIECE_TIMEOUT_MS, 1, 1, 2, "y").is_err());
        assert_eq!(asm.dropped, 1);
        for pid in 0..(PIECE_MAX_STREAMS as u64 + 3) {
            asm.feed(100 + pid, pid + 10, 0, 2, "x").unwrap();
        }
        assert_eq!(asm.open_streams(), PIECE_MAX_STREAMS);
    }

    #[test]
    fn mixed_version_capability() {
        assert!(!peer_release_supports_pieces("")); // 2.10.2 Hello: no field
        assert!(!peer_release_supports_pieces("2.10.2"));
        assert!(!peer_release_supports_pieces("garbage"));
        assert!(peer_release_supports_pieces("2.10.3"));
        assert!(peer_release_supports_pieces("2.11.0"));
    }

    #[test]
    fn legacy_peer_cannot_parse_piece_so_we_never_send_it() {
        // A 2.10.2 node has no `piece` variant: such a line would be an
        // unknown-variant parse error there. This is why sending is gated.
        let ps = split_line(&big_line(4000), 3).unwrap();
        let v: serde_json::Value = serde_json::from_str(ps[0].trim()).unwrap();
        assert_eq!(v["type"], "piece");
    }
}
