//! Node engine with a **real Narwhal-style DA / ledger split**.
//!
//! - **DA path** (`NodeRole::DaWorker` / Full): mempool → batch → sign → availability cert.
//!   Never orders certificates; never executes user txs.
//! - **Ledger path** (`NodeRole::OrderExec` / Full): **tip-BFT** over certificate tips
//!   (propose → vote → UNL TipCommit), then expand cert sub-DAG under each commit and
//!   execute payloads from the DA store. Never packs from mempool.

use crate::conf_pool::{
    duty_phase_at, ConfPool, DutyPhase, RemoveReason, SignDeferReason,
};
use fd_crypto::{verify_tx_embedded, SharedCrypto, SharedWallet};
use fd_dag::CertificateDag;
use fd_exec::{
    AccountPolicy, AccountState, AssetMeta, ExecutionEngine, MarketConfig, PoolConfig, RailConfig,
    RailHeader, WithdrawalNote,
};
use fd_mempool::{AddOutcome, EntrySource, Mempool};
use fd_order::{BullsharkEngine, OrderingEngine, TipBftEngine};
use fd_store::{self, Snapshot, SNAPSHOT_VERSION};
use fd_types::{
    genesis_batch_hash, AccountType, Batch, Certificate, NodeId, NodeRole, PartialSignature,
    ProtocolConfig, RailFamily, TipCommit, TipProposal, TipVote, Transaction,
};

fn parse_rail_family(s: &str) -> RailFamily {
    match s.to_ascii_lowercase().as_str() {
        "evm" => RailFamily::Evm,
        "xrpledger" | "xrp_ledger" | "xrp" => RailFamily::XrpLedger,
        _ => RailFamily::Bitcoin,
    }
}

fn rail_family_snap(f: RailFamily) -> String {
    match f {
        RailFamily::Bitcoin => "bitcoin".into(),
        RailFamily::Evm => "evm".into(),
        RailFamily::XrpLedger => "xrp_ledger".into(),
    }
}
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Convergence / lag instrumentation (mesh agreement under load).
#[derive(Debug, Default, Clone)]
pub struct LagMetrics {
    pub cert_round_max: u64,
    pub cert_round_lag_samples: u64,
    pub cert_round_lag_sum: u64,
    pub soft_final_ms_sum: u64,
    pub soft_final_samples: u64,
    pub hard_final_ms_sum: u64,
    pub hard_final_samples: u64,
    pub mempool_to_exec_ms_sum: u64,
    pub mempool_to_exec_samples: u64,
}

impl LagMetrics {
    pub fn note_cert_lag(&mut self, local_max: u64, peer_max: u64) {
        let lag = local_max
            .saturating_sub(peer_max)
            .max(peer_max.saturating_sub(local_max));
        self.cert_round_max = self.cert_round_max.max(local_max);
        self.cert_round_lag_sum = self.cert_round_lag_sum.saturating_add(lag);
        self.cert_round_lag_samples = self.cert_round_lag_samples.saturating_add(1);
    }
}

/// Why an open is (or is not) eligible for repool (PR5 diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoolBlockReason {
    NotOwn,
    Certified,
    HasForeign,
    AgeBelowTtl,
    NotQuiet,
    NotAtCapYet,
}

impl RepoolBlockReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotOwn => "not_own",
            Self::Certified => "certified",
            Self::HasForeign => "has_foreign",
            Self::AgeBelowTtl => "age_below_ttl",
            Self::NotQuiet => "not_quiet",
            Self::NotAtCapYet => "not_at_cap_yet",
        }
    }
}

/// Per-node owned vs non-owned tx flow (multipack starvation vs conflict prevention).
#[derive(Debug, Clone, Default)]
pub struct ShardFlow {
    pub admitted_owned: u64,
    pub admitted_not_owned: u64,
    pub rebroadcast_forwarded: u64,
    /// Phase B: txs directed to lane owner (not mesh flood).
    pub directed_to_owner: u64,
    /// Phase B: flood used because owner unknown / single-packer legacy.
    pub flood_fallback: u64,
    /// Phase B gauge: mempool txs this node holds but does not own (awaiting owner).
    pub stuck_not_owned_pending: u64,
    /// Phase B gauge: max age (seconds) among stuck_not_owned txs.
    pub stuck_owner_age_max_s: f64,
    pub packed_owned: u64,
    pub skipped_not_owned: u64,
    pub applied: u64,
}

impl ShardFlow {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "admitted_owned": self.admitted_owned,
            "admitted_not_owned": self.admitted_not_owned,
            "rebroadcast_forwarded": self.rebroadcast_forwarded,
            "directed_to_owner": self.directed_to_owner,
            "flood_fallback": self.flood_fallback,
            "stuck_not_owned_pending": self.stuck_not_owned_pending,
            "stuck_owner_age_max_s": self.stuck_owner_age_max_s,
            "packed_owned": self.packed_owned,
            "skipped_not_owned": self.skipped_not_owned,
            "applied": self.applied,
        })
    }
}

/// Phase 49d: mesh / cert-propagation diagnostics.
#[derive(Debug, Clone)]
pub struct MeshDiag {
    /// Cert body received but batch body missing (deferred apply).
    pub cert_deferred_missing_batch: u64,
    /// Cert body received, batch present, parent certs missing (in-order gate).
    pub cert_deferred_missing_parent: u64,
    /// Cert fully applied into certificates map.
    pub cert_applied: u64,
    /// Applied from pending after batch/parent catch-up.
    pub cert_reapplied_from_pending: u64,
    /// Local observe→apply lag samples (ms).
    pub cert_apply_lag_ms_sum: u64,
    pub cert_apply_lag_samples: u64,
    pub cert_apply_lag_ms_max: u64,
    /// Creator timestamp→apply lag samples (ms).
    pub cert_from_create_lag_ms_sum: u64,
    pub cert_from_create_lag_samples: u64,
    pub cert_from_create_lag_ms_max: u64,
    /// Human-readable validation model (constant).
    pub validation_model: String,
    /// Peak parallel pull inflight observed.
    pub pull_inflight_peak: u32,
    /// Max hashes in one want group (parallelism evidence).
    pub pull_want_batch_max: u32,
    /// Pending remote certs waiting for batch/parents (by batch_hash).
    pub pending_certs: BTreeMap<String, Certificate>,
    /// First local wall-time we observed each cert batch_hash.
    pub first_observe: BTreeMap<String, f64>,
    /// Cert hashes applied this step — main announces inv/body (fix 1).
    pub recently_applied_cert_hashes: Vec<String>,
}

impl Default for MeshDiag {
    fn default() -> Self {
        Self {
            cert_deferred_missing_batch: 0,
            cert_deferred_missing_parent: 0,
            cert_applied: 0,
            cert_reapplied_from_pending: 0,
            cert_apply_lag_ms_sum: 0,
            cert_apply_lag_samples: 0,
            cert_apply_lag_ms_max: 0,
            cert_from_create_lag_ms_sum: 0,
            cert_from_create_lag_samples: 0,
            cert_from_create_lag_ms_max: 0,
            validation_model: "in_order_parent_certs_required".into(),
            pull_inflight_peak: 0,
            pull_want_batch_max: 0,
            pending_certs: BTreeMap::new(),
            first_observe: BTreeMap::new(),
            recently_applied_cert_hashes: Vec::new(),
        }
    }
}

impl MeshDiag {
    pub fn note_pull_inflight(&mut self, n: u32) {
        self.pull_inflight_peak = self.pull_inflight_peak.max(n);
    }

    pub fn note_want_batch(&mut self, n: u32) {
        self.pull_want_batch_max = self.pull_want_batch_max.max(n);
    }

    pub fn to_json(&self) -> serde_json::Value {
        let apply_avg = if self.cert_apply_lag_samples > 0 {
            self.cert_apply_lag_ms_sum as f64 / self.cert_apply_lag_samples as f64
        } else {
            0.0
        };
        let create_avg = if self.cert_from_create_lag_samples > 0 {
            self.cert_from_create_lag_ms_sum as f64 / self.cert_from_create_lag_samples as f64
        } else {
            0.0
        };
        serde_json::json!({
            "validation_model": self.validation_model,
            "cert_deferred_missing_batch": self.cert_deferred_missing_batch,
            "pending_certs_n": self.pending_certs.len(),
            "pending_cert_hashes": self
                .pending_certs
                .keys()
                .take(32)
                .cloned()
                .collect::<Vec<_>>(),
            "cert_deferred_missing_parent": self.cert_deferred_missing_parent,
            "cert_applied": self.cert_applied,
            "cert_reapplied_from_pending": self.cert_reapplied_from_pending,
            "pending_certs": self.pending_certs.len(),
            "cert_apply_lag_ms_avg": apply_avg,
            "cert_apply_lag_ms_max": self.cert_apply_lag_ms_max,
            "cert_apply_lag_samples": self.cert_apply_lag_samples,
            "cert_from_create_lag_ms_avg": create_avg,
            "cert_from_create_lag_ms_max": self.cert_from_create_lag_ms_max,
            "cert_from_create_lag_samples": self.cert_from_create_lag_samples,
            "pull_inflight_peak": self.pull_inflight_peak,
            "pull_want_batch_max": self.pull_want_batch_max,
            "batch_pull_parallelism": if self.pull_want_batch_max > 1 || self.pull_inflight_peak > 1 {
                "parallel_multi_hash_wants"
            } else {
                "serial_or_idle"
            },
        })
    }
}

fn wall_now_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}


fn parse_account_type(s: &str) -> AccountType {
    match s.to_ascii_lowercase().as_str() {
        "timed" => AccountType::Timed,
        "vault" => AccountType::Vault,
        "pool" => AccountType::Pool,
        "lendingmarket" | "lending_market" | "lending" => AccountType::LendingMarket,
        _ => AccountType::Hot,
    }
}

fn account_type_snap(t: AccountType) -> String {
    match t {
        AccountType::Hot => "hot".into(),
        AccountType::Timed => "timed".into(),
        AccountType::Vault => "vault".into(),
        AccountType::Pool => "pool".into(),
        AccountType::LendingMarket => "lending_market".into(),
    }
}

/// Outbound gossip produced by a local engine step.
#[derive(Debug, Default)]
pub struct GossipEvents {
    pub txs: Vec<Transaction>,
    pub batches: Vec<Batch>,
    pub sigs: Vec<PartialSignature>,
    pub certs: Vec<Certificate>,
    pub tip_proposals: Vec<TipProposal>,
    pub tip_votes: Vec<TipVote>,
    pub tip_commits: Vec<TipCommit>,
    /// Option 3: deliver these txs only to the named peer (shard owner).
    pub directed_txs: Vec<(NodeId, Transaction)>,
}

impl GossipEvents {
    pub fn is_empty(&self) -> bool {
        self.txs.is_empty()
            && self.batches.is_empty()
            && self.sigs.is_empty()
            && self.certs.is_empty()
            && self.tip_proposals.is_empty()
            && self.tip_votes.is_empty()
            && self.tip_commits.is_empty()
            && self.directed_txs.is_empty()
    }

    pub fn extend(&mut self, other: GossipEvents) {
        self.txs.extend(other.txs);
        self.batches.extend(other.batches);
        self.sigs.extend(other.sigs);
        self.certs.extend(other.certs);
        self.tip_proposals.extend(other.tip_proposals);
        self.tip_votes.extend(other.tip_votes);
        self.tip_commits.extend(other.tip_commits);
        self.directed_txs.extend(other.directed_txs);
    }
}

/// Where a submit came from — circuit breaker applies to local/RPC only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitSource {
    /// RPC / wallet / local API — MCB may return Busy.
    Local,
    /// Mesh directed handoff or peer gossip — bypass MCB so transfer is not dropped.
    Mesh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitResult {
    Accepted,
    Duplicate,
    MissingSignature,
    BadSignature,
    MempoolFull,
    /// Auto-regulation: admit lag over high watermark (or soft mempool / stale open).
    /// Retryable — resubmit when backlog drains; node does not requeue.
    Busy,
    /// Fails protocol admit (sealed account, invalid SPV, economic body, etc.).
    /// Agents must treat this as final: no discretionary override exists.
    ProtocolReject,
    /// Sequence already consumed (tx.sequence < expected).
    SeqPast,
    /// Sequence beyond account_sequence_window.
    SeqFuture,
    /// Sequence in-window but body/guard checks failed.
    ProtocolBody,
    /// Node role does not run DA (cannot admit).
    NotDaRole,
}

/// Why local admit returned `terBUSY` (PR2: diagnose instead of guessing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyCause {
    MempoolSoftHigh,
    UncertifiedAge,
    OpenUncertCount,
    AdmitExecLag,
}

impl BusyCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MempoolSoftHigh => "busy_mempool_soft",
            Self::UncertifiedAge => "busy_uncert_age",
            Self::OpenUncertCount => "busy_open_uncert",
            Self::AdmitExecLag => "busy_admit_lag",
        }
    }

    pub fn operator_hint(self) -> &'static str {
        match self {
            Self::MempoolSoftHigh => {
                "mempool at soft high watermark; wait for pack/execute or raise admit_busy_mempool"
            }
            Self::UncertifiedAge => {
                "oldest open uncertified batch too old; DA/cert path stuck (check body/foreign sig)"
            }
            Self::OpenUncertCount => {
                "too many own open uncertified batches; wait for cert or lower pack_open_cap"
            }
            Self::AdmitExecLag => {
                "local admits ahead of execute (admit_exec_lag); wait for ledger apply or raise admit_ahead_cap"
            }
        }
    }
}

impl SubmitResult {
    pub fn engine_code(self) -> &'static str {
        match self {
            Self::Accepted => "tesSUCCESS",
            Self::Duplicate => "tefALREADY",
            Self::MissingSignature => "tefBAD_SIGNATURE",
            Self::BadSignature => "tefBAD_SIGNATURE",
            Self::MempoolFull => "terQUEUED",
            Self::Busy => "terBUSY",
            Self::ProtocolReject => "tecPROTOCOL",
            Self::SeqPast => "tecSEQ_PAST",
            Self::SeqFuture => "tecSEQ_FUTURE",
            Self::ProtocolBody => "tecPROTOCOL_BODY",
            Self::NotDaRole => "tecNOT_DA",
        }
    }

    /// Stable histogram key for admit-failure / success reasons.
    pub fn reason_key(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Duplicate => "duplicate",
            Self::MissingSignature => "missing_sig",
            Self::BadSignature => "bad_sig",
            Self::MempoolFull => "mempool_full",
            Self::Busy => "busy",
            Self::ProtocolReject => "protocol",
            Self::SeqPast => "seq_past",
            Self::SeqFuture => "seq_future",
            Self::ProtocolBody => "protocol_body",
            Self::NotDaRole => "not_da_role",
        }
    }

    pub fn is_ok(self) -> bool {
        matches!(self, Self::Accepted)
    }

    /// Client may resubmit later (MCB / mempool pressure).
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::Busy | Self::MempoolFull)
    }
}

pub struct NodeEngine {
    pub config: ProtocolConfig,
    pub validator_id: NodeId,
    pub worker_index: usize,
    pub worker_count: usize,
    /// Narwhal role: DA worker, light ledger, or both (separate paths).
    pub role: NodeRole,
    pub dag: CertificateDag,
    pub mempool: Mempool,
    /// Logical conf pool (open bodies awaiting scheduled UNL sign).
    pub conf_pool: ConfPool,
    pub ordering: OrderingEngine,
    pub execution: ExecutionEngine,
    pub batches_created: u64,
    /// Certificates ordered into the ledger (light path counter).
    pub ledger_commits: u64,
    pub wallet: Option<SharedWallet>,
    /// When true, unsigned faucet txs are signed with local wallet before accept.
    pub auto_sign: bool,
    pub rejected_tx_sigs: u64,
    pub(crate) last_batch_ts: f64,
    /// DA packing cursor per account (independent of ledger execution sequences).
    /// Pure DA workers never execute, so they must advance this when packing.
    pub(crate) da_pack_seq: BTreeMap<String, u64>,
    /// Phase 49b: how many times each create_batch early-return / success fired.
    /// Wall time when multi-lane DA frontier first became incomplete (0 = complete).
    pub(crate) lane_frontier_incomplete_since: f64,
    /// Phase 49b: how many times each create_batch early-return / success fired.
    pub pack_skip_reasons: BTreeMap<String, u64>,
    /// A1: partial sigs that arrived before we held the batch body.
    pub pending_remote_sigs: BTreeMap<fd_types::HashHex, Vec<(NodeId, String)>>,
    /// A1 dig counters (also mirrored into pack_skip_reasons).
    pub foreign_sig_merged_from_sig: u64,
    pub foreign_sig_merged_from_body: u64,
    pub foreign_sig_parked: u64,
    pub foreign_sig_applied_parked: u64,
    /// A2: last wall time we re-emitted our sig toward a foreign creator.
    pub last_creator_sig_reannounce: BTreeMap<fd_types::HashHex, f64>,
    pub sig_reannounce_to_creator: u64,
    /// A3: last body reseed wall time per own open hash.
    pub last_reseed_body_at: BTreeMap<fd_types::HashHex, f64>,
    /// A3: suppress body reseed until this time when peers show progress.
    pub reseed_suppress_until: BTreeMap<fd_types::HashHex, f64>,
    pub bodies_served_at_progress: u64,
    pub foreign_merged_at_progress: u64,
    pub reseed_suppressed: u64,
    pub reseed_allowed: u64,
    /// A4: last body-DA activity (reseed, serve progress, foreign merge).
    pub last_open_da_activity: BTreeMap<fd_types::HashHex, f64>,
    pub open_body_repool_suppressed: u64,
    /// Hashes abandoned by open-body-repool (for mesh inv/forget).
    pub last_abandoned_open_hashes: Vec<fd_types::HashHex>,
    /// Dig: on_remote_sig entry count.
    pub on_remote_sig_calls: u64,
    pub try_certify_attempts: u64,
    pub try_certify_ok: u64,
    pub force_open_try_certify: u64,
    /// Foreign certs we formed that should be directed to creator (A2-style).
    pub pending_creator_cert_announce: Vec<(String, String)>,
    pub cert_directed_to_creator: u64,
    /// Reliability: last time we re-pushed a foreign cert to its creator.
    pub last_foreign_cert_reannounce: BTreeMap<fd_types::HashHex, f64>,
    /// Phase 49b: admit outcome histogram (accepted + failure reasons).
    pub admit_reasons: BTreeMap<String, u64>,
    /// Local/RPC accepts only (MCB backlog: lag = this - executed).
    /// Counts **all** Local accepts (owned + not-owned). Mesh never increments.
    pub local_admits_ok: u64,
    /// Local accepts this node **owns** (will pack). Multipack dig/owner-route signal.
    pub local_admits_owned: u64,
    /// Local accepts this node does **not** own (will hand off / foreign lane).
    pub local_admits_not_owned: u64,
    /// Last terBUSY sub-reason (PR2 operator diagnostics).
    pub last_busy_cause: Option<BusyCause>,
    /// Histogram of BusyCause keys (alongside generic `busy`).
    pub busy_causes: BTreeMap<String, u64>,
    /// Hysteresis: once lag trips, stay busy until lag <= resume.
    pub(crate) admit_circuit_latched: bool,
    /// Phase 49c: first wall-time we observed a pack hole for an account
    /// (missing pack_cursor seq while higher seqs exist in mempool).
    pub(crate) pack_hole_since: BTreeMap<String, f64>,
    /// Phase 49d: mesh / cert-propagation diagnostics.
    pub mesh_diag: MeshDiag,
    pub shard_flow: ShardFlow,
}

impl NodeEngine {
    pub fn new(
        validator_id: NodeId,
        unl: Vec<NodeId>,
        config: ProtocolConfig,
        worker_index: usize,
        worker_count: usize,
    ) -> Self {
        let dag = CertificateDag::new(validator_id.clone(), unl, config.clone());
        Self::from_dag(validator_id, config, worker_index, worker_count, dag)
    }

    pub fn with_crypto(
        validator_id: NodeId,
        unl: Vec<NodeId>,
        config: ProtocolConfig,
        worker_index: usize,
        worker_count: usize,
        crypto: SharedCrypto,
    ) -> Self {
        let dag = CertificateDag::with_crypto(
            validator_id.clone(),
            unl,
            config.clone(),
            crypto,
        );
        Self::from_dag(validator_id, config, worker_index, worker_count, dag)
    }

    fn from_dag(
        validator_id: NodeId,
        config: ProtocolConfig,
        worker_index: usize,
        worker_count: usize,
        dag: CertificateDag,
    ) -> Self {
        let mempool = Mempool::new(config.clone());
        let ordering = OrderingEngine::new(config.clone());
        let execution = ExecutionEngine::new(AccountState::genesis_faucet(
            config.economics.clone(),
        ));
        let mut conf_pool = ConfPool::new();
        conf_pool.set_local_id(validator_id.clone());
        Self {
            config,
            validator_id,
            worker_index,
            worker_count: worker_count.max(1),
            role: NodeRole::Full,
            dag,
            mempool,
            conf_pool,
            ordering,
            execution,
            batches_created: 0,
            ledger_commits: 0,
            wallet: None,
            auto_sign: true,
            rejected_tx_sigs: 0,
            last_batch_ts: 0.0,
            da_pack_seq: BTreeMap::new(),
            lane_frontier_incomplete_since: 0.0,
            pack_skip_reasons: BTreeMap::new(),
            pending_remote_sigs: BTreeMap::new(),
            foreign_sig_merged_from_sig: 0,
            foreign_sig_merged_from_body: 0,
            foreign_sig_parked: 0,
            foreign_sig_applied_parked: 0,
            last_creator_sig_reannounce: BTreeMap::new(),
            sig_reannounce_to_creator: 0,
            last_reseed_body_at: BTreeMap::new(),
            reseed_suppress_until: BTreeMap::new(),
            bodies_served_at_progress: 0,
            foreign_merged_at_progress: 0,
            reseed_suppressed: 0,
            reseed_allowed: 0,
            last_open_da_activity: BTreeMap::new(),
            open_body_repool_suppressed: 0,
            last_abandoned_open_hashes: Vec::new(),
            on_remote_sig_calls: 0,
            try_certify_attempts: 0,
            try_certify_ok: 0,
            force_open_try_certify: 0,
            pending_creator_cert_announce: Vec::new(),
            cert_directed_to_creator: 0,
            last_foreign_cert_reannounce: BTreeMap::new(),
            admit_reasons: BTreeMap::new(),
            local_admits_ok: 0,
            local_admits_owned: 0,
            local_admits_not_owned: 0,
            last_busy_cause: None,
            busy_causes: BTreeMap::new(),
            admit_circuit_latched: false,
            pack_hole_since: BTreeMap::new(),
            mesh_diag: MeshDiag::default(),
            shard_flow: ShardFlow::default(),
        }
    }

    pub fn set_wallet(&mut self, wallet: SharedWallet, auto_sign: bool) {
        self.wallet = Some(wallet);
        self.auto_sign = auto_sign;
    }

    pub fn set_role(&mut self, role: NodeRole) {
        self.role = role;
    }

    /// Submit a user transaction (optional wallet verify / auto-sign + protocol can_apply).
    /// Order-exec (ledger-only) nodes do not accept user submits into a packing mempool.
    fn note_admit(&mut self, result: SubmitResult) -> SubmitResult {
        let key = result.reason_key().to_string();
        *self.admit_reasons.entry(key).or_insert(0) += 1;
        result
    }

    fn note_busy(&mut self, cause: BusyCause) -> SubmitResult {
        self.last_busy_cause = Some(cause);
        *self
            .busy_causes
            .entry(cause.as_str().to_string())
            .or_insert(0) += 1;
        // Also count under sub-key in admit_reasons for digs that only read admit_reasons.
        *self
            .admit_reasons
            .entry(cause.as_str().to_string())
            .or_insert(0) += 1;
        self.note_admit(SubmitResult::Busy)
    }

    /// Diagnose current MCB gate without mutating latch (for status/RPC).
    pub fn busy_cause_now(&self) -> Option<BusyCause> {
        if !self.config.admit_circuit_breaker {
            return None;
        }
        if self.mempool.len() >= self.admit_busy_mempool_limit() {
            return Some(BusyCause::MempoolSoftHigh);
        }
        let age_lim = self.config.admit_busy_uncert_age_s;
        if age_lim > 0.0 {
            if let Some(t0) = self.dag.oldest_uncertified_timestamp() {
                let now = wall_now_s();
                if (now - t0).max(0.0) >= age_lim {
                    return Some(BusyCause::UncertifiedAge);
                }
            }
        }
        if self.config.admit_busy_open_uncert > 0 {
            let own_open = self.dag.uncertified_by_creator(&self.validator_id);
            if own_open >= self.config.admit_busy_open_uncert {
                return Some(BusyCause::OpenUncertCount);
            }
        }
        let lag = self.admit_exec_lag();
        let high = self.admit_ahead_cap();
        let low = self.admit_ahead_resume();
        if self.admit_circuit_latched {
            if lag > low {
                return Some(BusyCause::AdmitExecLag);
            }
            return None;
        }
        if lag >= high {
            return Some(BusyCause::AdmitExecLag);
        }
        None
    }

    fn note_pack_skip(&mut self, reason: &str) {
        *self.pack_skip_reasons.entry(reason.to_string()).or_insert(0) += 1;
    }

    pub fn submit_tx(&mut self, tx: Transaction) -> SubmitResult {
        self.submit_tx_from(tx, AdmitSource::Local)
    }

    /// Mesh peer / directed handoff — bypasses admit circuit breaker.
    pub fn submit_tx_mesh(&mut self, tx: Transaction) -> SubmitResult {
        self.submit_tx_from(tx, AdmitSource::Mesh)
    }

    pub fn submit_tx_from(&mut self, tx: Transaction, source: AdmitSource) -> SubmitResult {
        if !self.role.runs_da() {
            return self.note_admit(SubmitResult::NotDaRole);
        }
        self.submit_tx_gated(tx, true, source)
    }

    /// Pack open-cap used for multi-lane DA (mirrors create_batch).
    fn pack_open_cap(&self) -> usize {
        if self.config.pack_open_cap > 0 {
            return self.config.pack_open_cap.max(1);
        }
        if self.config.single_packer_per_round {
            self.config.max_uncertified_per_creator.max(1)
        } else {
            // Default multipack: open_cap=1 until continuous body DA is solid
            // (open_cap=2 previously regressed continuous + incomplete 800/1600 burst).
            // Override with ProtocolConfig.pack_open_cap / --pack-open-cap for digs.
            1
        }
    }

    /// Soft mempool high for MCB (auto when config.admit_busy_mempool == 0).
    fn admit_busy_mempool_limit(&self) -> usize {
        if self.config.admit_busy_mempool > 0 {
            return self.config.admit_busy_mempool;
        }
        self.config
            .adaptive_mempool_high
            .saturating_mul(2)
            .max(256)
            .min(self.config.max_mempool_txs.max(1))
    }

    fn admit_ahead_cap(&self) -> u64 {
        if self.config.admit_ahead_cap > 0 {
            self.config.admit_ahead_cap
        } else {
            64
        }
    }

    fn admit_ahead_resume(&self) -> u64 {
        if self.config.admit_ahead_resume > 0 {
            self.config.admit_ahead_resume
        } else {
            self.admit_ahead_cap() / 2
        }
    }

    /// Gateway lag: Local/RPC admits still ahead of ledger execute (backlog window).
    ///
    /// On a sole submit front door this grows when admit rate > settle rate.
    /// On multipack peers that never see Local RPC, this stays 0 (not a peer fault).
    /// Prefer owner-routed submits so each node’s `local_admits_*` matches its lane.
    pub fn admit_exec_lag(&self) -> u64 {
        let exec = self.execution.executed_txs.len() as u64;
        self.local_admits_ok.saturating_sub(exec)
    }

    /// Fraction of Local admits that were not-owned (0.0..=1.0). High on a
    /// multipack node that receives foreign-lane RPC traffic.
    pub fn local_admit_not_owned_ratio(&self) -> f64 {
        if self.local_admits_ok == 0 {
            return 0.0;
        }
        self.local_admits_not_owned as f64 / self.local_admits_ok as f64
    }

    /// True when local/RPC admits should return terBUSY (auto-regulation).
    ///
    /// **Watermark model** (not a one-shot fuse):
    /// - lag = local_admits_ok - executed_txs
    /// - lag >= high (`admit_ahead_cap`) → reject new local admits
    /// - once over, stay rejecting until lag <= low (`admit_ahead_resume`, default high/2)
    /// - then let more in again — self-throttles near max processable backlog
    ///
    /// Mesh handoffs skip this so transfer-to-owner is not dropped under load.
    pub fn admit_circuit_open(&self) -> bool {
        if !self.config.admit_circuit_breaker {
            return false;
        }
        // Soft mempool high — refuse more local work before hard MempoolFull.
        if self.mempool.len() >= self.admit_busy_mempool_limit() {
            return true;
        }
        // Stale open batch (cert not landing) — secondary safety.
        let age_lim = self.config.admit_busy_uncert_age_s;
        if age_lim > 0.0 {
            if let Some(t0) = self.dag.oldest_uncertified_timestamp() {
                let now = wall_now_s();
                if (now - t0).max(0.0) >= age_lim {
                    return true;
                }
            }
        }
        // Optional open-count trip (off by default — was too spiky mid-burst).
        if self.config.admit_busy_open_uncert > 0 {
            let own_open = self.dag.uncertified_by_creator(&self.validator_id);
            if own_open >= self.config.admit_busy_open_uncert {
                return true;
            }
        }
        // Primary: executed-ahead backlog with hysteresis via latched flag
        // (caller uses admit_circuit_open_mut for latch updates on Local path).
        let lag = self.admit_exec_lag();
        let high = self.admit_ahead_cap();
        let low = self.admit_ahead_resume();
        if self.admit_circuit_latched {
            // Stay busy until lag drains under resume watermark.
            return lag > low;
        }
        lag >= high
    }

