//! Disk hygiene for node-local dumps (v2.9.53).
//!
//! 2026-10-07: host2 (v3 hub + v11) hit 100% disk. The hub wrote a fresh
//! ~200 MB `/tmp/pl-hub-join-v3-<tip>.json` on every `NeedSnapshot` and never
//! deleted any of them (~1,000 files, 173 GB back to Sep 8). v11 was the
//! proposer, could not write its snapshot, and the chain stalled at 374384.
//!
//! This module holds the pure pieces so they can be unit-tested:
//! - [`stat`]: free/total bytes for the filesystem holding a path (statvfs).
//! - [`DiskGuard`]: thresholds. *Optional* dumps (hub join serve, archive
//!   serve refresh) are refused below the low-water mark; *essential* writes
//!   (the seat's own snapshot) only need to fit with a small hard reserve, so
//!   we never write the disk to 100%.
//! - path helpers + pruning for legacy `/tmp/pl-hub-join-<id>-<tip>.json`,
//!   the single bounded hub dump, and `snapshots/<seat>.json*.bak*`.

use std::path::{Path, PathBuf};

/// Free space for the filesystem that holds `path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskStat {
    pub path: PathBuf,
    /// Bytes available to an unprivileged writer (`f_bavail * f_frsize`).
    pub avail_bytes: u64,
    pub total_bytes: u64,
}

impl DiskStat {
    pub fn free_pct(&self) -> f64 {
        if self.total_bytes == 0 {
            return 100.0;
        }
        (self.avail_bytes as f64) * 100.0 / (self.total_bytes as f64)
    }
}

/// statvfs(2) on `path` (or its nearest existing ancestor).
pub fn stat(path: &Path) -> Option<DiskStat> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let mut p = if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path.to_path_buf()
    };
    while !p.exists() {
        match p.parent() {
            Some(q) if !q.as_os_str().is_empty() => p = q.to_path_buf(),
            _ => {
                p = PathBuf::from(".");
                break;
            }
        }
    }
    let c = CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut s) };
    if rc != 0 {
        return None;
    }
    let frsize = if s.f_frsize > 0 { s.f_frsize as u64 } else { s.f_bsize as u64 };
    Some(DiskStat {
        path: p,
        avail_bytes: (s.f_bavail as u64).saturating_mul(frsize),
        total_bytes: (s.f_blocks as u64).saturating_mul(frsize),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskLevel {
    Ok,
    /// Below `warn_pct`: loud warning, everything still written.
    Warn,
    /// Below the low-water mark: optional dumps refused.
    Low,
}

impl DiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            DiskLevel::Ok => "ok",
            DiskLevel::Warn => "warn",
            DiskLevel::Low => "low",
        }
    }
}

/// Thresholds. Env overrides (read once at start by the node binary):
/// `FALCON_PL_DISK_WARN_PCT` (10), `FALCON_PL_DISK_MIN_FREE_PCT` (5),
/// `FALCON_PL_DISK_MIN_FREE_MB` (2048), `FALCON_PL_DISK_HARD_RESERVE_MB` (256).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiskGuard {
    pub warn_pct: f64,
    /// Low-water mark (percent of the filesystem).
    pub min_free_pct: f64,
    /// Low-water mark (absolute). Low = below **either** floor.
    pub min_free_bytes: u64,
    /// Essential writes must leave at least this much behind.
    pub hard_reserve_bytes: u64,
}

impl Default for DiskGuard {
    fn default() -> Self {
        Self {
            warn_pct: 10.0,
            min_free_pct: 5.0,
            min_free_bytes: 2048 * 1024 * 1024,
            hard_reserve_bytes: 256 * 1024 * 1024,
        }
    }
}

const MB: u64 = 1024 * 1024;

