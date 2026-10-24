//! Global mempool: deterministic admission, ownership visibility, and repool-safe reinsert.
//!
//! PR2 contract (IMPLEMENTATION_GUIDE):
//! - Exact duplicates are harmless (`ExactDuplicate`).
//! - Same-account/sequence competitors are tracked explicitly.
//! - Owned vs not-owned pending counts are first-class status.
//! - Repool reinsertion preserves sequence intent and priority age.

use fd_types::{HashHex, ProtocolConfig, Transaction};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// How a tx entered (or re-entered) the mempool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntrySource {
    /// Local RPC / client submit.
    Local,
    /// Mesh peer or directed owner handoff.
    Mesh,
    /// Reinserted after open-body repool / abandon.
    Repool,
}

impl EntrySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Mesh => "mesh",
            Self::Repool => "repool",
        }
    }
}

/// Outcome of [`Mempool::add`] / [`Mempool::add_with_source`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    /// New tx_id accepted.
    Inserted,
    /// Same tx_id already pending or done — no-op, harmless.
    ExactDuplicate,
    /// New tx_id for an (account, sequence) that already has ≥1 other pending tx.
    /// Still inserted when capacity allows (callers see competitor signal).
    InsertedCompetitor,
    /// Capacity full (`max_mempool_txs`).
    Full,
}

impl AddOutcome {
    pub fn accepted(self) -> bool {
        matches!(self, Self::Inserted | Self::InsertedCompetitor)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inserted => "inserted",
            Self::ExactDuplicate => "exact_duplicate",
            Self::InsertedCompetitor => "inserted_competitor",
            Self::Full => "full",
        }
    }
}

/// Per-tx metadata for routing / age / source (PR2).
#[derive(Debug, Clone)]
pub struct EntryMeta {
    pub source: EntrySource,
    /// Wall time first admitted (or original first_seen preserved on repool).
    pub admitted_at: f64,
    /// Directed route-to-owner attempts while sitting not-owned (engine updates).
    pub route_attempts: u32,
    /// Last time a directed owner route was recorded (0 = never).
    pub last_route_at: f64,
}

/// Operator-facing mempool snapshot (owned vs not-owned + competitors).
#[derive(Debug, Clone)]
pub struct MempoolStatus {
    pub total: usize,
    pub owned_pending: usize,
    pub not_owned_pending: usize,
    /// Number of (account, sequence) keys with more than one distinct tx_id.
    pub competitor_groups: usize,
    /// Total pending txs that share a sequence with at least one other.
    pub competitor_txs: usize,
    pub oldest_age_s: f64,
    pub oldest_not_owned_age_s: f64,
    pub done_marked: usize,
    pub drops_full: u64,
    pub drops_duplicate: u64,
    pub inserts_ok: u64,
    pub inserts_competitor: u64,
    pub reinserts_ok: u64,
    pub reinserts_duplicate: u64,
}

impl MempoolStatus {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "total": self.total,
            "owned_pending": self.owned_pending,
            "not_owned_pending": self.not_owned_pending,
            "competitor_groups": self.competitor_groups,
            "competitor_txs": self.competitor_txs,
            "oldest_age_s": self.oldest_age_s,
            "oldest_not_owned_age_s": self.oldest_not_owned_age_s,
            "done_marked": self.done_marked,
            "drops_full": self.drops_full,
            "drops_duplicate": self.drops_duplicate,
            "inserts_ok": self.inserts_ok,
            "inserts_competitor": self.inserts_competitor,
            "reinserts_ok": self.reinserts_ok,
            "reinserts_duplicate": self.reinserts_duplicate,
        })
    }
}

pub struct Mempool {
    config: ProtocolConfig,
    txs: HashMap<HashHex, Transaction>,
    meta: HashMap<HashHex, EntryMeta>,
    by_acct_seq: BTreeMap<(String, u64), BTreeSet<HashHex>>,
    done: BTreeSet<HashHex>,
    // Counters (survives role separation; process-local accounting)
    pub drops_full: u64,
    pub drops_duplicate: u64,
    pub inserts_ok: u64,
    pub inserts_competitor: u64,
    pub reinserts_ok: u64,
    pub reinserts_duplicate: u64,
}