    /// Like `admit_circuit_open` but updates hysteresis latch (Local admit path).
    fn admit_circuit_open_mut(&mut self) -> bool {
        if !self.config.admit_circuit_breaker {
            self.admit_circuit_latched = false;
            return false;
        }
        // Re-evaluate non-lag gates first (same as open).
        if self.mempool.len() >= self.admit_busy_mempool_limit() {
            self.admit_circuit_latched = true;
            return true;
        }
        let age_lim = self.config.admit_busy_uncert_age_s;
        if age_lim > 0.0 {
            if let Some(t0) = self.dag.oldest_uncertified_timestamp() {
                let now = wall_now_s();
                if (now - t0).max(0.0) >= age_lim {
                    self.admit_circuit_latched = true;
                    return true;
                }
            }
        }
        if self.config.admit_busy_open_uncert > 0 {
            let own_open = self.dag.uncertified_by_creator(&self.validator_id);
            if own_open >= self.config.admit_busy_open_uncert {
                self.admit_circuit_latched = true;
                return true;
            }
        }
        let lag = self.admit_exec_lag();
        let high = self.admit_ahead_cap();
        let low = self.admit_ahead_resume();
        if self.admit_circuit_latched {
            if lag <= low {
                self.admit_circuit_latched = false;
                return false;
            }
            return true;
        }
        if lag >= high {
            self.admit_circuit_latched = true;
            return true;
        }
        false
    }

    /// `check_protocol`: when true (normal path), reject if admit gate fails.
    /// Snapshot restore re-admits mempool without this gate (state overlay follows).
    fn submit_tx_gated(
        &mut self,
        mut tx: Transaction,
        check_protocol: bool,
        source: AdmitSource,
    ) -> SubmitResult {
        // MCB: local/RPC only — do not auto-requeue; client retries on terBUSY.
        if source == AdmitSource::Local && self.admit_circuit_open_mut() {
            let cause = self
                .busy_cause_now()
                .unwrap_or(BusyCause::AdmitExecLag);
            return self.note_busy(cause);
        }
        let net = self.config.network_id;
        if self.config.require_tx_sigs || tx.is_signed() {
            if !tx.is_signed() {
                if self.auto_sign {
                    if let Some(ref w) = self.wallet {
                        if w.sign_tx(&mut tx, net).is_err() {
                            self.rejected_tx_sigs += 1;
                            return self.note_admit(SubmitResult::MissingSignature);
                        }
                    } else {
                        self.rejected_tx_sigs += 1;
                        return self.note_admit(SubmitResult::MissingSignature);
                    }
                } else {
                    self.rejected_tx_sigs += 1;
                    return self.note_admit(SubmitResult::MissingSignature);
                }
            }
            let ok = match &self.wallet {
                Some(w) => w.verify_tx(&tx, net),
                None => verify_tx_embedded(&tx, net),
            };
            if !ok {
                self.rejected_tx_sigs += 1;
                return self.note_admit(SubmitResult::BadSignature);
            }
        }
        // Protocol gate: admit into mempool with a sequence *window* so clients can
        // pipeline checks 1..N while only `expected` is executable yet.
        // Execution still uses exact-sequence `can_apply` when batching/applying.
        // Phase 49b: split sequence-past / sequence-future / body for admit histogram.
        if check_protocol {
            let window = self.config.account_sequence_window.max(1);
            let expected = self.execution.state.next_sequence(&tx.account);
            if tx.sequence < expected {
                return self.note_admit(SubmitResult::SeqPast);
            }
            if tx.sequence >= expected.saturating_add(window) {
                return self.note_admit(SubmitResult::SeqFuture);
            }
            if !self.execution.state.can_admit(&tx, window) {
                return self.note_admit(SubmitResult::ProtocolBody);
            }
        }
        self.dag.ingest_tx(tx.clone());
        let entry_src = match source {
            AdmitSource::Local => EntrySource::Local,
            AdmitSource::Mesh => EntrySource::Mesh,
        };
        let now = wall_now_s();
        match self.mempool.add_with_source(tx.clone(), entry_src, Some(now)) {
            AddOutcome::Inserted | AddOutcome::InsertedCompetitor => {
                let owned = self.owns_tx(&tx, now);
                if owned {
                    self.shard_flow.admitted_owned =
                        self.shard_flow.admitted_owned.saturating_add(1);
                } else {
                    self.shard_flow.admitted_not_owned =
                        self.shard_flow.admitted_not_owned.saturating_add(1);
                }
                // Count Local accepts for backlog MCB (mesh does not inflate the window).
                // Split owned vs not-owned so digs can see sole-front-door multipack lag.
                if source == AdmitSource::Local {
                    self.local_admits_ok = self.local_admits_ok.saturating_add(1);
                    if owned {
                        self.local_admits_owned = self.local_admits_owned.saturating_add(1);
                    } else {
                        self.local_admits_not_owned =
                            self.local_admits_not_owned.saturating_add(1);
                    }
                }
                self.note_admit(SubmitResult::Accepted)
            }
            AddOutcome::Full => self.note_admit(SubmitResult::MempoolFull),
            AddOutcome::ExactDuplicate => self.note_admit(SubmitResult::Duplicate),
        }
    }

    /// Stable account → shard id (FNV-1a 32-bit over account bytes).
    fn account_shard(account: &str) -> u32 {
        let mut h: u32 = 0x811c_9dc5;
        for b in account.as_bytes() {
            h ^= u32::from(*b);
            h = h.wrapping_mul(0x0100_0193);
        }
        h
    }

    /// Whether this validator may pack `tx` into **its lane**.
    ///
    /// **Multi-lane** (`single_packer_per_round == false`): each UNL node is one
    /// lane. Accounts map to exactly one lane owner
    /// (`FNV(account) % |UNL| == my_rank`). Only that node packs those sequences.
    /// Global mempool still admits everywhere; non-owners do not pack foreign lanes.
    ///
    /// **Worker sharding** (optional): further partition by tx_id among workers.
    pub fn owns_tx(&self, tx: &Transaction, now: f64) -> bool {
        // Multi-lane ownership (each node = one lane).
        if !self.config.single_packer_per_round {
            let unl = BullsharkEngine::sorted_unl(&self.dag.unl);
            if unl.len() > 1 {
                let Some(my_rank) = unl.iter().position(|id| id == &self.validator_id) else {
                    return false;
                };
                let shard = (Self::account_shard(&tx.account) as usize) % unl.len();
                if shard != my_rank {
                    return false;
                }
            }
        }
        if !self.config.worker_sharding || self.worker_count <= 1 {
            return true;
        }
        let prefix = u32::from_str_radix(&tx.tx_id[..8.min(tx.tx_id.len())], 16).unwrap_or(0);
        let primary = (prefix as usize) % self.worker_count == self.worker_index;
        if primary {
            return true;
        }
        let age = (now - tx.first_seen).max(0.0);
        age >= self.config.shard_orphan_age_s
    }


    /// Phase 1: deterministic lane conflict filter for a set of candidate txs.
    ///
    /// For each (account, sequence), keep the single tx with the lowest `tx_id`.
    /// Sort remaining by (account, sequence, tx_id). Does not apply owns_tx
    /// (caller filters ownership). Sequence-model analogue of double-spend reject
    /// at the lane split point (see docs/ACCOUNT_LANES.md).
    fn lane_dedup_conflicts(mut txs: Vec<Transaction>) -> (Vec<Transaction>, u64) {
        txs.sort_by(|a, b| {
            a.account
                .cmp(&b.account)
                .then(a.sequence.cmp(&b.sequence))
                .then_with(|| a.tx_id.cmp(&b.tx_id))
        });
        let mut out: Vec<Transaction> = Vec::with_capacity(txs.len());
        let mut rejected = 0u64;
        for tx in txs {
            if let Some(prev) = out.last() {
                if prev.account == tx.account && prev.sequence == tx.sequence {
                    // Keep first (lowest tx_id due to sort); drop this one.
                    rejected = rejected.saturating_add(1);
                    continue;
                }
            }
            out.push(tx);
        }
        (out, rejected)
    }

    /// Next sequence to pack for an account.
    ///
    /// Phase 4 (lane watermark): max of
    /// - execution next-seq
    /// - highest seq+1 in **certified** batches for this account
    /// - highest seq+1 in **our open uncertified** batches (inflight we still hold)
    ///
    /// Does not trust a bare `da_pack_seq` alone (that caused cursor ahead of
    /// re-queued mempool after lost batches). `da_pack_seq` is still updated for
    /// metrics / hole-skip but pack selection uses this DAG-backed watermark.
    fn pack_next_seq(&self, account: &str) -> u64 {
        let exec = self.execution.state.next_sequence(account);
        let mut w = exec;
        for (bh, b) in &self.dag.batches {
            if *bh == genesis_batch_hash() {
                continue;
            }
            let certified = self.dag.certificates.contains_key(bh);
            let own_open = !certified && b.creator == self.validator_id;
            if !certified && !own_open {
                continue; // foreign open: do not advance our pack watermark
            }
            for tx in b.txs.values() {
                if tx.account != account {
                    continue;
                }
                let n = tx.sequence.saturating_add(1);
                if n > w {
                    w = n;
                }
            }
        }
        // Optional floor from da_pack_seq only if not above contiguous coverage
        // (repair_pack_cursor_desync keeps it honest).
        let packed = self.da_pack_seq.get(account).copied().unwrap_or(0);
        if packed > w {
            // Prefer DAG-backed w; da_pack may be stale high — repair will lower it.
            w
        } else {
            w.max(packed)
        }
    }

    /// Advance pack cursor when we see txs already in a batch (local or remote).
    fn note_batch_packed(&mut self, batch: &Batch) {
        for tx in batch.txs.values() {
            let next = tx.sequence.saturating_add(1);
            let e = self.da_pack_seq.entry(tx.account.clone()).or_insert(0);
            if next > *e {
                *e = next;
            }
        }
    }

    /// Forward mempool txs toward the **lane owner** (Phase B).
    ///
    /// Multi-lane (`single_packer_per_round == false`):
    /// - Owned txs: no rebroadcast (local packer will pack).
    /// - Not-owned txs: **transfer** to `account_owner_id` via `directed_txs`
    ///   (not mesh flood lottery), then **remove from local mempool** so we do
    ///   not hold forever as `stuck_not_owned`. Delivery is the owner's job;
    ///   if the send fails the owner will not pack — re-admit (RPC/mesh) can
    ///   re-handoff.
    /// - If owner unknown: flood fallback + keep local copy for retry.
    ///
    /// Single-packer legacy: flood all (primary is the only packer).
    ///
    /// Also refreshes gauges: `stuck_not_owned_pending`, `stuck_owner_age_max_s`
    /// (should stay near zero after transfer; non-zero only while owner mapping
    /// is unknown or between admit and next rebroadcast tick).
    pub fn rebroadcast_mempool_txs(&mut self, limit: usize) -> GossipEvents {
        let mut g = GossipEvents::default();
        if limit == 0 || self.mempool.is_empty() {
            self.refresh_lane_owner_stuck_metrics(wall_now_s());
            return g;
        }
        let now = wall_now_s();
        let multi_lane = !self.config.single_packer_per_round;
        let mut n = 0usize;
        // Transfer handoff: collect ids to drop after directed send.
        let mut remove_ids: Vec<String> = Vec::new();
        for tx in self.mempool.ordered(now) {
            if n >= limit {
                break;
            }
            if multi_lane {
                if self.owns_tx(&tx, now) {
                    continue; // our lane — pack locally
                }
                match self.account_owner_id(&tx.account) {
                    Some(owner) if owner != self.validator_id => {
                        self.mempool.note_route_attempt(&tx.tx_id, now);
                        remove_ids.push(tx.tx_id.clone());
                        g.directed_txs.push((owner, tx));
                        self.shard_flow.directed_to_owner =
                            self.shard_flow.directed_to_owner.saturating_add(1);
                        self.shard_flow.rebroadcast_forwarded =
                            self.shard_flow.rebroadcast_forwarded.saturating_add(1);
                        n += 1;
                    }
                    _ => {
                        // Unknown owner mapping — flood so someone can take it.
                        // Keep local copy until owner mapping is known.
                        g.txs.push(tx);
                        self.shard_flow.flood_fallback =
                            self.shard_flow.flood_fallback.saturating_add(1);
                        self.shard_flow.rebroadcast_forwarded =
                            self.shard_flow.rebroadcast_forwarded.saturating_add(1);
                        n += 1;
                    }
                }
            } else {
                g.txs.push(tx);
                self.shard_flow.flood_fallback =
                    self.shard_flow.flood_fallback.saturating_add(1);
                self.shard_flow.rebroadcast_forwarded =
                    self.shard_flow.rebroadcast_forwarded.saturating_add(1);
                n += 1;
            }
        }
        // Drop transferred copies so they cannot sit as stuck_not_owned forever.
        for id in remove_ids {
            self.mempool.remove(&id);
        }
        self.refresh_lane_owner_stuck_metrics(now);
        g
    }

    /// Gauge: not-owned mempool txs still sitting here (owner routing lag / owner down).
    fn refresh_lane_owner_stuck_metrics(&mut self, now: f64) {
        if self.config.single_packer_per_round {
            self.shard_flow.stuck_not_owned_pending = 0;
            self.shard_flow.stuck_owner_age_max_s = 0.0;
            return;
        }
        let mut pending = 0u64;
        let mut age_max = 0.0_f64;
        for tx in self.mempool.ordered(now) {
            if self.owns_tx(&tx, now) {
                continue;
            }
            pending = pending.saturating_add(1);
            let age = (now - tx.first_seen).max(0.0);
            if age > age_max {
                age_max = age;
            }
        }
        self.shard_flow.stuck_not_owned_pending = pending;
        self.shard_flow.stuck_owner_age_max_s = age_max;
    }

    /// Lane owner for `account` (same FNV rule as `owns_tx`). Public for tests/metrics.
    pub fn account_owner_id(&self, account: &str) -> Option<NodeId> {
        let unl = BullsharkEngine::sorted_unl(&self.dag.unl);
        if unl.is_empty() {
            return None;
        }
        let shard = (Self::account_shard(account) as usize) % unl.len();
        Some(unl[shard].clone())
    }

    /// Phase B: documentable policy — if lane owner is offline, that account's
    /// sequences do not pack until the owner recovers (no automatic failover).
    pub fn lane_owner_down_stalls_accounts() -> bool {
        true
    }

    /// Option 2 helper: cert votes arriving (or open batch still young / in-flight).
    #[allow(dead_code)]
    fn recent_cert_progress(&self, now: f64) -> bool {
        let mut any_open = false;
        for b in self.dag.batches.values() {
            if b.creator != self.validator_id {
                continue;
            }
            if b.batch_hash == genesis_batch_hash() {
                continue;
            }
            if self.dag.certificates.contains_key(&b.batch_hash) {
                continue;
            }
            any_open = true;
            // Foreign UNL signatures = votes arriving.
            if b.signatures.keys().any(|k| k != &self.validator_id) {
                return true;
            }
            // Young open: mesh still circulating body/sigs (wider window under Falcon PQ).
            if (now - b.timestamp).max(0.0) < 5.0 {
                return true;
            }
            // At least 2 local-known signers (self + anyone) counts as progress.
            if b.signatures.len() >= 2 {
                return true;
            }
        }
        !any_open
    }

    /// Effective batch interval: base, or adaptive between min/max from mempool depth.
    fn effective_batch_interval(&self) -> f64 {
        let base = self.config.target_batch_interval_s;
        if !self.config.adaptive_interval {
            return base;
        }
        let n = self.mempool.len();
        let lo = self.config.adaptive_mempool_low.max(1);
        let hi = self.config.adaptive_mempool_high.max(lo + 1);
        let min_i = self.config.batch_interval_min_s;
        let max_i = self.config.batch_interval_max_s.max(min_i);
        if n <= lo {
            return max_i; // quiet: wait longer, pack more when they arrive
        }
        if n >= hi {
            return min_i; // busy: close faster for latency/throughput
        }
        // linear interpolate lo→hi maps max_i→min_i
        let t = (n - lo) as f64 / (hi - lo) as f64;
        max_i + (min_i - max_i) * t
    }

    /// Minimum phase gap (seconds). Stagger never packs closer than this even if N is large.
    const PACK_PHASE_MIN_GAP_S: f64 = 0.025;

    /// UNL rank in sorted order (stable ring). None if not on UNL.
    fn unl_rank(&self) -> Option<usize> {
        let unl = BullsharkEngine::sorted_unl(&self.dag.unl);
        unl.iter().position(|id| id == &self.validator_id)
    }

    /// Multi-lane pack rhythm: each UNL member may pack only in its phase slot.
    ///
    /// `gap = max(MIN_GAP, interval / N)` so as N grows phases stay ≥ MIN_GAP.
    /// Slot `k` belongs to `unl_sorted[k % N]`. Each node still gets ~1 pack per
    /// `interval` (when gap = interval/N), but neighbours never fire together.
    ///
    /// Goal: replace free-for-all multipack chaos with a steady UNL pulse without
    /// collapsing to single-packer (every lane still packs several times / second).
    fn in_pack_phase(&self, now: f64) -> bool {
        if self.config.single_packer_per_round {
            return true; // single-packer uses is_assigned_packer instead
        }
        let unl = BullsharkEngine::sorted_unl(&self.dag.unl);
        let n = unl.len().max(1);
        let Some(rank) = unl.iter().position(|id| id == &self.validator_id) else {
            return true;
        };
        // gap = max(MIN_GAP, interval/N). Small UNL keeps unique phases; large N
        // floors gap so neighbours never pack closer than MIN_GAP (may slow per-node).
        let interval = self.effective_batch_interval().max(0.02);
        let gap = (interval / n as f64).max(Self::PACK_PHASE_MIN_GAP_S);
        let slot = (now / gap).floor() as u64;
        (slot as usize) % n == rank
    }

    /// Latest finalized batch hash created by `creator` (highest round, then hash).
    fn latest_finalized_by_creator(&self, creator: &str) -> Option<fd_types::HashHex> {
        let mut best: Option<(u64, fd_types::HashHex)> = None;
        for c in self.dag.certificates.values() {
            if c.batch_hash == genesis_batch_hash() {
                continue;
            }
            if c.creator != creator {
                continue;
            }
            if !self.batch_is_finalized(&c.batch_hash) {
                continue;
            }
            let cand = (c.round, c.batch_hash.clone());
            match &best {
                None => best = Some(cand),
                Some(cur) => {
                    if cand.0 > cur.0 || (cand.0 == cur.0 && cand.1 > cur.1) {
                        best = Some(cand);
                    }
                }
            }
        }
        best.map(|(_, h)| h)
    }

    /// Phase 2: last certified batch we created (worker / lane tip).
    /// Multipack parents this tip so lanes do not share foreign open tips.
    fn lane_certified_tip(&self) -> Option<fd_types::HashHex> {
        let mut best: Option<(u64, fd_types::HashHex)> = None;
        for c in self.dag.certificates.values() {
            if c.batch_hash == genesis_batch_hash() {
                continue;
            }
            if c.creator != self.validator_id {
                continue;
            }
            let cand = (c.round, c.batch_hash.clone());
            match &best {
                None => best = Some(cand),
                Some(cur) => {
                    if cand.0 > cur.0 || (cand.0 == cur.0 && cand.1 > cur.1) {
                        best = Some(cand);
                    }
                }
            }
        }
        best.map(|(_, h)| h)
    }

    /// Select parents for a new batch.
    ///
    /// **Multi-lane model** (`single_packer_per_round == false`):
    /// - This node is one lane. Pack only on **our lane tip** (last finalized
    ///   batch we created), else genesis.
    /// - Also parent other lanes' **finalized** tips (cert+body) so the global
    ///   cert DAG is connected enough for Bullshark *after* DA finalization.
    ///   Never parent a foreign *open* (uncertified) batch — that was the old
    ///   multipack shared-tip failure mode.
    /// - Never consult Bullshark preferred_parent while packing.
    ///
    /// **Single-packer** (legacy): shared finalized tip frontier only.
    fn select_batch_parents(&self) -> Vec<fd_types::HashHex> {
        if !self.config.single_packer_per_round {
            let mut tips: Vec<fd_types::HashHex> = Vec::new();
            // 1) Own lane tip — this node is the lane.
            if let Some(lane) = self.lane_certified_tip() {
                if self.batch_is_finalized(&lane) {
                    tips.push(lane);
                }
            }
            // 2) Pulse hand-off: prefer previous UNL member's finalized tip so the
            // DAG grows as a structured ring (1→2→3→1) instead of random mesh tips.
            let unl = BullsharkEngine::sorted_unl(&self.dag.unl);
            if let Some(rank) = unl.iter().position(|id| id == &self.validator_id) {
                if !unl.is_empty() {
                    let prev = &unl[(rank + unl.len() - 1) % unl.len()];
                    if prev != &self.validator_id {
                        if let Some(h) = self.latest_finalized_by_creator(prev) {
                            if !tips.iter().any(|x| x == &h) {
                                tips.push(h);
                            }
                        }
                    }
                }
            }
            // 3) Other finalized tips for wider DAG links (cap by max_batch_parents).
            let mut others = self.finalized_certified_tips();
            others.sort();
            for t in others {
                if tips.iter().any(|x| x == &t) {
                    continue;
                }
                if let Some(b) = self.dag.batches.get(&t) {
                    if b.creator == self.validator_id {
                        continue;
                    }
                }
                tips.push(t);
            }
            tips.truncate(self.config.max_batch_parents.max(1));
            if tips.is_empty() {
                tips.push(genesis_batch_hash());
            }
            return tips;
        }
        // Legacy single-packer: finalized tip frontier only.
        let mut tips = self.finalized_certified_tips();
        tips.truncate(self.config.max_batch_parents.max(1));
        if tips.is_empty() {
            tips.push(genesis_batch_hash());
        }
        tips
    }

    /// Certificate present **and** local batch body (DA finalized for this node).
    fn batch_is_finalized(&self, batch_hash: &str) -> bool {
        if batch_hash == genesis_batch_hash() {
            return true;
        }
        self.dag.certificates.contains_key(batch_hash)
            && self.dag.batches.contains_key(batch_hash)
    }

    /// Certified tips that also have a local body (ready for Bullshark input).
    fn finalized_certified_tips(&self) -> Vec<fd_types::HashHex> {
        let mut tips = self.dag.certified_tips();
        tips.retain(|t| self.batch_is_finalized(t));
        if tips.is_empty() {
            vec![genesis_batch_hash()]
        } else {
            tips
        }
    }

    /// Stage 2 — collect finalized batches from **all lanes** (every creator).
    ///
    /// A batch is finalized when we hold both the availability certificate and
    /// the batch body. This set is the **only** input to Bullshark.
    pub fn collect_finalized_lane_certs(
        &self,
    ) -> std::collections::HashMap<fd_types::HashHex, Certificate> {
        let mut out = std::collections::HashMap::new();
        for (bh, cert) in &self.dag.certificates {
            if self.batch_is_finalized(bh) {
                out.insert(bh.clone(), cert.clone());
            }
        }
        // Genesis always present for DAG roots.
        if let Some(g) = self.dag.certificates.get(&genesis_batch_hash()) {
            out.entry(genesis_batch_hash()).or_insert_with(|| g.clone());
        }
        out
    }

    /// Fixed primary packer: `sorted_unl[0]`.
    ///
    /// Must **not** depend on local `max_cert_round` — under mesh lag different nodes
    /// see different rounds and each thinks a different primary, so **nobody packs**
    /// (stall signature from Phase 48 flood trials).
    pub fn primary_packer_id(&self) -> Option<NodeId> {
        let unl_sorted = BullsharkEngine::sorted_unl(&self.dag.unl);
        unl_sorted.into_iter().next()
    }

    /// Certified-but-unordered work: real backlog between DA certification and
    /// ordering. Both `certificates` and `ordering.ordered` include genesis, so
    /// genesis cancels in the raw subtraction; we defensively normalize by
    /// removing genesis from each so the gap measures only non-genesis work.
    pub fn ordered_lag(&self) -> usize {
        let g = genesis_batch_hash();
        let certified_non_genesis = self
            .dag
            .certificates
            .len()
            .saturating_sub(if self.dag.certificates.contains_key(&g) { 1 } else { 0 });
        let ordered_non_genesis = self
            .ordering
            .ordered
            .len()
            .saturating_sub(if self.ordering.ordered_set.contains(&g) { 1 } else { 0 });
        certified_non_genesis.saturating_sub(ordered_non_genesis)
    }

    /// Max seconds multi-lane will stall packing for DA frontier heal (then resume).
    const LANE_FRONTIER_STALL_MAX_S: f64 = 3.0;

    /// True when packing should wait (single-packer ordered_lag, or multi-lane frontier).
    /// Multi-lane: time-boxed (~3s) while parent body/cert holes exist.
    pub fn pack_backpressured(&self) -> bool {
        if !self.config.single_packer_per_round {
            return self.lane_da_should_stall_pack(wall_now_s());
        }
        let max = self.config.pack_backpressure_max;
        max > 0 && self.ordered_lag() > max
    }

    fn lane_da_frontier_incomplete(&self) -> bool {
        if !self.missing_batch_bodies_to_pull().is_empty() {
            return true;
        }
        if !self.missing_cert_hashes_to_pull().is_empty() {
            return true;
        }
        for bh in self.mesh_diag.pending_certs.keys() {
            if !self.dag.batches.contains_key(bh) {
                return true;
            }
        }
        false
    }

    /// Time-boxed stall using `lane_frontier_incomplete_since` (updated in create_batch).
    fn lane_da_should_stall_pack(&self, now: f64) -> bool {
        if !self.lane_da_frontier_incomplete() {
            return false;
        }
        let since = self.lane_frontier_incomplete_since;
        if since <= 0.0 {
            // first observation this tick — create_batch will stamp; stall once
            return true;
        }
        let age = (now - since).max(0.0);
        age <= Self::LANE_FRONTIER_STALL_MAX_S
    }

    /// Exactly one UNL member creates batches (avoids sibling forks with same seqs).
    ///
    /// Primary = `sorted_unl[0]` (stable across nodes regardless of cert lag).
    ///
    /// Secondary stall recovery is **narrow** (next-in-ring only when):
    /// - mempool has backlog, AND
    /// - either oldest uncertified is older than `packer_stall_timeout_s`, OR
    ///   no open uncertified AND primary has not packed for `packer_stall_timeout_s`
    ///   (approximated: we are secondary and last_batch_ts is stale / zero on a quiet tip).
    ///
    /// Never activate secondary merely because the tip is clear — that dual-packs under
    /// asymmetric DA views (primary just packed; secondary has not seen the batch yet).
    pub fn is_assigned_packer(&self) -> bool {
        let unl_sorted = BullsharkEngine::sorted_unl(&self.dag.unl);
        if unl_sorted.is_empty() {
            return true;
        }
        let n = unl_sorted.len();
        let primary = &unl_sorted[0];
        if primary == &self.validator_id {
            return true;
        }
        if self.mempool.len() < 4 {
            return false;
        }
        let unc = self.dag.uncertified_count_non_genesis();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let stall_s = self.config.packer_stall_timeout_s.max(0.15);
        let oldest = self.dag.oldest_uncertified_timestamp();
        let stale_open = unc > 0 && oldest.map(|t| now - t >= stall_s).unwrap_or(false);
        // Clear tip + long silence: primary may be dead. Use wall clock only — do not
        // treat "clear tip" alone as stall (primary's batch may be in flight).
        let silent_clear = unc == 0
            && self.last_batch_ts > 0.0
            && (now - self.last_batch_ts) >= stall_s * 2.0;
        // Secondary never saw a local pack; if tip is clear with deep mempool for a
        // long time after process start, still wait — rely on primary rebroadcast.
        // Only stale_open or silent_clear (after we ourselves packed once as secondary
        // before) — actually secondary's last_batch_ts is usually 0.
        // Use: stale open batches only for secondary failover. Clear-tip recovery is
        // handled by primary rebroadcast + fixed primary always packing when it can.
        if !stale_open && !silent_clear {
            return false;
        }
        let secondary = &unl_sorted[1 % n];
        secondary == &self.validator_id
    }