impl DiskGuard {
    pub fn from_env() -> Self {
        let d = Self::default();
        let f = |k: &str| std::env::var(k).ok().and_then(|v| v.trim().parse::<f64>().ok());
        let u = |k: &str| std::env::var(k).ok().and_then(|v| v.trim().parse::<u64>().ok());
        Self {
            warn_pct: f("FALCON_PL_DISK_WARN_PCT").unwrap_or(d.warn_pct),
            min_free_pct: f("FALCON_PL_DISK_MIN_FREE_PCT").unwrap_or(d.min_free_pct),
            min_free_bytes: u("FALCON_PL_DISK_MIN_FREE_MB")
                .map(|m| m.saturating_mul(MB))
                .unwrap_or(d.min_free_bytes),
            hard_reserve_bytes: u("FALCON_PL_DISK_HARD_RESERVE_MB")
                .map(|m| m.saturating_mul(MB))
                .unwrap_or(d.hard_reserve_bytes),
        }
    }

    fn low_water_bytes(&self, total: u64) -> u64 {
        let pct = ((total as f64) * self.min_free_pct / 100.0) as u64;
        pct.max(self.min_free_bytes)
    }

    pub fn level(&self, s: &DiskStat) -> DiskLevel {
        if s.avail_bytes < self.low_water_bytes(s.total_bytes) {
            DiskLevel::Low
        } else if s.free_pct() < self.warn_pct {
            DiskLevel::Warn
        } else {
            DiskLevel::Ok
        }
    }

    /// Optional dump of ~`need` bytes (hub join serve, archive serve refresh,
    /// extra copies). Refused if free space is already under the low-water
    /// mark or would drop under it after the write.
    pub fn check_optional(&self, s: &DiskStat, need: u64) -> Result<(), String> {
        let floor = self.low_water_bytes(s.total_bytes);
        if s.avail_bytes < floor.saturating_add(need) {
            return Err(format!(
                "low disk: {} MB free ({:.1}%) at {:?}; optional dump of {} MB needs {} MB (floor {} MB)",
                s.avail_bytes / MB,
                s.free_pct(),
                s.path,
                need / MB,
                floor.saturating_add(need) / MB,
                floor / MB
            ));
        }
        Ok(())
    }

    /// Essential write (seat snapshot / snapshot pull). Only refused if it
    /// would leave less than the hard reserve — never write the disk to 100%.
    pub fn check_essential(&self, s: &DiskStat, need: u64) -> Result<(), String> {
        if s.avail_bytes < need.saturating_add(self.hard_reserve_bytes) {
            return Err(format!(
                "disk full: {} MB free at {:?}; write of {} MB would leave < {} MB reserve",
                s.avail_bytes / MB,
                s.path,
                need / MB,
                self.hard_reserve_bytes / MB
            ));
        }
        Ok(())
    }

    pub fn status_json(&self, s: Option<&DiskStat>) -> serde_json::Value {
        match s {
            None => serde_json::json!({ "known": false }),
            Some(s) => serde_json::json!({
                "known": true,
                "path": s.path.to_string_lossy(),
                "avail_bytes": s.avail_bytes,
                "total_bytes": s.total_bytes,
                "free_pct": (s.free_pct() * 100.0).round() / 100.0,
                "level": self.level(s).as_str(),
                "warn_pct": self.warn_pct,
                "min_free_pct": self.min_free_pct,
                "min_free_bytes": self.min_free_bytes,
            }),
        }
    }
}

/// Single bounded hub join dump inside the node's data dir (overwritten in
/// place via tmp+rename; deleted when idle). Replaces `/tmp/pl-hub-join-<id>-<tip>.json`.
pub fn hub_join_path(data_dir: &Path, id: &str) -> PathBuf {
    data_dir.join(format!("{id}.hub-join.json"))
}

