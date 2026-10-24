//! Logical conf pool: open DA batches awaiting UNL signatures.
//!
//! PR3: explicit state machine + timestamps/counters matching dig failure modes.
//! Bodies and partial sigs land here so check-phase can sign in a scheduled window
//! instead of only via free-for-all mesh timing. Not a separate consensus voter —
//! only UNL weight in the DAG forms certificates.
//!
//! ## Pack / check duty split
//!
//! Even slots = **Pack** (create new open work). Odd slots = **Check** (sign /
//! advance open conf-pool work, creator-directed recovery). Slot length is
//! `pack_check_slot_s` (default 0.25s → 2 pack windows/s).

use fd_types::{genesis_batch_hash, Batch, HashHex};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Entry phase (operator-visible)
// ---------------------------------------------------------------------------

/// Where an open batch is in the conf-pool lifecycle (PR3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfPoolPhase {
    /// Body known locally; may still need local sig.
    AwaitBodyOrLocalSig,
    /// Local signature present; waiting for foreign UNL weight.
    AwaitForeign,
    /// At least one foreign UNL signer visible on this entry.
    ForeignVisible,
    /// Creator-directed reannounce of our sig (or body) is in flight / recorded.
    CreatorAckPending,
    /// Remote cert observed/parked; apply still needed.
    ParkedCert,
    /// Eligible for abandon/repool (caller drives actual repool).
    RepoolEligible,
    /// Certified / removed from open set (terminal for status snapshots).
    Certified,
}

impl ConfPoolPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwaitBodyOrLocalSig => "await_body_or_local_sig",
            Self::AwaitForeign => "await_foreign",
            Self::ForeignVisible => "foreign_visible",
            Self::CreatorAckPending => "creator_ack_pending",
            Self::ParkedCert => "parked_cert",
            Self::RepoolEligible => "repool_eligible",
            Self::Certified => "certified",
        }
    }
}

/// Why local sign was deferred in check phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignDeferReason {
    Cap,
    AlreadySigned,
    NoBody,
    Policy,
}

impl SignDeferReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cap => "cap",
            Self::AlreadySigned => "already_signed",
            Self::NoBody => "no_body",
            Self::Policy => "policy",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfPoolEntry {
    pub batch: Batch,
    pub first_seen: f64,
    pub last_touch: f64,
    /// First time body with non-empty tx_ids was observed.
    pub body_at: f64,
    /// First time local validator signature was recorded on this entry.
    pub local_sig_at: f64,
    /// First time a foreign UNL signature was observed.
    pub foreign_visible_at: f64,
    /// Last creator-directed reannounce timestamp (0 = never).
    pub last_creator_reannounce_at: f64,
    /// Bounded creator-directed retry counter.
    pub creator_reannounce_count: u32,
    /// First time a parked remote cert was associated.
    pub parked_cert_at: f64,
    /// Parked-cert apply retry count.
    pub parked_cert_retries: u32,
    /// First time marked repool-eligible (0 = not).
    pub repool_eligible_at: f64,
    /// Explicit operator phase (recomputed on mutations when possible).
    pub phase: ConfPoolPhase,
    /// Consecutive zero-foreign observations while we are creator (for status).
    pub zero_foreign_since: f64,
}

impl ConfPoolEntry {
    fn new(batch: Batch, now: f64) -> Self {
        let has_body = !batch.tx_ids.is_empty();
        Self {
            body_at: if has_body { now } else { 0.0 },
            batch,
            first_seen: now,
            last_touch: now,
            local_sig_at: 0.0,
            foreign_visible_at: 0.0,
            last_creator_reannounce_at: 0.0,
            creator_reannounce_count: 0,
            parked_cert_at: 0.0,
            parked_cert_retries: 0,
            repool_eligible_at: 0.0,
            phase: if has_body {
                ConfPoolPhase::AwaitBodyOrLocalSig
            } else {
                ConfPoolPhase::AwaitBodyOrLocalSig
            },
            zero_foreign_since: 0.0,
        }
    }