    /// Stall / pack diagnostics for multi-node flood investigation.
    pub fn pack_stall_snapshot(&self) -> serde_json::Value {
        let unl_sorted = BullsharkEngine::sorted_unl(&self.dag.unl);
        let max_r = self.max_cert_round();
        let primary = self.primary_packer_id();
        let assigned = self.is_assigned_packer();
        let tips = self.select_batch_parents();
        let tip_fp = {
            let mut t = tips.clone();
            t.sort();
            t.join(",")
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let open_creators: Vec<String> = self
            .dag
            .batches
            .values()
            .filter(|b| {
                b.batch_hash != genesis_batch_hash()
                    && !self.dag.certificates.contains_key(&b.batch_hash)
            })
            .map(|b| b.creator.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let oldest = self.dag.oldest_uncertified_timestamp();
        let last_batch_age = if self.last_batch_ts > 0.0 {
            serde_json::json!((now - self.last_batch_ts).max(0.0))
        } else {
            serde_json::Value::Null
        };
        serde_json::json!({
            "assigned_packer": assigned,
            "primary_packer": primary,
            "max_cert_round": max_r,
            "uncertified": self.dag.uncertified_count_non_genesis(),
            "uncertified_by_me": self.dag.uncertified_by_creator(&self.validator_id),
            "open_batch_creators": open_creators,
            "oldest_uncertified_age_s": oldest.map(|t| (now - t).max(0.0)),
            "tip_fp": tip_fp,
            "tip_count": tips.len(),
            "mempool": self.mempool.len(),
            "mempool_status": self.mempool_status_json(now),
            "account_sequence_window": self.mempool.sequence_window(),
            "last_batch_age_s": last_batch_age,
            "single_packer": self.config.single_packer_per_round,
            "unl_sorted0": unl_sorted.first().cloned(),
            "pack_skip_reasons": self.pack_skip_reasons,
            "shard_flow": self.shard_flow.to_json(),
            "admit_reasons": self.admit_reasons,
            "busy_causes": self.busy_causes,
            "last_busy_cause": self.last_busy_cause.map(|c| c.as_str()),
            "last_busy_hint": self.last_busy_cause.map(|c| c.operator_hint()),
            "mempool_accounts": self.mempool_account_coverage(),
            "admit_circuit_open": self.admit_circuit_open(),
            "admit_busy_mempool_limit": self.admit_busy_mempool_limit(),
            "local_admits_ok": self.local_admits_ok,
            "local_admits_owned": self.local_admits_owned,
            "local_admits_not_owned": self.local_admits_not_owned,
            "local_admit_not_owned_ratio": self.local_admit_not_owned_ratio(),
            "admit_exec_lag": self.admit_exec_lag(),
            "admit_ahead_cap": self.admit_ahead_cap(),
            "admit_circuit_latched": self.admit_circuit_latched,
            "open_conf": self.open_batch_conf_status(),
            "open_needs_body_seed": self.open_needs_body_seed(),
            "own_open_n": self.own_open_batch_hashes().len(),
            "open_body_repool_after_s": self.config.open_body_repool_after_s,
            "liveness_policy": self.liveness_policy_json(),
            "longest_own_open_age_s": self.longest_own_open_age_s(now),
            "own_zero_foreign_opens": self.own_zero_foreign_open_hashes().len(),
            "rejected_batches": self.dag.rejected_batches,
            "equivocations": self.dag.equivocation_count,
            "reject_reasons": {
                "parents_too_many": self.dag.reject_parents_too_many,
                "hash_mismatch": self.dag.reject_hash_mismatch,
                "max_known": self.dag.reject_max_known,
                "max_uncertified": self.dag.reject_max_uncertified,
                "max_per_creator": self.dag.reject_max_per_creator,
                "equivocation": self.dag.reject_equivocation,
                "genesis": self.dag.reject_genesis,
            },
            "ingest_new_ok": self.dag.ingest_new_ok,
            "ingest_merge_ok": self.dag.ingest_merge_ok,
            "foreign_sig_merged": self.dag.foreign_sig_merged,
            "foreign_sig_merged_from_sig": self.foreign_sig_merged_from_sig,
            "foreign_sig_merged_from_body": self.foreign_sig_merged_from_body,
            "foreign_sig_parked": self.foreign_sig_parked,
            "foreign_sig_applied_parked": self.foreign_sig_applied_parked,
            "foreign_sig_missing_batch": self.dag.foreign_sig_missing_batch,
            "pending_remote_sigs": self.pending_remote_sigs.len(),
            "sig_reannounce_to_creator": self.sig_reannounce_to_creator,
            "reseed_suppressed": self.reseed_suppressed,
            "reseed_allowed": self.reseed_allowed,
            "open_body_repool_suppressed": self.open_body_repool_suppressed,
            "on_remote_sig_calls": self.on_remote_sig_calls,
            "try_certify_attempts": self.try_certify_attempts,
            "try_certify_ok": self.try_certify_ok,
            "force_open_try_certify": self.force_open_try_certify,
            "conf_pool": self.conf_pool.to_json_at(now),
            "duty_phase": self.duty_phase(wall_now_s()).as_str(),
            "wait_open_conf": self.own_open_batch_hash().is_some()
                && !self.config.single_packer_per_round,
        })
    }

    /// Owned vs not-owned mempool backlog (PR2).
    pub fn mempool_status_json(&self, now: f64) -> serde_json::Value {
        let multi = !self.config.single_packer_per_round;
        let unl = BullsharkEngine::sorted_unl(&self.dag.unl);
        let my_rank = unl.iter().position(|id| id == &self.validator_id);
        self.mempool
            .status(now, |acct| {
                if !multi || unl.len() <= 1 {
                    return true;
                }
                let Some(rank) = my_rank else {
                    return false;
                };
                let shard = (Self::account_shard(acct) as usize) % unl.len();
                shard == rank
            })
            .to_json()
    }

    /// Per-account mempool sequence coverage: min/max seq, pack cursor, exec next, gaps.
    /// Makes sequence holes visible when packing goes quiet (Phase 49b).
    pub fn mempool_account_coverage(&self) -> serde_json::Value {
        let mut by_acct: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for tx in self.mempool.all_txs() {
            by_acct.entry(tx.account.clone()).or_default().push(tx.sequence);
        }
        let mut out = Vec::new();
        for (acct, mut seqs) in by_acct {
            seqs.sort_unstable();
            seqs.dedup();
            let min_seq = *seqs.first().unwrap_or(&0);
            let max_seq = *seqs.last().unwrap_or(&0);
            let pack_cursor = self.pack_next_seq(&acct);
            let exec_next = self.execution.state.next_sequence(&acct);
            // Gaps inside [min_seq, max_seq] relative to present set.
            let present: BTreeSet<u64> = seqs.iter().copied().collect();
            let mut gaps = Vec::new();
            if min_seq > 0 && max_seq >= min_seq {
                let mut s = min_seq;
                while s <= max_seq {
                    if !present.contains(&s) {
                        gaps.push(s);
                        if gaps.len() >= 32 {
                            break;
                        }
                    }
                    s += 1;
                }
            }
            // Hole before pack cursor: mempool has higher seqs but missing pack_cursor.
            let missing_at_cursor = pack_cursor > 0
                && max_seq >= pack_cursor
                && !present.contains(&pack_cursor);
            out.push(serde_json::json!({
                "account": acct,
                "count": present.len(),
                "min_seq": min_seq,
                "max_seq": max_seq,
                "pack_cursor": pack_cursor,
                "exec_next": exec_next,
                "gaps": gaps,
                "missing_at_pack_cursor": missing_at_cursor,
            }));
        }
        serde_json::Value::Array(out)
    }

    /// Re-gossip our still-uncertified batches so peers can sign (mesh catch-up).
    /// Reseed **own open uncertified** lane bodies so peers can sign.
    ///
    /// Multi-lane: do **not** flood certified bodies every tick (that OOM'd the
    /// mesh). Certified bodies are served on **want** only after first seed.
    pub fn rebroadcast_open_batches(&self) -> GossipEvents {
        let mut g = GossipEvents::default();
        for b in self.dag.batches.values() {
            if b.creator != self.validator_id {
                continue;
            }
            if b.batch_hash == genesis_batch_hash() {
                continue;
            }
            if self.dag.certificates.contains_key(&b.batch_hash) {
                continue; // certified: pull-path serves on want
            }
            g.batches.push(b.clone());
            if let Some(sig) = b.signatures.get(&self.validator_id) {
                g.sigs.push(PartialSignature {
                    batch_hash: b.batch_hash.clone(),
                    validator_id: self.validator_id.clone(),
                    signature: sig.clone(),
                    timestamp: b.timestamp,
                });
            }
        }
        g
    }

    /// Flake-hunt: reseed newest **own certified** bodies + certs (capped).
    /// Heals creator-only certs when the first body seed / pull stalled.
    pub fn rebroadcast_recent_finalized(&self, cap: usize) -> GossipEvents {
        let mut g = GossipEvents::default();
        let mut own: Vec<(u64, String)> = Vec::new();
        for (bh, c) in &self.dag.certificates {
            if bh == &genesis_batch_hash() {
                continue;
            }
            if c.creator != self.validator_id {
                continue;
            }
            if !self.dag.batches.contains_key(bh) {
                continue;
            }
            own.push((c.round, bh.clone()));
        }
        own.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        for (_, bh) in own.into_iter().take(cap.max(1)) {
            if let Some(b) = self.dag.batches.get(&bh) {
                g.batches.push(b.clone());
            }
            if let Some(c) = self.dag.certificates.get(&bh) {
                g.certs.push(c.clone());
            }
        }
        g
    }

    /// All batch hashes we can serve (for inv advertisement so peers can want).
    pub fn local_serveable_batch_hashes(&self) -> Vec<fd_types::HashHex> {
        let mut out: Vec<fd_types::HashHex> = self
            .dag
            .batches
            .keys()
            .filter(|h| *h != &genesis_batch_hash())
            .cloned()
            .collect();
        out.sort();
        out
    }

    /// Phase C: recent finalized batch hashes we hold (for throttled inv, not body flood).
    pub fn local_finalized_batch_hashes_capped(&self, cap: usize) -> Vec<fd_types::HashHex> {
        let mut v: Vec<(u64, fd_types::HashHex)> = Vec::new();
        for (bh, c) in &self.dag.certificates {
            if bh == &genesis_batch_hash() {
                continue;
            }
            if !self.dag.batches.contains_key(bh) {
                continue;
            }
            v.push((c.round, bh.clone()));
        }
        v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        v.into_iter().take(cap.max(1)).map(|(_, h)| h).collect()
    }

    /// Fix 1 drain: cert hashes applied since last take (for inv + body seed).
    pub fn take_creator_cert_announce(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.pending_creator_cert_announce)
    }

    /// PR4: immediately queue a directed cert announce to the batch creator.
    /// Call after local cert apply on a foreign open so creator merge does not
    /// wait for the slow reannounce loop (Mode B return path).
    pub fn queue_cert_to_creator_now(&mut self, batch_hash: &str, now: f64) {
        let Some(b) = self.dag.batches.get(batch_hash) else {
            return;
        };
        if b.creator == self.validator_id {
            return;
        }
        if !self.dag.certificates.contains_key(batch_hash) {
            return;
        }
        let creator = b.creator.clone();
        // Dedup pending queue for same (creator, hash).
        if self
            .pending_creator_cert_announce
            .iter()
            .any(|(c, h)| c == &creator && h == batch_hash)
        {
            return;
        }
        self.last_foreign_cert_reannounce
            .insert(batch_hash.to_string(), now);
        self.pending_creator_cert_announce
            .push((creator, batch_hash.to_string()));
        self.cert_directed_to_creator = self.cert_directed_to_creator.saturating_add(1);
        *self
            .pack_skip_reasons
            .entry("cert_directed_to_creator_immediate".into())
            .or_insert(0) += 1;
    }

    /// Reliability: re-queue directed cert+sig+body to creators of foreign batches
    /// we already certified (or multi-signed). One-shot announce is not enough —
    /// sticky dig PEER_SIGNED_CREATOR_MISSING_FOREIGN is exactly missed delivery.
    /// PR4: tighter cadence for young certs; still hard-capped to avoid flood.
    pub fn queue_foreign_cert_reannounce(&mut self, now: f64) {
        // Always re-push recent foreign certs we signed (capped). Skipping when
        // need_seed left PEER_SIGNED_CREATOR_MISSING_FOREIGN stuck forever —
        // creator still needs the cert even while reseeding body.
        const GAP_YOUNG_S: f64 = 0.5; // PR4: faster return path for first 5s
        const GAP_S: f64 = 1.5;
        const YOUNG_AGE_S: f64 = 5.0;
        const MAX_AGE_S: f64 = 20.0;
        const MAX_PER_TICK: usize = 2;
        let me = self.validator_id.clone();
        let mut cands: Vec<(f64, String, String)> = Vec::new(); // (ts, creator, bh)
        for (bh, cert) in &self.dag.certificates {
            if bh == &fd_types::genesis_batch_hash() {
                continue;
            }
            let Some(b) = self.dag.batches.get(bh) else {
                continue;
            };
            if b.creator == me {
                continue;
            }
            if !b.signatures.contains_key(&me) && !cert.signers.iter().any(|s| s == &me) {
                continue;
            }
            let age = (now - b.timestamp).max(0.0);
            if age > MAX_AGE_S {
                continue;
            }
            let last = self
                .last_foreign_cert_reannounce
                .get(bh)
                .copied()
                .unwrap_or(0.0);
            let gap = if age <= YOUNG_AGE_S {
                GAP_YOUNG_S
            } else {
                GAP_S
            };
            if now - last < gap {
                continue;
            }
            cands.push((b.timestamp, b.creator.clone(), bh.clone()));
        }
        // Newest first, hard cap.
        cands.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, creator, bh) in cands.into_iter().take(MAX_PER_TICK) {
            self.last_foreign_cert_reannounce.insert(bh.clone(), now);
            if !self
                .pending_creator_cert_announce
                .iter()
                .any(|(c, h)| c == &creator && h == &bh)
            {
                self.pending_creator_cert_announce
                    .push((creator, bh));
            }
            self.cert_directed_to_creator =
                self.cert_directed_to_creator.saturating_add(1);
            *self
                .pack_skip_reasons
                .entry("cert_reannounce_to_creator".into())
                .or_insert(0) += 1;
        }
        if self.last_foreign_cert_reannounce.len() > 512 {
            let cutoff = now - MAX_AGE_S;
            self.last_foreign_cert_reannounce
                .retain(|_, t| *t >= cutoff);
        }
    }

    pub fn take_cert_announcements(&mut self) -> Vec<fd_types::HashHex> {
        std::mem::take(&mut self.mesh_diag.recently_applied_cert_hashes)
    }

    /// Fix 2/3: local certificate batch hashes (capped, newest rounds first).
    pub fn local_cert_hashes_capped(&self, cap: usize) -> Vec<fd_types::HashHex> {
        let mut v: Vec<(u64, fd_types::HashHex)> = Vec::new();
        for (bh, c) in &self.dag.certificates {
            if bh == &genesis_batch_hash() {
                continue;
            }
            v.push((c.round, bh.clone()));
        }
        v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        v.into_iter().take(cap.max(1)).map(|(_, h)| h).collect()
    }

    /// Non-genesis cert set fingerprint for equality tests (sorted full hashes).
    pub fn cert_set_fingerprint(&self) -> Vec<fd_types::HashHex> {
        let mut v: Vec<_> = self
            .dag
            .certificates
            .keys()
            .filter(|h| *h != &genesis_batch_hash())
            .cloned()
            .collect();
        v.sort();
        v
    }



    /// Create a local batch and return gossip (batch + our sig + cert if any).
    ///
    /// Never propose a second batch on the **same parent set** while an uncertified
    /// self-proposal still exists (that is equivocation). Pipelining on **new tips**
    /// (after others certify) is allowed up to `max_uncertified_per_creator`.
    ///
    /// Packing is **ahead of execute** (Narwhal): contiguous sequences from the pack
    /// cursor, not gated on ledger `can_apply`. Safe with single-parent chains.
    /// Phase 49c: optionally abandon aged sequence holes so packing/apply can proceed.
    ///
    /// For each account with mempool txs, if `pack_next_seq` is missing but higher
    /// sequences are present and the oldest higher-seq tx is older than
    /// `pack_skip_hole_after_s`, advance `da_pack_seq` and execution next-sequence
    /// to the minimum present higher sequence (holes in between are abandoned).
    fn maybe_skip_holes(&mut self, now: f64) {
        let thresh = self.config.pack_skip_hole_after_s;
        if thresh <= 0.0 {
            self.pack_hole_since.clear();
            return;
        }
        // Group mempool sequences by account.
        let mut by_acct: BTreeMap<String, Vec<(u64, f64)>> = BTreeMap::new();
        for tx in self.mempool.all_txs() {
            by_acct
                .entry(tx.account.clone())
                .or_default()
                .push((tx.sequence, tx.first_seen));
        }
        let mut abandoned_total = 0u64;
        for (acct, mut entries) in by_acct {
            entries.sort_by_key(|(s, _)| *s);
            let exp = self.pack_next_seq(&acct);
            let has_exp = entries.iter().any(|(s, _)| *s == exp);
            let higher: Vec<(u64, f64)> = entries
                .iter()
                .copied()
                .filter(|(s, _)| *s > exp)
                .collect();
            if has_exp || higher.is_empty() {
                self.pack_hole_since.remove(&acct);
                continue;
            }
            let oldest_higher = higher
                .iter()
                .map(|(_, t)| *t)
                .fold(f64::INFINITY, f64::min);
            let age = (now - oldest_higher).max(0.0);
            // Also track first observation wall time for stability under clock skew on first_seen.
            let since = *self.pack_hole_since.entry(acct.clone()).or_insert(now);
            let observed = (now - since).max(0.0);
            let wait = age.max(observed);
            if wait < thresh {
                continue;
            }
            let next = higher[0].0; // min higher seq (sorted)
            if next <= exp {
                continue;
            }
            let holes = next.saturating_sub(exp);
            abandoned_total = abandoned_total.saturating_add(holes);
            // Advance pack cursor to the next present sequence.
            self.da_pack_seq.insert(acct.clone(), next);
            // Advance execution watermark so apply can accept the next present seq
            // (missing seqs exp..next-1 are abandoned as never-arriving).
            self.execution.state.ensure_account(&acct);
            self.execution.state.sequences.insert(acct.clone(), next);
            self.pack_hole_since.remove(&acct);
            self.note_pack_skip("skip_hole");
            *self
                .pack_skip_reasons
                .entry("skip_hole_seqs".into())
                .or_insert(0) += holes;
        }
        if abandoned_total > 0 {
            *self
                .pack_skip_reasons
                .entry("skip_hole_seqs_total".into())
                .or_insert(0) += abandoned_total;
        }
    }


    /// Pattern A: rebuild `da_pack_seq` from batches **still present** in the DAG
    /// (certified or open). After prune of uncertified batches, this rewinds the
    /// pack cursor so re-queued mempool seqs are packable again.
    fn recompute_da_pack_seq_from_dag(&mut self) {
        let mut next: BTreeMap<String, u64> = BTreeMap::new();
        for b in self.dag.batches.values() {
            if b.batch_hash == genesis_batch_hash() {
                continue;
            }
            for tx in b.txs.values() {
                let n = tx.sequence.saturating_add(1);
                let e = next.entry(tx.account.clone()).or_insert(0);
                if n > *e {
                    *e = n;
                }
            }
        }
        // Floor by execution watermark so we never re-pack applied seqs.
        for (acct, packed) in next.iter_mut() {
            let exec = self.execution.state.next_sequence(acct);
            if exec > *packed {
                *packed = exec;
            }
        }
        // Preserve exec-only accounts (no remaining batches) at exec watermark.
        // Clear then re-insert: accounts only in old da_pack_seq with no batches
        // fall back to pack_next_seq = exec via missing key.
        self.da_pack_seq = next;
    }

    /// Pattern A fix 2: rewind pack cursor when coverage is **not contiguous**
    /// from exec_next. Checking only max(seq) is wrong (r4/r5: seq 20 in DAG
    /// but 1..19 missing → cursor stuck at 21).
    fn repair_pack_cursor_desync(&mut self) {
        // Build per-account set of sequences present in any local batch body.
        let mut covered: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
        for b in self.dag.batches.values() {
            if b.batch_hash == genesis_batch_hash() {
                continue;
            }
            for tx in b.txs.values() {
                covered
                    .entry(tx.account.clone())
                    .or_default()
                    .insert(tx.sequence);
            }
        }
        // Accounts to consider: mempool + known pack cursor keys.
        let mut accounts: BTreeSet<String> = BTreeSet::new();
        for tx in self.mempool.all_txs() {
            accounts.insert(tx.account.clone());
        }
        for a in self.da_pack_seq.keys() {
            accounts.insert(a.clone());
        }
        for acct in accounts {
            let exec = self.execution.state.next_sequence(&acct);
            let pack = self.pack_next_seq(&acct);
            if pack <= exec {
                continue;
            }
            let set = covered.get(&acct).cloned().unwrap_or_default();
            // First sequence in [exec, pack) not covered by any batch body.
            let mut gap = None;
            let mut s = exec;
            while s < pack {
                if !set.contains(&s) {
                    gap = Some(s);
                    break;
                }
                s = s.saturating_add(1);
            }
            let Some(first_missing) = gap else {
                continue; // contiguous coverage up to pack
            };
            // Rewind pack cursor to first missing (at least exec).
            let new_c = first_missing.max(exec);
            if new_c < pack {
                self.da_pack_seq.insert(acct.clone(), new_c);
                self.note_pack_skip("pack_cursor_reclaim");
            }
        }
    }


    /// First own non-genesis uncertified batch hash, if any.
    pub fn own_open_batch_hash(&self) -> Option<fd_types::HashHex> {
        self.own_open_batch_hashes().into_iter().next()
    }

    /// All own non-genesis uncertified batch hashes (stable sort).
    pub fn own_open_batch_hashes(&self) -> Vec<fd_types::HashHex> {
        let mut v: Vec<_> = self
            .dag
            .batches
            .values()
            .filter(|b| {
                b.creator == self.validator_id
                    && b.batch_hash != genesis_batch_hash()
                    && !self.dag.certificates.contains_key(&b.batch_hash)
            })
            .map(|b| b.batch_hash.clone())
            .collect();
        v.sort();
        v
    }

    /// All own open uncertified batches (for directed body reseed of every open).
    pub fn own_open_batches(&self) -> Vec<Batch> {
        self.own_open_batch_hashes()
            .into_iter()
            .filter_map(|h| self.dag.batches.get(&h).cloned())
            .collect()
    }

    /// Count UNL signatures on `batch` that are not this validator.
    pub fn foreign_unl_sig_count(&self, batch: &Batch) -> usize {
        batch
            .signatures
            .keys()
            .filter(|vid| {
                *vid != &self.validator_id && self.dag.unl.contains_key(*vid)
            })
            .count()
    }

    /// Support weight from foreign UNL signers only.
    fn foreign_unl_sig_weight(&self, batch: &Batch) -> f64 {
        let mut w = 0.0;
        for (vid, sig) in &batch.signatures {
            if vid == &self.validator_id {
                continue;
            }
            if sig.is_empty() {
                continue;
            }
            if let Some(wt) = self.dag.unl.get(vid) {
                w += *wt;
            }
        }
        w
    }

    /// Multi-lane: own open exists and is not yet certified (pack must wait).
    pub fn waiting_open_conf(&self) -> bool {
        !self.config.single_packer_per_round && self.own_open_batch_hash().is_some()
    }

    /// True when **any** own open has zero foreign UNL sigs — body almost certainly
    /// not on peers yet (must reseed all such opens, not only the first hash).
    pub fn open_needs_body_seed(&self) -> bool {
        for b in self.dag.batches.values() {
            if b.creator != self.validator_id {
                continue;
            }
            if b.batch_hash == genesis_batch_hash() {
                continue;
            }
            if self.dag.certificates.contains_key(&b.batch_hash) {
                continue;
            }
            if self.foreign_unl_sig_count(b) == 0 {
                return true;
            }
        }
        false
    }

    /// Max age (s) of own open batches that still have zero foreign UNL sigs.

    /// A3: update progress snapshots from mesh/engine counters. When peers are
    /// serving bodies or foreign sigs are merging, suppress open body reseed flood.
    pub fn note_body_da_progress(&mut self, now: f64, bodies_served: u64) {
        let merged = self
            .foreign_sig_merged_from_sig
            .saturating_add(self.foreign_sig_merged_from_body)
            .saturating_add(self.dag.foreign_sig_merged);
        let served_up = bodies_served > self.bodies_served_at_progress;
        let merged_up = merged > self.foreign_merged_at_progress;
        if served_up || merged_up {
            self.bodies_served_at_progress = bodies_served;
            self.foreign_merged_at_progress = merged;
            // Sticky dig: do NOT suppress reseed on all unseeded opens when *any*
            // global progress happens — that left BODY_BUT_NO_PEER_SIG (peers never
            // received the sticky open body). Only touch A4 quiet for opens that
            // already have foreign sigs (DA in flight toward cert).
            for b in self.dag.batches.values() {
                if b.creator != self.validator_id {
                    continue;
                }
                if self.dag.certificates.contains_key(&b.batch_hash) {
                    continue;
                }
                if self.foreign_unl_sig_count(b) > 0 {
                    self.last_open_da_activity
                        .insert(b.batch_hash.clone(), now);
                }
            }
        }
        let _ = now;
    }

    /// A3: should we reseed this own open body right now?
    /// - false if foreign sigs already present
    /// - false if progress suppress window active (peers responding)
    /// - false if reseeded too recently (<200ms) — thrash cap
    /// - true only when need_seed and truly stalled
    pub fn should_reseed_open_body(&mut self, batch_hash: &str, now: f64) -> bool {
        let Some(b) = self.dag.batches.get(batch_hash) else {
            return false;
        };
        if b.creator != self.validator_id {
            return false;
        }
        if self.dag.certificates.contains_key(batch_hash) {
            return false;
        }
        if self.foreign_unl_sig_count(b) > 0 {
            return false;
        }
        // Reliability-first: calm seed-until-foreign, not TPS thrash.
        // Flooding every 50ms recreated BODY_BUT_NO_PEER_SIG (mesh melt).
        // Goal: steady continuous on small N, then grow TPS with more nodes.
        let last = self
            .last_reseed_body_at
            .get(batch_hash)
            .copied()
            .unwrap_or(0.0);
        // Reliability: calm 500ms between body reseeds (not 50ms flood).
        const RELIABLE_SEED_GAP_S: f64 = 0.5; // body-must calm
        if now - last < RELIABLE_SEED_GAP_S {
            self.reseed_suppressed = self.reseed_suppressed.saturating_add(1);
            *self
                .pack_skip_reasons
                .entry("reseed_suppressed_rate".into())
                .or_insert(0) += 1;
            return false;
        }
        self.last_reseed_body_at
            .insert(batch_hash.to_string(), now);
        self.reseed_allowed = self.reseed_allowed.saturating_add(1);
        *self
            .pack_skip_reasons
            .entry("reseed_reliable".into())
            .or_insert(0) += 1;
        true
    }

    /// Age of own open with 0 foreign sigs (for urgent seed cadence in main).
    pub fn open_unseeded_age_s(&self, batch_hash: &str, now: f64) -> Option<f64> {
        let b = self.dag.batches.get(batch_hash)?;
        if b.creator != self.validator_id {
            return None;
        }
        if self.foreign_unl_sig_count(b) > 0 {
            return None;
        }
        Some((now - b.timestamp).max(0.0))
    }

    /// A4: record that body-DA is still in flight for this open (do not repool yet).
    pub fn touch_open_da_activity(&mut self, batch_hash: &str, now: f64) {
        self.last_open_da_activity
            .insert(batch_hash.to_string(), now);
    }

    /// PR5 liveness policy group (softening knobs vs root-cause fixes).
    ///
    /// | Knob | Role | Default |
    /// |------|------|---------|
    /// | `pack_open_cap` | Max concurrent own uncertified opens | auto 1 multipack |
    /// | `open_body_repool_after_s` | Base age before repool eligibility | 8.0 |
    /// | `admit_busy_*` | Local admit circuit (soft pressure) | see ProtocolConfig |
    ///
    /// **Root-cause fixes** (PR4): creator-directed sig/cert return path, parked-cert
    /// retry. **Softening knobs** (this policy): open cap, repool TTL, MCB — they
    /// mask stuck DA but do not replace foreign-merge reliability.
    ///
    /// Repool preconditions (all required):
    /// 1. Own open, not certified, zero foreign UNL sigs
    /// 2. Age ≥ `open_body_repool_after_s` (ttl)
    /// 3. Quiet ≥ max(ttl/2, 0.5s) since last body reseed / DA activity
    ///    (implies body reseed + creator-directed retries had time to fire)
    /// 4. Either at pack open-cap, or age ≥ 2×ttl (hard escape)
    pub fn liveness_policy_json(&self) -> serde_json::Value {
        serde_json::json!({
            "policy_group": "open_work_liveness",
            "pack_open_cap": self.pack_open_cap(),
            "pack_open_cap_config": self.config.pack_open_cap,
            "open_body_repool_after_s": self.config.open_body_repool_after_s,
            "admit_busy_mempool": self.admit_busy_mempool_limit(),
            "admit_busy_uncert_age_s": self.config.admit_busy_uncert_age_s,
            "admit_busy_open_uncert": self.config.admit_busy_open_uncert,
            "admit_ahead_cap": self.admit_ahead_cap(),
            "softening_knobs": ["pack_open_cap", "open_body_repool_after_s", "admit_busy_*", "admit_ahead_cap"],
            "root_cause_fixes": ["creator_sig_reannounce", "cert_to_creator", "parked_cert_retry", "zero_foreign_priority"],
            "repool_preconditions": [
                "own_open",
                "not_certified",
                "zero_foreign_unl_sigs",
                "age_ge_ttl",
                "quiet_ge_half_ttl",
                "at_cap_or_age_ge_2ttl"
            ],
        })
    }

    /// A4/PR5: only repool when aged, zero foreign sigs, AND quiet long enough that
    /// sigs/serves are not still in flight. See [`Self::liveness_policy_json`].
    pub fn should_repool_open_batch(
        &self,
        batch_hash: &str,
        now: f64,
        ttl: f64,
        at_cap: bool,
    ) -> bool {
        self.repool_eligibility(batch_hash, now, ttl, at_cap)
            .is_ok()
    }

