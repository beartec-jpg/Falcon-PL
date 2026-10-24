//! Concurrent engine hub — subsystem locks with fixed acquisition order.
//!
//! # Lock order (never reverse; never hold across `.await`)
//! 1. `mempool` (RwLock)
//! 2. `dag` (RwLock)
//! 3. `ordering` (Mutex)
//! 4. `execution` (RwLock)
//! 5. `meta` (Mutex)
//!
//! ## Concurrency
//! - `advance_da` only needs `dag` write → can run beside pure metrics reads.
//! - Complex multi-field mutations use `with_all_mut` (exclusive, order-safe).
//! - Subsystems are stored as `Option<T>` so `with_all_mut` can **take** without
//!   allocating placeholder DAG/mempool on every call (that path was a live TPS cliff).

use crate::conf_pool::ConfPool;
use crate::engine::{GossipEvents, LagMetrics, MeshDiag, SubmitResult};
use fd_crypto::SharedWallet;
use fd_dag::CertificateDag;
use fd_exec::ExecutionEngine;
use fd_mempool::Mempool;
use fd_order::OrderingEngine;
use fd_types::{
    genesis_batch_hash, Batch, Certificate, NodeId, NodeRole, ProtocolConfig, Transaction,
};
use parking_lot::{Mutex, RwLock};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

struct Meta {
    role: NodeRole,
    wallet: Option<SharedWallet>,
    auto_sign: bool,
    last_batch_ts: f64,
    da_pack_seq: BTreeMap<String, u64>,
    /// Multi-lane: time-box for DA frontier pack stall.
    lane_frontier_incomplete_since: f64,
    /// Phase 49b: create_batch early-return / success counters (persist across ticks).
    pack_skip_reasons: BTreeMap<String, u64>,
    /// Phase 49b: admit outcome histogram (persist across ticks).
    admit_reasons: BTreeMap<String, u64>,
    /// Local/RPC accept count for admit auto-regulation (lag vs executed).
    local_admits_ok: u64,
    local_admits_owned: u64,
    local_admits_not_owned: u64,
    /// PR2: last terBUSY sub-reason + histogram.
    last_busy_cause: Option<crate::engine::BusyCause>,
    busy_causes: BTreeMap<String, u64>,
    /// Hysteresis: over high watermark until lag drains to resume.
    admit_reg_latched: bool,
    /// Phase 49c: pack hole first-seen times.
    pack_hole_since: BTreeMap<String, f64>,
    /// Phase 49d: mesh diagnostics (pending certs + lag samples).
    mesh_diag: MeshDiag,
    /// Multipack ownership flow counters.
    shard_flow: crate::engine::ShardFlow,
    conf_pool: ConfPool,
    // A1
    pending_remote_sigs: BTreeMap<String, Vec<(String, String)>>,
    foreign_sig_merged_from_sig: u64,
    foreign_sig_merged_from_body: u64,
    foreign_sig_parked: u64,
    foreign_sig_applied_parked: u64,
    // A2
    last_creator_sig_reannounce: BTreeMap<String, f64>,
    sig_reannounce_to_creator: u64,
    // A3
    last_reseed_body_at: BTreeMap<String, f64>,
    reseed_suppress_until: BTreeMap<String, f64>,
    bodies_served_at_progress: u64,
    foreign_merged_at_progress: u64,
    reseed_suppressed: u64,
    reseed_allowed: u64,
    last_open_da_activity: BTreeMap<String, f64>,
    open_body_repool_suppressed: u64,
    last_abandoned_open_hashes: Vec<String>,
    on_remote_sig_calls: u64,
    try_certify_attempts: u64,
    try_certify_ok: u64,
    force_open_try_certify: u64,
    last_foreign_cert_reannounce: BTreeMap<String, f64>,
    pending_creator_cert_announce: Vec<(String, String)>,
    cert_directed_to_creator: u64,
}