impl Mempool {
    pub fn new(config: ProtocolConfig) -> Self {
        Self {
            config,
            txs: HashMap::new(),
            meta: HashMap::new(),
            by_acct_seq: BTreeMap::new(),
            done: BTreeSet::new(),
            drops_full: 0,
            drops_duplicate: 0,
            inserts_ok: 0,
            inserts_competitor: 0,
            reinserts_ok: 0,
            reinserts_duplicate: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.txs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.txs.is_empty()
    }

    pub fn config(&self) -> &ProtocolConfig {
        &self.config
    }

    /// Configured account sequence window (visible to operators / tests).
    pub fn sequence_window(&self) -> u64 {
        self.config.account_sequence_window.max(1)
    }

    /// Legacy bool API (true = newly accepted).
    pub fn add(&mut self, tx: Transaction) -> bool {
        self.add_with_source(tx, EntrySource::Local, None).accepted()
    }

    /// Admit with explicit source. `now` defaults to `tx.first_seen` when `None`.
    pub fn add_with_source(
        &mut self,
        tx: Transaction,
        source: EntrySource,
        now: Option<f64>,
    ) -> AddOutcome {
        if self.txs.contains_key(&tx.tx_id) || self.done.contains(&tx.tx_id) {
            self.drops_duplicate = self.drops_duplicate.saturating_add(1);
            return AddOutcome::ExactDuplicate;
        }
        if self.txs.len() >= self.config.max_mempool_txs {
            self.drops_full = self.drops_full.saturating_add(1);
            return AddOutcome::Full;
        }
        let key = (tx.account.clone(), tx.sequence);
        let had_competitor = self
            .by_acct_seq
            .get(&key)
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let admitted_at = now.unwrap_or(tx.first_seen);
        self.by_acct_seq
            .entry(key)
            .or_default()
            .insert(tx.tx_id.clone());
        self.meta.insert(
            tx.tx_id.clone(),
            EntryMeta {
                source,
                admitted_at,
                route_attempts: 0,
                last_route_at: 0.0,
            },
        );
        self.txs.insert(tx.tx_id.clone(), tx);
        if had_competitor {
            self.inserts_competitor = self.inserts_competitor.saturating_add(1);
            AddOutcome::InsertedCompetitor
        } else {
            self.inserts_ok = self.inserts_ok.saturating_add(1);
            AddOutcome::Inserted
        }
    }

    /// Repool / reinsert path: preserves original `first_seen` for priority age.
    /// Does not silently drop sequence intent — exact duplicate is reported.
    pub fn reinsert(&mut self, mut tx: Transaction, now: f64) -> AddOutcome {
        // Keep original first_seen if already set; only stamp when missing/zero.
        if tx.first_seen <= 0.0 {
            tx.first_seen = now;
        }
        let out = self.add_with_source(tx, EntrySource::Repool, Some(now));
        match out {
            AddOutcome::Inserted | AddOutcome::InsertedCompetitor => {
                self.reinserts_ok = self.reinserts_ok.saturating_add(1);
            }
            AddOutcome::ExactDuplicate => {
                self.reinserts_duplicate = self.reinserts_duplicate.saturating_add(1);
            }
            AddOutcome::Full => {}
        }
        out
    }

    pub fn remove(&mut self, tx_id: &str) {
        if let Some(tx) = self.txs.remove(tx_id) {
            self.meta.remove(tx_id);
            let key = (tx.account.clone(), tx.sequence);
            if let Some(set) = self.by_acct_seq.get_mut(&key) {
                set.remove(tx_id);
                if set.is_empty() {
                    self.by_acct_seq.remove(&key);
                }
            }
        }
    }

    pub fn contains(&self, tx_id: &str) -> bool {
        self.txs.contains_key(tx_id)
    }

    pub fn get(&self, tx_id: &str) -> Option<&Transaction> {
        self.txs.get(tx_id)
    }

    pub fn meta(&self, tx_id: &str) -> Option<&EntryMeta> {
        self.meta.get(tx_id)
    }

    /// Record a directed route-to-owner attempt for a pending tx.
    pub fn note_route_attempt(&mut self, tx_id: &str, now: f64) {
        if let Some(m) = self.meta.get_mut(tx_id) {
            m.route_attempts = m.route_attempts.saturating_add(1);
            m.last_route_at = now;
        }
    }

    /// Distinct pending tx_ids competing on the same (account, sequence).
    pub fn competitors(&self, account: &str, sequence: u64) -> Vec<HashHex> {
        self.by_acct_seq
            .get(&(account.to_string(), sequence))
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn competitor_count(&self, account: &str, sequence: u64) -> usize {
        self.by_acct_seq
            .get(&(account.to_string(), sequence))
            .map(|s| s.len())
            .unwrap_or(0)
    }

    pub fn mark_done(&mut self, tx_ids: impl IntoIterator<Item = impl AsRef<str>>) {
        for tid in tx_ids {
            let tid = tid.as_ref().to_string();
            self.done.insert(tid.clone());
            if let Some(tx) = self.txs.get(&tid).cloned() {
                let comps: Vec<_> = self
                    .by_acct_seq
                    .get(&(tx.account.clone(), tx.sequence))
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default();
                for c in comps {
                    self.remove(&c);
                }
            } else {
                self.remove(&tid);
            }
        }
    }

    pub fn ordered(&self, now: f64) -> Vec<Transaction> {
        let mut v: Vec<_> = self.txs.values().cloned().collect();
        v.sort_by(|a, b| {
            let ka = priority_key(a, now, self.config.starvation_boost_per_s);
            let kb = priority_key(b, now, self.config.starvation_boost_per_s);
            kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    /// All pending txs (for snapshot).
    pub fn all_txs(&self) -> Vec<Transaction> {
        self.txs.values().cloned().collect()
    }

    pub fn mark_done_ids(&mut self, ids: impl IntoIterator<Item = impl AsRef<str>>) {
        self.mark_done(ids);
    }

    /// Build status using ownership predicate `owns(account) -> bool`.
    pub fn status(&self, now: f64, mut owns: impl FnMut(&str) -> bool) -> MempoolStatus {
        let mut owned = 0usize;
        let mut not_owned = 0usize;
        let mut oldest = 0.0_f64;
        let mut oldest_not_owned = 0.0_f64;
        for tx in self.txs.values() {
            let age = (now - tx.first_seen).max(0.0);
            if age > oldest {
                oldest = age;
            }
            if owns(&tx.account) {
                owned += 1;
            } else {
                not_owned += 1;
                if age > oldest_not_owned {
                    oldest_not_owned = age;
                }
            }
        }
        let mut competitor_groups = 0usize;
        let mut competitor_txs = 0usize;
        for set in self.by_acct_seq.values() {
            if set.len() > 1 {
                competitor_groups += 1;
                competitor_txs += set.len();
            }
        }
        MempoolStatus {
            total: self.txs.len(),
            owned_pending: owned,
            not_owned_pending: not_owned,
            competitor_groups,
            competitor_txs,
            oldest_age_s: oldest,
            oldest_not_owned_age_s: oldest_not_owned,
            done_marked: self.done.len(),
            drops_full: self.drops_full,
            drops_duplicate: self.drops_duplicate,
            inserts_ok: self.inserts_ok,
            inserts_competitor: self.inserts_competitor,
            reinserts_ok: self.reinserts_ok,
            reinserts_duplicate: self.reinserts_duplicate,
        }
    }

    /// True if `tx.sequence` is within [expected, expected + window).
    pub fn sequence_in_window(tx_seq: u64, expected: u64, window: u64) -> SequenceWindowCheck {
        let w = window.max(1);
        if tx_seq < expected {
            SequenceWindowCheck::Past
        } else if tx_seq >= expected.saturating_add(w) {
            SequenceWindowCheck::Future
        } else {
            SequenceWindowCheck::InWindow
        }
    }
}

/// Result of sequence-window admission check (visible/configurable via config window).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceWindowCheck {
    Past,
    InWindow,
    Future,
}

fn priority_key(tx: &Transaction, now: f64, boost: f64) -> (f64, f64, &str) {
    let age = (now - tx.first_seen).max(0.0);
    let density = tx.fee_density() + age * boost;
    (density, -tx.first_seen, tx.tx_id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_priority() {
        let mut mp = Mempool::new(ProtocolConfig::default());
        let low = Transaction::create("a", 1, "b", 1, 1, 0.0);
        let high = Transaction::create("c", 1, "d", 1, 50, 1.0);
        mp.add(low.clone());
        mp.add(high.clone());
        let o = mp.ordered(10.0);
        assert_eq!(o[0].tx_id, high.tx_id);
    }

    #[test]
    fn exact_duplicate_harmless() {
        let mut mp = Mempool::new(ProtocolConfig::default());
        let tx = Transaction::create("a", 1, "b", 1, 1, 0.0);
        assert_eq!(
            mp.add_with_source(tx.clone(), EntrySource::Local, Some(0.0)),
            AddOutcome::Inserted
        );
        assert_eq!(
            mp.add_with_source(tx, EntrySource::Local, Some(1.0)),
            AddOutcome::ExactDuplicate
        );
        assert_eq!(mp.len(), 1);
        assert_eq!(mp.drops_duplicate, 1);
    }

    #[test]
    fn sequence_competitors_tracked() {
        let mut mp = Mempool::new(ProtocolConfig::default());
        // Two different bodies for same account+seq (different destinations → different ids).
        let t1 = Transaction::create("acct", 1, "dest_a", 1, 1, 0.0);
        let t2 = Transaction::create("acct", 1, "dest_b", 1, 2, 0.0);
        assert_ne!(t1.tx_id, t2.tx_id);
        assert_eq!(
            mp.add_with_source(t1.clone(), EntrySource::Local, Some(0.0)),
            AddOutcome::Inserted
        );
        assert_eq!(
            mp.add_with_source(t2.clone(), EntrySource::Local, Some(0.1)),
            AddOutcome::InsertedCompetitor
        );
        assert_eq!(mp.competitor_count("acct", 1), 2);
        let st = mp.status(1.0, |_| true);
        assert_eq!(st.competitor_groups, 1);
        assert_eq!(st.competitor_txs, 2);
        // mark_done on one clears all competitors for that seq
        mp.mark_done([t1.tx_id.as_str()]);
        assert_eq!(mp.len(), 0);
    }

    #[test]
    fn owned_vs_not_owned_status() {
        let mut mp = Mempool::new(ProtocolConfig::default());
        mp.add(Transaction::create("mine", 1, "x", 1, 1, 0.0));
        mp.add(Transaction::create("theirs", 1, "y", 1, 1, 0.0));
        let st = mp.status(5.0, |a| a == "mine");
        assert_eq!(st.total, 2);
        assert_eq!(st.owned_pending, 1);
        assert_eq!(st.not_owned_pending, 1);
        assert!(st.oldest_not_owned_age_s >= 4.9);
    }

    #[test]
    fn reinsert_preserves_first_seen_and_sequence() {
        let mut mp = Mempool::new(ProtocolConfig::default());
        let tx = Transaction::create("acct", 7, "d", 1, 1, 10.0);
        assert_eq!(tx.sequence, 7);
        assert_eq!(tx.first_seen, 10.0);
        let out = mp.reinsert(tx.clone(), 100.0);
        assert!(out.accepted());
        let got = mp.get(&tx.tx_id).unwrap();
        assert_eq!(got.first_seen, 10.0); // not rewritten to 100
        assert_eq!(got.sequence, 7);
        assert_eq!(mp.meta(&tx.tx_id).unwrap().source, EntrySource::Repool);
        // second reinsert is exact duplicate — sequence intent not lost silently
        assert_eq!(mp.reinsert(tx, 101.0), AddOutcome::ExactDuplicate);
        assert_eq!(mp.reinserts_duplicate, 1);
    }

    #[test]
    fn sequence_window_check() {
        assert_eq!(
            Mempool::sequence_in_window(5, 5, 8),
            SequenceWindowCheck::InWindow
        );
        assert_eq!(
            Mempool::sequence_in_window(4, 5, 8),
            SequenceWindowCheck::Past
        );
        assert_eq!(
            Mempool::sequence_in_window(13, 5, 8),
            SequenceWindowCheck::Future
        );
    }

    #[test]
    fn full_drop_counted() {
        let mut cfg = ProtocolConfig::default();
        cfg.max_mempool_txs = 1;
        let mut mp = Mempool::new(cfg);
        assert!(mp
            .add_with_source(
                Transaction::create("a", 1, "b", 1, 1, 0.0),
                EntrySource::Local,
                Some(0.0)
            )
            .accepted());
        assert_eq!(
            mp.add_with_source(
                Transaction::create("c", 1, "d", 1, 1, 0.0),
                EntrySource::Local,
                Some(0.0)
            ),
            AddOutcome::Full
        );
        assert_eq!(mp.drops_full, 1);
    }

    #[test]
    fn route_attempt_metadata() {
        let mut mp = Mempool::new(ProtocolConfig::default());
        let tx = Transaction::create("foreign", 1, "x", 1, 1, 1.0);
        let id = tx.tx_id.clone();
        mp.add_with_source(tx, EntrySource::Mesh, Some(1.0));
        mp.note_route_attempt(&id, 2.0);
        mp.note_route_attempt(&id, 3.0);
        let m = mp.meta(&id).unwrap();
        assert_eq!(m.route_attempts, 2);
        assert_eq!(m.last_route_at, 3.0);
    }
}