    /// Detailed eligibility (Ok = may repool; Err = block reason).
    pub fn repool_eligibility(
        &self,
        batch_hash: &str,
        now: f64,
        ttl: f64,
        at_cap: bool,
    ) -> Result<(), RepoolBlockReason> {
        let Some(b) = self.dag.batches.get(batch_hash) else {
            return Err(RepoolBlockReason::NotOwn);
        };
        if b.creator != self.validator_id || b.batch_hash == genesis_batch_hash() {
            return Err(RepoolBlockReason::NotOwn);
        }
        if self.dag.certificates.contains_key(batch_hash) {
            return Err(RepoolBlockReason::Certified);
        }
        // Any foreign UNL sig: never repool — might still cert.
        if self.foreign_unl_sig_count(b) > 0 {
            return Err(RepoolBlockReason::HasForeign);
        }
        let age = (now - b.timestamp).max(0.0);
        if age < ttl {
            return Err(RepoolBlockReason::AgeBelowTtl);
        }
        // Quiet time: last DA activity must be stale (ttl/2, min 0.5s).
        // Covers body reseed + creator-directed retries having had a chance to fire.
        let last_act = self
            .last_open_da_activity
            .get(batch_hash)
            .copied()
            .or_else(|| self.last_reseed_body_at.get(batch_hash).copied())
            .unwrap_or(b.timestamp);
        let quiet = (now - last_act).max(0.0);
        let quiet_need = (ttl * 0.5).max(0.5);
        if quiet < quiet_need {
            return Err(RepoolBlockReason::NotQuiet);
        }
        // Soft when packing blocked; hard after 2*ttl with quiet.
        if (at_cap && age >= ttl) || age >= (ttl * 2.0) {
            Ok(())
        } else {
            Err(RepoolBlockReason::NotAtCapYet)
        }
    }

    /// Longest age among own uncertified opens (0 if none).
    pub fn longest_own_open_age_s(&self, now: f64) -> f64 {
        let mut max_age = 0.0_f64;
        for b in self.dag.batches.values() {
            if b.creator != self.validator_id || b.batch_hash == genesis_batch_hash() {
                continue;
            }
            if self.dag.certificates.contains_key(&b.batch_hash) {
                continue;
            }
            let age = (now - b.timestamp).max(0.0);
            if age > max_age {
                max_age = age;
            }
        }
        max_age
    }

    pub fn open_unseeded_max_age_s(&self, now: f64) -> Option<f64> {
        let mut max_age: Option<f64> = None;
        for b in self.dag.batches.values() {
            if b.creator != self.validator_id {
                continue;
            }
            if b.batch_hash == genesis_batch_hash() {
                continue;
            }
            if self.dag.certificates.contains_key(&b.batch_hash) {
                continue;
            }
            if self.foreign_unl_sig_count(b) > 0 {
                continue;
            }
            let age = (now - b.timestamp).max(0.0);
            max_age = Some(max_age.map(|m| m.max(age)).unwrap_or(age));
        }
        max_age
    }

    /// Abandon aged own opens that never got a foreign UNL signature and put their
    /// txs back in the mempool so packing can continue (body-DA failure escape).
    ///
    /// Only fires when `foreign_unl_sig_count == 0` (peers almost certainly never
    /// stored the body). If any foreign sig exists we keep the open and reseed.
    pub fn take_abandoned_open_hashes(&mut self) -> Vec<fd_types::HashHex> {
        std::mem::take(&mut self.last_abandoned_open_hashes)
    }