pub struct EngineParts {
    pub config: ProtocolConfig,
    pub validator_id: NodeId,
    pub worker_index: usize,
    pub worker_count: usize,
    /// Multipack packer set (may include non-UNL). Empty = use UNL.
    pub packer_ids: Vec<NodeId>,
    /// `None` only while `with_all_mut` holds all write locks (other waiters block).
    pub mempool: RwLock<Option<Mempool>>,
    pub dag: RwLock<Option<CertificateDag>>,
    pub ordering: Mutex<Option<OrderingEngine>>,
    pub execution: RwLock<Option<ExecutionEngine>>,
    meta: Mutex<Meta>,
    pub batches_created: AtomicU64,
    pub ledger_commits: AtomicU64,
    pub rejected_tx_sigs: AtomicU64,
    pub lag: Mutex<LagMetrics>,
}

impl EngineParts {
    pub fn from_sequential(eng: crate::engine::NodeEngine) -> Arc<Self> {
        Arc::new(Self {
            config: eng.config.clone(),
            validator_id: eng.validator_id.clone(),
            worker_index: eng.worker_index,
            worker_count: eng.worker_count,
            packer_ids: eng.packer_ids.clone(),
            mempool: RwLock::new(Some(eng.mempool)),
            dag: RwLock::new(Some(eng.dag)),
            ordering: Mutex::new(Some(eng.ordering)),
            execution: RwLock::new(Some(eng.execution)),
            meta: Mutex::new(Meta {
                role: eng.role,
                wallet: eng.wallet,
                auto_sign: eng.auto_sign,
                last_batch_ts: eng.last_batch_ts,
                da_pack_seq: eng.da_pack_seq,
                lane_frontier_incomplete_since: eng.lane_frontier_incomplete_since,
                pack_skip_reasons: eng.pack_skip_reasons,
                admit_reasons: eng.admit_reasons,
                local_admits_ok: eng.local_admits_ok,
                local_admits_owned: eng.local_admits_owned,
                local_admits_not_owned: eng.local_admits_not_owned,
                last_busy_cause: eng.last_busy_cause,
                busy_causes: eng.busy_causes,
                admit_reg_latched: eng.admit_circuit_latched,
                pack_hole_since: eng.pack_hole_since,
                mesh_diag: eng.mesh_diag,
                shard_flow: eng.shard_flow,
                conf_pool: eng.conf_pool,
                pending_remote_sigs: eng.pending_remote_sigs,
                foreign_sig_merged_from_sig: eng.foreign_sig_merged_from_sig,
                foreign_sig_merged_from_body: eng.foreign_sig_merged_from_body,
                foreign_sig_parked: eng.foreign_sig_parked,
                foreign_sig_applied_parked: eng.foreign_sig_applied_parked,
                last_creator_sig_reannounce: eng.last_creator_sig_reannounce,
                sig_reannounce_to_creator: eng.sig_reannounce_to_creator,
                last_reseed_body_at: eng.last_reseed_body_at,
                reseed_suppress_until: eng.reseed_suppress_until,
                bodies_served_at_progress: eng.bodies_served_at_progress,
                foreign_merged_at_progress: eng.foreign_merged_at_progress,
                reseed_suppressed: eng.reseed_suppressed,
                reseed_allowed: eng.reseed_allowed,
                last_open_da_activity: eng.last_open_da_activity,
                open_body_repool_suppressed: eng.open_body_repool_suppressed,
                last_abandoned_open_hashes: eng.last_abandoned_open_hashes,
                on_remote_sig_calls: eng.on_remote_sig_calls,
                try_certify_attempts: eng.try_certify_attempts,
                try_certify_ok: eng.try_certify_ok,
                force_open_try_certify: eng.force_open_try_certify,
                last_foreign_cert_reannounce: eng.last_foreign_cert_reannounce,
                pending_creator_cert_announce: eng.pending_creator_cert_announce,
                cert_directed_to_creator: eng.cert_directed_to_creator,
            }),
            batches_created: AtomicU64::new(eng.batches_created),
            ledger_commits: AtomicU64::new(eng.ledger_commits),
            rejected_tx_sigs: AtomicU64::new(eng.rejected_tx_sigs),
            lag: Mutex::new(LagMetrics::default()),
        })
    }

    pub fn role(&self) -> NodeRole {
        self.meta.lock().role
    }