/// Legacy `pl-hub-join-<id>-<digits>.json[.tmp]` for exactly this seat id
/// (`v1` must not match `v11`).
pub fn is_legacy_hub_join(name: &str, id: &str) -> bool {
    let Some(rest) = name.strip_prefix(&format!("pl-hub-join-{id}-")) else {
        return false;
    };
    let rest = rest.strip_suffix(".tmp").unwrap_or(rest);
    let Some(digits) = rest.strip_suffix(".json") else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Legacy joiner pull file `pl-snap-pull-<digits>.json[.tmp]` (temp dir only).
pub fn is_legacy_snap_pull(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("pl-snap-pull-") else {
        return false;
    };
    let rest = rest.strip_suffix(".tmp").unwrap_or(rest);
    let Some(digits) = rest.strip_suffix(".json") else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Delete legacy hub-join dumps for `id` in `dir` (normally the temp dir).
/// Returns (files removed, bytes freed).
pub fn prune_legacy_hub_join(dir: &Path, id: &str) -> (usize, u64) {
    prune_matching(dir, |n| is_legacy_hub_join(n, id))
}

/// Delete legacy `pl-snap-pull-<tip>.json` in `dir` (normally the temp dir)
/// older than `min_age` (they carry no seat id; another seat on the host may
/// have just written one and be loading it).
pub fn prune_legacy_snap_pull(dir: &Path, min_age: std::time::Duration) -> (usize, u64) {
    prune_matching_aged(dir, is_legacy_snap_pull, min_age)
}

fn prune_matching(dir: &Path, is_target: impl Fn(&str) -> bool) -> (usize, u64) {
    prune_matching_aged(dir, is_target, std::time::Duration::ZERO)
}

fn prune_matching_aged(
    dir: &Path,
    is_target: impl Fn(&str) -> bool,
    min_age: std::time::Duration,
) -> (usize, u64) {
    let now = std::time::SystemTime::now();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut n = 0usize;
    let mut bytes = 0u64;
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_target(name) {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        if !md.is_file() {
            continue;
        }
        if !min_age.is_zero() {
            let age = md
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .unwrap_or_default();
            if age < min_age {
                continue;
            }
        }
        if std::fs::remove_file(e.path()).is_ok() {
            n += 1;
            bytes = bytes.saturating_add(md.len());
        }
    }
    (n, bytes)
}

/// Operator backups of the seat snapshot `<name>.json`: `<name>.json.<ts>.bak`
/// and `<name>.json.bak-<ts>` (both styles exist on live hosts).
pub fn is_snapshot_bak(name: &str, snap_file_name: &str) -> bool {
    let Some(rest) = name.strip_prefix(snap_file_name) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('.') else {
        return false;
    };
    (rest.ends_with(".bak") && rest.len() > 4) || (rest.starts_with("bak-") && rest.len() > 4)
}

/// Keep the newest `keep` (by mtime) `.bak` copies of `snapshot_out`; delete
/// the rest. `keep == 0` disables pruning. Returns deleted paths.
pub fn prune_snapshot_baks(snapshot_out: &Path, keep: usize) -> Vec<PathBuf> {
    if keep == 0 {
        return Vec::new();
    }
    let Some(name) = snapshot_out.file_name().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    let dir = match snapshot_out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut baks: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name();
            let n = n.to_str()?;
            if !is_snapshot_bak(n, name) {
                return None;
            }
            let md = e.metadata().ok()?;
            if !md.is_file() {
                return None;
            }
            Some((md.modified().unwrap_or(std::time::UNIX_EPOCH), e.path()))
        })
        .collect();
    // Newest first; ties broken by name so the result is deterministic.
    baks.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let mut gone = Vec::new();
    for (_, p) in baks.into_iter().skip(keep) {
        if std::fs::remove_file(&p).is_ok() {
            gone.push(p);
        }
    }
    gone
}

/// v2.9.67: the `<sha>` in `<stem>.join.serve.<sha>.json` (64 hex), else `None`.
pub fn join_serve_sha<'a>(name: &'a str, stem: &str) -> Option<&'a str> {
    let rest = name.strip_prefix(stem)?.strip_prefix(".join.serve.")?;
    let sha = rest.strip_suffix(".json")?;
    (sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())).then_some(sha)
}