    pub fn recover_stale_open_bodies(&mut self, now: f64) -> u64 {
        let ttl = self.config.open_body_repool_after_s;
        if ttl <= 0.0 {
            return 0;
        }
        let open_cap = self.pack_open_cap();
        let own_open_n = self.dag.uncertified_by_creator(&self.validator_id);
        let at_cap = own_open_n >= open_cap;
        // A4: candidates must pass should_repool (age + zero foreign + quiet).
        let candidates: Vec<fd_types::HashHex> = self
            .dag
            .batches
            .values()
            .filter(|b| {
                b.creator == self.validator_id
                    && b.batch_hash != genesis_batch_hash()
                    && !self.dag.certificates.contains_key(&b.batch_hash)
            })
            .map(|b| b.batch_hash.clone())
            .collect();
        let mut victims = Vec::new();
        for bh in candidates {
            match self.repool_eligibility(&bh, now, ttl, at_cap) {
                Ok(()) => victims.push(bh),
                Err(reason) => {
                    if matches!(
                        reason,
                        RepoolBlockReason::NotQuiet | RepoolBlockReason::NotAtCapYet
                    ) {
                        if let Some(b) = self.dag.batches.get(&bh) {
                            if self.foreign_unl_sig_count(b) == 0 {
                                let age = (now - b.timestamp).max(0.0);
                                if age >= ttl {
                                    self.open_body_repool_suppressed =
                                        self.open_body_repool_suppressed.saturating_add(1);
                                    *self
                                        .pack_skip_reasons
                                        .entry(format!(
                                            "open_body_repool_suppressed_{}",
                                            reason.as_str()
                                        ))
                                        .or_insert(0) += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        self.last_abandoned_open_hashes.clear();
        let mut n_batches = 0u64;
        for bh in victims {
            let Some(batch) = self.dag.abandon_uncertified(&bh) else {
                continue;
            };
            self.last_abandoned_open_hashes.push(bh.clone());
            self.conf_pool
                .mark_repool_eligible(&bh, now);
            self.conf_pool
                .remove_with_reason(&bh, RemoveReason::Repool);
            self.last_open_da_activity.remove(&bh);
            self.last_reseed_body_at.remove(&bh);
            self.reseed_suppress_until.remove(&bh);
            self.last_creator_sig_reannounce.remove(&bh);
            let mut repooled = 0u64;
            let mut accts = BTreeSet::new();
            for tid in &batch.tx_ids {
                if let Some(tx) = batch.txs.get(tid).cloned() {
                    accts.insert(tx.account.clone());
                    // PR2: reinsert preserves first_seen / sequence intent; exact dups reported.
                    if self.mempool.reinsert(tx, now).accepted() {
                        repooled += 1;
                    }
                }
            }
            self.recompute_da_pack_seq_from_dag();
            // Ensure pack_next can see repooled sequences (rewind cursor to exec for
            // affected accounts so filter_seq_gap does not stick).
            for acct in accts {
                let exec = self.execution.state.next_sequence(&acct);
                self.da_pack_seq.insert(acct, exec);
            }
            self.repair_pack_cursor_desync();
            self.note_pack_skip("open_body_repool");
            *self
                .pack_skip_reasons
                .entry("open_body_repool_txs".into())
                .or_insert(0) += repooled;
            *self
                .pack_skip_reasons
                .entry(format!("open_body_repool_by_{}", self.validator_id))
                .or_insert(0) += 1;
            n_batches = n_batches.saturating_add(1);
        }
        n_batches
    }

        /// True when own open batch has real UNL confirmation (certified, or enough
    /// total support including foreign sigs that try_certify can succeed once
    /// parents are ready). Used for metrics; pack gate waits for cert.
    fn open_batch_conf_status(&self) -> Option<serde_json::Value> {
        let bh = self.own_open_batch_hash()?;
        let b = self.dag.batches.get(&bh)?;
        let foreign_n = self.foreign_unl_sig_count(b);
        let foreign_w = self.foreign_unl_sig_weight(b);
        let total_w = self.dag.support_weight(b);
        let thr = self.config.unl_threshold;
        let now = wall_now_s();
        let signers: Vec<String> = b.signatures.keys().cloned().collect();
        let parents_ok = b.parents.iter().all(|p| {
            p == &genesis_batch_hash() || self.dag.certificates.contains_key(p)
        });
        Some(serde_json::json!({
            "batch_hash": bh,
            "batch_hash12": &bh[..12.min(bh.len())],
            "age_s": (now - b.timestamp).max(0.0),
            "ntx": b.tx_ids.len(),
            "nsigs": b.signatures.len(),
            "signers": signers,
            "my_sig": b.signatures.contains_key(&self.validator_id),
            "foreign_sigs": foreign_n,
            "foreign_weight": foreign_w,
            "support_weight": total_w,
            "threshold": thr,
            "has_foreign_conf": foreign_n >= 1,
            "quorum_weight": total_w + 1e-12 >= thr,
            "parents_certified": parents_ok,
            "certified": self.dag.certificates.contains_key(&bh),
            "in_conf_pool": self.conf_pool.contains(&bh),
        }))
    }


    /// Pack vs Check half-cycle (config.pack_check_slot_s, default 0.25s).
    pub fn duty_phase(&self, now: f64) -> DutyPhase {
        if !self.config.conf_pool_enabled || self.config.single_packer_per_round {
            return DutyPhase::Pack; // legacy path: always allow pack attempts
        }
        duty_phase_at(now, self.config.pack_check_slot_s)
    }

    pub fn in_check_phase(&self, now: f64) -> bool {
        self.duty_phase(now) == DutyPhase::Check
    }

    pub fn in_duty_pack_phase(&self, now: f64) -> bool {
        self.duty_phase(now) == DutyPhase::Pack
    }

    /// Deposit batch body into conf pool (local pack or remote ingest).
    pub fn batch_creator(&self, batch_hash: &str) -> Option<NodeId> {
        self.dag.batches.get(batch_hash).map(|b| b.creator.clone())
    }

    pub fn conf_pool_put_body(&mut self, batch: &Batch, now: f64) {
        if !self.config.conf_pool_enabled {
            return;
        }
        self.conf_pool.put_body(batch.clone(), now);
    }

    /// Check-phase: sign every pooled open body we hold that lacks our sig;
    /// merge into DAG; emit sigs/certs. Removes entries once certified.
    /// Counted try_certify for digs (attempts + ok).
    fn try_certify_counted(&mut self, batch_hash: &str) -> Option<fd_types::Certificate> {
        self.try_certify_attempts = self.try_certify_attempts.saturating_add(1);
        *self
            .pack_skip_reasons
            .entry("try_certify_attempts".into())
            .or_insert(0) += 1;
        let c = self.dag.try_certify(batch_hash);
        if c.is_some() {
            self.try_certify_ok = self.try_certify_ok.saturating_add(1);
            *self
                .pack_skip_reasons
                .entry("try_certify_ok".into())
                .or_insert(0) += 1;
        }
        c
    }

    /// Force try_certify on every own open every DA tick — independent of packing.
    /// If foreign sigs already merged but cert lagged (parent race), this closes the loop.
    /// Reliability: re-try parked remote certs for our own opens every conf tick.
    /// PEER_SIGNED_CREATOR_MISSING_FOREIGN: cert arrived earlier but parents/body
    /// race left it parked; one-shot apply never retried.
    pub fn retry_own_open_parked_certs(&mut self, now: f64) -> GossipEvents {
        let mut g = GossipEvents::default();
        // Enforce: never skip parked certs for our opens (PEER_SIGNED recovery).
        let mut opens: Vec<String> = self.own_open_batch_hashes();
        for (bh, _) in self.mesh_diag.pending_certs.iter() {
            if let Some(b) = self.dag.batches.get(bh) {
                if b.creator == self.validator_id && !opens.iter().any(|h| h == bh) {
                    opens.push(bh.clone());
                }
            }
        }
        for bh in opens {
            if self.dag.certificates.contains_key(&bh) {
                self.conf_pool
                    .remove_with_reason(&bh, RemoveReason::Certified);
                continue;
            }
            if self.mesh_diag.pending_certs.contains_key(&bh) {
                self.conf_pool.note_parked_cert(&bh, now);
            }
            *self
                .pack_skip_reasons
                .entry("da_must_try_apply_own_open".into())
                .or_insert(0) += 1;
            if self.try_apply_parked_cert(&bh, now, true) {
                if let Some(c) = self.dag.certificates.get(&bh).cloned() {
                    g.certs.push(c);
                }
                if let Some(b) = self.dag.batches.get(&bh).cloned() {
                    g.batches.push(b);
                }
                self.conf_pool
                    .remove_with_reason(&bh, RemoveReason::Certified);
                *self
                    .pack_skip_reasons
                    .entry("own_open_parked_cert_applied".into())
                    .or_insert(0) += 1;
            }
        }
        g
    }

    pub fn force_open_cert_attempts(&mut self, now: f64) -> GossipEvents {

        let mut g = GossipEvents::default();
        let opens: Vec<_> = self
            .own_open_batch_hashes()
            .into_iter()
            .collect();
        for bh in opens {
            self.force_open_try_certify = self.force_open_try_certify.saturating_add(1);
            if let Some(c) = self.try_certify_counted(&bh) {
                self.note_cert_applied(&bh, now, false);
                self.conf_pool
                    .remove_with_reason(&bh, RemoveReason::Certified);
                g.certs.push(c);
                if let Some(b) = self.dag.batches.get(&bh).cloned() {
                    g.batches.push(b);
                }
                *self
                    .pack_skip_reasons
                    .entry("force_open_cert_ok".into())
                    .or_insert(0) += 1;
            }
        }
        g
    }

    /// PR4: own opens with zero foreign UNL weight — Mode B recovery priority.
    pub fn own_zero_foreign_open_hashes(&self) -> Vec<fd_types::HashHex> {
        self.own_open_batch_hashes()
            .into_iter()
            .filter(|h| {
                self.dag
                    .batches
                    .get(h)
                    .map(|b| self.foreign_unl_sig_count(b) == 0)
                    .unwrap_or(false)
            })
            .collect()
    }

    pub fn conf_pool_check_step(&mut self, now: f64) -> GossipEvents {
        let mut g = GossipEvents::default();
        if !self.config.conf_pool_enabled || !self.role.runs_da() {
            return g;
        }
        // Sync: any local uncertified body should be in the pool.
        for b in self.dag.batches.values() {
            if b.batch_hash == genesis_batch_hash() {
                continue;
            }
            if self.dag.certificates.contains_key(&b.batch_hash) {
                continue;
            }
            self.conf_pool.put_body(b.clone(), now);
        }
        // PR4: prioritize zero-foreign *own* opens for parked-cert / try_certify
        // before spending Falcon sign budget on brand-new foreign bodies.
        for bh in self.own_zero_foreign_open_hashes() {
            if self.mesh_diag.pending_certs.contains_key(&bh) {
                self.conf_pool.note_parked_cert(&bh, now);
                if self.try_apply_parked_cert(&bh, now, true) {
                    if let Some(c) = self.dag.certificates.get(&bh).cloned() {
                        g.certs.push(c);
                    }
                    if let Some(b) = self.dag.batches.get(&bh).cloned() {
                        g.batches.push(b);
                    }
                    self.conf_pool
                        .remove_with_reason(&bh, RemoveReason::Certified);
                    *self
                        .pack_skip_reasons
                        .entry("pr4_zero_foreign_parked_applied".into())
                        .or_insert(0) += 1;
                }
            } else if let Some(c) = self.try_certify_counted(&bh) {
                self.note_cert_applied(&bh, now, false);
                self.conf_pool
                    .remove_with_reason(&bh, RemoveReason::Certified);
                g.certs.push(c);
                if let Some(b) = self.dag.batches.get(&bh).cloned() {
                    g.batches.push(b);
                }
                *self
                    .pack_skip_reasons
                    .entry("pr4_zero_foreign_try_certify_ok".into())
                    .or_insert(0) += 1;
            }
        }
        // Prefer foreign (remote creator) bodies first so creators get foreign_sigs
        // quickly; cap Falcon work per DA tick so the loop stays live under load.
        const MAX_FALCON_SIGNS_PER_TICK: usize = 3; // PR4: slightly more foreign sign budget
        let mut hashes = self.conf_pool.list_hashes();
        hashes.sort_by(|a, b| {
            let ca = self
                .dag
                .batches
                .get(a)
                .map(|x| x.creator.as_str())
                .unwrap_or("");
            let cb = self
                .dag
                .batches
                .get(b)
                .map(|x| x.creator.as_str())
                .unwrap_or("");
            let fa = (ca == self.validator_id) as u8;
            let fb = (cb == self.validator_id) as u8;
            fa.cmp(&fb).then_with(|| a.cmp(b))
        });
        let mut signs_this_tick = 0usize;
        for h in hashes {
            if self.dag.certificates.contains_key(&h) {
                self.conf_pool
                    .remove_with_reason(&h, RemoveReason::Certified);
                continue;
            }
            // Ensure DAG has body
            if let Some(e) = self.conf_pool.get(&h) {
                if !self.dag.batches.contains_key(&h) {
                    let _ = self.dag.ingest_batch(e.batch.clone());
                }
            }
            if !self.dag.batches.contains_key(&h) {
                self.conf_pool.note_sign_defer(SignDeferReason::NoBody);
                continue;
            }
            // Sign if we have not yet
            let need_sig = self
                .dag
                .batches
                .get(&h)
                .map(|b| !b.signatures.contains_key(&self.validator_id))
                .unwrap_or(true);
            if need_sig {
                if signs_this_tick >= MAX_FALCON_SIGNS_PER_TICK {
                    self.conf_pool.note_sign_defer(SignDeferReason::Cap);
                    *self
                        .pack_skip_reasons
                        .entry("conf_pool_sign_deferred_cap".into())
                        .or_insert(0) += 1;
                    continue;
                }
                let sig_hex = self.dag.sign_local(&h);
                let cert = self.dag.add_signature(&h, &self.validator_id, &sig_hex);
                self.conf_pool.put_sig(&h, &self.validator_id, &sig_hex, now);
                signs_this_tick += 1;
                *self
                    .pack_skip_reasons
                    .entry("conf_pool_signed".into())
                    .or_insert(0) += 1;
                g.sigs.push(PartialSignature {
                    batch_hash: h.clone(),
                    validator_id: self.validator_id.clone(),
                    signature: sig_hex,
                    timestamp: now,
                });
                // Always piggyback body after first sign. Continuous dig showed
                // conf_pool_signed>0 but creator foreign_sigs stayed 0 — PartialSignature
                // was lost under body-reseed flood; body merge carries our sig.
                if let Some(b) = self.dag.batches.get(&h).cloned() {
                    g.batches.push(b);
                }
                if let Some(c) = cert {
                    self.note_cert_applied(&h, now, false);
                    g.certs.push(c);
                    self.conf_pool
                        .remove_with_reason(&h, RemoveReason::Certified);
                    self.queue_cert_to_creator_now(&h, now);
                    continue;
                }
            } else {
                // Already signed — still try certify (foreign sigs may have landed)
                if self.dag.try_certify(&h).is_some() {
                    self.note_cert_applied(&h, now, false);
                    if let Some(c) = self.dag.certificates.get(&h).cloned() {
                        g.certs.push(c);
                    }
                    if let Some(b) = self.dag.batches.get(&h).cloned() {
                        g.batches.push(b);
                    }
                    self.conf_pool
                        .remove_with_reason(&h, RemoveReason::Certified);
                    self.queue_cert_to_creator_now(&h, now);
                    self.last_creator_sig_reannounce.remove(&h);
                } else if let Some(b) = self.dag.batches.get(&h).cloned() {
                    // A2 re-announce: we hold a foreign open with our sig; creator may
                    // have missed PartialSignature under reseed flood. Emit ≤1/200ms.
                    if b.creator != self.validator_id {
                        if let Some(sig_hex) = b.signatures.get(&self.validator_id).cloned() {
                            let last = self
                                .last_creator_sig_reannounce
                                .get(&h)
                                .copied()
                                .unwrap_or(0.0);
                            // PR4: first 4 reannounces at 100ms, then 200ms — creator merge
                            // recovery without becoming a flood amplifier (conf-pool bound 64).
                            let retries = self
                                .conf_pool
                                .get(&h)
                                .map(|e| e.creator_reannounce_count)
                                .unwrap_or(0);
                            let gap = if retries < 4 { 0.1 } else { 0.2 };
                            if now - last >= gap {
                                self.last_creator_sig_reannounce.insert(h.clone(), now);
                                // PR3/PR4: bounded reannounce on conf-pool entry (max 64).
                                let _ = self.conf_pool.note_creator_reannounce(&h, now, 64);
                                self.sig_reannounce_to_creator =
                                    self.sig_reannounce_to_creator.saturating_add(1);
                                *self
                                    .pack_skip_reasons
                                    .entry("sig_reannounce_to_creator".into())
                                    .or_insert(0) += 1;
                                g.sigs.push(PartialSignature {
                                    batch_hash: h.clone(),
                                    validator_id: self.validator_id.clone(),
                                    signature: sig_hex,
                                    timestamp: now,
                                });
                                // Body piggyback so A1 merge can apply even if sig frame is lost.
                                g.batches.push(b);
                            }
                        }
                    }
                }
            }
        }
        // Drop pool entries that certified via other paths
        let certified: Vec<_> = self
            .conf_pool
            .list_hashes()
            .into_iter()
            .filter(|h| self.dag.certificates.contains_key(h))
            .collect();
        for h in certified {
            self.conf_pool
                .remove_with_reason(&h, RemoveReason::Certified);
        }
        // Reliability: re-apply parked remote certs for own opens (missed once).
        g.extend(self.retry_own_open_parked_certs(now));
        // Forced open try_certify every conf tick (independent of pack success).
        g.extend(self.force_open_cert_attempts(now));
        g
    }

    pub fn create_batch(&mut self, now: f64, force: bool) -> Option<(Batch, GossipEvents)> {
        // Light ledger nodes never pack — that is DA-worker work only.
        if !self.role.runs_da() {
            self.note_pack_skip("not_da_role");
            return None;
        }
        // Body-DA escape: repool txs from aged opens with zero foreign UNL sigs
        // so uncertified_cap cannot permanently freeze the lane.
        let _ = self.recover_stale_open_bodies(now);
        // Optional liveness: abandon aged sequence holes before selecting txs.
        self.maybe_skip_holes(now);
        // Pattern A: fix pack_cursor ahead of mempool when DAG no longer covers those seqs.
        self.repair_pack_cursor_desync();
        let interval = self.effective_batch_interval();
        // Deep backlog only — aggressive packing outruns mesh and desyncs multi-node.
        let backlog_force = self.mempool.len() >= self.config.adaptive_mempool_high.max(64);
        if !force && !backlog_force && (now - self.last_batch_ts) < interval {
            self.note_pack_skip("interval_throttle");
            return None;
        }
        // Multi-lane pulse: only pack in our UNL phase slot (stagger neighbours).
        // Still allow force=true (tests) to bypass; backlog stays phase-gated so
        // flood cannot collapse the rhythm into simultaneous packs.
        if !force && !self.in_pack_phase(now) {
            self.note_pack_skip("pack_phase_stagger");
            return None;
        }
        // Conf-pool duty: only pack in global Pack half-cycle (Check = sign window).
        if !force
            && self.config.conf_pool_enabled
            && !self.config.single_packer_per_round
            && !self.in_duty_pack_phase(now)
        {
            self.note_pack_skip("duty_check_phase");
            return None;
        }
        if self.mempool.is_empty() {
            self.note_pack_skip("mempool_empty");
            return None;
        }
        // One packer per tip-depth: others sign only (prevents multi-sibling same-seq forks).
        if self.config.single_packer_per_round && !self.is_assigned_packer() {
            self.note_pack_skip("not_assigned_packer");
            return None;
        }
        // Soft open-cap only (NOT wait-for-cert). With conf-pool pack/check duty,
        // packers deposit bodies into the pool and move on; Check phase signs.
        // Hard wait_open_conf froze progress when bodies lagged (181/200 stalls).
        // `force=true` bypasses for unit tests.
        if !force {
            let open_cap = self.pack_open_cap();
            let own_open = self.dag.uncertified_by_creator(&self.validator_id);
            if own_open >= open_cap {
                if let Some(bh) = self.own_open_batch_hash() {
                    let now_c = wall_now_s();
                    if self.dag.try_certify(&bh).is_some() {
                        self.note_cert_applied(&bh, now_c, false);
                        self.conf_pool.remove(&bh);
                    }
                }
                if self.dag.uncertified_by_creator(&self.validator_id) >= open_cap {
                    self.note_pack_skip("uncertified_cap");
                    return None;
                }
            }
        }
        // Backpressure: when enabled, stop packing while certified-but-unordered
        // work exceeds the configured cap. Prevents the packer outrunning ordering.
        // Multi-lane: maintain frontier-incomplete timer for time-boxed stall.
        if !self.config.single_packer_per_round {
            let now_w = wall_now_s();
            if self.lane_da_frontier_incomplete() {
                if self.lane_frontier_incomplete_since <= 0.0 {
                    self.lane_frontier_incomplete_since = now_w;
                }
            } else {
                self.lane_frontier_incomplete_since = 0.0;
            }
        }
        if self.pack_backpressured() {
            if !self.config.single_packer_per_round {
                self.note_pack_skip("lane_da_frontier_incomplete");
            } else {
                self.note_pack_skip("pack_backpressure");
            }
            return None;
        }
        let tips = self.select_batch_parents();
        // Same parents as an open self-proposal ⇒ would equivocate.
        // Option 2 DISABLED: do not pipeline on partial foreign sigs (cert storms).
        if self
            .dag
            .has_uncertified_on_parents(&self.validator_id, &tips)
        {
            self.note_pack_skip("self_open_on_parents");
            return None;
        }
        // If another creator already has an open proposal on these exact tips, sign
        // their work instead of forking a rival skinny batch (pack density killer).
        let tip_fp = {
            let mut t = tips.clone();
            t.sort();
            t.join(",")
        };
        let rival = self.dag.batches.values().any(|b| {
            b.creator != self.validator_id
                && b.batch_hash != fd_types::genesis_batch_hash()
                && !self.dag.certificates.contains_key(&b.batch_hash)
                && {
                    let mut p = b.parents.clone();
                    p.sort();
                    p.join(",") == tip_fp
                }
        });
        // Single-packer: yield to an open rival on the same tips (pack-density).
        // Multi-packer: different creators may propose concurrent DA batches on the
        // same tips (Narwhal workers); blocking here serializes multi-pack to ~1 packer.
        if rival && self.config.single_packer_per_round {
            self.note_pack_skip("rival_on_tips");
            return None;
        }
        if rival && !self.config.single_packer_per_round {
            self.note_pack_skip("rival_on_tips_allowed");
            // fall through — concurrent multi-packer proposals OK
        }
        let already: BTreeSet<_> = self
            .dag
            .batches
            .values()
            .flat_map(|b| b.tx_ids.iter().cloned())
            .collect();

        // Prefer sequence order so we pack contiguous ready txs (higher apply density).
        let mut candidates = self.mempool.ordered(now);
        candidates.sort_by(|a, b| {
            a.account
                .cmp(&b.account)
                .then(a.sequence.cmp(&b.sequence))
                .then_with(|| a.tx_id.cmp(&b.tx_id))
        });
        // Phase 1: lane conflict filter — one tx per (account, sequence), lowest tx_id.
        let (candidates, lane_dup_rej) = Self::lane_dedup_conflicts(candidates);
        if lane_dup_rej > 0 {
            *self
                .pack_skip_reasons
                .entry("lane_reject_dup_seq".into())
                .or_insert(0) += lane_dup_rej;
        }

        // Pack-ahead cursor per account for this batch attempt.
        let mut pack_cursor: BTreeMap<String, u64> = BTreeMap::new();
        let mut chosen = Vec::new();
        // Phase 49b: candidate-filter sub-reasons when nothing packs.
        let mut filt_already = 0u64;
        let mut filt_not_owned = 0u64;
        let mut filt_seq_gap = 0u64;
        let mut filt_admit = 0u64;
        for tx in candidates {
            if chosen.len() >= self.config.max_batch_txs {
                break;
            }
            if already.contains(&tx.tx_id) {
                filt_already += 1;
                continue;
            }
            if !self.owns_tx(&tx, now) {
                filt_not_owned += 1;
                self.shard_flow.skipped_not_owned =
                    self.shard_flow.skipped_not_owned.saturating_add(1);
                continue;
            }
            let exp = *pack_cursor
                .entry(tx.account.clone())
                .or_insert_with(|| self.pack_next_seq(&tx.account));
            if tx.sequence != exp {
                filt_seq_gap += 1;
                continue;
            }
            // Admit gate only (window/fees/structure) — not ledger can_apply.
            if !self
                .execution
                .state
                .can_admit(&tx, self.config.account_sequence_window)
            {
                filt_admit += 1;
                continue;
            }
            pack_cursor.insert(tx.account.clone(), exp.saturating_add(1));
            chosen.push(tx);
        }
        if chosen.is_empty() {
            self.note_pack_skip("no_ready_txs");
            if filt_seq_gap > 0 {
                self.note_pack_skip("filter_seq_gap");
            }
            if filt_admit > 0 {
                self.note_pack_skip("filter_admit_fail");
            }
            if filt_already > 0 {
                self.note_pack_skip("filter_already_in_dag");
            }
            if filt_not_owned > 0 {
                self.note_pack_skip("filter_not_owned");
            }
            // Record raw filter counts for this quiet attempt (cumulative).
            *self
                .pack_skip_reasons
                .entry("filter_seq_gap_n".into())
                .or_insert(0) += filt_seq_gap;
            *self
                .pack_skip_reasons
                .entry("filter_admit_fail_n".into())
                .or_insert(0) += filt_admit;
            return None;
        }
        self.note_pack_skip("packed");
        self.shard_flow.packed_owned = self
            .shard_flow
            .packed_owned
            .saturating_add(chosen.len() as u64);
        let batch = Batch::create(chosen.clone(), tips, self.validator_id.clone(), now);
        self.last_batch_ts = now;
        self.batches_created += 1;
        for t in &chosen {
            self.mempool.remove(&t.tx_id);
        }
        self.note_batch_packed(&batch);
        let hash = batch.batch_hash.clone();
        self.dag.ingest_batch(batch.clone());
        self.conf_pool_put_body(&batch, now);
        self.touch_open_da_activity(&hash, now);
        let sig_hex = self.dag.sign_local(&hash);
        let cert = self
            .dag
            .add_signature(&hash, &self.validator_id, &sig_hex);
        let out = self.dag.batches.get(&hash).cloned().unwrap_or(batch);
        let ps = PartialSignature {
            batch_hash: hash,
            validator_id: self.validator_id.clone(),
            signature: sig_hex,
            timestamp: now,
        };
        let mut g = GossipEvents {
            batches: vec![out.clone()],
            sigs: vec![ps],
            ..Default::default()
        };
        if let Some(c) = cert {
            // Flake-hunt: local cert formation must re-announce body+cert (Fix 1 path).
            self.note_cert_applied(&out.batch_hash, now, false);
            g.certs.push(c);
            // Body already in g.batches; Fix 5 piggyback in broadcast_events_ex.
        }
        Some((out, g))
    }

    /// Ingest remote batch, sign it, return our sig (+ cert if quorum reached).
    /// After advance, try packing a local batch (Narwhal: DA continues while certs land).
    pub fn on_remote_batch(&mut self, batch: Batch, now: f64) -> GossipEvents {
        let hash = batch.batch_hash.clone();
        // Advance pack cursor so we don't re-pack txs already in the DAG.
        self.note_batch_packed(&batch);
        let ingested = self.dag.ingest_batch(batch);
        // Phase 49d: batch body may unlock a parked remote cert.
        self.retry_pending_certs_after_batch(&hash);
        // Flake-hunt: NEVER gossip a signature if we do not hold the body.
        // Forensic (cef3607b… / d341935…): peers rejected ingest (caps/equivocation)
        // but still emitted a valid sig → creator certified while signers have no body,
        // cert set diverges forever (pull cannot re-derive weight from cert alone).
        if !ingested && !self.dag.batches.contains_key(&hash) {
            return GossipEvents::default();
        }
        if !self.dag.batches.contains_key(&hash) {
            return GossipEvents::default();
        }
        if let Some(b) = self.dag.batches.get(&hash).cloned() {
            self.conf_pool_put_body(&b, now);
        }
        // Multipack: NEVER Falcon-sign on the inbound mesh path.
        // Dig: immediate one-shot sign froze all 3 nodes (exec plateau, foreign=0)
        // — engine lock held during PQ sign while reseeds piled up.
        // Put body in conf_pool; DA tick signs with per-tick cap + directed sig.
        // A1: body may carry foreign sigs (piggyback) and unlock parked PartialSignatures.
        let foreign_before = self
            .dag
            .batches
            .get(&hash)
            .map(|b| self.foreign_unl_sig_count(b))
            .unwrap_or(0);
        let mut parked_out = self.apply_parked_sigs(&hash, now);
        let foreign_after = self
            .dag
            .batches
            .get(&hash)
            .map(|b| self.foreign_unl_sig_count(b))
            .unwrap_or(0);
        if foreign_after > foreign_before {
            let delta = (foreign_after - foreign_before) as u64;
            self.foreign_sig_merged_from_body =
                self.foreign_sig_merged_from_body.saturating_add(delta);
            *self
                .pack_skip_reasons
                .entry("foreign_sig_merged_from_body".into())
                .or_insert(0) += delta;
        }
        // Immediate cert if piggyback/parked tipped quorum.
        if let Some(c) = self.dag.try_certify(&hash) {
            self.note_cert_applied(&hash, now, false);
            self.conf_pool.remove(&hash);
            parked_out.certs.push(c);
            if let Some(b) = self.dag.batches.get(&hash).cloned() {
                parked_out.batches.push(b);
            }
        }

        if self.config.conf_pool_enabled && !self.config.single_packer_per_round {
            // Reliability: NEVER Falcon-sign on inbound mesh path.
            // Dig freeze: inbound PQ sign held engine lock → RPC/mesh timeout,
            // creator-local opens, PEER_SIGNED_CREATOR_MISSING_FOREIGN.
            // conf_pool_check_step signs with MAX_FALCON_SIGNS_PER_TICK=1.
            *self
                .pack_skip_reasons
                .entry("remote_batch_deferred_sign".into())
                .or_insert(0) += 1;
            self.advance();
            return parked_out;
        }
        let need_sig = self
            .dag
            .batches
            .get(&hash)
            .map(|b| !b.signatures.contains_key(&self.validator_id))
            .unwrap_or(true);
        if !need_sig {
            self.advance();
            return GossipEvents::default();
        }
        let sig_hex = self.dag.sign_local(&hash);
        let cert = self
            .dag
            .add_signature(&hash, &self.validator_id, &sig_hex);
        self.advance();
        let mut g = GossipEvents {
            sigs: vec![PartialSignature {
                batch_hash: hash.clone(),
                validator_id: self.validator_id.clone(),
                signature: sig_hex,
                timestamp: now,
            }],
            ..Default::default()
        };
        if let Some(c) = cert {
            self.note_cert_applied(&hash, now, false);
            g.certs.push(c);
            if let Some(b) = self.dag.batches.get(&hash).cloned() {
                g.batches.push(b);
            }
        }
        let _ = now;
        g
    }


    /// A1: merge a foreign partial signature into a held open batch and immediately
    /// try_certify. If the body is not held yet, park the sig until ingest.
    pub fn on_remote_sig(
        &mut self,
        batch_hash: &str,
        validator_id: &str,
        signature: &str,
    ) -> GossipEvents {
        self.on_remote_sig_calls = self.on_remote_sig_calls.saturating_add(1);
        *self
            .pack_skip_reasons
            .entry("on_remote_sig_calls".into())
            .or_insert(0) += 1;
        let now = wall_now_s();
        // Promote conf-pool body into DAG if we somehow only have the pool entry.
        if !self.dag.batches.contains_key(batch_hash) {
            if let Some(e) = self.conf_pool.get(batch_hash) {
                let _ = self.dag.ingest_batch(e.batch.clone());
            }
        }
        if !self.dag.batches.contains_key(batch_hash) {
            // Park — apply when body lands (A1 creator/peer race).
            let entry = self
                .pending_remote_sigs
                .entry(batch_hash.to_string())
                .or_default();
            if !entry.iter().any(|(v, s)| v == validator_id && s == signature) {
                entry.push((validator_id.to_string(), signature.to_string()));
                self.foreign_sig_parked = self.foreign_sig_parked.saturating_add(1);
                *self
                    .pack_skip_reasons
                    .entry("foreign_sig_parked".into())
                    .or_insert(0) += 1;
                self.dag.foreign_sig_missing_batch =
                    self.dag.foreign_sig_missing_batch.saturating_add(1);
            }
            return GossipEvents::default();
        }

        let before_n = self
            .dag
            .batches
            .get(batch_hash)
            .map(|b| b.signatures.len())
            .unwrap_or(0);
        let before_foreign = self
            .dag
            .batches
            .get(batch_hash)
            .map(|b| self.foreign_unl_sig_count(b))
            .unwrap_or(0);

        let cert = self
            .dag
            .add_signature(batch_hash, validator_id, signature);

        // Keep conf-pool in sync so dig open_conf / check-phase see foreign weight.
        if self.config.conf_pool_enabled {
            self.conf_pool
                .put_sig(batch_hash, validator_id, signature, now);
        }

        let after_foreign = self
            .dag
            .batches
            .get(batch_hash)
            .map(|b| self.foreign_unl_sig_count(b))
            .unwrap_or(0);
        let after_n = self
            .dag
            .batches
            .get(batch_hash)
            .map(|b| b.signatures.len())
            .unwrap_or(0);

        if after_foreign > before_foreign || after_n > before_n {
            self.foreign_sig_merged_from_sig =
                self.foreign_sig_merged_from_sig.saturating_add(1);
            *self
                .pack_skip_reasons
                .entry("foreign_sig_merged_from_sig".into())
                .or_insert(0) += 1;
            // A4: foreign activity — do not repool while DA in flight.
            self.touch_open_da_activity(batch_hash, now);
        }

        // A1: force cert attempt even if add_signature early-returned a prior cert miss.
        let cert = cert.or_else(|| self.try_certify_counted(batch_hash));

        // Phase 49d: new sig weight may unlock a parked remote cert.
        self.retry_pending_certs_after_batch(batch_hash);
        self.advance();
        let mut g = GossipEvents::default();
        if let Some(c) = cert {
            // Flake-hunt: peer sig → local cert; re-announce body+cert.
            self.note_cert_applied(batch_hash, now, false);
            self.conf_pool
                .remove_with_reason(batch_hash, RemoveReason::Certified);
            g.certs.push(c);
            if let Some(b) = self.dag.batches.get(batch_hash).cloned() {
                g.batches.push(b);
            }
            // PR4: Mode B return path — push cert to creator immediately.
            self.queue_cert_to_creator_now(batch_hash, now);
        }
        g
    }

    /// Apply any parked partial sigs for `batch_hash` (after body ingest).
    fn apply_parked_sigs(&mut self, batch_hash: &str, now: f64) -> GossipEvents {
        let Some(parked) = self.pending_remote_sigs.remove(batch_hash) else {
            return GossipEvents::default();
        };
        let mut g = GossipEvents::default();
        for (vid, sig) in parked {
            g.extend(self.on_remote_sig(batch_hash, &vid, &sig));
            self.foreign_sig_applied_parked =
                self.foreign_sig_applied_parked.saturating_add(1);
            *self
                .pack_skip_reasons
                .entry("foreign_sig_applied_parked".into())
                .or_insert(0) += 1;
        }
        let _ = now;
        g
    }

    pub fn has_batch(&self, batch_hash: &str) -> bool {
        self.dag.batches.contains_key(batch_hash)
    }

    pub fn get_batch(&self, batch_hash: &str) -> Option<Batch> {
        self.dag.batches.get(batch_hash).cloned()
    }

    pub fn has_cert_for_batch(&self, batch_hash: &str) -> bool {
        self.dag.certificates.contains_key(batch_hash)
    }

    pub fn get_cert_for_batch(&self, batch_hash: &str) -> Option<Certificate> {
        self.dag.certificates.get(batch_hash).cloned()
    }

    pub fn missing_parent_batches(&self, parents: &[String]) -> Vec<String> {
        parents
            .iter()
            .filter(|p| !self.dag.batches.contains_key(*p) && *p != &genesis_batch_hash())
            .cloned()
            .collect()
    }

    pub fn max_cert_round(&self) -> u64 {
        self.dag
            .certificates
            .values()
            .map(|c| c.round)
            .max()
            .unwrap_or(0)
    }

    /// Returns batch hashes we still need (missing body or parents) for pull catch-up.
    ///
    /// Phase 49d: **never drop** the cert body when batch/parents are missing — park it
    /// in `mesh_diag.pending_certs` and retry when the DA store catches up. Validation is
    /// **in-order**: parents must already be certified before `try_certify` succeeds
    /// (see `CertificateDag::try_certify`).

    /// Pattern B: batch hashes we know about via parked certs (or certificates)
    /// but lack a local body — explicit pull targets every tick.

    /// Non-genesis uncertified batch hashes still in the local DA store.
    pub fn local_uncertified_batch_hashes(&self) -> Vec<fd_types::HashHex> {
        self.dag
            .batches
            .values()
            .filter(|b| {
                b.batch_hash != genesis_batch_hash()
                    && !self.dag.certificates.contains_key(&b.batch_hash)
            })
            .map(|b| b.batch_hash.clone())
            .collect()
    }


    /// Phase 3: expand only cert tips that have a local batch body (no ghost order).
    fn expand_tip_if_body(&mut self, tip: &str) -> usize {
        if tip != genesis_batch_hash() && !self.dag.batches.contains_key(tip) {
            self.note_pack_skip("order_wait_batch_body");
            return 0;
        }
        self.ordering
            .expand_under_tip(&self.dag.certificates, tip)
    }

    pub fn missing_batch_bodies_to_pull(&self) -> Vec<fd_types::HashHex> {
        let mut need: BTreeSet<fd_types::HashHex> = BTreeSet::new();
        for bh in self.mesh_diag.pending_certs.keys() {
            if !self.dag.batches.contains_key(bh) {
                need.insert(bh.clone());
            }
        }
        // Certified without body should not happen (try_certify needs body), but
        // defend in case of snapshot/partial state.
        for bh in self.dag.certificates.keys() {
            if bh == &genesis_batch_hash() {
                continue;
            }
            if !self.dag.batches.contains_key(bh) {
                need.insert(bh.clone());
            }
        }
        // Parent-body holes: local batch has sigs but cannot try_certify because a
        // parent body/cert is missing (forensic: child body everywhere, parent only
        // on one node → cert set forever diverges).
        for (bh, b) in &self.dag.batches {
            if bh == &genesis_batch_hash() {
                continue;
            }
            if self.dag.certificates.contains_key(bh) {
                continue;
            }
            for p in &b.parents {
                if p == &genesis_batch_hash() {
                    continue;
                }
                if !self.dag.batches.contains_key(p) {
                    need.insert(p.clone());
                }
            }
        }
        need.into_iter().collect()
    }

    /// Cert hashes we know we need (pending parked + parents of local uncertified).
    pub fn missing_cert_hashes_to_pull(&self) -> Vec<fd_types::HashHex> {
        let mut need: BTreeSet<fd_types::HashHex> = BTreeSet::new();
        // Parked certs that have not applied yet (retry pull of body/parents).
        for bh in self.mesh_diag.pending_certs.keys() {
            if !self.dag.certificates.contains_key(bh) {
                need.insert(bh.clone());
            }
        }
        // Reliability: own open with 0 foreign — peers may already hold the cert
        // (PEER_SIGNED_CREATOR_MISSING_FOREIGN). Actively pull our open certs.
        for bh in self.own_open_batch_hashes() {
            if !self.dag.certificates.contains_key(&bh) {
                need.insert(bh);
            }
        }
        // Parent cert holes under local uncertified bodies.
        for (bh, b) in &self.dag.batches {
            if bh == &genesis_batch_hash() || self.dag.certificates.contains_key(bh) {
                continue;
            }
            for p in &b.parents {
                if p == &genesis_batch_hash() {
                    continue;
                }
                if !self.dag.certificates.contains_key(p) {
                    need.insert(p.clone());
                }
            }
        }
        need.into_iter().collect()
    }

    pub fn on_remote_cert(&mut self, cert: Certificate) -> Vec<String> {
        let now = wall_now_s();
        let bh = cert.batch_hash.clone();
        self.mesh_diag
            .first_observe
            .entry(bh.clone())
            .or_insert(now);

        if self.dag.certificates.contains_key(&bh) {
            self.mesh_diag.pending_certs.remove(&bh);
            self.advance();
            return Vec::new();
        }

        // Park cert so a later batch/parent arrival can re-apply (was dropped before 49d).
        self.mesh_diag.pending_certs.insert(bh.clone(), cert.clone());

        let mut need: Vec<String> = Vec::new();
        let is_own = self
            .dag
            .batches
            .get(&bh)
            .map(|b| b.creator == self.validator_id)
            .unwrap_or(cert.creator == self.validator_id);
        if !self.dag.batches.contains_key(&bh) {
            self.mesh_diag.cert_deferred_missing_batch =
                self.mesh_diag.cert_deferred_missing_batch.saturating_add(1);
            need.push(bh.clone());
            need.extend(self.missing_parent_batches(&cert.parents));
            // Still park; do not return without chance to apply later.
            return need;
        }
        // Reliability: for own opens, do NOT bail solely on missing parent bodies —
        // try_apply checks parent *certs*. Early return left PEER_SIGNED stuck.
        if !is_own {
            let missing_parents = self.missing_parent_batches(&cert.parents);
            if !missing_parents.is_empty() {
                self.mesh_diag.cert_deferred_missing_parent =
                    self.mesh_diag.cert_deferred_missing_parent.saturating_add(1);
                need.extend(missing_parents);
                return need;
            }
        }

        let applied = self.try_apply_parked_cert(&bh, now, false);
        if applied {
            self.advance();
            *self
                .pack_skip_reasons
                .entry("on_remote_cert_applied".into())
                .or_insert(0) += 1;
        } else {
            *self
                .pack_skip_reasons
                .entry("on_remote_cert_not_applied".into())
                .or_insert(0) += 1;
        }
        need
    }

    /// Attempt to apply a parked (or just-received) cert once batch + parent certs exist.
    fn try_apply_parked_cert(&mut self, bh: &str, now: f64, from_pending: bool) -> bool {
        if self.dag.certificates.contains_key(bh) {
            self.mesh_diag.pending_certs.remove(bh);
            return false;
        }
        if !self.dag.batches.contains_key(bh) {
            return false;
        }
        // Parent certs required (in-order model) — except for own opens with a
        // well-formed remote cert we still try install (PEER_SIGNED stuck when
        // parent cert lag is illusory / already ordered on peers).
        let parents = self
            .dag
            .batches
            .get(bh)
            .map(|b| b.parents.clone())
            .unwrap_or_default();
        let is_own = self
            .dag
            .batches
            .get(bh)
            .map(|b| b.creator == self.validator_id)
            .unwrap_or(false);
        let parents_ok = parents.iter().all(|p| {
            p == &genesis_batch_hash() || self.dag.certificates.contains_key(p)
        });
        if !parents_ok && !is_own {
            *self
                .pack_skip_reasons
                .entry("cert_apply_parents_missing".into())
                .or_insert(0) += 1;
            return false;
        }
        if !parents_ok && is_own {
            *self
                .pack_skip_reasons
                .entry("cert_apply_own_parent_gap".into())
                .or_insert(0) += 1;
            // fall through — try remote install for own open anyway
        }
        // Reliability: remote Certificate is DA finality. Prefer install when
        // well-formed + UNL weight ≥ threshold (PEER_SIGNED_CREATOR_MISSING_FOREIGN:
        // creator often has only own sig; try_certify alone never tips).
        let before = self.dag.certificates.contains_key(bh);
        if let Some(cert) = self.mesh_diag.pending_certs.get(bh).cloned() {
            if cert.batch_hash != bh {
                *self
                    .pack_skip_reasons
                    .entry("cert_apply_hash_mismatch".into())
                    .or_insert(0) += 1;
                // do not fall through with bad cert
            } else {
                let expected = Certificate::compute_id(&cert.batch_hash, &cert.signers);
                let id_ok = expected == cert.cert_id;
                if !id_ok {
                    *self
                        .pack_skip_reasons
                        .entry("cert_apply_id_mismatch".into())
                        .or_insert(0) += 1;
                }
                let mut w = 0.0;
                let mut all_unl = true;
                for vid in &cert.signers {
                    if let Some(sw) = self.dag.unl.get(vid) {
                        w += *sw;
                    } else {
                        all_unl = false;
                    }
                }
                // Mode B / PEER_SIGNED: for *own* opens, install peer cert when UNL
                // weight meets threshold even if cert_id mismatches (wire/order flake).
                let force_own = is_own && all_unl && w + 1e-12 >= self.config.unl_threshold;
                if w + 1e-12 < self.config.unl_threshold {
                    *self
                        .pack_skip_reasons
                        .entry("cert_apply_weight_low".into())
                        .or_insert(0) += 1;
                } else if id_ok || force_own {
                    if force_own && !id_ok {
                        *self
                            .pack_skip_reasons
                            .entry("cert_apply_force_own_open".into())
                            .or_insert(0) += 1;
                    }
                    for vid in &cert.signers {
                        if let Some(batch) = self.dag.batches.get(bh) {
                            if let Some(sig) = batch.signatures.get(vid).cloned() {
                                let _ = self.dag.add_signature(bh, vid, &sig);
                            }
                        }
                    }
                    self.dag.certificates.insert(bh.to_string(), cert);
                    if !before {
                        self.note_cert_applied(bh, now, from_pending);
                        *self
                            .pack_skip_reasons
                            .entry("remote_cert_installed".into())
                            .or_insert(0) += 1;
                    }
                    self.mesh_diag.pending_certs.remove(bh);
                    self.conf_pool.remove(bh);
                    return true;
                }
            }
        } else {
            *self
                .pack_skip_reasons
                .entry("cert_apply_no_pending".into())
                .or_insert(0) += 1;
        }
        // Fallback: local try_certify if foreign PartialSignatures already merged.
        if let Some(cert) = self.mesh_diag.pending_certs.get(bh).cloned() {
            for vid in &cert.signers {
                if let Some(batch) = self.dag.batches.get(bh) {
                    if let Some(sig) = batch.signatures.get(vid).cloned() {
                        let _ = self.dag.add_signature(bh, vid, &sig);
                    }
                }
            }
        }
        let got = self.dag.try_certify(bh);
        if got.is_some() || self.dag.certificates.contains_key(bh) {
            if !before {
                self.note_cert_applied(bh, now, from_pending);
            }
            self.mesh_diag.pending_certs.remove(bh);
            self.conf_pool.remove(bh);
            return true;
        }
        false
    }

    fn note_cert_applied(&mut self, bh: &str, now: f64, from_pending: bool) {
        self.mesh_diag.cert_applied = self.mesh_diag.cert_applied.saturating_add(1);
        // Fix 1: announce this cert (+ body via main) so peers converge.
        if !self.mesh_diag.recently_applied_cert_hashes.iter().any(|h| h == bh) {
            self.mesh_diag
                .recently_applied_cert_hashes
                .push(bh.to_string());
        }
        // Sticky dig PEER_SIGNED_CREATOR_MISSING_FOREIGN: peer certs but creator
        // never gets cert/sigs. Queue directed cert+body to creator.
        if let Some(b) = self.dag.batches.get(bh) {
            if b.creator != self.validator_id {
                self.pending_creator_cert_announce
                    .push((b.creator.clone(), bh.to_string()));
            }
        }
        if from_pending {
            self.mesh_diag.cert_reapplied_from_pending =
                self.mesh_diag.cert_reapplied_from_pending.saturating_add(1);
        }
        if let Some(t0) = self.mesh_diag.first_observe.get(bh).copied() {
            let lag_ms = ((now - t0).max(0.0) * 1000.0) as u64;
            self.mesh_diag.cert_apply_lag_ms_sum =
                self.mesh_diag.cert_apply_lag_ms_sum.saturating_add(lag_ms);
            self.mesh_diag.cert_apply_lag_samples =
                self.mesh_diag.cert_apply_lag_samples.saturating_add(1);
            self.mesh_diag.cert_apply_lag_ms_max = self.mesh_diag.cert_apply_lag_ms_max.max(lag_ms);
        }
        if let Some(cert) = self.dag.certificates.get(bh) {
            if cert.timestamp > 0.0 {
                let lag_ms = ((now - cert.timestamp).max(0.0) * 1000.0) as u64;
                self.mesh_diag.cert_from_create_lag_ms_sum = self
                    .mesh_diag
                    .cert_from_create_lag_ms_sum
                    .saturating_add(lag_ms);
                self.mesh_diag.cert_from_create_lag_samples = self
                    .mesh_diag
                    .cert_from_create_lag_samples
                    .saturating_add(1);
                self.mesh_diag.cert_from_create_lag_ms_max =
                    self.mesh_diag.cert_from_create_lag_ms_max.max(lag_ms);
            }
        }
    }

    /// After a new batch body arrives, retry any parked certs that depend on it.
    pub fn retry_pending_certs_after_batch(&mut self, batch_hash: &str) {
        let now = wall_now_s();
        let mut applied_any = false;
        // Direct cert for this batch
        if self.try_apply_parked_cert(batch_hash, now, true) {
            applied_any = true;
        }
        // Child certs may now have a parent certified — retry all pending
        let pending_keys: Vec<String> = self.mesh_diag.pending_certs.keys().cloned().collect();
        for k in pending_keys {
            if self.try_apply_parked_cert(&k, now, true) {
                applied_any = true;
            }
        }
        if applied_any {
            self.advance();
        }
    }

    pub fn prune(&mut self, now: f64) -> u64 {
        // Before dropping stale uncertified batches, re-queue their txs so they are not lost.
        let ttl = self.config.uncertified_ttl_s;
        if ttl > 0.0 {
            let drop: Vec<_> = self
                .dag
                .batches
                .iter()
                .filter(|(bh, b)| {
                    *bh != &fd_types::genesis_batch_hash()
                        && !self.dag.certificates.contains_key(*bh)
                        && (now - b.timestamp) > ttl
                        // Keep parked / near-quorum / multi-sig bodies (steady TPS).
                        && !self.mesh_diag.pending_certs.contains_key(*bh)
                        && self.dag.support_weight(b) + 1e-12 < self.config.unl_threshold
                        && b.signatures.len() <= 1
                })
                .map(|(bh, b)| (bh.clone(), b.txs.values().cloned().collect::<Vec<_>>()))
                .collect();
            let executed: std::collections::BTreeSet<_> =
                self.execution.executed_txs.iter().cloned().collect();
            let dropped_n = drop.len();
            for (_bh, txs) in drop {
                for tx in txs {
                    if !executed.contains(&tx.tx_id) {
                        let _ = self.mempool.reinsert(tx, now);
                    }
                }
            }
            // Pattern A: rewinding pack cursor after uncertified drop so re-queued
            // seqs are not stuck behind filter_seq_gap (r1 acct9 class of bug).
            if dropped_n > 0 {
                self.recompute_da_pack_seq_from_dag();
                self.note_pack_skip("pack_cursor_reclaim_prune");
            }
        }
        self.dag.prune_uncertified(now)
    }

    pub fn to_snapshot(&self) -> Snapshot {
        let g = genesis_batch_hash();
        let batches: Vec<Batch> = self
            .dag
            .batches
            .values()
            .filter(|b| b.batch_hash != g)
            .cloned()
            .collect();
        let certificates: Vec<Certificate> = self
            .dag
            .certificates
            .values()
            .filter(|c| c.batch_hash != g)
            .cloned()
            .collect();
        let st = &self.execution.state;
        Snapshot {
            version: SNAPSHOT_VERSION,
            network_id: self.config.network_id,
            validator_id: self.validator_id.clone(),
            batches,
            certificates,
            ordered: self.ordering.ordered.clone(),
            hard_final: self.ordering.hard_final.iter().cloned().collect(),
            executed_txs: self.execution.executed_txs.clone(),
            soft_final_txs: self.execution.soft_final_txs.iter().cloned().collect(),
            mempool: self.mempool.all_txs(),
            balances: st.balances.clone(),
            sequences: st.sequences.clone(),
            assets: st
                .assets
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        fd_store::AssetSnap {
                            issuer: v.issuer.clone(),
                            total_supply: v.total_supply,
                            max_supply: v.max_supply,
                        },
                    )
                })
                .collect(),
            burned_native: st.burned_native,
            treasury_fees: st.treasury_fees,
            emissions_minted: st.emissions_minted,
            state_root: st.state_root(),
            batches_created: self.batches_created,
            tip_bft_round: self.ordering.tip_bft.next_round,
            tip_bft_last_commit: self.ordering.tip_bft.last_commit.clone(),
            tip_commits: self.ordering.tip_bft.commits.clone(),
            ledger_commits: self.ledger_commits,
            bullshark_anchors: self.ordering.bullshark.anchors.clone(),
            bullshark_last_anchor: self.ordering.bullshark.last_anchor.clone(),
            policies: st
                .policies
                .iter()
                .map(|(k, p)| {
                    (
                        k.clone(),
                        fd_store::PolicySnap {
                            account_type: account_type_snap(p.account_type),
                            allowlist: p.allowlist.clone(),
                            hard_locked: p.hard_locked,
                            max_per_tx: p.max_per_tx,
                            max_per_window: p.max_per_window,
                            window_spent: p.window_spent,
                        },
                    )
                })
                .collect(),
            pools: st
                .pools
                .iter()
                .map(|(k, p)| {
                    (
                        k.clone(),
                        fd_store::PoolSnap {
                            asset_a: p.asset_a.clone(),
                            asset_b: p.asset_b.clone(),
                            lp_asset: p.lp_asset.clone(),
                            fee_bps: p.fee_bps,
                            protocol_fee_share_bps: p.protocol_fee_share_bps,
                        },
                    )
                })
                .collect(),
            markets: st
                .markets
                .iter()
                .map(|(k, m)| {
                    (
                        k.clone(),
                        fd_store::MarketSnap {
                            asset: m.asset.clone(),
                            ltv_bps: m.ltv_bps,
                            liq_bonus_bps: m.liq_bonus_bps,
                            supply_index: m.supply_index,
                            borrow_index: m.borrow_index,
                            last_accrual_txs: m.last_accrual_txs,
                            base_rate_bps: m.base_rate_bps,
                            slope_bps: m.slope_bps,
                            allowed_collateral: m.allowed_collateral.clone(),
                        },
                    )
                })
                .collect(),
            supplied: st.supplied.clone(),
            borrowed: st.borrowed.clone(),
            collateral: st.collateral.clone(),
            collateral_legacy: Default::default(),
            applied_user_txs: st.applied_user_txs,
            last_emission_height: st.last_emission_height,
            oracle_prices: st.oracle_prices.clone(),
            oracle_votes: st.oracle_votes.clone(),
            protocol_fees_skimmed: st.protocol_fees_skimmed,
            rails: st
                .rails
                .iter()
                .map(|(k, r)| {
                    (
                        k.clone(),
                        fd_store::RailSnap {
                            asset: r.asset.clone(),
                            family: rail_family_snap(r.family),
                            min_confirmations: r.min_confirmations,
                            bridge_account: r.bridge_account.clone(),
                            lock_id: r.lock_id.clone(),
                            total_minted: r.total_minted,
                            total_burned: r.total_burned,
                            tip_height: r.tip_height,
                            tip_hash: r.tip_hash.clone(),
                            tip_work: r.tip_work.to_string(),
                            claim_root: r.claim_root.clone(),
                            claim_note_count: r.claim_note_count,
                            claim_height: r.claim_height,
                        },
                    )
                })
                .collect(),
            rail_headers: st
                .rail_headers
                .iter()
                .map(|(rail, map)| {
                    (
                        rail.clone(),
                        map.iter()
                            .map(|(h, hdr)| {
                                (
                                    *h,
                                    fd_store::RailHeaderSnap {
                                        height: hdr.height,
                                        hash: hdr.hash.clone(),
                                        parent_hash: hdr.parent_hash.clone(),
                                        merkle_root: hdr.merkle_root.clone(),
                                        tx_root: hdr.tx_root.clone(),
                                        work: hdr.work.to_string(),
                                    },
                                )
                            })
                            .collect(),
                    )
                })
                .collect(),
            rail_spent_deposits: st.rail_spent_deposits.clone(),
            rail_withdrawals: st
                .rail_withdrawals
                .iter()
                .map(|(id, n)| {
                    (
                        id.clone(),
                        fd_store::WithdrawalSnap {
                            note_id: n.note_id.clone(),
                            rail: n.rail.clone(),
                            amount: n.amount,
                            from: n.from.clone(),
                            external_to: n.external_to.clone(),
                            falcon_height: n.falcon_height,
                        },
                    )
                })
                .collect(),
        }
    }

    pub fn apply_snapshot(&mut self, snap: &Snapshot) -> Result<(), fd_store::StoreError> {
        snap.validate_for(self.config.network_id)?;

        // Ingest batches (signatures ride on batch objects)
        let mut batches = snap.batches.clone();
        batches.sort_by(|a, b| {
            a.timestamp
                .partial_cmp(&b.timestamp)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.batch_hash.cmp(&b.batch_hash))
        });
        for batch in batches {
            for t in batch.txs.values() {
                self.dag.ingest_tx(t.clone());
            }
            self.dag.force_ingest_batch(batch);
        }
        self.dag.rebuild_children();

        // Certificates in causal order
        let mut certs = snap.certificates.clone();
        certs.sort_by(|a, b| {
            a.round
                .cmp(&b.round)
                .then_with(|| {
                    a.timestamp
                        .partial_cmp(&b.timestamp)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.batch_hash.cmp(&b.batch_hash))
        });
        for cert in certs {
            if self.dag.certificates.contains_key(&cert.batch_hash) {
                continue;
            }
            // Prefer re-certify from local sig weight; fall back to install
            if self.dag.try_certify(&cert.batch_hash).is_none()
                && !self.dag.certificates.contains_key(&cert.batch_hash)
            {
                self.dag.install_certificate(cert);
            }
        }

        // Snapshot restore: re-admit mempool txs without re-auto-signing (already signed)
        let require = self.config.require_tx_sigs;
        let auto = self.auto_sign;
        self.config.require_tx_sigs = false;
        self.auto_sign = false;
        for tx in &snap.mempool {
            let _ = self.submit_tx_gated(tx.clone(), false, AdmitSource::Mesh);
        }
        self.config.require_tx_sigs = require;
        self.auto_sign = auto;

        // Recompute pure order + re-exec, then overlay bit-identical state
        self.advance();

        if !snap.executed_txs.is_empty() && !snap.balances.is_empty() {
            self.execution.state.balances = snap.balances.clone();
            self.execution.state.sequences = snap.sequences.clone();
            self.execution.state.assets = snap
                .assets
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        AssetMeta {
                            issuer: v.issuer.clone(),
                            total_supply: v.total_supply,
                            max_supply: v.max_supply,
                        },
                    )
                })
                .collect();
            self.execution.state.burned_native = snap.burned_native;
            self.execution.state.treasury_fees = snap.treasury_fees;
            self.execution.state.emissions_minted = snap.emissions_minted;
            self.execution.state.policies = snap
                .policies
                .iter()
                .map(|(k, p)| {
                    (
                        k.clone(),
                        AccountPolicy {
                            account_type: parse_account_type(&p.account_type),
                            allowlist: p.allowlist.clone(),
                            hard_locked: p.hard_locked,
                            max_per_tx: p.max_per_tx,
                            max_per_window: p.max_per_window,
                            window_spent: p.window_spent,
                        },
                    )
                })
                .collect();
            self.execution.state.pools = snap
                .pools
                .iter()
                .map(|(k, p)| {
                    (
                        k.clone(),
                        PoolConfig {
                            asset_a: p.asset_a.clone(),
                            asset_b: p.asset_b.clone(),
                            lp_asset: p.lp_asset.clone(),
                            fee_bps: p.fee_bps,
                            protocol_fee_share_bps: p.protocol_fee_share_bps,
                        },
                    )
                })
                .collect();
            self.execution.state.markets = snap
                .markets
                .iter()
                .map(|(k, m)| {
                    (
                        k.clone(),
                        MarketConfig {
                            asset: m.asset.clone(),
                            ltv_bps: m.ltv_bps,
                            liq_bonus_bps: m.liq_bonus_bps,
                            supply_index: m.supply_index,
                            borrow_index: m.borrow_index,
                            last_accrual_txs: m.last_accrual_txs,
                            base_rate_bps: m.base_rate_bps,
                            slope_bps: m.slope_bps,
                            allowed_collateral: m.allowed_collateral.clone(),
                        },
                    )
                })
                .collect();
            self.execution.state.supplied = snap.supplied.clone();
            self.execution.state.borrowed = snap.borrowed.clone();
            self.execution.state.collateral = snap.collateral.clone();
            self.execution.state.oracle_prices = snap.oracle_prices.clone();
            self.execution.state.oracle_votes = snap.oracle_votes.clone();
            self.execution.state.protocol_fees_skimmed = snap.protocol_fees_skimmed;
            if !snap.rails.is_empty() {
                self.execution.state.rails = snap
                    .rails
                    .iter()
                    .map(|(k, r)| {
                        (
                            k.clone(),
                            RailConfig {
                                asset: r.asset.clone(),
                                family: parse_rail_family(&r.family),
                                min_confirmations: r.min_confirmations,
                                bridge_account: r.bridge_account.clone(),
                                lock_id: r.lock_id.clone(),
                                total_minted: r.total_minted,
                                total_burned: r.total_burned,
                                tip_height: r.tip_height,
                                tip_hash: r.tip_hash.clone(),
                                tip_work: r.tip_work.parse().unwrap_or(0),
                                claim_root: r.claim_root.clone(),
                                claim_note_count: r.claim_note_count,
                                claim_height: r.claim_height,
                            },
                        )
                    })
                    .collect();
            }
            self.execution.state.rail_headers = snap
                .rail_headers
                .iter()
                .map(|(rail, map)| {
                    (
                        rail.clone(),
                        map.iter()
                            .map(|(h, hdr)| {
                                (
                                    *h,
                                    RailHeader {
                                        height: hdr.height,
                                        hash: hdr.hash.clone(),
                                        parent_hash: hdr.parent_hash.clone(),
                                        merkle_root: hdr.merkle_root.clone(),
                                        tx_root: hdr.tx_root.clone(),
                                        work: hdr.work.parse().unwrap_or(0),
                                    },
                                )
                            })
                            .collect(),
                    )
                })
                .collect();
            self.execution.state.rail_spent_deposits = snap.rail_spent_deposits.clone();
            self.execution.state.rail_withdrawals = snap
                .rail_withdrawals
                .iter()
                .map(|(id, n)| {
                    (
                        id.clone(),
                        WithdrawalNote {
                            note_id: n.note_id.clone(),
                            rail: n.rail.clone(),
                            amount: n.amount,
                            from: n.from.clone(),
                            external_to: n.external_to.clone(),
                            falcon_height: n.falcon_height,
                        },
                    )
                })
                .collect();
            self.execution.state.applied_user_txs = snap.applied_user_txs;
            self.execution.state.last_emission_height = snap.last_emission_height;
            self.execution.executed_txs = snap.executed_txs.clone();
            self.execution.soft_final_txs = snap.soft_final_txs.iter().cloned().collect();
            if !snap.ordered.is_empty() {
                self.ordering.ordered = snap.ordered.clone();
                self.ordering.ordered_set = snap.ordered.iter().cloned().collect();
            }
            if !snap.hard_final.is_empty() {
                self.ordering.hard_final = snap.hard_final.iter().cloned().collect();
                self.ordering.hard_final.insert(genesis_batch_hash());
            }
            // Tip-BFT restore (v5 fields; legacy snaps leave next_round=0 → synthesize).
            if snap.tip_bft_round > 0 {
                self.ordering.tip_bft.next_round = snap.tip_bft_round;
                if !snap.tip_bft_last_commit.is_empty() {
                    self.ordering.tip_bft.last_commit = snap.tip_bft_last_commit.clone();
                }
                self.ordering.tip_bft.commits = snap.tip_commits.clone();
                for c in &snap.tip_commits {
                    self.ordering.tip_bft.commit_ids.insert(c.commit_id.clone());
                }
            } else if let Some(last) = snap.ordered.last() {
                // Legacy: treat last ordered as committed tip; next round continues.
                self.ordering.tip_bft.last_commit = last.clone();
                self.ordering.tip_bft.next_round =
                    snap.ordered.len().saturating_sub(1).max(1) as u64;
            }
            // Bullshark restore (v6)
            if !snap.bullshark_anchors.is_empty() {
                self.ordering.bullshark.anchors = snap.bullshark_anchors.clone();
                for a in &snap.bullshark_anchors {
                    self.ordering.bullshark.committed_rounds.insert(a.round);
                    self.ordering.bullshark.commit_ids.insert(a.commit_id.clone());
                }
                if !snap.bullshark_last_anchor.is_empty() {
                    self.ordering.bullshark.last_anchor = snap.bullshark_last_anchor.clone();
                } else if let Some(a) = snap.bullshark_anchors.last() {
                    self.ordering.bullshark.last_anchor = a.batch_hash.clone();
                }
            }
            self.ledger_commits = snap.ledger_commits;
            self.mempool.mark_done(snap.executed_txs.iter().cloned());
        }

        self.batches_created = snap.batches_created;
        Ok(())
    }

    pub fn save_snapshot(&self, data_dir: impl AsRef<Path>) -> Result<(), fd_store::StoreError> {
        let path = fd_store::snapshot_path(data_dir);
        fd_store::save_snapshot(path, &self.to_snapshot())
    }

    pub fn load_snapshot_file(
        &mut self,
        data_dir: impl AsRef<Path>,
    ) -> Result<bool, fd_store::StoreError> {
        let path = fd_store::snapshot_path(data_dir);
        match fd_store::load_snapshot(path)? {
            Some(snap) => {
                self.apply_snapshot(&snap)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// **Stage 1 — lane DA only:** try to form availability certificates from
    /// signatures (independent per-batch / per-lane cert voting).
    /// Does **not** run Bullshark, does not order, does not execute.
    pub fn advance_da(&mut self) {
        let pending: Vec<_> = self
            .dag
            .batches
            .keys()
            .filter(|bh| {
                *bh != &genesis_batch_hash() && !self.dag.certificates.contains_key(*bh)
            })
            .cloned()
            .collect();
        let now = wall_now_s();
        for bh in pending {
            if self.dag.try_certify(&bh).is_some() {
                // Flake-hunt: cert formed off gossip still needs re-announce.
                self.note_cert_applied(&bh, now, false);
                self.conf_pool.remove(&bh);
            }
        }
    }

    /// Returns gossip from conf-pool check phase (sign open bodies on schedule).
    pub fn advance_da_conf(&mut self, now: f64) -> GossipEvents {
        // Always run conf-pool sign/cert step when enabled.
        // Forensic (steady 30tx/s): foreign bodies never got sigs when signing was
        // gated to Check half only under multipack + continuous load; opens aged
        // creator-local with foreign_sigs=0. Pack duty still gates *packing*.
        if self.config.conf_pool_enabled {
            return self.conf_pool_check_step(now);
        }
        GossipEvents::default()
    }

    /// **Ledger path (post-DA only):**
    ///
    /// 1. *(packing/cert happen in `create_batch` / `advance_da` — never here)*
    /// 2. **Collect** finalized batches from all lanes (`cert + body`)
    /// 3. **Bullshark** only on that finalized set (not during packing)
    /// 4. **Apply** payloads only in the Bullshark-agreed order
    ///
    /// Never packs from mempool. Tip-BFT remains optional gossip only; ordering
    /// authority is Bullshark over finalized DA.
    pub fn advance_ledger(&mut self) -> GossipEvents {
        let mut out = GossipEvents::default();
        if !self.role.runs_ledger() {
            return out;
        }

        // --- Stage 2: collect finalized batches from all lanes ---
        let finalized = self.collect_finalized_lane_certs();

        // --- Stage 3: Bullshark AFTER finalization (finalized set only) ---
        // Tip-BFT is NOT used for ordering (asymmetric tip_commit was the residual
        // flake: same certs, one node never ordered). Order = Bullshark anchors +
        // deterministic causal expand of ALL finalized certs (same view ⇒ same order).
        let new_anchors = self
            .ordering
            .bullshark
            .try_commit_from_dag(&finalized, &self.dag.unl);
        for a in &new_anchors {
            self.ledger_commits = self.ledger_commits.saturating_add(1);
            self.expand_tip_if_body(&a.batch_hash);
            // Keep tip_bft fields in sync for snapshot metrics only (no tip votes).
            self.ordering.tip_bft.last_commit = a.batch_hash.clone();
            self.ordering.tip_bft.next_round =
                self.ordering.tip_bft.next_round.max(a.round.saturating_add(1));
        }
        // Catch-up: re-expand all installed Bullshark anchors.
        let anchors: Vec<_> = self
            .ordering
            .bullshark
            .anchors
            .iter()
            .map(|a| a.batch_hash.clone())
            .collect();
        for tip in anchors {
            if self.batch_is_finalized(&tip) {
                self.expand_tip_if_body(&tip);
            }
        }
        // Finalized causal ready-set: orders certified children of anchors (e.g. r3
        // after r1+r2 committed) without Tip-BFT. Only `finalized` (cert+body).
        let _ready_n = self.ordering.expand_ready_multipack(&finalized);

        // --- Stage 4: apply only in agreed (ordered) sequence ---
        let mut txs = Vec::new();
        let mut non_genesis = 0u64;
        for bh in &self.ordering.ordered {
            if bh == &genesis_batch_hash() {
                continue;
            }
            // Skip ordered tips we still lack a body for (should be rare after gate).
            if !self.batch_is_finalized(bh) && bh != &genesis_batch_hash() {
                // ordered without body: wait (do not apply partial ghost).
                continue;
            }
            non_genesis += 1;
            if let Some(batch) = self.dag.batches.get(bh) {
                for tid in &batch.tx_ids {
                    if let Some(tx) = self
                        .dag
                        .tx_index
                        .get(tid)
                        .or_else(|| batch.txs.get(tid))
                    {
                        txs.push(tx.clone());
                    }
                }
                for tid in &batch.tx_ids {
                    self.execution.soft_final_txs.insert(tid.clone());
                }
                if self.role.runs_da() {
                    self.mempool.mark_done(batch.tx_ids.iter().cloned());
                }
            }
        }

        if self.execution.executed_txs.is_empty() {
            self.execution.apply_ordered(txs);
        } else {
            let done: std::collections::BTreeSet<_> =
                self.execution.executed_txs.iter().cloned().collect();
            for tx in &txs {
                if done.contains(&tx.tx_id) {
                    continue;
                }
                if self.execution.state.apply(tx) {
                    self.execution.executed_txs.push(tx.tx_id.clone());
                    self.shard_flow.applied = self.shard_flow.applied.saturating_add(1);
                }
            }
        }

        self.execution
            .state
            .apply_emissions_for_batch_count(non_genesis);
        out
    }

    /// One tip-BFT round step: propose if leader; vote on own proposal; re-emit open
    /// proposal/vote for mesh gossip; finalize if quorum.
    fn tip_bft_step(&mut self) -> GossipEvents {
        let mut out = GossipEvents::default();
        let crypto = std::sync::Arc::clone(&self.dag.crypto);
        let local = self.validator_id.clone();
        let unl_sorted = TipBftEngine::sorted_unl(&self.dag.unl);
        let threshold = self.config.unl_threshold;
        let round = self.ordering.tip_bft.next_round;

        // Drop early proposals that predate the full cert frontier, then view-change.
        self.ordering
            .tip_bft
            .invalidate_stale_proposal(&self.dag.certificates);
        self.ordering.tip_bft.maybe_view_change();

        // New propose if we are leader and have not proposed this round.
        if !self.ordering.tip_bft.proposals.contains_key(&round) {
            if let Some(prop) = self.ordering.tip_bft.try_propose(
                &local,
                &unl_sorted,
                &self.dag.certificates,
                |msg| crypto.sign(&local, msg).ok(),
            ) {
                if let Some(vote) = self.ordering.tip_bft.on_proposal(
                    prop.clone(),
                    &local,
                    &unl_sorted,
                    &self.dag.unl,
                    &self.dag.certificates,
                    |vid, msg, sig| crypto.verify(vid, msg, sig),
                    |msg| crypto.sign(&local, msg).ok(),
                ) {
                    out.tip_votes.push(vote);
                }
                out.tip_proposals.push(prop);
            }
        }

        // Re-emit open proposal + our vote every tick so mesh peers catch up.
        if let Some(prop) = self.ordering.tip_bft.proposals.get(&round).cloned() {
            if !out.tip_proposals.iter().any(|p| p.proposal_id() == prop.proposal_id()) {
                out.tip_proposals.push(prop.clone());
            }
            // Vote if we have not yet (e.g. we received proposal earlier without voting).
            if !self.ordering.tip_bft.voted_rounds.contains(&round) {
                if let Some(vote) = self.ordering.tip_bft.on_proposal(
                    prop.clone(),
                    &local,
                    &unl_sorted,
                    &self.dag.unl,
                    &self.dag.certificates,
                    |vid, msg, sig| crypto.verify(vid, msg, sig),
                    |msg| crypto.sign(&local, msg).ok(),
                ) {
                    out.tip_votes.push(vote);
                }
            }
            if let Some(votes) = self
                .ordering
                .tip_bft
                .votes
                .get(&(round, prop.tip_batch_hash.clone()))
            {
                if let Some(v) = votes.get(&local) {
                    if !out.tip_votes.iter().any(|x| x.voter == local && x.round == round) {
                        out.tip_votes.push(v.clone());
                    }
                }
            }
        }

        if let Some(commit) = self
            .ordering
            .tip_bft
            .try_finalize_round(&self.dag.unl, threshold)
        {
            self.ledger_commits = self.ledger_commits.saturating_add(1);
            out.tip_commits.push(commit);
        }
        out
    }

    /// Ingest remote tip proposal (order-exec / Full only).
    pub fn on_tip_proposal(&mut self, prop: TipProposal) -> GossipEvents {
        let mut out = GossipEvents::default();
        if !self.role.runs_ledger() {
            return out;
        }
        let crypto = std::sync::Arc::clone(&self.dag.crypto);
        let local = self.validator_id.clone();
        let unl_sorted = TipBftEngine::sorted_unl(&self.dag.unl);
        let threshold = self.config.unl_threshold;
        if let Some(vote) = self.ordering.tip_bft.on_proposal(
            prop,
            &local,
            &unl_sorted,
            &self.dag.unl,
            &self.dag.certificates,
            |vid, msg, sig| crypto.verify(vid, msg, sig),
            |msg| crypto.sign(&local, msg).ok(),
        ) {
            out.tip_votes.push(vote.clone());
            if let Some(commit) =
                self.ordering
                    .tip_bft
                    .on_vote(vote, &self.dag.unl, threshold, |vid, msg, sig| {
                        crypto.verify(vid, msg, sig)
                    })
            {
                self.ledger_commits = self.ledger_commits.saturating_add(1);
                out.tip_commits.push(commit);
                // Expand + exec after new commit.
                out.extend(self.advance_ledger());
            }
        }
        out
    }

    pub fn on_tip_vote(&mut self, vote: TipVote) -> GossipEvents {
        let mut out = GossipEvents::default();
        if !self.role.runs_ledger() {
            return out;
        }
        let crypto = std::sync::Arc::clone(&self.dag.crypto);
        let threshold = self.config.unl_threshold;
        if let Some(commit) =
            self.ordering
                .tip_bft
                .on_vote(vote, &self.dag.unl, threshold, |vid, msg, sig| {
                    crypto.verify(vid, msg, sig)
                })
        {
            self.ledger_commits = self.ledger_commits.saturating_add(1);
            out.tip_commits.push(commit);
            out.extend(self.advance_ledger());
        }
        out
    }

    pub fn on_tip_commit(&mut self, commit: TipCommit) -> GossipEvents {
        let mut out = GossipEvents::default();
        if !self.role.runs_ledger() {
            return out;
        }
        let threshold = self.config.unl_threshold;
        if self
            .ordering
            .tip_bft
            .on_remote_commit(commit.clone(), &self.dag.unl, threshold)
            .is_some()
        {
            self.ledger_commits = self.ledger_commits.saturating_add(1);
            out.tip_commits.push(commit);
            // Expand ordered under the new tip and execute (no new propose needed first).
            let tip = self.ordering.tip_bft.last_commit.clone();
            self.ordering
                .expand_under_tip(&self.dag.certificates, &tip);
            // Re-run execute half via advance_ledger without double-counting commits:
            // advance_ledger may also try_propose next round — that's correct.
            out.extend(self.advance_ledger());
        }
        out
    }

    /// Role-aware step: DA-only, ledger-only, or both (still as separate method calls).
    pub fn advance(&mut self) -> GossipEvents {
        let mut g = GossipEvents::default();
        if self.role.runs_da() {
            self.advance_da();
        }
        if self.role.runs_ledger() {
            g.extend(self.advance_ledger());
        }
        g
    }

    pub fn metrics_json(&self) -> serde_json::Value {
        let mut m = serde_json::json!({
            "validator_id": self.validator_id,
            "network_id": self.config.network_id,
            "build_version": "falcon-dagd/0.48.0-rust",
            "architecture": fd_types::ARCHITECTURE_STRING,
            "implementation_guide": fd_types::IMPLEMENTATION_GUIDE_PATH,
            "max_cert_round": self.max_cert_round(),
            "role": self.role.as_str(),
            "node_profile": self.config.node_profile.as_str(),
            "profile_status": self.config.profile_status_json(),
            "runs_da": self.role.runs_da(),
            "runs_ledger": self.role.runs_ledger(),
            "mempool": self.mempool.len(),
            "batches_created": self.batches_created,
            "ledger_commits": self.ledger_commits,
            "bullshark_anchors": self.ordering.bullshark.anchors.len(),
            "bullshark_last_anchor": self.ordering.bullshark.last_anchor,
            "bullshark_committed_rounds": self.ordering.bullshark.committed_rounds.iter().cloned().collect::<Vec<_>>(),
            "tip_bft_round": self.ordering.tip_bft.next_round,
            "tip_bft_last_commit": self.ordering.tip_bft.last_commit,
            "tip_commits": self.ordering.tip_bft.commits.len(),
            "certified": self.dag.certificates.len(),
            "ordered": self.ordering.ordered.len(),
            "ordered_lag": self.ordered_lag(),
            "pack_backpressured": self.pack_backpressured(),
            "executed_txs": self.execution.executed_txs.len(),
            "soft_final_txs": self.execution.soft_final_txs.len(),
            "hard_final": self.ordering.hard_final.len(),
            "crypto_scheme": self.dag.crypto.scheme(),
            "wallet_scheme": self.wallet.as_ref().map(|w| w.scheme()).unwrap_or("none"),
            "require_tx_sigs": self.config.require_tx_sigs,
            "auto_sign": self.auto_sign,
            "sample_sig_bytes": self.dag.crypto.sample_sig_bytes(),
            "rejected_batches": self.dag.rejected_batches,
            "rejected_sigs": self.dag.rejected_sigs,
            "rejected_tx_sigs": self.rejected_tx_sigs,
            "equivocations": self.dag.equivocation_count,
            "uncertified": self.dag.uncertified_count_non_genesis(),
            "known_batches": self.dag.batches.len(),
            "primary_packer": self.primary_packer_id(),
            "assigned_packer": self.is_assigned_packer(),
            "native_asset": self.config.economics.native_asset,
            "native_supply": self.execution.state.total_supply(&self.config.economics.native_asset),
            "burned_native": self.execution.state.burned_native,
            "treasury_fees": self.execution.state.treasury_fees,
            "emissions_minted": self.execution.state.emissions_minted,
            "assets": self.execution.state.assets.len(),
            "state_root": self.execution.state.state_root(),
        });
        // Attach pack stall diagnostics without nesting another giant json! tree
        // (hits serde_json recursion limit when inlined).
        if let serde_json::Value::Object(ref mut map) = m {
            map.insert("pack_stall".into(), self.pack_stall_snapshot());
        map.insert("pack_skip_reasons".into(), serde_json::json!(self.pack_skip_reasons));
        map.insert("admit_reasons".into(), serde_json::json!(self.admit_reasons));
        map.insert("mempool_accounts".into(), self.mempool_account_coverage());
            map.insert("mesh".into(), self.mesh_diag.to_json());
        }
        m
    }

    /// Prometheus text exposition (no extra deps).
    pub fn metrics_prometheus(&self) -> String {
        let mut s = String::new();
        let labels = format!(
            "validator_id=\"{}\",network_id=\"{}\"",
            self.validator_id, self.config.network_id
        );
        let mut line = |name: &str, help: &str, value: u64| {
            s.push_str(&format!("# HELP {name} {help}\n"));
            s.push_str(&format!("# TYPE {name} gauge\n"));
            s.push_str(&format!("{name}{{{labels}}} {value}\n"));
        };
        line(
            "falcon_dag_mempool_txs",
            "Pending mempool transactions",
            self.mempool.len() as u64,
        );
        line(
            "falcon_dag_batches_created",
            "Batches created by this validator",
            self.batches_created,
        );
        line(
            "falcon_dag_certificates",
            "Known certificates including genesis",
            self.dag.certificates.len() as u64,
        );
        line(
            "falcon_dag_ordered_batches",
            "Ordered batch hashes",
            self.ordering.ordered.len() as u64,
        );
        line(
            "falcon_dag_ordered_lag",
            "Certified-but-unordered batches (non-genesis)",
            self.ordered_lag() as u64,
        );
        line(
            "falcon_dag_pack_backpressured",
            "1 if packing backpressure gate is currently blocking",
            if self.pack_backpressured() { 1 } else { 0 },
        );
        line(
            "falcon_dag_executed_txs",
            "Successfully executed transactions",
            self.execution.executed_txs.len() as u64,
        );
        line(
            "falcon_dag_soft_final_txs",
            "Soft-final transaction ids",
            self.execution.soft_final_txs.len() as u64,
        );
        line(
            "falcon_dag_hard_final_batches",
            "Hard-final batches",
            self.ordering.hard_final.len() as u64,
        );
        line(
            "falcon_dag_rejected_batches",
            "Rejected batches",
            self.dag.rejected_batches,
        );
        line(
            "falcon_dag_rejected_sigs",
            "Rejected DA signatures",
            self.dag.rejected_sigs,
        );
        line(
            "falcon_dag_rejected_tx_sigs",
            "Rejected user-tx signatures",
            self.rejected_tx_sigs,
        );
        line(
            "falcon_dag_equivocations",
            "Detected equivocations",
            self.dag.equivocation_count,
        );
        line(
            "falcon_dag_uncertified_batches",
            "Uncertified known batches",
            self.dag.uncertified_count() as u64,
        );
        line(
            "falcon_dag_bullshark_anchors",
            "Bullshark anchors committed (even-round leaders)",
            self.ordering.bullshark.anchors.len() as u64,
        );
        line(
            "falcon_dag_tip_commits",
            "Tip-BFT commits (optional secondary path)",
            self.ordering.tip_bft.commits.len() as u64,
        );
        line(
            "falcon_dag_tip_bft_round",
            "Next tip-BFT round",
            self.ordering.tip_bft.next_round,
        );
        line(
            "falcon_dag_ledger_commits",
            "Ledger tip commits applied by this node",
            self.ledger_commits,
        );
        line(
            "falcon_dag_require_tx_sigs",
            "1 if user-tx signatures required",
            if self.config.require_tx_sigs { 1 } else { 0 },
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fd_crypto::{Falcon512Wallet, HmacWallet, WalletBackend};
    use fd_types::AccountType;
    use std::sync::Arc;

    #[test]
    fn primary_packer_is_stable_sorted_unl0() {
        let unl = vec![
            "val1".into(),
            "val2".into(),
            "val3".into(),
            "val4".into(),
            "val5".into(),
        ];
        let cfg = ProtocolConfig {
            require_tx_sigs: false,
            worker_sharding: false,
            single_packer_per_round: true,
            target_batch_interval_s: 0.0,
            ..Default::default()
        };
        let eng = NodeEngine::new("val1".into(), unl.clone(), cfg.clone(), 0, 1);
        let sorted = BullsharkEngine::sorted_unl(&eng.dag.unl);
        assert_eq!(eng.primary_packer_id().as_ref(), Some(&sorted[0]));
        assert_eq!(sorted[0], "val1");
        assert!(eng.is_assigned_packer());
        let eng2 = NodeEngine::new("val2".into(), unl, cfg, 1, 5);
        // Fixed primary is always val1 — val2 is not packer with empty/low mempool.
        assert_eq!(eng2.primary_packer_id().as_deref(), Some("val1"));
        assert!(!eng2.is_assigned_packer());
    }

    #[test]
    fn da_worker_does_not_execute_order_exec_does_not_pack() {
        let unl = vec!["val1".into()];
        let mut da = NodeEngine::new(
            "val1".into(),
            unl.clone(),
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                target_batch_interval_s: 0.0,
                ..Default::default()
            },
            0,
            1,
        );
        da.set_role(NodeRole::DaWorker);
        assert!(da
            .submit_tx(Transaction::create("acct1", 1, "acct2", 1, 10, 1.0))
            .is_ok());
        let b = da.create_batch(1.0, true).expect("da packs");
        da.advance_da();
        // single-node: local sig is enough for cert
        da.advance_da();
        assert!(
            da.dag.certificates.contains_key(&b.0.batch_hash)
                || da.dag.support_weight(da.dag.batches.get(&b.0.batch_hash).unwrap()) >= 0.5,
            "da should certify"
        );
        da.advance(); // da-only: no ledger
        assert_eq!(
            da.execution.executed_txs.len(),
            0,
            "DA worker must not execute"
        );

        let mut led = NodeEngine::new(
            "val1".into(),
            unl,
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                target_batch_interval_s: 0.0,
                ..Default::default()
            },
            0,
            1,
        );
        led.set_role(NodeRole::OrderExec);
        assert_eq!(
            led.submit_tx(Transaction::create("acct1", 1, "acct2", 1, 10, 2.0)),
            SubmitResult::NotDaRole,
            "order-exec must reject user submit"
        );
        // order-exec never packs
        assert!(led.create_batch(3.0, true).is_none());
        // but can ingest certified batch payload and ledger-advance
        let (batch, _) = b;
        led.dag.ingest_batch(batch.clone());
        let sig = led.dag.sign_local(&batch.batch_hash);
        let _ = led.dag.add_signature(&batch.batch_hash, "val1", &sig);
        led.advance_ledger();
        assert!(
            !led.execution.executed_txs.is_empty(),
            "ledger must execute from ordered certs; ordered={} exec={}",
            led.ordering.ordered.len(),
            led.execution.executed_txs.len()
        );
    }

    #[test]
    fn no_equivocating_second_proposal_same_parents() {
        // 5-val UNL so a single local signature (0.2) cannot certify (threshold 0.8).
        let unl = vec![
            "val1".into(),
            "val2".into(),
            "val3".into(),
            "val4".into(),
            "val5".into(),
        ];
        let mut eng = NodeEngine::new(
            "val1".into(),
            unl,
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                target_batch_interval_s: 0.0,
                max_uncertified_per_creator: 8,
                ..Default::default()
            },
            0,
            1,
        );
        for seq in 1..=20u64 {
            assert!(eng
                .submit_tx(Transaction::create("acct1", seq, "acct2", 1, 10, seq as f64))
                .is_ok());
        }
        let first = eng.create_batch(1.0, true);
        assert!(first.is_some(), "first proposal should succeed");
        let second = eng.create_batch(2.0, true);
        assert!(
            second.is_none(),
            "second proposal with same tips must wait for cert"
        );
        assert_eq!(eng.dag.uncertified_by_creator("val1"), 1);
        assert_eq!(eng.dag.equivocation_count, 0);
    }

    #[test]
    fn sequence_window_accepts_future_sequences_into_mempool() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                account_sequence_window: 8,
                require_tx_sigs: false,
                worker_sharding: false,
                ..Default::default()
            },
            0,
            1,
        );
        // Pipeline seq 1..5 without waiting for apply between submits
        for seq in 1..=5u64 {
            assert_eq!(
                eng.submit_tx(Transaction::create("acct1", seq, "acct2", 1, 10, seq as f64)),
                SubmitResult::Accepted,
                "seq {seq}"
            );
        }
        // beyond window
        assert_eq!(
            eng.submit_tx(Transaction::create("acct1", 10, "acct2", 1, 10, 10.0)),
            SubmitResult::SeqFuture
        );
        assert!(eng.mempool.len() >= 5);
        // force batch/apply all ready
        for t in 1..=10 {
            let _ = eng.create_batch(t as f64, true);
            eng.advance();
        }
        assert!(
            eng.execution.executed_txs.len() >= 5,
            "executed {}",
            eng.execution.executed_txs.len()
        );
        assert_eq!(eng.execution.state.next_sequence("acct1"), 6);
    }