    pub fn set_role(&self, role: NodeRole) {
        self.meta.lock().role = role;
    }

    pub fn max_cert_round(&self) -> u64 {
        self.dag
            .read()
            .as_ref()
            .map(|d| d.certificates.values().map(|c| c.round).max().unwrap_or(0))
            .unwrap_or(0)
    }

    pub fn cert_count(&self) -> usize {
        self.dag
            .read()
            .as_ref()
            .map(|d| d.certificates.len())
            .unwrap_or(0)
    }

    pub fn executed_len(&self) -> usize {
        self.execution
            .read()
            .as_ref()
            .map(|e| e.executed_txs.len())
            .unwrap_or(0)
    }

    pub fn ordered_len(&self) -> usize {
        self.ordering
            .lock()
            .as_ref()
            .map(|o| o.ordered.len())
            .unwrap_or(0)
    }

    pub fn tip_bft_snapshot(&self) -> (usize, u64, String) {
        let g = self.ordering.lock();
        let o = g.as_ref().expect("ordering present");
        (
            o.tip_bft.commits.len(),
            o.tip_bft.next_round,
            o.tip_bft.last_commit.clone(),
        )
    }

    pub fn crypto_scheme(&self) -> String {
        self.dag
            .read()
            .as_ref()
            .map(|d| d.crypto.scheme().to_string())
            .unwrap_or_else(|| "unknown".into())
    }

    pub fn crypto_arc(&self) -> fd_crypto::SharedCrypto {
        std::sync::Arc::clone(
            &self
                .dag
                .read()
                .as_ref()
                .expect("dag present")
                .crypto,
        )
    }

    pub fn has_batch(&self, h: &str) -> bool {
        self.dag
            .read()
            .as_ref()
            .map(|d| d.batches.contains_key(h))
            .unwrap_or(false)
    }

    pub fn get_batch(&self, h: &str) -> Option<Batch> {
        self.dag
            .read()
            .as_ref()
            .and_then(|d| d.batches.get(h).cloned())
    }

    pub fn has_cert_for_batch(&self, h: &str) -> bool {
        self.dag
            .read()
            .as_ref()
            .map(|d| d.certificates.contains_key(h))
            .unwrap_or(false)
    }

    pub fn get_cert_for_batch(&self, h: &str) -> Option<Certificate> {
        self.dag
            .read()
            .as_ref()
            .and_then(|d| d.certificates.get(h).cloned())
    }

    /// DA certify path. Uses full engine so newly-formed certs are announced
    /// (flake-hunt: note_cert_applied on try_certify success).
    pub fn advance_da(&self) {
        if !self.role().runs_da() {
            return;
        }
        self.with_all_mut(|e| e.advance_da());
    }

    /// Ledger path. Exclusive bridge; DA certify can still interleave with metrics reads.
    pub fn advance_ledger(&self) -> GossipEvents {
        if !self.role().runs_ledger() {
            return GossipEvents::default();
        }
        self.with_all_mut(|e| e.advance_ledger())
    }

    pub fn create_batch(&self, now: f64, force: bool) -> Option<(Batch, GossipEvents)> {
        self.with_all_mut(|e| e.create_batch(now, force))
    }

    pub fn submit_tx(&self, tx: Transaction) -> SubmitResult {
        self.with_all_mut(|e| e.submit_tx(tx))
    }

    /// Mesh / directed handoff — bypasses admit circuit breaker.
    pub fn submit_tx_mesh(&self, tx: Transaction) -> SubmitResult {
        self.with_all_mut(|e| e.submit_tx_mesh(tx))
    }

    /// True when running legacy single-packer (mesh-flood txs is OK).
    pub fn single_packer_mode(&self) -> bool {
        self.config.single_packer_per_round
    }

    pub fn waiting_open_conf(&self) -> bool {
        self.with_all_mut(|e| e.waiting_open_conf())
    }

    pub fn open_needs_body_seed(&self) -> bool {
        self.with_all_mut(|e| e.open_needs_body_seed())
    }

    pub fn note_body_da_progress(&self, now: f64, bodies_served: u64) {
        self.with_all_mut(|e| e.note_body_da_progress(now, bodies_served))
    }