    /// Recompute phase from timestamps + batch signatures.
    pub fn recompute_phase(&mut self, local_id: &str, now: f64) {
        if self.repool_eligible_at > 0.0 {
            self.phase = ConfPoolPhase::RepoolEligible;
            return;
        }
        if self.parked_cert_at > 0.0 {
            self.phase = ConfPoolPhase::ParkedCert;
            return;
        }
        let foreign_n = self
            .batch
            .signatures
            .keys()
            .filter(|k| k.as_str() != local_id)
            .count();
        if foreign_n > 0 {
            if self.foreign_visible_at <= 0.0 {
                self.foreign_visible_at = now;
            }
            self.zero_foreign_since = 0.0;
            if self.last_creator_reannounce_at > 0.0
                && self.batch.creator != local_id
            {
                self.phase = ConfPoolPhase::CreatorAckPending;
            } else {
                self.phase = ConfPoolPhase::ForeignVisible;
            }
            return;
        }
        // Zero foreign.
        if self.batch.creator == local_id {
            if self.zero_foreign_since <= 0.0 {
                self.zero_foreign_since = if self.local_sig_at > 0.0 {
                    self.local_sig_at
                } else {
                    self.first_seen
                };
            }
        }
        if self.local_sig_at > 0.0 || self.batch.signatures.contains_key(local_id) {
            if self.local_sig_at <= 0.0 {
                self.local_sig_at = now;
            }
            if self.batch.creator != local_id && self.last_creator_reannounce_at > 0.0 {
                self.phase = ConfPoolPhase::CreatorAckPending;
            } else {
                self.phase = ConfPoolPhase::AwaitForeign;
            }
            return;
        }
        self.phase = ConfPoolPhase::AwaitBodyOrLocalSig;
    }

    pub fn zero_foreign_duration_s(&self, now: f64) -> f64 {
        if self.zero_foreign_since <= 0.0 {
            0.0
        } else {
            (now - self.zero_foreign_since).max(0.0)
        }
    }

    pub fn age_s(&self, now: f64) -> f64 {
        (now - self.first_seen).max(0.0)
    }
}

/// Aggregate counters for conf-pool lifecycle (auditable cleanup / deferrals).
#[derive(Debug, Clone, Default)]
pub struct ConfPoolCounters {
    pub put_body: u64,
    pub put_sig_local: u64,
    pub put_sig_foreign: u64,
    pub creator_reannounce: u64,
    pub parked_cert_noted: u64,
    pub parked_cert_retry: u64,
    pub removed_certified: u64,
    pub removed_repool: u64,
    pub removed_other: u64,
    pub sign_defer_cap: u64,
    pub sign_defer_already: u64,
    pub sign_defer_no_body: u64,
    pub sign_defer_policy: u64,
}

impl ConfPoolCounters {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "put_body": self.put_body,
            "put_sig_local": self.put_sig_local,
            "put_sig_foreign": self.put_sig_foreign,
            "creator_reannounce": self.creator_reannounce,
            "parked_cert_noted": self.parked_cert_noted,
            "parked_cert_retry": self.parked_cert_retry,
            "removed_certified": self.removed_certified,
            "removed_repool": self.removed_repool,
            "removed_other": self.removed_other,
            "sign_defer_cap": self.sign_defer_cap,
            "sign_defer_already": self.sign_defer_already,
            "sign_defer_no_body": self.sign_defer_no_body,
            "sign_defer_policy": self.sign_defer_policy,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveReason {
    Certified,
    Repool,
    Other,
}

#[derive(Debug, Default)]
pub struct ConfPool {
    entries: BTreeMap<HashHex, ConfPoolEntry>,
    pub counters: ConfPoolCounters,
    /// Local validator id for phase classification (set by engine).
    local_id: String,
}

impl ConfPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_local_id(&mut self, id: impl Into<String>) {
        self.local_id = id.into();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, batch_hash: &str) -> bool {
        self.entries.contains_key(batch_hash)
    }

    pub fn get(&self, batch_hash: &str) -> Option<&ConfPoolEntry> {
        self.entries.get(batch_hash)
    }

    pub fn get_mut(&mut self, batch_hash: &str) -> Option<&mut ConfPoolEntry> {
        self.entries.get_mut(batch_hash)
    }

    pub fn put_body(&mut self, mut batch: Batch, now: f64) {
        if batch.batch_hash == genesis_batch_hash() {
            return;
        }
        let h = batch.batch_hash.clone();
        let local = self.local_id.clone();
        match self.entries.get_mut(&h) {
            Some(e) => {
                for (vid, sig) in &batch.signatures {
                    e.batch
                        .signatures
                        .entry(vid.clone())
                        .or_insert_with(|| sig.clone());
                }
                if e.batch.tx_ids.is_empty() && !batch.tx_ids.is_empty() {
                    e.batch.tx_ids = batch.tx_ids.clone();
                    e.batch.txs = batch.txs.clone();
                    e.batch.parents = batch.parents.clone();
                    e.batch.creator = batch.creator.clone();
                    e.batch.timestamp = batch.timestamp;
                    if e.body_at <= 0.0 {
                        e.body_at = now;
                    }
                }
                e.last_touch = now;
                e.recompute_phase(&local, now);
            }
            None => {
                let mut e = ConfPoolEntry::new(batch, now);
                e.recompute_phase(&local, now);
                self.entries.insert(h, e);
            }
        }
        self.counters.put_body = self.counters.put_body.saturating_add(1);
    }