/// v2.9.67 retention for hashed join-serve files next to `snapshot_out`.
/// Keeps the `keep_newest` newest, anything younger than `min_age`, and every
/// sha in `protect`. Never touches `<stem>.join.serve.json` / `<stem>.join.json`.
/// Returns the removed paths and their total bytes.
pub fn prune_join_serve(
    snapshot_out: &Path,
    keep_newest: usize,
    min_age: std::time::Duration,
    protect: &[String],
) -> (Vec<PathBuf>, u64) {
    let Some(stem) = snapshot_out.file_stem().and_then(|s| s.to_str()) else {
        return (Vec::new(), 0);
    };
    let dir = match snapshot_out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return (Vec::new(), 0);
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf, String, u64)> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name();
            let n = n.to_str()?;
            let sha = join_serve_sha(n, stem)?.to_string();
            let md = e.metadata().ok()?;
            if !md.is_file() {
                return None;
            }
            Some((md.modified().unwrap_or(std::time::UNIX_EPOCH), e.path(), sha, md.len()))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let now = std::time::SystemTime::now();
    let mut gone = Vec::new();
    let mut bytes = 0u64;
    for (mtime, p, sha, len) in files.into_iter().skip(keep_newest) {
        if protect.iter().any(|s| s.eq_ignore_ascii_case(&sha)) {
            continue;
        }
        let age = now.duration_since(mtime).unwrap_or_default();
        if age < min_age {
            continue;
        }
        if std::fs::remove_file(&p).is_ok() {
            bytes += len;
            gone.push(p);
        }
    }
    (gone, bytes)
}

/// Hub join serve: may an existing dump (or the periodic pinned serve file)
/// at `pin_tip` answer a `NeedSnapshot` at `live_tip` instead of writing a
/// new ~200 MB file? Fresh (`< SNAPSHOT_SERVE_STALE_LAG` behind), or a
/// joiner pulled a chunk within `SNAPSHOT_PIN_IDLE_MS` and the pin is still
/// `< SNAPSHOT_PULL_LAG` behind (do not rotate mid-transfer, 2.9.22).
pub fn hub_snap_reusable(live_tip: u64, pin_tip: u64, prev_chunk_ms: u64, now_ms: u64) -> bool {
    if pin_tip == 0 || pin_tip > live_tip {
        return false;
    }
    let lag = live_tip - pin_tip;
    if lag < crate::Node::SNAPSHOT_SERVE_STALE_LAG {
        return true;
    }
    let busy = prev_chunk_ms > 0
        && now_ms.saturating_sub(prev_chunk_ms) <= crate::Node::SNAPSHOT_PIN_IDLE_MS;
    busy && lag < crate::Node::SNAPSHOT_PULL_LAG
}

/// Delete the bounded hub dump after this long without a chunk/offer.
pub const HUB_DUMP_IDLE_DELETE_MS: u64 = 10 * 60 * 1000;

#[cfg(test)]
mod tests {
    use super::*;