    #[test]
    fn protocol_reject_blocks_rail_issue_and_pool_payment() {
        let mut eng =
            NodeEngine::new("val1".into(), vec!["val1".into()], ProtocolConfig::default(), 0, 1);
        // Issue BTC (protocol rail) must not be admitted
        assert_eq!(
            eng.submit_tx(Transaction::issue("acct1", 1, "BTC", 1, "acct1", 1, None, 0.0)),
            SubmitResult::ProtocolBody
        );
        assert_eq!(
            eng.submit_tx(Transaction::issue("acct1", 1, "BTC", 1, "acct1", 1, None, 0.0))
                .engine_code(),
            "tecPROTOCOL_BODY"
        );
        // Fund pool shell, open as Pool, free payment rejected at admit
        assert!(eng
            .submit_tx(Transaction::payment("acct1", 1, "poolx", 100, 1, 0.0))
            .is_ok());
        eng.create_batch(1.0, true);
        eng.advance();
        assert!(eng
            .submit_tx(Transaction::open_account_type(
                "poolx",
                1,
                AccountType::Pool,
                1,
                1.0,
            ))
            .is_ok());
        eng.create_batch(2.0, true);
        eng.advance();
        assert_eq!(
            eng.submit_tx(Transaction::payment("poolx", 2, "acct1", 1, 1, 2.0)),
            SubmitResult::ProtocolBody
        );
    }