    pub fn should_reseed_open_body(&self, batch_hash: &str, now: f64) -> bool {
        self.with_all_mut(|e| e.should_reseed_open_body(batch_hash, now))
    }

    pub fn open_unseeded_max_age_s(&self, now: f64) -> Option<f64> {
        self.with_all_mut(|e| e.open_unseeded_max_age_s(now))
    }
    pub fn advance_da_conf(&self, now: f64) -> GossipEvents {
        self.with_all_mut(|e| e.advance_da_conf(now))
    }


    /// Own open uncertified batch if any (clone for directed seed).
    pub fn own_open_batch(&self) -> Option<Batch> {
        self.with_all_mut(|e| {
            let bh = e.own_open_batch_hash()?;
            e.dag.batches.get(&bh).cloned()
        })
    }

    /// All own open uncertified batches (directed reseed every open, not just first).
    pub fn own_open_batches(&self) -> Vec<Batch> {
        self.with_all_mut(|e| e.own_open_batches())
    }

    pub fn own_open_batch_hashes(&self) -> Vec<fd_types::HashHex> {
        self.with_all_mut(|e| e.own_open_batch_hashes())
    }

    pub fn batch_creator(&self, h: &str) -> Option<fd_types::NodeId> {
        self.with_engine(|e| e.batch_creator(h))
    }

    /// Age-out own opens with 0 foreign UNL sigs → repool txs (body-DA escape).
    pub fn recover_stale_open_bodies(&self, now: f64) -> u64 {
        self.with_all_mut(|e| e.recover_stale_open_bodies(now))
    }

    pub fn advance(&self) -> GossipEvents {
        if self.role().runs_da() {
            self.advance_da();
        }
        if self.role().runs_ledger() {
            return self.advance_ledger();
        }
        GossipEvents::default()
    }

    pub fn on_remote_batch(&self, batch: Batch, now: f64) -> GossipEvents {
        self.with_all_mut(|e| e.on_remote_batch(batch, now))
    }

    pub fn on_remote_sig(&self, bh: &str, vid: &str, sig: &str) -> GossipEvents {
        self.with_all_mut(|e| e.on_remote_sig(bh, vid, sig))
    }

    pub fn on_remote_cert(&self, cert: Certificate) -> Vec<String> {
        self.with_all_mut(|e| e.on_remote_cert(cert))
    }

    pub fn on_tip_proposal(&self, p: fd_types::TipProposal) -> GossipEvents {
        self.with_all_mut(|e| e.on_tip_proposal(p))
    }

    pub fn on_tip_vote(&self, v: fd_types::TipVote) -> GossipEvents {
        self.with_all_mut(|e| e.on_tip_vote(v))
    }

    pub fn on_tip_commit(&self, c: fd_types::TipCommit) -> GossipEvents {
        self.with_all_mut(|e| e.on_tip_commit(c))
    }

    pub fn prune(&self, now: f64) -> u64 {
        self.with_all_mut(|e| e.prune(now))
    }

    /// Re-seed open uncertified self-batches (mesh catch-up under stall).
    pub fn rebroadcast_open_batches(&self) -> GossipEvents {
        self.with_all_mut(|e| e.rebroadcast_open_batches())
    }

    pub fn rebroadcast_recent_finalized(&self, cap: usize) -> GossipEvents {
        self.with_engine(|e| e.rebroadcast_recent_finalized(cap))
    }

    /// Forward non-owned mempool txs so shard owners receive them (multi-pack routing).
    pub fn rebroadcast_mempool_txs(&self, limit: usize) -> GossipEvents {
        self.with_all_mut(|e| e.rebroadcast_mempool_txs(limit))
    }

    /// Pattern B: parked/known certs whose batch bodies we still lack.
    pub fn missing_batch_bodies_to_pull(&self) -> Vec<fd_types::HashHex> {
        self.with_all_mut(|e| e.missing_batch_bodies_to_pull())
    }

    pub fn missing_cert_hashes_to_pull(&self) -> Vec<fd_types::HashHex> {
        self.with_all_mut(|e| e.missing_cert_hashes_to_pull())
    }