    pub fn put_sig(
        &mut self,
        batch_hash: &str,
        validator_id: &str,
        signature: &str,
        now: f64,
    ) -> bool {
        let Some(e) = self.entries.get_mut(batch_hash) else {
            return false;
        };
        if signature.is_empty() {
            return false;
        }
        let is_new = !e.batch.signatures.contains_key(validator_id);
        e.batch
            .signatures
            .entry(validator_id.to_string())
            .or_insert_with(|| signature.to_string());
        e.last_touch = now;
        let local = self.local_id.clone();
        if is_new {
            if validator_id == local {
                if e.local_sig_at <= 0.0 {
                    e.local_sig_at = now;
                }
                self.counters.put_sig_local = self.counters.put_sig_local.saturating_add(1);
            } else {
                if e.foreign_visible_at <= 0.0 {
                    e.foreign_visible_at = now;
                }
                self.counters.put_sig_foreign = self.counters.put_sig_foreign.saturating_add(1);
            }
        }
        e.recompute_phase(&local, now);
        true
    }

    /// Record creator-directed reannounce (bounded retries live on the entry).
    pub fn note_creator_reannounce(
        &mut self,
        batch_hash: &str,
        now: f64,
        max_retries: u32,
    ) -> bool {
        let Some(e) = self.entries.get_mut(batch_hash) else {
            return false;
        };
        if max_retries > 0 && e.creator_reannounce_count >= max_retries {
            return false;
        }
        e.creator_reannounce_count = e.creator_reannounce_count.saturating_add(1);
        e.last_creator_reannounce_at = now;
        e.last_touch = now;
        let local = self.local_id.clone();
        e.recompute_phase(&local, now);
        self.counters.creator_reannounce = self.counters.creator_reannounce.saturating_add(1);
        true
    }

    pub fn note_parked_cert(&mut self, batch_hash: &str, now: f64) {
        let Some(e) = self.entries.get_mut(batch_hash) else {
            return;
        };
        if e.parked_cert_at <= 0.0 {
            e.parked_cert_at = now;
            self.counters.parked_cert_noted = self.counters.parked_cert_noted.saturating_add(1);
        }
        e.parked_cert_retries = e.parked_cert_retries.saturating_add(1);
        self.counters.parked_cert_retry = self.counters.parked_cert_retry.saturating_add(1);
        e.last_touch = now;
        let local = self.local_id.clone();
        e.recompute_phase(&local, now);
    }

    pub fn mark_repool_eligible(&mut self, batch_hash: &str, now: f64) {
        let Some(e) = self.entries.get_mut(batch_hash) else {
            return;
        };
        if e.repool_eligible_at <= 0.0 {
            e.repool_eligible_at = now;
        }
        e.last_touch = now;
        let local = self.local_id.clone();
        e.recompute_phase(&local, now);
    }

    pub fn note_sign_defer(&mut self, reason: SignDeferReason) {
        match reason {
            SignDeferReason::Cap => {
                self.counters.sign_defer_cap = self.counters.sign_defer_cap.saturating_add(1)
            }
            SignDeferReason::AlreadySigned => {
                self.counters.sign_defer_already =
                    self.counters.sign_defer_already.saturating_add(1)
            }
            SignDeferReason::NoBody => {
                self.counters.sign_defer_no_body =
                    self.counters.sign_defer_no_body.saturating_add(1)
            }
            SignDeferReason::Policy => {
                self.counters.sign_defer_policy =
                    self.counters.sign_defer_policy.saturating_add(1)
            }
        }
    }

    pub fn remove(&mut self, batch_hash: &str) {
        self.remove_with_reason(batch_hash, RemoveReason::Other);
    }

    pub fn remove_with_reason(&mut self, batch_hash: &str, reason: RemoveReason) {
        if self.entries.remove(batch_hash).is_some() {
            match reason {
                RemoveReason::Certified => {
                    self.counters.removed_certified =
                        self.counters.removed_certified.saturating_add(1)
                }
                RemoveReason::Repool => {
                    self.counters.removed_repool = self.counters.removed_repool.saturating_add(1)
                }
                RemoveReason::Other => {
                    self.counters.removed_other = self.counters.removed_other.saturating_add(1)
                }
            }
        }
    }