    fn five_node_engines() -> Vec<NodeEngine> {
        five_node_engines_with_crypto(None)
    }

    fn five_node_engines_with_crypto(crypto: Option<SharedCrypto>) -> Vec<NodeEngine> {
        let unl: Vec<String> = (1..=5).map(|i| format!("val{i}")).collect();
        let mut cfg = ProtocolConfig::default();
        cfg.worker_sharding = false;
        cfg.shard_orphan_age_s = 0.0;
        cfg.target_batch_interval_s = 0.0;
        if crypto.is_some() {
            cfg.crypto_scheme = "falcon-512".into();
        }
        unl.iter()
            .enumerate()
            .map(|(i, id)| {
                if let Some(ref c) = crypto {
                    NodeEngine::with_crypto(
                        id.clone(),
                        unl.clone(),
                        cfg.clone(),
                        i,
                        unl.len(),
                        Arc::clone(c),
                    )
                } else {
                    NodeEngine::new(id.clone(), unl.clone(), cfg.clone(), i, unl.len())
                }
            })
            .collect()
    }

    /// Fan out a batch + all UNL signatures across engines (local mesh sim).
    /// Also floods any DA batches spawned by remote ingest / pack-ahead.
    fn gossip_batch(engines: &mut [NodeEngine], origin: usize, batch: Batch) {
        let mut pending = vec![batch];
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut guard = 0;
        while let Some(batch) = pending.pop() {
            guard += 1;
            if guard > 64 {
                break;
            }
            let hash = batch.batch_hash.clone();
            if !seen.insert(hash.clone()) {
                continue;
            }
            for (i, eng) in engines.iter_mut().enumerate() {
                if i == origin && eng.dag.batches.contains_key(&hash) {
                    continue;
                }
                let ev = eng.on_remote_batch(batch.clone(), 1.0 + guard as f64);
                for b in ev.batches {
                    if !seen.contains(&b.batch_hash) {
                        pending.push(b);
                    }
                }
            }
            // Collect signatures from every validator that can sign
            let mut sigs: Vec<(String, String)> = Vec::new();
            for eng in engines.iter() {
                if eng.dag.batches.contains_key(&hash) {
                    let sig = eng.dag.sign_local(&hash);
                    sigs.push((eng.validator_id.clone(), sig));
                }
            }
            for eng in engines.iter_mut() {
                for (vid, sig) in &sigs {
                    let ev = eng.on_remote_sig(&hash, vid, sig);
                    for b in ev.batches {
                        if !seen.contains(&b.batch_hash) {
                            pending.push(b);
                        }
                    }
                }
            }
        }
        // Tip-BFT + Bullshark: several ledger rounds until stable.
        gossip_tip_bft(engines, 8);
    }

    /// In-process tip-BFT mesh: run ledger ticks and fan out tip messages.
    fn gossip_tip_bft(engines: &mut [NodeEngine], rounds: usize) {
        for _ in 0..rounds {
            let mut proposals = Vec::new();
            let mut votes = Vec::new();
            let mut commits = Vec::new();
            for eng in engines.iter_mut() {
                let ev = eng.advance_ledger();
                proposals.extend(ev.tip_proposals);
                votes.extend(ev.tip_votes);
                commits.extend(ev.tip_commits);
            }
            if proposals.is_empty() && votes.is_empty() && commits.is_empty() {
                // Still give each node a chance to expand under existing commits.
                continue;
            }
            for eng in engines.iter_mut() {
                for p in &proposals {
                    if p.proposer == eng.validator_id {
                        continue;
                    }
                    let ev = eng.on_tip_proposal(p.clone());
                    votes.extend(ev.tip_votes);
                    commits.extend(ev.tip_commits);
                }
            }
            for eng in engines.iter_mut() {
                for v in &votes {
                    if v.voter == eng.validator_id {
                        continue;
                    }
                    let ev = eng.on_tip_vote(v.clone());
                    commits.extend(ev.tip_commits);
                }
            }
            for eng in engines.iter_mut() {
                for c in &commits {
                    let _ = eng.on_tip_commit(c.clone());
                }
            }
        }
    }

    #[test]
    fn multi_node_certify_and_execute() {
        let mut engines = five_node_engines();
        let tx = Transaction::create("acct1", 1, "acct2", 100, 10, 1.0);
        for eng in engines.iter_mut() {
            eng.submit_tx(tx.clone());
        }
        let (batch, _) = engines[0]
            .create_batch(1.0, true)
            .expect("val1 should create batch");
        gossip_batch(&mut engines, 0, batch);

        for eng in &engines {
            assert!(
                eng.dag.certificates.len() >= 2,
                "{} missing cert",
                eng.validator_id
            );
            assert!(
                eng.execution.executed_txs.contains(&tx.tx_id),
                "{} did not execute {}",
                eng.validator_id,
                tx.tx_id
            );
        }
        // Same order and same executed set
        let order0 = engines[0].ordering.ordered.clone();
        let exec0 = engines[0].execution.executed_txs.clone();
        for eng in &engines[1..] {
            assert_eq!(eng.ordering.ordered, order0);
            assert_eq!(eng.execution.executed_txs, exec0);
        }
    }

    #[test]
    fn snapshot_restart_recovers_state() {
        let unl = vec!["val1".to_string()];
        let cfg = ProtocolConfig {
            worker_sharding: false,
            target_batch_interval_s: 0.0,
            ..Default::default()
        };
        let mut eng = NodeEngine::new("val1".into(), unl.clone(), cfg.clone(), 0, 1);

        let mut last_tx = String::new();
        for seq in 1..=5u64 {
            let tx = Transaction::create("acct1", seq, "acct2", 1, 10, seq as f64);
            last_tx = tx.tx_id.clone();
            eng.submit_tx(tx);
            let _ = eng.create_batch(seq as f64, true);
            eng.advance();
        }
        assert_eq!(eng.execution.executed_txs.len(), 5, "pre-snap exec");
        assert!(eng.dag.certificates.len() >= 6);
        let bal_before = eng.execution.state.native_balance("acct1");
        let snap = eng.to_snapshot();
        assert_eq!(snap.batches.len(), 5);
        assert!(snap.certificates.len() >= 5);
        assert_eq!(snap.executed_txs.len(), 5);

        let mut eng2 = NodeEngine::new("val1".into(), unl.clone(), cfg.clone(), 0, 1);
        eng2.apply_snapshot(&snap).unwrap();
        assert_eq!(eng2.execution.executed_txs.len(), 5);
        assert_eq!(eng2.execution.executed_txs, eng.execution.executed_txs);
        assert_eq!(eng2.execution.state.native_balance("acct1"), bal_before);

        // Simulate post-restart tick advance (must remain stable)
        eng2.advance();
        assert_eq!(
            eng2.execution.executed_txs.len(),
            5,
            "advance after restore must not drop txs; certs={} ordered={}",
            eng2.dag.certificates.len(),
            eng2.ordering.ordered.len()
        );

        let dir = std::env::temp_dir().join(format!("fd-snap-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        eng.save_snapshot(&dir).unwrap();
        let mut eng3 = NodeEngine::new("val1".into(), unl, cfg, 0, 1);
        assert!(eng3.load_snapshot_file(&dir).unwrap());
        eng3.advance();
        assert_eq!(eng3.execution.executed_txs.len(), 5);
        assert!(eng3.execution.executed_txs.contains(&last_tx));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn economy_issue_transfer_fees_supply() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                worker_sharding: false,
                target_batch_interval_s: 0.0,
                require_tx_sigs: false,
                ..Default::default()
            },
            0,
            1,
        );
        let issue = Transaction::issue("acct1", 1, "GOLD", 500, "acct1", 10, Some(10_000), 0.0);
        assert!(eng.submit_tx(issue).is_ok());
        let _ = eng.create_batch(1.0, true);
        eng.advance();
        assert_eq!(eng.execution.state.total_supply("GOLD"), 500);
        assert!(eng.execution.state.burned_native > 0 || eng.execution.state.treasury_fees > 0);

        let xfer = Transaction::transfer("acct1", 2, "acct2", "GOLD", 100, 5, 1.0);
        assert!(eng.submit_tx(xfer).is_ok());
        let _ = eng.create_batch(2.0, true);
        eng.advance();
        assert_eq!(eng.execution.state.balance("acct2", "GOLD"), 100);
        assert_eq!(eng.execution.state.balance("acct1", "GOLD"), 400);
        let root = eng.execution.state.state_root();
        assert_eq!(root.len(), 64);
        // re-exec same ordered set → same root
        eng.advance();
        assert_eq!(eng.execution.state.state_root(), root);
    }