    pub fn local_uncertified_batch_hashes(&self) -> Vec<fd_types::HashHex> {
        self.with_all_mut(|e| e.local_uncertified_batch_hashes())
    }

    pub fn local_serveable_batch_hashes(&self) -> Vec<fd_types::HashHex> {
        self.with_all_mut(|e| e.local_serveable_batch_hashes())
    }

    pub fn local_finalized_batch_hashes_capped(&self, cap: usize) -> Vec<fd_types::HashHex> {
        self.with_all_mut(|e| e.local_finalized_batch_hashes_capped(cap))
    }

    pub fn take_creator_cert_announce(&self) -> Vec<(String, String)> {
        self.with_all_mut(|e| e.take_creator_cert_announce())
    }

    pub fn take_cert_announcements(&self) -> Vec<fd_types::HashHex> {
        self.with_all_mut(|e| e.take_cert_announcements())
    }

    pub fn take_abandoned_open_hashes(&self) -> Vec<fd_types::HashHex> {
        self.with_all_mut(|e| e.take_abandoned_open_hashes())
    }

    pub fn queue_foreign_cert_reannounce(&self, now: f64) {
        self.with_all_mut(|e| e.queue_foreign_cert_reannounce(now))
    }

    pub fn local_cert_hashes_capped(&self, cap: usize) -> Vec<fd_types::HashHex> {
        self.with_all_mut(|e| e.local_cert_hashes_capped(cap))
    }

    pub fn cert_set_fingerprint(&self) -> Vec<fd_types::HashHex> {
        self.with_all_mut(|e| e.cert_set_fingerprint())
    }

    pub fn is_assigned_packer(&self) -> bool {
        self.with_all_mut(|e| e.is_assigned_packer())
    }

    pub fn mempool_len(&self) -> usize {
        self.mempool
            .read()
            .as_ref()
            .map(|m| m.len())
            .unwrap_or(0)
    }

    pub fn uncertified_non_genesis(&self) -> usize {
        self.dag
            .read()
            .as_ref()
            .map(|d| d.uncertified_count_non_genesis())
            .unwrap_or(0)
    }

    pub fn metrics_json(&self) -> serde_json::Value {
        let mut m = self.with_all_mut(|e| e.metrics_json());
        let lag = self.lag.lock().clone();
        let rmax = self.max_cert_round();
        m["cert_round_max"] = serde_json::json!(lag.cert_round_max.max(rmax));
        m["cert_round_lag_avg"] = serde_json::json!(if lag.cert_round_lag_samples > 0 {
            lag.cert_round_lag_sum as f64 / lag.cert_round_lag_samples as f64
        } else {
            0.0
        });
        m["lock_split"] = serde_json::json!("mempool|dag|ordering|execution");
        m["pull_path"] = serde_json::json!(true);
        m["build_version"] = serde_json::json!("falcon-dagd/0.48.0-rust");
        m
    }

    pub fn metrics_prometheus(&self) -> String {
        self.with_all_mut(|e| e.metrics_prometheus())
    }

    pub fn to_snapshot(&self) -> fd_store::Snapshot {
        self.with_all_mut(|e| e.to_snapshot())
    }

    pub fn load_snapshot_file(
        &self,
        dir: impl AsRef<std::path::Path>,
    ) -> Result<bool, fd_store::StoreError> {
        self.with_all_mut(|e| e.load_snapshot_file(dir))
    }

    pub fn save_snapshot(
        &self,
        dir: impl AsRef<std::path::Path>,
    ) -> Result<(), fd_store::StoreError> {
        let path = fd_store::snapshot_path(dir);
        fd_store::save_snapshot(path, &self.to_snapshot())
    }

    pub fn note_peer_cert_round(&self, peer_max: u64) {
        let local = self.max_cert_round();
        self.lag.lock().note_cert_lag(local, peer_max);
    }

    /// Public exclusive access for RPC / complex paths (still lock-order safe).
    pub fn with_engine<R>(&self, f: impl FnOnce(&mut crate::engine::NodeEngine) -> R) -> R {
        self.with_all_mut(f)
    }