    fn st(avail_mb: u64, total_mb: u64) -> DiskStat {
        DiskStat {
            path: PathBuf::from("/data"),
            avail_bytes: avail_mb * MB,
            total_bytes: total_mb * MB,
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fdpl-disk-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn levels_follow_pct_and_absolute_floor() {
        let g = DiskGuard::default();
        // 232 GB disk (host2): 50% free ok, 8% warn, 4% low.
        let total = 232 * 1024;
        assert_eq!(g.level(&st(total / 2, total)), DiskLevel::Ok);
        assert_eq!(g.level(&st(total * 8 / 100, total)), DiskLevel::Warn);
        assert_eq!(g.level(&st(total * 4 / 100, total)), DiskLevel::Low);
        // Small disk: 20% free but under the 2 GB absolute floor → low.
        assert_eq!(g.level(&st(1024, 5 * 1024)), DiskLevel::Low);
        // Live incident: 1.7 MB free.
        assert_eq!(g.level(&DiskStat { path: "/".into(), avail_bytes: 1_700_000, total_bytes: total * MB }), DiskLevel::Low);
    }

    #[test]
    fn optional_dump_refused_before_essential() {
        let g = DiskGuard::default();
        let total = 232 * 1024;
        let snap = 200; // MB
        // Plenty of room: both ok.
        assert!(g.check_optional(&st(50 * 1024, total), snap * MB).is_ok());
        assert!(g.check_essential(&st(50 * 1024, total), snap * MB).is_ok());
        // ~11.7 GB free (5.04%, still above the 5% floor): the 200 MB dump would
        // cross it → optional refused; the seat's own snapshot still fits.
        let s = st(11_980, total);
        assert_eq!(g.level(&s), DiskLevel::Warn);
        assert!(g.check_optional(&s, snap * MB).is_err());
        assert!(g.check_essential(&s, snap * MB).is_ok());
        // 300 MB free: even the essential write is refused (would leave < 256 MB).
        let s = st(300, total);
        let e = g.check_essential(&s, snap * MB).unwrap_err();
        assert!(e.contains("disk full"), "{e}");
    }

    #[test]
    fn status_json_reports_level() {
        let g = DiskGuard::default();
        let v = g.status_json(Some(&st(4 * 1024, 100 * 1024)));
        assert_eq!(v["level"], "low");
        assert_eq!(v["avail_bytes"], 4 * 1024 * MB);
        assert_eq!(g.status_json(None)["known"], false);
    }

    #[test]
    fn stat_real_dir() {
        let d = tmpdir("stat");
        let s = stat(&d.join("does/not/exist/yet.json")).expect("statvfs");
        assert!(s.total_bytes > 0);
        assert!(s.avail_bytes <= s.total_bytes);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn hub_snap_reuse_instead_of_dump_per_height() {
        let stale = crate::Node::SNAPSHOT_SERVE_STALE_LAG;
        let pull = crate::Node::SNAPSHOT_PULL_LAG;
        let now = 1_000_000;
        // Old code dumped on every NeedSnapshot (one per height on host2).
        // A pin a few heights old is reused.
        assert!(hub_snap_reusable(374_384, 374_380, 0, now));
        assert!(!hub_snap_reusable(374_384, 374_384 - stale, 0, now));
        // Mid-transfer: keep the pin until it is a full pull-lag stale.
        assert!(hub_snap_reusable(10_000, 10_000 - stale, now - 1_000, now));
        assert!(!hub_snap_reusable(10_000, 10_000 - pull, now - 1_000, now));
        // Idle joiner: rotate.
        let idle = now - crate::Node::SNAPSHOT_PIN_IDLE_MS - 1;
        assert!(!hub_snap_reusable(10_000, 10_000 - stale, idle, now));
        // No pin / pin ahead of us (after a reset) → never reuse.
        assert!(!hub_snap_reusable(10, 0, now, now));
        assert!(!hub_snap_reusable(10, 11, now, now));
    }

    #[test]
    fn legacy_hub_join_match_is_id_exact() {
        assert!(is_legacy_hub_join("pl-hub-join-v3-374384.json", "v3"));
        assert!(is_legacy_hub_join("pl-hub-join-v3-374384.json.tmp", "v3"));
        assert!(!is_legacy_hub_join("pl-hub-join-v3-374384.json", "v1"));
        assert!(!is_legacy_hub_join("pl-hub-join-v11-5.json", "v1"));
        assert!(is_legacy_hub_join("pl-hub-join-v11-5.json", "v11"));
        assert!(!is_legacy_hub_join("pl-hub-join-v3-.json", "v3"));
        assert!(!is_legacy_hub_join("pl-hub-join-v3-12x.json", "v3"));
        assert!(!is_legacy_hub_join("v3.json", "v3"));
        assert!(is_legacy_snap_pull("pl-snap-pull-374000.json"));
        assert!(!is_legacy_snap_pull("pl-snap-pull-.json"));
    }

    #[test]
    fn prune_legacy_hub_join_only_own_seat() {
        let d = tmpdir("hubjoin");
        for n in [
            "pl-hub-join-v3-1.json",
            "pl-hub-join-v3-2.json",
            "pl-hub-join-v3-3.json.tmp",
            "pl-hub-join-v11-4.json",
            "unrelated.json",
        ] {
            std::fs::write(d.join(n), b"0123456789").unwrap();
        }
        let (n, bytes) = prune_legacy_hub_join(&d, "v3");
        assert_eq!(n, 3);
        assert_eq!(bytes, 30);
        assert!(d.join("pl-hub-join-v11-4.json").exists());
        assert!(d.join("unrelated.json").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn prune_snapshot_baks_keeps_newest_n() {
        let d = tmpdir("baks");
        let snap = d.join("v3.json");
        std::fs::write(&snap, b"live").unwrap();
        let names = [
            "v3.json.bak-20261004T192815Z",
            "v3.json.20261005T061515Z.bak",
            "v3.json.20261005T121643Z.bak",
            "v3.json.20261005T125940Z.bak",
        ];
        let base = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for (i, n) in names.iter().enumerate() {
            let p = d.join(n);
            std::fs::write(&p, b"x").unwrap();
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            f.set_modified(base + std::time::Duration::from_secs(i as u64 * 60)).unwrap();
        }
        // Other seats' backups and the live files are never touched.
        for n in ["v11.json.20261005T061515Z.bak", "v3.json.own-votes.json", "v3.join.json", "v3.json.tmp"] {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        assert!(prune_snapshot_baks(&snap, 0).is_empty(), "0 disables");
        let gone = prune_snapshot_baks(&snap, 3);
        assert_eq!(gone, vec![d.join(names[0])]);
        for n in &names[1..] {
            assert!(d.join(n).exists(), "{n}");
        }
        for n in ["v3.json", "v11.json.20261005T061515Z.bak", "v3.json.own-votes.json", "v3.join.json", "v3.json.tmp"] {
            assert!(d.join(n).exists(), "{n}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn prune_join_serve_keeps_newest_young_and_protected() {
        let d = tmpdir("joinserve");
        let snap = d.join("v1.json");
        let sha = |c: char| std::iter::repeat(c).take(64).collect::<String>();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let mut all = Vec::new();
        for (i, c) in ['a', 'b', 'c', 'd', 'e', 'f', '1', '2', '3'].iter().enumerate() {
            let p = d.join(format!("v1.join.serve.{}.json", sha(*c)));
            std::fs::write(&p, b"x").unwrap();
            let t = old - std::time::Duration::from_secs(i as u64 * 60);
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
            all.push(p);
        }
        for n in ["v1.join.serve.json", "v1.join.json", "v1.json", "v1.join.serve.zz.json"] {
            std::fs::write(d.join(n), b"x").unwrap();
            std::fs::File::options()
                .write(true)
                .open(d.join(n))
                .unwrap()
                .set_modified(old - std::time::Duration::from_secs(9999))
                .unwrap();
        }
        // a young one beyond the newest window must survive
        let young = d.join(format!("v1.join.serve.{}.json", sha('9')));
        std::fs::write(&young, b"x").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&young)
            .unwrap()
            .set_modified(old + std::time::Duration::from_secs(10))
            .unwrap();
        let (gone, _) = prune_join_serve(
            &snap,
            3,
            std::time::Duration::from_secs(1800),
            &[sha('2')],
        );
        // newest 3 by mtime: young(9), a, b. Protected: 2. Deleted: c d e f 1 3.
        assert_eq!(gone.len(), 6, "{gone:?}");
        assert!(young.exists() && all[0].exists() && all[1].exists() && all[7].exists());
        for i in [2, 3, 4, 5, 6, 8] {
            assert!(!all[i].exists());
        }
        for n in ["v1.join.serve.json", "v1.join.json", "v1.json", "v1.join.serve.zz.json"] {
            assert!(d.join(n).exists());
        }
        assert_eq!(join_serve_sha(&format!("v1.join.serve.{}.json", sha('a')), "v1"), Some(sha('a').as_str()));
        assert_eq!(join_serve_sha("v1.join.serve.json", "v1"), None);
    }
}