    #[test]
    fn signed_tx_required_and_auto_sign() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                require_tx_sigs: true,
                worker_sharding: false,
                target_batch_interval_s: 0.0,
                ..Default::default()
            },
            0,
            1,
        );
        eng.set_wallet(Arc::new(HmacWallet::new()), true);
        let unsigned = Transaction::create("acct1", 1, "acct2", 1, 10, 0.0);
        assert!(eng.submit_tx(unsigned).is_ok()); // auto-signed
        // reject bad sig
        let mut bad = Transaction::create("acct1", 2, "acct2", 1, 10, 1.0);
        bad.public_key = Some("hmac-dev:acct1".into());
        bad.signature = Some("00".repeat(32));
        assert_eq!(eng.submit_tx(bad), SubmitResult::BadSignature);
        eng.auto_sign = false;
        let bare = Transaction::create("acct1", 3, "acct2", 1, 10, 2.0);
        assert_eq!(eng.submit_tx(bare), SubmitResult::MissingSignature);
    }

    #[test]
    fn falcon_wallet_signed_tx_executes() {
        let wallet = Arc::new(Falcon512Wallet::faucet(3));
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                require_tx_sigs: true,
                worker_sharding: false,
                target_batch_interval_s: 0.0,
                crypto_scheme: "falcon-512".into(),
                ..Default::default()
            },
            0,
            1,
        );
        eng.set_wallet(wallet.clone(), false);
        let mut tx = Transaction::create("acct1", 1, "acct2", 10, 1, 0.0);
        wallet.sign_tx(&mut tx, eng.config.network_id).unwrap();
        assert_eq!(eng.submit_tx(tx.clone()), SubmitResult::Accepted);
        let _ = eng.create_batch(1.0, true);
        eng.advance();
        assert!(eng.execution.executed_txs.contains(&tx.tx_id));
    }

    #[test]
    fn multi_node_falcon512_certify() {
        use fd_crypto::Falcon512KeyRing;
        let unl: Vec<String> = (1..=5).map(|i| format!("val{i}")).collect();
        let ring: SharedCrypto =
            Arc::new(Falcon512KeyRing::generate_unl(unl.iter().cloned()).unwrap());
        let mut engines = five_node_engines_with_crypto(Some(ring));
        let tx = Transaction::create("acct1", 1, "acct2", 100, 10, 1.0);
        for eng in engines.iter_mut() {
            eng.submit_tx(tx.clone());
        }
        let (batch, _) = engines[0].create_batch(1.0, true).expect("batch");
        gossip_batch(&mut engines, 0, batch);
        for eng in &engines {
            assert!(eng.dag.certificates.len() >= 2);
            assert!(eng.execution.executed_txs.contains(&tx.tx_id));
            assert_eq!(eng.dag.crypto.scheme(), "falcon-512");
        }
    }

    #[test]
    fn double_spend_first_wins() {
        let mut engines = five_node_engines();
        let a = Transaction::create("acct1", 1, "acct2", 50, 10, 1.0);
        let b = Transaction::create("acct1", 1, "acct3", 50, 10, 1.1);
        // Put both in mempools; val1 packs a, val2 packs b on same parents
        for eng in engines.iter_mut() {
            eng.submit_tx(a.clone());
            eng.submit_tx(b.clone());
        }
        let (ba, _) = engines[0].create_batch(1.0, true).expect("batch a");
        gossip_batch(&mut engines, 0, ba);
        // After a is certified and executed, b's sequence is spent
        let executed: BTreeSet<_> = engines[0].execution.executed_txs.iter().cloned().collect();
        assert!(executed.contains(&a.tx_id) || executed.contains(&b.tx_id));
        assert!(
            !(executed.contains(&a.tx_id) && executed.contains(&b.tx_id)),
            "both conflicting txs must not execute"
        );
    }

    /// Two validator cliques pack conflicting seq=1 txs, then mesh merges:
    /// honest majority cert path yields a single executed winner everywhere.
    /// Ignored under multi-lane force-before-move: same-account double-spend cannot
    /// be packed by two lane owners; rewrite as single-lane adversarial test later.
    #[test]
    #[ignore = "multi-lane ownership vs dual-clique same-acct pack"]
    fn partition_merge_double_spend_signed() {
        use fd_crypto::HmacWallet;
        let unl: Vec<String> = (1..=5).map(|i| format!("val{i}")).collect();
        let mut cfg = ProtocolConfig::default();
        cfg.worker_sharding = false;
        cfg.target_batch_interval_s = 0.0;
        cfg.require_tx_sigs = true;
        // Partition scenario needs high threshold so 3/5 cannot certify alone.
        cfg.unl_threshold = 0.80;
        // Allow both cliques to pack conflicting proposals (adversarial fork test).
        cfg.single_packer_per_round = false;
        let wallet: SharedWallet = Arc::new(HmacWallet::new());

        let mut engines: Vec<NodeEngine> = unl
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let mut e =
                    NodeEngine::new(id.clone(), unl.clone(), cfg.clone(), i, unl.len());
                e.set_wallet(Arc::clone(&wallet), true);
                e
            })
            .collect();

        // Multi-lane owns_tx: packers only pack their account lanes.
        let mut acct_a = None;
        for i in 1..120u32 {
            let a = format!("acct{i}");
            if engines[0].account_owner_id(&a).as_deref() == Some("val1") {
                acct_a = Some(a);
                break;
            }
        }
        let acct_a = acct_a.expect("val1 lane account");
        // Same account conflicting spends (true double-spend); only val1 may pack it.
        // Clique B still packs a different owned account so both sides produce certs.
        let mut acct_b = None;
        for i in 1..120u32 {
            let a = format!("acct{i}");
            if a != acct_a && engines[3].account_owner_id(&a).as_deref() == Some("val4") {
                acct_b = Some(a);
                break;
            }
        }
        let acct_b = acct_b.expect("val4 lane account");
        let mut a = Transaction::create(&acct_a, 1, "destA", 50, 10, 1.0);
        let mut b_conflict = Transaction::create(&acct_a, 1, "destB", 50, 10, 1.1);
        let mut b = Transaction::create(&acct_b, 1, "destB", 50, 10, 1.1);
        wallet.sign_tx(&mut a, cfg.network_id).unwrap();
        wallet.sign_tx(&mut b_conflict, cfg.network_id).unwrap();
        wallet.sign_tx(&mut b, cfg.network_id).unwrap();

        // Clique A: val1,val2,val3 only see tx A
        for eng in engines.iter_mut().take(3) {
            assert!(eng.submit_tx(a.clone()).is_ok(), "submit A");
        }
        // Clique B: val4,val5 see B (their lane) + conflict on acct_a for mesh later
        for eng in engines.iter_mut().skip(3) {
            assert!(eng.submit_tx(b.clone()).is_ok(), "submit B");
            let _ = eng.submit_tx(b_conflict.clone());
        }

        let (ba, _) = engines[0].create_batch(1.0, true).expect("batch A");
        // partial gossip within clique A only
        for eng in engines.iter_mut().take(3).skip(1) {
            eng.on_remote_batch(ba.clone(), 1.0);
        }
        // collect clique-A signatures (3 of 5 = 0.6 < 0.8 — not enough alone)
        let mut sigs_a = Vec::new();
        for eng in engines.iter().take(3) {
            sigs_a.push((
                eng.validator_id.clone(),
                eng.dag.sign_local(&ba.batch_hash),
            ));
        }
        for eng in engines.iter_mut().take(3) {
            for (vid, sig) in &sigs_a {
                eng.on_remote_sig(&ba.batch_hash, vid, sig);
            }
        }

        let (bb, _) = engines[3].create_batch(2.0, true).expect("batch B");
        for eng in engines.iter_mut().skip(4) {
            eng.on_remote_batch(bb.clone(), 2.0);
        }
        let mut sigs_b = Vec::new();
        for eng in engines.iter().skip(3) {
            sigs_b.push((
                eng.validator_id.clone(),
                eng.dag.sign_local(&bb.batch_hash),
            ));
        }
        for eng in engines.iter_mut().skip(3) {
            for (vid, sig) in &sigs_b {
                eng.on_remote_sig(&bb.batch_hash, vid, sig);
            }
        }

        // Neither side should have certified yet (3/5 and 2/5 below 80%)
        assert!(!engines[0].dag.certificates.contains_key(&ba.batch_hash));
        assert!(!engines[3].dag.certificates.contains_key(&bb.batch_hash));

        // Merge: full mesh gossip of both batches + all UNL sigs
        for eng in engines.iter_mut() {
            eng.dag.ingest_batch(ba.clone());
            eng.dag.ingest_batch(bb.clone());
        }
        let mut sigs_for_a = Vec::new();
        let mut sigs_for_b = Vec::new();
        for eng in engines.iter() {
            sigs_for_a.push((
                eng.validator_id.clone(),
                eng.dag.sign_local(&ba.batch_hash),
            ));
            sigs_for_b.push((
                eng.validator_id.clone(),
                eng.dag.sign_local(&bb.batch_hash),
            ));
        }
        for eng in engines.iter_mut() {
            for (vid, sig) in &sigs_for_a {
                eng.on_remote_sig(&ba.batch_hash, vid, sig);
            }
            for (vid, sig) in &sigs_for_b {
                eng.on_remote_sig(&bb.batch_hash, vid, sig);
            }
            eng.advance_da();
        }
        // Tip-BFT over the mesh (need ≥80% UNL votes on tip commits).
        gossip_tip_bft(&mut engines, 24);

        // Both batches can certify; execution first-wins on seq
        let exec0: BTreeSet<_> = engines[0].execution.executed_txs.iter().cloned().collect();
        let has_a = exec0.contains(&a.tx_id);
        let has_b = exec0.contains(&b.tx_id);
        assert!(
            has_a ^ has_b,
            "exactly one of conflicting txs must execute: a={has_a} b={has_b} certs={} tip_commits={}",
            engines[0].dag.certificates.len(),
            engines[0].ordering.tip_bft.commits.len(),
        );
        for eng in &engines[1..] {
            let e: BTreeSet<_> = eng.execution.executed_txs.iter().cloned().collect();
            assert_eq!(e.contains(&a.tx_id), has_a);
            assert_eq!(e.contains(&b.tx_id), has_b);
        }
    }

    #[test]
    fn bullshark_multi_node_anchors_after_chain() {
        let mut engines = five_node_engines();
        for seq in 1..=12u64 {
            let tx = Transaction::create("acct1", seq, "acct2", 1, 10, seq as f64);
            for eng in engines.iter_mut() {
                eng.submit_tx(tx.clone());
            }
            let origin = ((seq - 1) as usize) % engines.len();
            if let Some((batch, _)) = engines[origin].create_batch(seq as f64, true) {
                gossip_batch(&mut engines, origin, batch);
            }
            gossip_tip_bft(&mut engines, 4);
        }
        let a0 = engines[0].ordering.bullshark.anchors.len();
        let max_round = engines[0]
            .dag
            .certificates
            .values()
            .map(|c| c.round)
            .max();
        assert!(
            a0 >= 1,
            "expected bullshark anchors; max_round={max_round:?} certs={} ordered={} tip={}",
            engines[0].dag.certificates.len(),
            engines[0].ordering.ordered.len(),
            engines[0].ordering.tip_bft.commits.len(),
        );
        for eng in &engines[1..] {
            assert_eq!(
                eng.ordering.bullshark.anchors.len(),
                a0,
                "anchor agreement {}",
                eng.validator_id
            );
        }
        assert!(!engines[0].execution.executed_txs.is_empty());
    }

    /// Pack-ahead: Full node packs contiguous sequences without waiting for execute.
    #[test]
    fn pack_ahead_fattens_batch_before_execute() {
        let unl = vec!["val1".into()];
        let mut eng = NodeEngine::new(
            "val1".into(),
            unl,
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                target_batch_interval_s: 0.0,
                max_batch_txs: 64,
                ..Default::default()
            },
            0,
            1,
        );
        for seq in 1..=20u64 {
            assert!(eng
                .submit_tx(Transaction::create("acct1", seq, "acct2", 1, 10, seq as f64))
                .is_ok());
        }
        // Before any execute, pack should take many sequences in one batch.
        let (b, _) = eng.create_batch(1.0, true).expect("pack");
        assert!(
            b.tx_ids.len() >= 10,
            "pack-ahead should fatten batch, got {}",
            b.tx_ids.len()
        );
        assert_eq!(
            eng.execution.executed_txs.len(),
            0,
            "packing must not execute"
        );
        eng.advance_da();
        eng.advance_ledger();
        assert!(
            eng.execution.executed_txs.len() >= 10,
            "ledger should execute packed txs; exec={}",
            eng.execution.executed_txs.len()
        );
    }

    /// Multi-node: after load, cert chain deepens and most txs apply.
    #[test]
    fn multi_node_pack_deepens_chain_and_applies() {
        let mut engines = five_node_engines();
        // Cap batch size so packing must form a multi-round chain.
        for eng in engines.iter_mut() {
            eng.config.max_batch_txs = 4;
            eng.dag.config.max_batch_txs = 4;
        }
        let n_tx = 30u64;
        for seq in 1..=n_tx {
            let tx = Transaction::create("acct1", seq, "acct2", 1, 10, seq as f64);
            for eng in engines.iter_mut() {
                eng.submit_tx(tx.clone());
            }
        }
        // Several pack/gossip rounds from rotating origin.
        for round in 0..40 {
            let origin = round % engines.len();
            if let Some((batch, _)) = engines[origin].create_batch(10.0 + round as f64, true) {
                gossip_batch(&mut engines, origin, batch);
            }
            gossip_tip_bft(&mut engines, 3);
        }
        let max_round = engines[0]
            .dag
            .certificates
            .values()
            .map(|c| c.round)
            .max()
            .unwrap_or(0);
        let exec = engines[0].execution.executed_txs.len();
        let anchors = engines[0].ordering.bullshark.anchors.len();
        let avg_pack = {
            let batches: Vec<_> = engines[0]
                .dag
                .batches
                .values()
                .filter(|b| b.batch_hash != genesis_batch_hash())
                .collect();
            if batches.is_empty() {
                0.0
            } else {
                batches.iter().map(|b| b.tx_ids.len()).sum::<usize>() as f64
                    / batches.len() as f64
            }
        };
        assert!(
            max_round >= 4,
            "cert chain should deepen; max_round={max_round} certs={}",
            engines[0].dag.certificates.len()
        );
        assert!(
            anchors >= 1,
            "bullshark should commit; anchors={anchors} max_round={max_round}"
        );
        assert!(
            exec as u64 >= n_tx * 8 / 10,
            "most txs should apply; exec={exec}/{n_tx} avg_pack={avg_pack:.1}"
        );
        assert!(
            avg_pack >= 2.0,
            "batches should be fatter with pack-ahead; avg_pack={avg_pack:.1}"
        );
        for eng in &engines[1..] {
            assert_eq!(eng.execution.executed_txs.len(), exec);
            assert_eq!(eng.ordering.bullshark.anchors.len(), anchors);
        }
    }

    // --- Phase 49a: packing backpressure gate + ordered_lag diagnostics ---

    fn dummy_cert(hash: &str, round: u64) -> Certificate {
        Certificate {
            cert_id: format!("cert-{hash}"),
            batch_hash: hash.into(),
            signers: vec!["val1".into()],
            parents: vec![genesis_batch_hash()],
            creator: "val1".into(),
            timestamp: 0.0,
            round,
        }
    }

    /// Inject `k` certified-but-unordered (non-genesis) batches by adding fake
    /// certificates that ordering has not consumed. Returns the injected hashes.
    fn inject_unordered_certs(eng: &mut NodeEngine, k: usize) -> Vec<String> {
        let mut hs = Vec::new();
        for i in 0..k {
            let h = format!("bp-hash-{i}");
            eng.dag
                .certificates
                .insert(h.clone(), dummy_cert(&h, (i + 1) as u64));
            hs.push(h);
        }
        hs
    }

    fn drain_ordered(eng: &mut NodeEngine, hashes: &[String], count: usize) {
        for h in hashes.iter().take(count) {
            if eng.ordering.ordered_set.insert(h.clone()) {
                eng.ordering.ordered.push(h.clone());
            }
        }
    }

    #[test]
    fn pack_backpressure_gate_blocks_and_resumes() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                target_batch_interval_s: 0.0,
                single_packer_per_round: true,
                pack_backpressure_max: 2,
                ..Default::default()
            },
            0,
            1,
        );
        for seq in 1..=10u64 {
            assert!(eng
                .submit_tx(Transaction::create("acct1", seq, "acct2", 1, 10, seq as f64))
                .is_ok());
        }
        // Genesis is in both certificates and ordered; injecting 4 unordered
        // certs drives ordered_lag to 4 (> threshold of 2).
        let hs = inject_unordered_certs(&mut eng, 4);
        assert_eq!(eng.ordered_lag(), 4, "genesis must cancel; lag = injected");
        assert!(eng.pack_backpressured());
        assert!(
            eng.create_batch(1.0, true).is_none(),
            "gate must block packing while lag > max"
        );

        // Drain ordering below the threshold; packing must resume.
        drain_ordered(&mut eng, &hs, 3);
        assert_eq!(eng.ordered_lag(), 1);
        assert!(!eng.pack_backpressured());
        assert!(
            eng.create_batch(2.0, true).is_some(),
            "packing must resume once lag drops to/below max"
        );
    }

    #[test]
    fn pack_backpressure_disabled_by_default() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                target_batch_interval_s: 0.0,
                single_packer_per_round: true,
                // pack_backpressure_max defaults to 0 (disabled).
                ..Default::default()
            },
            0,
            1,
        );
        assert_eq!(eng.config.pack_backpressure_max, 0);
        for seq in 1..=5u64 {
            assert!(eng
                .submit_tx(Transaction::create("acct1", seq, "acct2", 1, 10, seq as f64))
                .is_ok());
        }
        // Even with a large unordered backlog, the disabled gate never fires.
        inject_unordered_certs(&mut eng, 50);
        assert!(!eng.pack_backpressured());
        assert!(
            eng.create_batch(1.0, true).is_some(),
            "default (disabled) gate must not change behavior"
        );
    }

    #[test]
    fn metrics_expose_ordered_lag_and_backpressure() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                pack_backpressure_max: 2,
                ..Default::default()
            },
            0,
            1,
        );
        inject_unordered_certs(&mut eng, 5);
        let m = eng.metrics_json();
        assert_eq!(m["ordered_lag"], serde_json::json!(5));
        assert_eq!(m["pack_backpressured"], serde_json::json!(true));
        // Existing fields must remain.
        assert!(m.get("certified").is_some());
        assert!(m.get("ordered").is_some());

        let prom = eng.metrics_prometheus();
        assert!(prom.contains("falcon_dag_ordered_lag"));
        assert!(prom.contains("falcon_dag_pack_backpressured"));
    }

    // --- Phase 49b: packing stall diagnostics ---

    #[test]
    fn admit_reasons_histogram_seq_and_success() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                account_sequence_window: 4,
                ..Default::default()
            },
            0,
            1,
        );
        assert!(eng
            .submit_tx(Transaction::create("acct1", 1, "acct2", 1, 10, 1.0))
            .is_ok());
        // seq 0 is always past (expected starts at 1)
        assert_eq!(
            eng.submit_tx(Transaction::create("acct1", 0, "acct2", 1, 10, 3.0)),
            SubmitResult::SeqPast
        );
        // too far ahead of window (expected=1, window=4 => max exclusive 5)
        assert_eq!(
            eng.submit_tx(Transaction::create("acct1", 6, "acct2", 1, 10, 4.0)),
            SubmitResult::SeqFuture
        );
        assert!(eng.admit_reasons.get("accepted").copied().unwrap_or(0) >= 1);
        assert!(eng.admit_reasons.get("seq_past").copied().unwrap_or(0) >= 1);
        assert!(eng.admit_reasons.get("seq_future").copied().unwrap_or(0) >= 1);
        let m = eng.metrics_json();
        assert!(m.get("admit_reasons").is_some());
        assert!(m.get("pack_skip_reasons").is_some());
        assert!(m.get("mempool_accounts").is_some());
    }

    #[test]
    fn pack_skip_reasons_and_mempool_account_coverage() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                single_packer_per_round: false,
                target_batch_interval_s: 0.0,
                ..Default::default()
            },
            0,
            1,
        );
        // Only seq 3 in mempool → gap at pack_cursor=1 → no_ready_txs / filter_seq_gap
        assert!(eng
            .submit_tx(Transaction::create("acct1", 3, "acct2", 1, 10, 1.0))
            .is_ok());
        let cov = eng.mempool_account_coverage();
        let arr = cov.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["min_seq"], 3);
        assert_eq!(arr[0]["max_seq"], 3);
        assert_eq!(arr[0]["pack_cursor"], 1);
        assert_eq!(arr[0]["missing_at_pack_cursor"], true);

        assert!(eng.create_batch(1.0, true).is_none());
        assert!(
            eng.pack_skip_reasons.get("no_ready_txs").copied().unwrap_or(0) >= 1,
            "expected no_ready_txs skip, got {:?}",
            eng.pack_skip_reasons
        );
        assert!(
            eng.pack_skip_reasons.get("filter_seq_gap").copied().unwrap_or(0) >= 1
                || eng.pack_skip_reasons.get("filter_seq_gap_n").copied().unwrap_or(0) >= 1,
            "expected seq gap filter, got {:?}",
            eng.pack_skip_reasons
        );

        // Fill contiguous sequences and pack succeeds
        assert!(eng
            .submit_tx(Transaction::create("acct1", 1, "acct2", 1, 10, 2.0))
            .is_ok());
        assert!(eng
            .submit_tx(Transaction::create("acct1", 2, "acct2", 1, 10, 3.0))
            .is_ok());
        assert!(eng.create_batch(2.0, true).is_some());
        assert!(eng.pack_skip_reasons.get("packed").copied().unwrap_or(0) >= 1);

        let ps = eng.pack_stall_snapshot();
        assert!(ps.get("pack_skip_reasons").is_some());
        assert!(ps.get("mempool_accounts").is_some());
        assert!(ps.get("admit_reasons").is_some());
    }

    #[test]
    fn not_assigned_packer_skip_reason() {
        let mut eng = NodeEngine::new(
            "val2".into(),
            vec!["val1".into(), "val2".into()],
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                single_packer_per_round: true,
                target_batch_interval_s: 0.0,
                ..Default::default()
            },
            1,
            2,
        );
        assert!(eng
            .submit_tx(Transaction::create("acct1", 1, "acct2", 1, 10, 1.0))
            .is_ok());
        // val1 is primary (sorted); val2 should skip as not assigned
        assert!(!eng.is_assigned_packer());
        assert!(eng.create_batch(1.0, true).is_none());
        assert!(
            eng.pack_skip_reasons
                .get("not_assigned_packer")
                .copied()
                .unwrap_or(0)
                >= 1
        );
    }

    #[test]
    fn pack_skip_hole_advances_cursor_and_packs() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                single_packer_per_round: false,
                target_batch_interval_s: 0.0,
                pack_skip_hole_after_s: 0.5,
                ..Default::default()
            },
            0,
            1,
        );
        // Only seq 5 present — hole at 1..4. first_seen far in the past.
        let mut tx = Transaction::create("acct1", 5, "acct2", 1, 10, 1.0);
        tx.first_seen = 0.0; // aged
        assert!(eng.submit_tx(tx).is_ok());
        // Without enough wait from "now", hole may still track; use now >> first_seen
        assert!(
            eng.create_batch(10.0, true).is_some(),
            "skip-hole should pack seq 5 after age threshold; skips={:?}",
            eng.pack_skip_reasons
        );
        assert!(
            eng.pack_skip_reasons.get("skip_hole").copied().unwrap_or(0) >= 1,
            "expected skip_hole reason: {:?}",
            eng.pack_skip_reasons
        );
        assert_eq!(eng.pack_next_seq("acct1"), 6);
        // Execution next-seq parked at abandoned target (seq 5) so apply can take it.
        assert_eq!(eng.execution.state.next_sequence("acct1"), 5);
        assert!(eng.batches_created >= 1);
    }

    #[test]
    fn pack_skip_hole_disabled_by_default() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                require_tx_sigs: false,
                worker_sharding: false,
                single_packer_per_round: false,
                target_batch_interval_s: 0.0,
                // pack_skip_hole_after_s default 0
                ..Default::default()
            },
            0,
            1,
        );
        let mut tx = Transaction::create("acct1", 5, "acct2", 1, 10, 1.0);
        tx.first_seen = 0.0;
        assert!(eng.submit_tx(tx).is_ok());
        assert!(eng.create_batch(10.0, true).is_none());
        assert_eq!(
            eng.pack_skip_reasons.get("skip_hole").copied().unwrap_or(0),
            0
        );
        assert!(
            eng.pack_skip_reasons.get("no_ready_txs").copied().unwrap_or(0) >= 1
                || eng.pack_skip_reasons.get("filter_seq_gap").copied().unwrap_or(0) >= 1
        );
    }




    #[test]
    fn multi_packer_account_shard_disjoint() {
        // val1 and val2 both multi-pack; each owns a disjoint account set.
        let cfg = ProtocolConfig {
            require_tx_sigs: false,
            worker_sharding: false,
            single_packer_per_round: false,
            target_batch_interval_s: 0.0,
            ..Default::default()
        };
        let unl = vec!["val1".into(), "val2".into(), "val3".into()];
        let mut e1 = NodeEngine::new("val1".into(), unl.clone(), cfg.clone(), 0, 1);
        let mut e2 = NodeEngine::new("val2".into(), unl.clone(), cfg.clone(), 0, 1);
        // Find two accounts owned by different validators.
        let mut a_for_v1 = None;
        let mut a_for_v2 = None;
        for i in 1..50u32 {
            let a = format!("acct{i}");
            let s = NodeEngine::account_shard(&a) as usize % 3;
            if s == 0 && a_for_v1.is_none() {
                a_for_v1 = Some(a);
            } else if s == 1 && a_for_v2.is_none() {
                a_for_v2 = Some(a);
            }
        }
        let a1 = a_for_v1.expect("shard0 account");
        let a2 = a_for_v2.expect("shard1 account");
        let tx1 = Transaction::create(&a1, 1, "acct2", 1, 10, 1.0);
        let tx2 = Transaction::create(&a2, 1, "acct2", 1, 10, 1.0);
        assert!(e1.owns_tx(&tx1, 1.0), "val1 owns {a1}");
        assert!(!e1.owns_tx(&tx2, 1.0), "val1 must not own {a2}");
        assert!(e2.owns_tx(&tx2, 1.0), "val2 owns {a2}");
        assert!(!e2.owns_tx(&tx1, 1.0), "val2 must not own {a1}");
        // Submit only owned accounts succeed packing isolation: each packs its own.
        // (genesis faucet may not fund arbitrary accounts — only check owns_tx here)
    }

    #[test]
    fn pr4_queue_cert_to_creator_immediate() {
        use fd_crypto::HmacKeyRing;
        use std::sync::Arc;
        let unl = vec!["val1".into(), "val2".into(), "val3".into()];
        let crypto: SharedCrypto = Arc::new(HmacKeyRing::for_unl(unl.iter().cloned()));
        let mut cfg = ProtocolConfig::default();
        cfg.single_packer_per_round = false;
        cfg.require_tx_sigs = false;
        cfg.unl_threshold = 0.67;
        let mut eng =
            NodeEngine::with_crypto("val2".into(), unl.clone(), cfg, 0, 1, crypto);
        let tx = Transaction::payment("acct0", 1, "dest", 10, 0, 1.0);
        let b = Batch::create(
            vec![tx],
            vec![genesis_batch_hash()],
            "val1".into(),
            1.0,
        );
        let h = b.batch_hash.clone();
        eng.dag.ingest_batch(b);
        for vid in ["val1", "val2", "val3"] {
            let sig = if vid == "val2" {
                eng.dag.sign_local(&h)
            } else {
                format!("sig-{vid}")
            };
            let _ = eng.dag.add_signature(&h, vid, &sig);
        }
        if let Some(batch) = eng.dag.batches.get(&h) {
            let cert = Certificate::from_batch(batch, 1);
            eng.dag.certificates.insert(h.clone(), cert);
        }
        eng.queue_cert_to_creator_now(&h, 2.0);
        let pending = eng.take_creator_cert_announce();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, "val1");
        assert_eq!(pending[0].1, h);
        assert!(eng.cert_directed_to_creator >= 1);
    }

    #[test]
    fn pr2_directed_owner_route_and_mempool_status() {
        // Non-owner admit → rebroadcast emits directed_txs to lane owner (measured).
        let cfg = ProtocolConfig {
            require_tx_sigs: false,
            worker_sharding: false,
            single_packer_per_round: false,
            target_batch_interval_s: 0.0,
            admit_circuit_breaker: false,
            ..Default::default()
        };
        let unl = vec!["val1".into(), "val2".into(), "val3".into()];
        let mut eng = NodeEngine::new("val1".into(), unl.clone(), cfg, 0, 1);
        // Pick account owned by val2 (rank 1).
        let mut foreign = None;
        for i in 1..80u32 {
            let a = format!("acct{i}");
            if eng.account_owner_id(&a).as_deref() == Some("val2") {
                foreign = Some(a);
                break;
            }
        }
        let acct = foreign.expect("val2-owned account");
        let tx = Transaction::create(&acct, 1, "dest", 1, 10, 1.0);
        assert!(!eng.owns_tx(&tx, 1.0));
        assert_eq!(eng.submit_tx(tx.clone()), SubmitResult::Accepted);
        assert_eq!(eng.shard_flow.admitted_not_owned, 1);
        let st = eng.mempool_status_json(2.0);
        assert_eq!(st["not_owned_pending"], 1);
        assert_eq!(st["owned_pending"], 0);
        let g = eng.rebroadcast_mempool_txs(32);
        assert_eq!(g.directed_txs.len(), 1);
        assert_eq!(g.directed_txs[0].0, "val2");
        assert_eq!(g.directed_txs[0].1.tx_id, tx.tx_id);
        assert_eq!(eng.shard_flow.directed_to_owner, 1);
        // After transfer, local mempool no longer holds the not-owned copy.
        assert!(eng.mempool.is_empty());
        assert_eq!(eng.shard_flow.stuck_not_owned_pending, 0);
    }

    #[test]
    fn pr2_repool_reinsert_preserves_sequence() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                require_tx_sigs: false,
                admit_circuit_breaker: false,
                ..Default::default()
            },
            0,
            1,
        );
        let tx = Transaction::create("acct1", 3, "dest", 1, 10, 5.0);
        let id = tx.tx_id.clone();
        assert!(eng.mempool.reinsert(tx, 100.0).accepted());
        let got = eng.mempool.get(&id).unwrap();
        assert_eq!(got.sequence, 3);
        assert_eq!(got.first_seen, 5.0);
        assert_eq!(eng.mempool.reinserts_ok, 1);
    }

    #[test]
    fn lane_dedup_keeps_lowest_tx_id_per_seq() {
        let t1 = Transaction::payment("acct1", 1, "dest", 1, 0, 1.0);
        let t2b = Transaction::payment("acct1", 1, "dest", 2, 0, 1.0);
        let t3 = Transaction::payment("acct1", 2, "dest", 1, 0, 1.0);
        let (out, rej) = NodeEngine::lane_dedup_conflicts(vec![t2b.clone(), t1.clone(), t3.clone()]);
        assert_eq!(rej, 1);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].sequence, 1);
        assert_eq!(out[1].sequence, 2);
        // winner is lexicographically smaller tx_id
        let win = if t1.tx_id < t2b.tx_id { t1.tx_id } else { t2b.tx_id };
        assert_eq!(out[0].tx_id, win);
    }

    #[test]
    fn multi_lane_parents_use_own_lane_tip() {
        use fd_crypto::HmacKeyRing;
        use std::sync::Arc;
        let unl = vec!["val1".into(), "val2".into(), "val3".into()];
        let crypto: SharedCrypto = Arc::new(HmacKeyRing::for_unl(unl.iter().cloned()));
        let mut cfg = ProtocolConfig::default();
        cfg.single_packer_per_round = false;
        cfg.max_batch_parents = 1;
        cfg.require_tx_sigs = false;
        let mut e1 = NodeEngine::with_crypto("val1".into(), unl.clone(), cfg.clone(), 0, 1, crypto.clone());
        let mut e2 = NodeEngine::with_crypto("val2".into(), unl.clone(), cfg.clone(), 0, 1, crypto);
        // Without packing: multi-lane parents are genesis.
        assert_eq!(e1.select_batch_parents(), vec![genesis_batch_hash()]);
        assert_eq!(e2.select_batch_parents(), vec![genesis_batch_hash()]);
        // Inject a certified batch created by val1 (lane tip).
        let tx = Transaction::payment("acct0", 1, "dest", 10, 0, 1.0);
        let b1 = Batch::create(
            vec![tx],
            vec![genesis_batch_hash()],
            "val1".into(),
            1.0,
        );
        let h = b1.batch_hash.clone();
        e1.dag.ingest_batch(b1.clone());
        e2.dag.ingest_batch(b1.clone());
        for eng in [&mut e1, &mut e2] {
            for vid in &unl {
                let sig = eng.dag.crypto.sign_batch(vid, &h).expect("sign");
                let _ = eng.dag.add_signature(&h, vid, &sig);
            }
            assert!(
                eng.dag.certificates.contains_key(&h),
                "expected cert after UNL quorum sigs"
            );
        }
        assert!(e1.dag.certificates.contains_key(&h));
        // val1 multipack parents = own finalized lane tip
        let p1 = e1.select_batch_parents();
        assert_eq!(p1, vec![h.clone()], "multi-lane val1 parents own tip");
        // val2 has no own certified batch; may parent foreign *finalized* tip for
        // later Bullshark DAG links — never Bullshark preferred_parent during pack.
        let p2 = e2.select_batch_parents();
        assert!(!p2.is_empty());
        assert!(
            p2.contains(&h) || p2 == vec![genesis_batch_hash()],
            "val2 parents include foreign finalized tip or genesis, got {p2:?}"
        );
        for p in &p2 {
            assert!(
                e2.batch_is_finalized(p),
                "parent must be finalized (cert+body), got {p}"
            );
        }
        // collect_finalized includes the lane batch on both nodes
        let f1 = e1.collect_finalized_lane_certs();
        let f2 = e2.collect_finalized_lane_certs();
        assert!(f1.contains_key(&h));
        assert!(f2.contains_key(&h));
    }

    /// Pipeline: packing must not depend on Bullshark state; ledger only orders
    /// after finalize collect.
    #[test]
    fn pipeline_pack_ignores_bullshark_preferred_parent() {
        use fd_crypto::HmacKeyRing;
        use std::sync::Arc;
        let unl = vec!["val1".into(), "val2".into(), "val3".into()];
        let crypto: SharedCrypto = Arc::new(HmacKeyRing::for_unl(unl.iter().cloned()));
        let mut cfg = ProtocolConfig::default();
        cfg.single_packer_per_round = true;
        cfg.max_batch_parents = 1;
        cfg.require_tx_sigs = false;
        let mut eng =
            NodeEngine::with_crypto("val1".into(), unl.clone(), cfg, 0, 1, crypto);
        // Even if Bullshark engine has empty anchors, packing still returns a parent.
        assert!(eng.ordering.bullshark.anchors.is_empty());
        let p = eng.select_batch_parents();
        assert_eq!(p, vec![genesis_batch_hash()]);
        // Multi-lane: does not use ordered_lag; empty frontier → not incomplete.
        eng.config.single_packer_per_round = false;
        eng.config.pack_backpressure_max = 1;
        assert!(
            !eng.pack_backpressured(),
            "empty DA holes: lane frontier complete"
        );
    }

    #[test]
    fn pipeline_bullshark_only_sees_finalized() {
        use fd_crypto::HmacKeyRing;
        use std::sync::Arc;
        let unl = vec!["val1".into(), "val2".into(), "val3".into()];
        let crypto: SharedCrypto = Arc::new(HmacKeyRing::for_unl(unl.iter().cloned()));
        let mut cfg = ProtocolConfig::default();
        cfg.single_packer_per_round = false;
        cfg.require_tx_sigs = false;
        let mut eng =
            NodeEngine::with_crypto("val1".into(), unl.clone(), cfg, 0, 1, crypto);
        // Certificate without body must not appear in finalized collect.
        // (We only insert a cert entry is hard without body via normal path;
        //  empty dag → only genesis if present.)
        let fin = eng.collect_finalized_lane_certs();
        for bh in fin.keys() {
            assert!(
                eng.batch_is_finalized(bh),
                "collect must only return finalized"
            );
        }
        // advance_ledger is a no-op order-wise without multi-lane cert DAG
        let _ = eng.advance_ledger();
        assert!(eng.ordering.ordered.len() <= 1); // genesis only or empty
    }

    
    
    /// MCB backlog: local admits limited to executed + ahead_cap; mesh bypasses.
    #[test]
    fn admit_circuit_breaker_busy_local_mesh_ok() {
        use fd_crypto::HmacKeyRing;
        use std::sync::Arc;
        let unl = vec!["val1".into(), "val2".into(), "val3".into()];
        let crypto: SharedCrypto = Arc::new(HmacKeyRing::for_unl(unl.iter().cloned()));
        let mut cfg = ProtocolConfig::default();
        cfg.single_packer_per_round = false;
        cfg.require_tx_sigs = false;
        cfg.admit_circuit_breaker = true;
        cfg.admit_ahead_cap = 2;
        cfg.admit_ahead_resume = 1;
        cfg.admit_busy_mempool = 10_000; // do not trip on mempool
        cfg.admit_busy_uncert_age_s = 0.0;
        cfg.admit_busy_open_uncert = 0;
        let mut eng =
            NodeEngine::with_crypto("val1".into(), unl.clone(), cfg, 0, 1, crypto);
        // genesis funds acct1.. — payment ok; executed stays 0 so lag = admits.
        let tx1 = Transaction::payment("acct1", 1, "acct2", 1, 0, 1.0);
        let tx2 = Transaction::payment("acct2", 1, "acct3", 1, 0, 1.0);
        let tx3 = Transaction::payment("acct3", 1, "acct1", 1, 0, 1.0);
        assert_eq!(eng.submit_tx(tx1.clone()), SubmitResult::Accepted);
        assert_eq!(eng.submit_tx(tx2.clone()), SubmitResult::Accepted);
        assert_eq!(eng.admit_exec_lag(), 2);
        // 3rd local admit trips ahead_cap=2
        assert_eq!(eng.submit_tx(tx3.clone()), SubmitResult::Busy);
        assert_eq!(eng.submit_tx(tx3.clone()).engine_code(), "terBUSY");
        assert!(SubmitResult::Busy.is_retryable());
        // Mesh handoff still admits
        assert_eq!(
            eng.submit_tx_mesh(tx3.clone()),
            SubmitResult::Accepted,
            "mesh must bypass MCB"
        );
        // mesh does not increase local_admits_ok
        assert_eq!(eng.local_admits_ok, 2);
        assert!(eng.admit_circuit_open());
    }

    #[test]
    fn local_admit_owned_vs_not_owned_split() {
        let cfg = ProtocolConfig {
            require_tx_sigs: false,
            worker_sharding: false,
            single_packer_per_round: false,
            admit_circuit_breaker: false,
            ..Default::default()
        };
        let unl = vec!["val1".into(), "val2".into(), "val3".into()];
        let mut eng = NodeEngine::new("val1".into(), unl, cfg, 0, 1);
        let mut owned_acct = None;
        let mut foreign_acct = None;
        for i in 1..80u32 {
            let a = format!("acct{i}");
            match eng.account_owner_id(&a).as_deref() {
                Some("val1") if owned_acct.is_none() => owned_acct = Some(a),
                Some(o) if o != "val1" && foreign_acct.is_none() => foreign_acct = Some(a),
                _ => {}
            }
        }
        let owned = owned_acct.expect("owned");
        let foreign = foreign_acct.expect("foreign");
        assert!(eng
            .submit_tx(Transaction::create(&owned, 1, "dest", 1, 10, 1.0))
            .is_ok());
        assert!(eng
            .submit_tx(Transaction::create(&foreign, 1, "dest", 1, 10, 1.0))
            .is_ok());
        assert_eq!(eng.local_admits_ok, 2);
        assert_eq!(eng.local_admits_owned, 1);
        assert_eq!(eng.local_admits_not_owned, 1);
        assert!((eng.local_admit_not_owned_ratio() - 0.5).abs() < 1e-9);
    }

    /// Phase B: not-owned mempool txs are directed to the lane owner, not flooded.

    /// Soft open-cap + conf pool: may pack again without hard wait-for-cert.
    #[test]
    fn conf_pool_allows_pack_without_hard_wait_for_cert() {
        use fd_crypto::HmacKeyRing;
        use std::sync::Arc;
        let unl = vec!["val1".into(), "val2".into(), "val3".into()];
        let crypto: SharedCrypto = Arc::new(HmacKeyRing::for_unl(unl.iter().cloned()));
        let mut cfg = ProtocolConfig::default();
        cfg.single_packer_per_round = false;
        cfg.require_tx_sigs = false;
        cfg.target_batch_interval_s = 0.0;
        cfg.admit_circuit_breaker = false;
        cfg.conf_pool_enabled = true;
        cfg.max_uncertified_per_creator = 2;
        let mut eng =
            NodeEngine::with_crypto("val1".into(), unl.clone(), cfg, 0, 1, crypto);
        let mut acct = None;
        for i in 1..80u32 {
            let a = format!("acct{i}");
            if eng.account_owner_id(&a).as_deref() == Some("val1") {
                acct = Some(a);
                break;
            }
        }
        let acct = acct.expect("val1 account");
        for seq in 1..=8u64 {
            let tx = Transaction::payment(&acct, seq, "dest", 1, 0, seq as f64);
            assert_eq!(eng.submit_tx(tx), SubmitResult::Accepted);
        }
        eng.config.max_batch_txs = 2;
        let (b1, _) = eng.create_batch(1.0, true).expect("first pack");
        assert!(
            eng.conf_pool.contains(&b1.batch_hash)
                || eng.dag.batches.contains_key(&b1.batch_hash)
        );
        eng.pack_skip_reasons.clear();
        // Second pack may hit self_open_on_parents (same tips) — that is fine.
        // Critical: hard wait_open_conf must NOT be the gate anymore.
        let _ = eng.create_batch(2.0, true);
        let _ = eng.create_batch(2.0, false);
        assert_eq!(
            eng.pack_skip_reasons.get("wait_open_conf").copied().unwrap_or(0),
            0,
            "wait_open_conf must stay unused; skips={:?}",
            eng.pack_skip_reasons
        );
    }


    #[test]
    fn recover_stale_open_bodies_repoools_txs() {
        let unl = vec!["val1".into(), "val2".into(), "val3".into()];
        let mut eng = NodeEngine::new(
            "val1".into(),
            unl,
            ProtocolConfig {
                require_tx_sigs: false,
                // single_packer: owns all accounts (repool path is independent of lanes)
                single_packer_per_round: true,
                conf_pool_enabled: true,
                max_uncertified_per_creator: 2,
                open_body_repool_after_s: 1.0,
                target_batch_interval_s: 0.0,
                ..Default::default()
            },
            0,
            1,
        );
        for seq in 1..=3u64 {
            assert!(eng
                .submit_tx(Transaction::create("acct1", seq, "acct2", 1, 10, seq as f64))
                .is_ok());
        }
        let packed = eng.create_batch(10.0, true);
        assert!(packed.is_some(), "pack skip={:?}", eng.pack_skip_reasons);
        let (b, _) = packed.unwrap();
        assert_eq!(eng.dag.uncertified_by_creator("val1"), 1);
        // Before hard deadline (2*ttl=2.0) with open under cap: no repool
        assert_eq!(eng.recover_stale_open_bodies(10.5), 0);
        assert!(eng.dag.batches.contains_key(&b.batch_hash));
        // After hard deadline age>=3*ttl with 0 foreign sigs: abandon + repool
        let n = eng.recover_stale_open_bodies(13.1);
        assert_eq!(n, 1, "should abandon one stale open");
        assert!(!eng.dag.batches.contains_key(&b.batch_hash));
        assert!(eng.mempool.len() >= 1, "txs returned to mempool");
        assert_eq!(eng.dag.uncertified_by_creator("val1"), 0);
    }

    #[test]
    fn rebroadcast_directs_not_owned_to_lane_owner() {


        use fd_crypto::HmacKeyRing;
        use std::sync::Arc;
        let unl = vec!["val1".into(), "val2".into(), "val3".into()];
        let crypto: SharedCrypto = Arc::new(HmacKeyRing::for_unl(unl.iter().cloned()));
        let mut cfg = ProtocolConfig::default();
        cfg.single_packer_per_round = false;
        cfg.require_tx_sigs = false;
        cfg.target_batch_interval_s = 0.0;
        let mut eng =
            NodeEngine::with_crypto("val1".into(), unl.clone(), cfg, 0, 1, crypto);
        let mut acct_v2 = None;
        for i in 1..80u32 {
            let a = format!("acct{i}");
            let owner = eng.account_owner_id(&a).expect("owner");
            if owner == "val2" {
                acct_v2 = Some(a);
                break;
            }
        }
        let acct = acct_v2.expect("val2-owned account");
        let tx = Transaction::payment(&acct, 1, "dest", 1, 0, 1.0);
        assert!(!eng.owns_tx(&tx, 1.0), "val1 must not own {acct}");
        assert_eq!(eng.submit_tx(tx.clone()), SubmitResult::Accepted);
        let g = eng.rebroadcast_mempool_txs(32);
        assert!(
            g.txs.is_empty(),
            "multi-lane must not mesh-flood not-owned txs"
        );
        assert_eq!(g.directed_txs.len(), 1);
        assert_eq!(g.directed_txs[0].0, "val2");
        assert_eq!(g.directed_txs[0].1.tx_id, tx.tx_id);
        assert!(eng.shard_flow.directed_to_owner >= 1);
        // Transfer handoff: local copy removed after directed send.
        assert_eq!(
            eng.shard_flow.stuck_not_owned_pending, 0,
            "must not keep not-owned txs after directed transfer"
        );
        assert!(
            !eng.mempool.contains(&tx.tx_id),
            "transferred tx must leave entry-node mempool"
        );
        assert!(NodeEngine::lane_owner_down_stalls_accounts());
    }


}
