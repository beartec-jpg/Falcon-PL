//! Global Mempool (GMP-B): multi-shard pending intent + capacity-weighted leases.
//! Global Conf Pool registry (GCP-B v0): shared open-batch / sig / cert records.

pub mod client;
pub mod http_util;
pub mod router;

pub use client::{GcpClient, GmpClient, NodeRpcClient};
pub use router::{AccountRouter, ShardAssign, ShardEndpoint};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn wall_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// FNV-1a 32-bit (match lane tooling).
pub fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GmpTx {
    pub account: String,
    pub sequence: u64,
    pub destination: String,
    pub amount: u64,
    pub fee: u64,
    #[serde(default)]
    pub tx_id: String,
}

impl GmpTx {
    pub fn ensure_id(&mut self) {
        if self.tx_id.is_empty() {
            let raw = format!(
                "{}:{}:{}:{}:{}",
                self.account, self.sequence, self.destination, self.amount, self.fee
            );
            let dig = Sha256::digest(raw.as_bytes());
            self.tx_id = hex::encode(&dig[..16]);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapacityAdvert {
    pub validator_id: String,
    pub mem_budget_mb: u64,
    pub mem_used_mb: u64,
    pub open_uncertified: u64,
    pub max_in_flight_leases: u64,
    #[serde(default)]
    pub recent_clearance_tps: f64,
    #[serde(default)]
    pub lease_expiry_rate: f64,
    #[serde(default)]
    pub ts: f64,
}

#[derive(Debug, Clone)]
struct PendingTx {
    tx: GmpTx,
    admitted_at: f64,
}

#[derive(Debug, Clone)]
struct Lease {
    lease_id: String,
    validator_id: String,
    tx_ids: Vec<String>,
    issued_at: f64,
    ttl_s: f64,
}

struct Shard {
    pending: VecDeque<PendingTx>,
    by_tx: HashMap<String, PendingTx>,
    /// (account, seq) -> tx_id while pending or leased
    acct_seq: HashMap<(String, u64), String>,
    leased_tx: HashMap<String, String>, // tx_id -> lease_id
}

impl Shard {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            by_tx: HashMap::new(),
            acct_seq: HashMap::new(),
            leased_tx: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct AdvertState {
    advert: CapacityAdvert,
    weight: f64,
    in_flight: u64,
    completed: u64,
    expired: u64,
}

/// GMP-B: multi-shard global mempool + capacity-weighted issuance.
pub struct GlobalMempool {
    num_shards: usize,
    shards: Vec<Mutex<Shard>>,
    leases: Mutex<HashMap<String, Lease>>,
    adverts: Mutex<HashMap<String, AdvertState>>,
    max_pending: usize,
    lease_ttl_s: f64,
    clear_ref_tps: f64,
    min_share: f64,
    max_share: f64,
    next_lease: Mutex<u64>,
}

impl GlobalMempool {
    pub fn new(num_shards: usize, max_pending: usize, lease_ttl_s: f64) -> Arc<Self> {
        let n = num_shards.max(1);
        let mut shards = Vec::with_capacity(n);
        for _ in 0..n {
            shards.push(Mutex::new(Shard::new()));
        }
        Arc::new(Self {
            num_shards: n,
            shards,
            leases: Mutex::new(HashMap::new()),
            adverts: Mutex::new(HashMap::new()),
            max_pending,
            lease_ttl_s,
            clear_ref_tps: 20.0,
            min_share: 0.05,
            max_share: 0.40,
            next_lease: Mutex::new(1),
        })
    }

    fn shard_idx(&self, account: &str) -> usize {
        (fnv1a(account) as usize) % self.num_shards
    }

    pub fn pending_count(&self) -> usize {
        self.shards
            .iter()
            .map(|s| s.lock().pending.len())
            .sum()
    }

    pub fn submit(&self, mut tx: GmpTx) -> Result<serde_json::Value, String> {
        tx.ensure_id();
        if self.pending_count() >= self.max_pending {
            return Err("terBUSY:gmp_full".into());
        }
        let si = self.shard_idx(&tx.account);
        let mut sh = self.shards[si].lock();
        let key = (tx.account.clone(), tx.sequence);
        if sh.acct_seq.contains_key(&key) {
            return Err("tefALREADY:account_seq_pending".into());
        }
        if sh.by_tx.contains_key(&tx.tx_id) {
            return Ok(serde_json::json!({
                "tx_id": tx.tx_id,
                "shard": si,
                "status": "duplicate"
            }));
        }
        let pending = PendingTx {
            tx: tx.clone(),
            admitted_at: wall_now(),
        };
        sh.acct_seq.insert(key, tx.tx_id.clone());
        sh.by_tx.insert(tx.tx_id.clone(), pending.clone());
        sh.pending.push_back(pending);
        Ok(serde_json::json!({
            "tx_id": tx.tx_id,
            "shard": si,
            "status": "pending"
        }))
    }

    pub fn upsert_capacity(&self, mut adv: CapacityAdvert) -> serde_json::Value {
        if adv.ts <= 0.0 {
            adv.ts = wall_now();
        }
        let headroom = if adv.mem_budget_mb == 0 {
            0.0
        } else {
            ((adv.mem_budget_mb as f64 - adv.mem_used_mb as f64).max(0.0))
                / (adv.mem_budget_mb as f64)
        };
        let clear = (adv.recent_clearance_tps / self.clear_ref_tps).clamp(0.0, 2.0);
        let penalty = adv.lease_expiry_rate.clamp(0.0, 1.0);
        let weight = (headroom * (0.5 + 0.5 * clear) * (1.0 - 0.5 * penalty)).max(0.01);

        let mut map = self.adverts.lock();
        let st = map.entry(adv.validator_id.clone()).or_insert(AdvertState {
            advert: adv.clone(),
            weight,
            in_flight: 0,
            completed: 0,
            expired: 0,
        });
        st.advert = adv;
        st.weight = weight;
        serde_json::json!({ "ok": true, "weight": weight, "in_flight": st.in_flight })
    }

    fn expire_leases(&self, now: f64) {
        let mut leases = self.leases.lock();
        let mut expired_ids = Vec::new();
        for (id, lease) in leases.iter() {
            if now - lease.issued_at >= lease.ttl_s {
                expired_ids.push(id.clone());
            }
        }
        for id in expired_ids {
            if let Some(lease) = leases.remove(&id) {
                {
                    let mut adv = self.adverts.lock();
                    if let Some(st) = adv.get_mut(&lease.validator_id) {
                        st.in_flight = st.in_flight.saturating_sub(lease.tx_ids.len() as u64);
                        st.expired = st.expired.saturating_add(1);
                        let total = (st.completed + st.expired).max(1) as f64;
                        st.advert.lease_expiry_rate = st.expired as f64 / total;
                    }
                }
                for tid in &lease.tx_ids {
                    // return to pending front of appropriate shard
                    for sh in &self.shards {
                        let mut s = sh.lock();
                        if let Some(p) = s.by_tx.get(tid).cloned() {
                            s.leased_tx.remove(tid);
                            // avoid dup in queue
                            if !s.pending.iter().any(|x| x.tx.tx_id == *tid) {
                                s.pending.push_front(p);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Lease up to `max_n` pending txs.
    /// Soft affinity (optional): when `affinity_mod`/`affinity_idx` set, prefer accounts
    /// with `fnv1a(account) % affinity_mod == affinity_idx` first, then fill remainder.
    pub fn lease(
        &self,
        validator_id: &str,
        max_n: usize,
        affinity_mod: Option<usize>,
        affinity_idx: Option<usize>,
    ) -> Result<serde_json::Value, String> {
        let now = wall_now();
        self.expire_leases(now);

        let pending = self.pending_count() as f64;
        let (weight, max_in_flight, in_flight, share_cap) = {
            let adv = self.adverts.lock();
            let st = adv
                .get(validator_id)
                .ok_or_else(|| "no_capacity_advert".to_string())?;
            // freshness: 5s
            if now - st.advert.ts > 5.0 {
                return Err("stale_capacity_advert".into());
            }
            let total_w: f64 = adv.values().map(|s| s.weight).sum::<f64>().max(1e-9);
            let share = (st.weight / total_w).clamp(self.min_share, self.max_share);
            let share_cap = ((pending * share).ceil() as usize).max(1);
            (
                st.weight,
                st.advert.max_in_flight_leases,
                st.in_flight,
                share_cap,
            )
        };

        let _ = weight;
        let room = max_in_flight.saturating_sub(in_flight) as usize;
        let want = max_n.min(room).min(share_cap).min(64);
        if want == 0 {
            return Ok(serde_json::json!({
                "lease_id": "",
                "txs": [],
                "ttl_s": self.lease_ttl_s,
                "reason": "at_in_flight_cap"
            }));
        }

        let mut taken: Vec<GmpTx> = Vec::new();
        let mut taken_ids: Vec<String> = Vec::new();
        let aff = match (affinity_mod, affinity_idx) {
            (Some(m), Some(i)) if m > 0 => Some((m, i % m)),
            _ => None,
        };

        let take_from_pending = |sh: &mut Shard,
                                 taken: &mut Vec<GmpTx>,
                                 taken_ids: &mut Vec<String>,
                                 want: usize,
                                 prefer_only: bool| {
            let mut put_back: VecDeque<PendingTx> = VecDeque::new();
            while taken.len() < want {
                let Some(p) = sh.pending.pop_front() else {
                    break;
                };
                if sh.leased_tx.contains_key(&p.tx.tx_id) {
                    continue;
                }
                if prefer_only {
                    if let Some((m, i)) = aff {
                        if (fnv1a(&p.tx.account) as usize) % m != i {
                            put_back.push_back(p);
                            continue;
                        }
                    }
                }
                sh.leased_tx
                    .insert(p.tx.tx_id.clone(), String::new()); // placeholder
                taken_ids.push(p.tx.tx_id.clone());
                taken.push(p.tx);
            }
            // restore non-matching to front in original order
            while let Some(p) = put_back.pop_back() {
                sh.pending.push_front(p);
            }
        };

        // Round-robin shards for fairness across accounts
        let start = (fnv1a(validator_id) as usize) % self.num_shards;
        // Pass 1: soft affinity preferred accounts
        if aff.is_some() {
            let mut guard = 0;
            let mut si = start;
            while taken.len() < want && guard < self.num_shards * 2 {
                {
                    let mut sh = self.shards[si].lock();
                    take_from_pending(&mut sh, &mut taken, &mut taken_ids, want, true);
                }
                si = (si + 1) % self.num_shards;
                guard += 1;
            }
        }
        // Pass 2: fill remainder from any pending (soft affinity overflow)
        {
            let mut guard = 0;
            let mut si = start;
            while taken.len() < want && guard < self.num_shards * 4 {
                {
                    let mut sh = self.shards[si].lock();
                    take_from_pending(&mut sh, &mut taken, &mut taken_ids, want, false);
                }
                si = (si + 1) % self.num_shards;
                guard += 1;
            }
        }

        if taken.is_empty() {
            return Ok(serde_json::json!({
                "lease_id": "",
                "txs": [],
                "ttl_s": self.lease_ttl_s,
                "reason": "empty"
            }));
        }

        let lease_id = {
            let mut n = self.next_lease.lock();
            let id = format!("L{}-{}", *n, validator_id);
            *n += 1;
            id
        };

        for tid in &taken_ids {
            for sh in &self.shards {
                let mut s = sh.lock();
                if s.leased_tx.contains_key(tid) {
                    s.leased_tx.insert(tid.clone(), lease_id.clone());
                }
            }
        }

        {
            let mut leases = self.leases.lock();
            leases.insert(
                lease_id.clone(),
                Lease {
                    lease_id: lease_id.clone(),
                    validator_id: validator_id.to_string(),
                    tx_ids: taken_ids.clone(),
                    issued_at: now,
                    ttl_s: self.lease_ttl_s,
                },
            );
        }
        {
            let mut adv = self.adverts.lock();
            if let Some(st) = adv.get_mut(validator_id) {
                st.in_flight = st.in_flight.saturating_add(taken.len() as u64);
            }
        }

        Ok(serde_json::json!({
            "lease_id": lease_id,
            "txs": taken,
            "ttl_s": self.lease_ttl_s,
            "n": taken.len()
        }))
    }

    pub fn complete(&self, lease_id: &str, _batch_hash: Option<&str>) -> Result<(), String> {
        let lease = {
            let mut leases = self.leases.lock();
            leases
                .remove(lease_id)
                .ok_or_else(|| "unknown_lease".to_string())?
        };
        for tid in &lease.tx_ids {
            for sh in &self.shards {
                let mut s = sh.lock();
                if let Some(p) = s.by_tx.remove(tid) {
                    s.acct_seq.remove(&(p.tx.account, p.tx.sequence));
                    s.leased_tx.remove(tid);
                }
            }
        }
        let mut adv = self.adverts.lock();
        if let Some(st) = adv.get_mut(&lease.validator_id) {
            st.in_flight = st.in_flight.saturating_sub(lease.tx_ids.len() as u64);
            st.completed = st.completed.saturating_add(1);
        }
        Ok(())
    }

    pub fn nack(&self, lease_id: &str, _reason: &str) -> Result<(), String> {
        let lease = {
            let mut leases = self.leases.lock();
            leases
                .remove(lease_id)
                .ok_or_else(|| "unknown_lease".to_string())?
        };
        for tid in &lease.tx_ids {
            for sh in &self.shards {
                let mut s = sh.lock();
                if let Some(p) = s.by_tx.get(tid).cloned() {
                    s.leased_tx.remove(tid);
                    if !s.pending.iter().any(|x| x.tx.tx_id == *tid) {
                        s.pending.push_front(p);
                    }
                }
            }
        }
        let mut adv = self.adverts.lock();
        if let Some(st) = adv.get_mut(&lease.validator_id) {
            st.in_flight = st.in_flight.saturating_sub(lease.tx_ids.len() as u64);
            st.expired = st.expired.saturating_add(1);
        }
        Ok(())
    }

    pub fn status(&self) -> serde_json::Value {
        self.expire_leases(wall_now());
        let pending = self.pending_count();
        let leases = self.leases.lock().len();
        let adv = self.adverts.lock();
        let mut weights = BTreeMap::new();
        for (k, v) in adv.iter() {
            weights.insert(
                k.clone(),
                serde_json::json!({
                    "weight": v.weight,
                    "in_flight": v.in_flight,
                    "completed": v.completed,
                    "expired": v.expired,
                    "mem_budget_mb": v.advert.mem_budget_mb,
                    "mem_used_mb": v.advert.mem_used_mb,
                    "advert_age_s": wall_now() - v.advert.ts,
                }),
            );
        }
        serde_json::json!({
            "pending": pending,
            "active_leases": leases,
            "num_shards": self.num_shards,
            "max_pending": self.max_pending,
            "validators": weights,
        })
    }
}

// ----- GCP-B v0 registry -----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfBatchRec {
    pub batch_hash: String,
    pub creator: String,
    pub ntx: u64,
    pub parents: Vec<String>,
    #[serde(default)]
    pub signers: BTreeMap<String, String>,
    #[serde(default)]
    pub certified: bool,
    pub ts: f64,
}

pub struct ConfRegistry {
    inner: Mutex<HashMap<String, ConfBatchRec>>,
}

impl ConfRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
        })
    }

    pub fn put_batch(&self, mut rec: ConfBatchRec) -> serde_json::Value {
        if rec.ts <= 0.0 {
            rec.ts = wall_now();
        }
        let mut m = self.inner.lock();
        let e = m.entry(rec.batch_hash.clone()).or_insert(rec.clone());
        if e.creator.is_empty() {
            e.creator = rec.creator;
        }
        e.ntx = rec.ntx;
        e.parents = rec.parents;
        serde_json::json!({ "ok": true, "batch_hash": e.batch_hash })
    }

    pub fn put_sig(&self, batch_hash: &str, vid: &str, sig: &str) -> Result<(), String> {
        let mut m = self.inner.lock();
        let e = m
            .get_mut(batch_hash)
            .ok_or_else(|| "unknown_batch".to_string())?;
        e.signers.insert(vid.to_string(), sig.to_string());
        Ok(())
    }

    pub fn put_cert(&self, batch_hash: &str) -> Result<(), String> {
        let mut m = self.inner.lock();
        let e = m
            .get_mut(batch_hash)
            .ok_or_else(|| "unknown_batch".to_string())?;
        e.certified = true;
        Ok(())
    }

    pub fn list_open(&self) -> Vec<ConfBatchRec> {
        self.inner
            .lock()
            .values()
            .filter(|r| !r.certified)
            .cloned()
            .collect()
    }

    pub fn status(&self) -> serde_json::Value {
        let m = self.inner.lock();
        let open = m.values().filter(|r| !r.certified).count();
        let cert = m.values().filter(|r| r.certified).count();
        serde_json::json!({ "open": open, "certified": cert, "total": m.len() })
    }
}

impl Default for ConfRegistry {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_exclusive_and_complete() {
        let g = GlobalMempool::new(4, 1000, 2.0);
        g.upsert_capacity(CapacityAdvert {
            validator_id: "val1".into(),
            mem_budget_mb: 2048,
            mem_used_mb: 100,
            open_uncertified: 0,
            max_in_flight_leases: 32,
            recent_clearance_tps: 10.0,
            lease_expiry_rate: 0.0,
            ts: wall_now(),
        });
        for i in 1..=5 {
            g.submit(GmpTx {
                account: format!("acct{i}"),
                sequence: 1,
                destination: "acct40".into(),
                amount: 1,
                fee: 10,
                tx_id: String::new(),
            })
            .unwrap();
        }
        let l1 = g.lease("val1", 3, None, None).unwrap();
        let n = l1["n"].as_u64().unwrap_or(0);
        assert!(n >= 1);
        let lid = l1["lease_id"].as_str().unwrap();
        g.complete(lid, None).unwrap();
        assert!(g.pending_count() <= 5);
    }

    #[test]
    fn soft_affinity_prefers_matching_accounts() {
        let g = GlobalMempool::new(8, 1000, 2.0);
        g.upsert_capacity(CapacityAdvert {
            validator_id: "val1".into(),
            mem_budget_mb: 4096,
            mem_used_mb: 100,
            open_uncertified: 0,
            max_in_flight_leases: 64,
            recent_clearance_tps: 10.0,
            lease_expiry_rate: 0.0,
            ts: wall_now(),
        });
        for i in 1..=30 {
            g.submit(GmpTx {
                account: format!("acct{i}"),
                sequence: 1,
                destination: "acct40".into(),
                amount: 1,
                fee: 10,
                tx_id: String::new(),
            })
            .unwrap();
        }
        let mod_n = 3usize;
        let idx = 0usize;
        let l = g.lease("val1", 5, Some(mod_n), Some(idx)).unwrap();
        let txs = l["txs"].as_array().unwrap();
        assert!(!txs.is_empty());
        // First pass should fill with affinity matches when available
        let mut aff = 0usize;
        for t in txs {
            let acct = t["account"].as_str().unwrap();
            if (fnv1a(acct) as usize) % mod_n == idx {
                aff += 1;
            }
        }
        assert!(aff >= 1, "expected at least one affinity match, got {txs:?}");
    }

    #[test]
    fn no_double_account_seq() {
        let g = GlobalMempool::new(4, 1000, 2.0);
        g.submit(GmpTx {
            account: "acct1".into(),
            sequence: 1,
            destination: "acct2".into(),
            amount: 1,
            fee: 10,
            tx_id: String::new(),
        })
        .unwrap();
        let err = g
            .submit(GmpTx {
                account: "acct1".into(),
                sequence: 1,
                destination: "acct3".into(),
                amount: 1,
                fee: 10,
                tx_id: String::new(),
            })
            .unwrap_err();
        assert!(err.contains("tefALREADY"));
    }
}