    /// Exclusive multi-subsystem mutation in lock order.
    /// Takes ownership via `Option::take` — **no** placeholder DAG allocation.
    fn with_all_mut<R>(&self, f: impl FnOnce(&mut crate::engine::NodeEngine) -> R) -> R {
        let mut mempool_g = self.mempool.write();
        let mut dag_g = self.dag.write();
        let mut ordering_g = self.ordering.lock();
        let mut execution_g = self.execution.write();
        let mut meta_g = self.meta.lock();

        let mempool = mempool_g.take().expect("mempool taken (re-entrant with_all_mut?)");
        let dag = dag_g.take().expect("dag taken (re-entrant with_all_mut?)");
        let ordering = ordering_g
            .take()
            .expect("ordering taken (re-entrant with_all_mut?)");
        let execution = execution_g
            .take()
            .expect("execution taken (re-entrant with_all_mut?)");

        let mut eng = crate::engine::NodeEngine {
            config: self.config.clone(),
            validator_id: self.validator_id.clone(),
            worker_index: self.worker_index,
            worker_count: self.worker_count,
            packer_ids: self.packer_ids.clone(),
            role: meta_g.role,
            dag,
            mempool,
            ordering,
            execution,
            batches_created: self.batches_created.load(Ordering::Relaxed),
            ledger_commits: self.ledger_commits.load(Ordering::Relaxed),
            wallet: meta_g.wallet.clone(),
            auto_sign: meta_g.auto_sign,
            rejected_tx_sigs: self.rejected_tx_sigs.load(Ordering::Relaxed),
            last_batch_ts: meta_g.last_batch_ts,
            da_pack_seq: std::mem::take(&mut meta_g.da_pack_seq),
            lane_frontier_incomplete_since: meta_g.lane_frontier_incomplete_since,
            pack_skip_reasons: std::mem::take(&mut meta_g.pack_skip_reasons),
            admit_reasons: std::mem::take(&mut meta_g.admit_reasons),
            local_admits_ok: meta_g.local_admits_ok,
            local_admits_owned: meta_g.local_admits_owned,
            local_admits_not_owned: meta_g.local_admits_not_owned,
            last_busy_cause: meta_g.last_busy_cause,
            busy_causes: std::mem::take(&mut meta_g.busy_causes),
            admit_circuit_latched: meta_g.admit_reg_latched,
            pack_hole_since: std::mem::take(&mut meta_g.pack_hole_since),
            mesh_diag: std::mem::take(&mut meta_g.mesh_diag),
            shard_flow: std::mem::take(&mut meta_g.shard_flow),
            conf_pool: std::mem::take(&mut meta_g.conf_pool),
            pending_remote_sigs: std::mem::take(&mut meta_g.pending_remote_sigs),
            foreign_sig_merged_from_sig: meta_g.foreign_sig_merged_from_sig,
            foreign_sig_merged_from_body: meta_g.foreign_sig_merged_from_body,
            foreign_sig_parked: meta_g.foreign_sig_parked,
            foreign_sig_applied_parked: meta_g.foreign_sig_applied_parked,
            last_creator_sig_reannounce: std::mem::take(&mut meta_g.last_creator_sig_reannounce),
            sig_reannounce_to_creator: meta_g.sig_reannounce_to_creator,
            last_reseed_body_at: std::mem::take(&mut meta_g.last_reseed_body_at),
            reseed_suppress_until: std::mem::take(&mut meta_g.reseed_suppress_until),
            bodies_served_at_progress: meta_g.bodies_served_at_progress,
            foreign_merged_at_progress: meta_g.foreign_merged_at_progress,
            reseed_suppressed: meta_g.reseed_suppressed,
            reseed_allowed: meta_g.reseed_allowed,
            last_open_da_activity: std::mem::take(&mut meta_g.last_open_da_activity),
            open_body_repool_suppressed: meta_g.open_body_repool_suppressed,
            last_abandoned_open_hashes: std::mem::take(&mut meta_g.last_abandoned_open_hashes),
            on_remote_sig_calls: meta_g.on_remote_sig_calls,
            try_certify_attempts: meta_g.try_certify_attempts,
            try_certify_ok: meta_g.try_certify_ok,
            force_open_try_certify: meta_g.force_open_try_certify,
            last_foreign_cert_reannounce: std::mem::take(
                &mut meta_g.last_foreign_cert_reannounce,
            ),
            pending_creator_cert_announce: std::mem::take(&mut meta_g.pending_creator_cert_announce),
            cert_directed_to_creator: meta_g.cert_directed_to_creator,
        };
        let r = f(&mut eng);
        self.batches_created
            .store(eng.batches_created, Ordering::Relaxed);
        self.ledger_commits
            .store(eng.ledger_commits, Ordering::Relaxed);
        self.rejected_tx_sigs
            .store(eng.rejected_tx_sigs, Ordering::Relaxed);
        meta_g.role = eng.role;
        meta_g.wallet = eng.wallet;
        meta_g.auto_sign = eng.auto_sign;
        meta_g.last_batch_ts = eng.last_batch_ts;
        meta_g.da_pack_seq = eng.da_pack_seq;
        meta_g.lane_frontier_incomplete_since = eng.lane_frontier_incomplete_since;
        meta_g.pack_skip_reasons = eng.pack_skip_reasons;
        meta_g.admit_reasons = eng.admit_reasons;
        meta_g.local_admits_ok = eng.local_admits_ok;
        meta_g.local_admits_owned = eng.local_admits_owned;
        meta_g.local_admits_not_owned = eng.local_admits_not_owned;
        meta_g.last_busy_cause = eng.last_busy_cause;
        meta_g.busy_causes = eng.busy_causes;
        meta_g.admit_reg_latched = eng.admit_circuit_latched;
        meta_g.pack_hole_since = eng.pack_hole_since;
        meta_g.mesh_diag = eng.mesh_diag;
        meta_g.shard_flow = eng.shard_flow;
        meta_g.conf_pool = eng.conf_pool;
        meta_g.pending_remote_sigs = eng.pending_remote_sigs;
        meta_g.foreign_sig_merged_from_sig = eng.foreign_sig_merged_from_sig;
        meta_g.foreign_sig_merged_from_body = eng.foreign_sig_merged_from_body;
        meta_g.foreign_sig_parked = eng.foreign_sig_parked;
        meta_g.foreign_sig_applied_parked = eng.foreign_sig_applied_parked;
        meta_g.last_creator_sig_reannounce = eng.last_creator_sig_reannounce;
        meta_g.sig_reannounce_to_creator = eng.sig_reannounce_to_creator;
        meta_g.last_reseed_body_at = eng.last_reseed_body_at;
        meta_g.reseed_suppress_until = eng.reseed_suppress_until;
        meta_g.bodies_served_at_progress = eng.bodies_served_at_progress;
        meta_g.foreign_merged_at_progress = eng.foreign_merged_at_progress;
        meta_g.reseed_suppressed = eng.reseed_suppressed;
        meta_g.reseed_allowed = eng.reseed_allowed;
        meta_g.last_open_da_activity = eng.last_open_da_activity;
        meta_g.open_body_repool_suppressed = eng.open_body_repool_suppressed;
        meta_g.last_abandoned_open_hashes = eng.last_abandoned_open_hashes;
        meta_g.on_remote_sig_calls = eng.on_remote_sig_calls;
        meta_g.try_certify_attempts = eng.try_certify_attempts;
        meta_g.try_certify_ok = eng.try_certify_ok;
        meta_g.force_open_try_certify = eng.force_open_try_certify;
        meta_g.last_foreign_cert_reannounce = eng.last_foreign_cert_reannounce;
        meta_g.pending_creator_cert_announce = eng.pending_creator_cert_announce;
        meta_g.cert_directed_to_creator = eng.cert_directed_to_creator;
        *mempool_g = Some(eng.mempool);
        *dag_g = Some(eng.dag);
        *ordering_g = Some(eng.ordering);
        *execution_g = Some(eng.execution);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fd_types::Transaction;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn concurrent_da_and_ledger_ticks_no_deadlock() {
        let eng = crate::engine::NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                target_batch_interval_s: 0.0,
                single_packer_per_round: false,
                ..Default::default()
            },
            0,
            1,
        );
        let parts = EngineParts::from_sequential(eng);
        for seq in 1..=20u64 {
            assert!(parts
                .submit_tx(Transaction::create("acct1", seq, "acct2", 1, 10, seq as f64))
                .is_ok());
        }
        let p1 = Arc::clone(&parts);
        let p2 = Arc::clone(&parts);
        let p3 = Arc::clone(&parts);
        let h1 = thread::spawn(move || {
            for i in 0..30 {
                let _ = p1.create_batch(i as f64, true);
                p1.advance_da();
            }
        });
        let h2 = thread::spawn(move || {
            for _ in 0..30 {
                let _ = p2.advance_ledger();
            }
        });
        let h3 = thread::spawn(move || {
            for _ in 0..30 {
                let _ = p3.metrics_json();
                let _ = p3.max_cert_round();
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();
        h3.join().unwrap();
        let _ = parts.advance();
        assert!(
            parts.executed_len() > 0 || parts.batches_created.load(Ordering::Relaxed) > 0,
            "should make progress under concurrent ticks"
        );
    }

    /// Composite methods under concurrent load must match single-lock sequential
    /// baseline on ordered set + state root.
    #[test]
    fn composite_create_batch_advance_ledger_matches_sequential_baseline() {
        fn make_eng(_seed: &str) -> crate::engine::NodeEngine {
            crate::engine::NodeEngine::new(
                "val1".into(),
                vec!["val1".into()],
                ProtocolConfig {
                    require_tx_sigs: false,
                    worker_sharding: false,
                    target_batch_interval_s: 0.0,
                    single_packer_per_round: false,
                    max_batch_txs: 32,
                    // Pipeline 80 admits before any execute; MCB would terBUSY at 64.
                    admit_circuit_breaker: false,
                    ..Default::default()
                },
                0,
                1,
            )
        }

        let mut base = make_eng("base");
        for seq in 1..=80u64 {
            assert!(base
                .submit_tx(Transaction::create("acct1", seq, "acct2", 1, 10, seq as f64))
                .is_ok());
        }
        for i in 0..40 {
            let t = i as f64 + 1.0;
            let _ = base.create_batch(t, true);
            base.advance_da();
            let _ = base.advance_ledger();
        }
        let base_ordered = base.ordering.ordered.clone();
        let base_root = base.execution.state.state_root();
        let base_exec = base.execution.executed_txs.len();

        let parts = EngineParts::from_sequential(make_eng("hub"));
        for seq in 1..=80u64 {
            assert!(parts
                .submit_tx(Transaction::create("acct1", seq, "acct2", 1, 10, seq as f64))
                .is_ok());
        }
        let p1 = Arc::clone(&parts);
        let p2 = Arc::clone(&parts);
        let p3 = Arc::clone(&parts);
        let h1 = thread::spawn(move || {
            for i in 0..40 {
                let t = i as f64 + 1.0;
                let _ = p1.create_batch(t, true);
            }
        });
        let h2 = thread::spawn(move || {
            for _ in 0..40 {
                p2.advance_da();
            }
        });
        let h3 = thread::spawn(move || {
            for _ in 0..40 {
                let _ = p3.advance_ledger();
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();
        h3.join().unwrap();
        for i in 0..20 {
            let t = 100.0 + i as f64;
            let _ = parts.create_batch(t, true);
            parts.advance_da();
            let _ = parts.advance_ledger();
        }

        let hub_ordered = parts
            .ordering
            .lock()
            .as_ref()
            .expect("ordering")
            .ordered
            .clone();
        let hub_root = parts
            .execution
            .read()
            .as_ref()
            .expect("exec")
            .state
            .state_root();
        let hub_exec = parts.executed_len();

        assert_eq!(
            base_ordered, hub_ordered,
            "ordered set must match sequential baseline under concurrent composite methods"
        );
        assert_eq!(
            base_root, hub_root,
            "state root must match sequential baseline"
        );
        assert_eq!(base_exec, hub_exec, "executed tx count must match");
        assert!(base_exec > 0, "baseline should execute txs");
    }
}
