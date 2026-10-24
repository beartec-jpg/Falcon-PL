//! Disk-backed ledger bodies for `--archive` (v2.9.1).
//!
//! RAM always strips txs older than `max_body_ledgers`. An archive node writes
//! the full body to `{dir}/{ledger_hash}.json` first, then serves
//! `NeedLedgerBody` / `ledgers_from` from disk so tip-0 joiners still work
//! without holding every Falcon-512 pack in RSS.
//!
//! v2.9.34: also write `{dir}/h-{height}.hash` so residual `NeedLedgers`
//! can walk by height after the RAM header tail has been pruned.

use crate::Ledger;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct BodyStore {
    pub dir: PathBuf,
}

impl BodyStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).context("create body store")?;
        let store = Self { dir };
        store.backfill_height_index();
        Ok(store)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, hash: &str) -> PathBuf {
        self.dir.join(format!("{hash}.json"))
    }

    fn height_path(&self, height: u64) -> PathBuf {
        self.dir.join(format!("h-{height}.hash"))
    }

    fn write_height_index(&self, height: u64, hash: &str) {
        if height == 0 || hash.is_empty() {
            return;
        }
        let path = self.height_path(height);
        if path.exists() {
            return;
        }
        let _ = std::fs::write(path, hash.as_bytes());
    }

    /// Best-effort index of existing `{hash}.json` files (upgrade path).
    fn backfill_height_index(&self) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for ent in rd.flatten() {
            let path = ent.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !name.ends_with(".json") || name.ends_with(".tmp") || name.starts_with("h-") {
                continue;
            }
            let Some((h, hash)) = peek_height_hash(&path) else {
                continue;
            };
            self.write_height_index(h, &hash);
        }
    }

    /// Idempotent. Skips ledgers that have already been stripped or written.
    pub fn put(&self, led: &Ledger) -> Result<()> {
        if !led.has_body() || led.ledger_hash.is_empty() {
            return Ok(());
        }
        let path = self.path(&led.ledger_hash);
        if !path.exists() {
            let bytes = serde_json::to_vec(led).context("serialize ledger body")?;
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, &bytes).context("write body tmp")?;
            std::fs::rename(&tmp, &path).context("rename body into place")?;
        }
        self.write_height_index(led.height, &led.ledger_hash);
        Ok(())
    }

    pub fn get(&self, hash: &str) -> Option<Ledger> {
        if hash.is_empty() {
            return None;
        }
        let bytes = std::fs::read(self.path(hash)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn hash_at_height(&self, height: u64) -> Option<String> {
        let s = std::fs::read_to_string(self.height_path(height)).ok()?;
        let h = s.trim().to_string();
        if h.is_empty() {
            None
        } else {
            Some(h)
        }
    }

    pub fn get_at_height(&self, height: u64) -> Option<Ledger> {
        let hash = self.hash_at_height(height)?;
        self.get(&hash)
    }
}

/// Height is the first field; `ledger_hash` sits after the txs blob — read both ends.
fn peek_height_hash(path: &Path) -> Option<(u64, String)> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 2048];
    let n = f.read(&mut head).ok()?;
    let hs = String::from_utf8_lossy(&head[..n]);
    let height: u64 = hs
        .split("\"height\":")
        .nth(1)?
        .split([',', '}'])
        .next()?
        .trim()
        .parse()
        .ok()?;
    let len = f.metadata().ok()?.len();
    let tail_n = 8192u64.min(len);
    if tail_n == 0 {
        return None;
    }
    let start = len.saturating_sub(tail_n);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = vec![0u8; tail_n as usize];
    f.read_exact(&mut tail).ok()?;
    let ts = String::from_utf8_lossy(&tail);
    let hash = ts
        .rsplit("\"ledger_hash\":\"")
        .next()?
        .split('"')
        .next()?
        .to_string();
    if hash.len() < 16 {
        return None;
    }
    Some((height, hash))
}