    pub fn list_open(&self) -> Vec<&ConfPoolEntry> {
        let mut v: Vec<_> = self.entries.values().collect();
        v.sort_by(|a, b| {
            a.first_seen
                .partial_cmp(&b.first_seen)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.batch.batch_hash.cmp(&b.batch.batch_hash))
        });
        v
    }

    pub fn list_hashes(&self) -> Vec<HashHex> {
        self.entries.keys().cloned().collect()
    }

    /// Own opens with zero foreign UNL sigs (Mode B candidates when peers have signed).
    pub fn own_zero_foreign(&self, creator: &str) -> Vec<&ConfPoolEntry> {
        self.list_open()
            .into_iter()
            .filter(|e| {
                e.batch.creator == creator
                    && e.batch
                        .signatures
                        .keys()
                        .filter(|k| k.as_str() != creator)
                        .count()
                        == 0
            })
            .collect()
    }

    pub fn to_json(&self) -> serde_json::Value {
        self.to_json_at(0.0)
    }

    pub fn to_json_at(&self, now: f64) -> serde_json::Value {
        let opens: Vec<_> = self
            .list_open()
            .iter()
            .take(16)
            .map(|e| {
                let foreign_n = e
                    .batch
                    .signatures
                    .keys()
                    .filter(|k| {
                        if self.local_id.is_empty() {
                            k.as_str() != e.batch.creator
                        } else {
                            k.as_str() != self.local_id
                        }
                    })
                    .count();
                serde_json::json!({
                    "batch_hash": e.batch.batch_hash,
                    "creator": e.batch.creator,
                    "nsigs": e.batch.signatures.len(),
                    "foreign_nsigs": foreign_n,
                    "signers": e.batch.signatures.keys().cloned().collect::<Vec<_>>(),
                    "ntx": e.batch.tx_ids.len(),
                    "age_s": if now > 0.0 { e.age_s(now) } else { (e.last_touch - e.first_seen).max(0.0) },
                    "phase": e.phase.as_str(),
                    "body_at": e.body_at,
                    "local_sig_at": e.local_sig_at,
                    "foreign_visible_at": e.foreign_visible_at,
                    "last_creator_reannounce_at": e.last_creator_reannounce_at,
                    "creator_reannounce_count": e.creator_reannounce_count,
                    "parked_cert_at": e.parked_cert_at,
                    "parked_cert_retries": e.parked_cert_retries,
                    "repool_eligible_at": e.repool_eligible_at,
                    "zero_foreign_duration_s": if now > 0.0 {
                        e.zero_foreign_duration_s(now)
                    } else if e.zero_foreign_since > 0.0 {
                        (e.last_touch - e.zero_foreign_since).max(0.0)
                    } else {
                        0.0
                    },
                })
            })
            .collect();
        // Phase histogram for dig alignment
        let mut phase_counts: BTreeMap<&'static str, u64> = BTreeMap::new();
        for e in self.entries.values() {
            *phase_counts.entry(e.phase.as_str()).or_insert(0) += 1;
        }
        serde_json::json!({
            "n": self.entries.len(),
            "open": opens,
            "phase_counts": phase_counts,
            "counters": self.counters.to_json(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DutyPhase {
    Pack,
    Check,
}

impl DutyPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pack => "pack",
            Self::Check => "check",
        }
    }
}

/// Even slots = Pack, odd = Check. Default slot 0.25s → 2 pack windows/s.
///
/// Pack creates open work; Check advances conf-pool (sign, creator reannounce,
/// parked-cert retry). See module docs.
pub fn duty_phase_at(now: f64, slot_s: f64) -> DutyPhase {
    let slot = if slot_s <= 1e-9 { 0.25 } else { slot_s };
    let i = (now / slot).floor() as i64;
    if i.rem_euclid(2) == 0 {
        DutyPhase::Pack
    } else {
        DutyPhase::Check
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fd_types::Transaction;

    #[test]
    fn duty_alternates() {
        assert_eq!(duty_phase_at(0.0, 0.25), DutyPhase::Pack);
        assert_eq!(duty_phase_at(0.24, 0.25), DutyPhase::Pack);
        assert_eq!(duty_phase_at(0.25, 0.25), DutyPhase::Check);
        assert_eq!(duty_phase_at(0.50, 0.25), DutyPhase::Pack);
    }

    #[test]
    fn put_body_and_sig_lifecycle() {
        let mut p = ConfPool::new();
        p.set_local_id("val1");
        let tx = Transaction::payment("acct1", 1, "d", 1, 0, 1.0);
        let b = Batch::create(vec![tx], vec![genesis_batch_hash()], "val2".into(), 1.0);
        let h = b.batch_hash.clone();
        p.put_body(b, 1.0);
        assert_eq!(p.get(&h).unwrap().phase, ConfPoolPhase::AwaitBodyOrLocalSig);
        assert!(p.put_sig(&h, "val1", "sig1", 1.1));
        assert_eq!(p.get(&h).unwrap().local_sig_at, 1.1);
        assert_eq!(p.get(&h).unwrap().phase, ConfPoolPhase::AwaitForeign);
        assert!(p.put_sig(&h, "val3", "sig3", 1.2));
        assert_eq!(p.get(&h).unwrap().phase, ConfPoolPhase::ForeignVisible);
        assert!(p.get(&h).unwrap().foreign_visible_at > 0.0);
    }

    #[test]
    fn creator_reannounce_and_parked_cert() {
        let mut p = ConfPool::new();
        p.set_local_id("val1");
        let tx = Transaction::payment("acct1", 1, "d", 1, 0, 1.0);
        let b = Batch::create(vec![tx], vec![genesis_batch_hash()], "val2".into(), 1.0);
        let h = b.batch_hash.clone();
        p.put_body(b, 1.0);
        p.put_sig(&h, "val1", "sig1", 1.1);
        assert!(p.note_creator_reannounce(&h, 1.2, 8));
        assert_eq!(p.get(&h).unwrap().creator_reannounce_count, 1);
        assert_eq!(p.get(&h).unwrap().phase, ConfPoolPhase::CreatorAckPending);
        p.note_parked_cert(&h, 1.3);
        assert_eq!(p.get(&h).unwrap().phase, ConfPoolPhase::ParkedCert);
        p.remove_with_reason(&h, RemoveReason::Certified);
        assert_eq!(p.counters.removed_certified, 1);
        assert!(p.is_empty());
    }

    #[test]
    fn zero_foreign_duration_for_own_open() {
        let mut p = ConfPool::new();
        p.set_local_id("val1");
        let tx = Transaction::payment("acct1", 1, "d", 1, 0, 1.0);
        let b = Batch::create(vec![tx], vec![genesis_batch_hash()], "val1".into(), 1.0);
        let h = b.batch_hash.clone();
        p.put_body(b, 1.0);
        p.put_sig(&h, "val1", "sig1", 1.0);
        let e = p.get(&h).unwrap();
        assert!(e.zero_foreign_since > 0.0);
        assert!(e.zero_foreign_duration_s(5.0) >= 3.9);
        let zf = p.own_zero_foreign("val1");
        assert_eq!(zf.len(), 1);
    }

    #[test]
    fn sign_defer_counters() {
        let mut p = ConfPool::new();
        p.note_sign_defer(SignDeferReason::Cap);
        p.note_sign_defer(SignDeferReason::Cap);
        p.note_sign_defer(SignDeferReason::Policy);
        assert_eq!(p.counters.sign_defer_cap, 2);
        assert_eq!(p.counters.sign_defer_policy, 1);
    }

    #[test]
    fn cleanup_after_certify_auditable() {
        let mut p = ConfPool::new();
        p.set_local_id("val1");
        let tx = Transaction::payment("acct1", 1, "d", 1, 0, 1.0);
        let b = Batch::create(vec![tx], vec![genesis_batch_hash()], "val1".into(), 1.0);
        let h = b.batch_hash.clone();
        p.put_body(b, 1.0);
        p.remove_with_reason(&h, RemoveReason::Certified);
        let j = p.to_json_at(2.0);
        assert_eq!(j["counters"]["removed_certified"], 1);
        assert_eq!(j["n"], 0);
    }

    #[test]
    fn status_json_exposes_phase() {
        let mut p = ConfPool::new();
        p.set_local_id("val1");
        let tx = Transaction::payment("acct1", 1, "d", 1, 0, 1.0);
        let b = Batch::create(vec![tx], vec![genesis_batch_hash()], "val1".into(), 1.0);
        p.put_body(b, 1.0);
        let j = p.to_json_at(2.0);
        assert_eq!(j["n"], 1);
        assert!(j["open"][0]["phase"].as_str().is_some());
        assert!(j["phase_counts"].is_object());
    }
}
