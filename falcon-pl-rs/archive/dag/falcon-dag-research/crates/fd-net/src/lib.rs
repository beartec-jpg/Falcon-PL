//! TCP peer mesh for Falcon DAG — **Narwhal-style inventory + pull**.
//!
//! ## Wire
//! - Length-prefixed JSON (`u32 LE` + body); legacy line-JSON accepted on read
//! - Bounded inbound with drop-on-full
//!
//! ## DA gossip (pull path)
//! 1. `inv` — announce hashes only (batch / cert)
//! 2. `want` — request missing hashes
//! 3. `batch` / `cert` — **response bodies** only to requesters (or first-hop creator)
//!
//! Full bodies are **not** rebroadcast to every peer. Use `PullTracker` for
//! timeout/retry and per-peer in-flight limits.

use anyhow::{Context, Result};
use fd_types::{
    Batch, Certificate, HashHex, PartialSignature, TipCommit, TipProposal, TipVote, Transaction,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Wire
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireMsg {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx: Option<Transaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<Batch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<PartialSignature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<Certificate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip_proposal: Option<TipProposal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip_vote: Option<TipVote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip_commit: Option<TipCommit>,
    /// `batch` | `cert`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inv_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inv_hashes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_id: Option<u32>,
}

impl WireMsg {
    fn empty_base(msg_type: &str, from: &str) -> Self {
        Self {
            msg_type: msg_type.into(),
            from: Some(from.into()),
            tx: None,
            batch: None,
            sig: None,
            cert: None,
            tip_proposal: None,
            tip_vote: None,
            tip_commit: None,
            inv_kind: None,
            inv_hashes: None,
            network_id: None,
        }
    }

    pub fn hello(from: &str, network_id: u32) -> Self {
        let mut m = Self::empty_base("hello", from);
        m.network_id = Some(network_id);
        m
    }

    pub fn tx(from: &str, tx: Transaction) -> Self {
        let mut m = Self::empty_base("tx", from);
        m.tx = Some(tx);
        m
    }

    pub fn batch(from: &str, batch: Batch) -> Self {
        let mut m = Self::empty_base("batch", from);
        m.batch = Some(batch);
        m
    }

    pub fn sig(from: &str, sig: PartialSignature) -> Self {
        let mut m = Self::empty_base("sig", from);
        m.sig = Some(sig);
        m
    }

    pub fn cert(from: &str, cert: Certificate) -> Self {
        let mut m = Self::empty_base("cert", from);
        m.cert = Some(cert);
        m
    }

    pub fn tip_proposal(from: &str, prop: TipProposal) -> Self {
        let mut m = Self::empty_base("tip_proposal", from);
        m.tip_proposal = Some(prop);
        m
    }

    pub fn tip_vote(from: &str, vote: TipVote) -> Self {
        let mut m = Self::empty_base("tip_vote", from);
        m.tip_vote = Some(vote);
        m
    }

    pub fn tip_commit(from: &str, commit: TipCommit) -> Self {
        let mut m = Self::empty_base("tip_commit", from);
        m.tip_commit = Some(commit);
        m
    }

    /// Hash-only inventory announce.
    pub fn inv(from: &str, kind: &str, hashes: Vec<String>) -> Self {
        let mut m = Self::empty_base("inv", from);
        m.inv_kind = Some(kind.into());
        m.inv_hashes = Some(hashes);
        m
    }

    /// Request missing bodies.
    pub fn want(from: &str, kind: &str, hashes: Vec<String>) -> Self {
        let mut m = Self::empty_base("want", from);
        m.inv_kind = Some(kind.into());
        m.inv_hashes = Some(hashes);
        m
    }

    fn encode_frame(&self) -> Result<Vec<u8>> {
        let body = serde_json::to_vec(self)?;
        let mut out = Vec::with_capacity(4 + body.len());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Pull tracker (timeout / retry / per-peer in-flight)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PullKind {
    Batch,
    Cert,
}

impl PullKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Batch => "batch",
            Self::Cert => "cert",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "batch" => Some(Self::Batch),
            "cert" => Some(Self::Cert),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct PendingPull {
    kind: PullKind,
    hash: HashHex,
    first_seen: Instant,
    last_request: Instant,
    peers_tried: HashSet<String>,
    retries: u32,
    /// Peer we last asked (for in-flight accounting).
    inflight_peer: Option<String>,
}

/// Tracks missing DA objects and schedules wants with retry + backpressure.
pub struct PullTracker {
    pending: HashMap<String, PendingPull>, // key = "batch:hash" or "cert:hash"
    /// peer_id → count of in-flight wants
    inflight_per_peer: HashMap<String, u32>,
    /// Which peers have announced each hash (for retry from another peer).
    announcers: HashMap<String, VecDeque<String>>, // key same as pending
    pub max_inflight_per_peer: u32,
    pub request_timeout: Duration,
    pub max_retries: u32,
    pub wants_sent: AtomicU64,
    pub bodies_served: AtomicU64,
    pub retries: AtomicU64,
    /// Peak concurrent inflight wants (all peers).
    pub inflight_peak: AtomicU32,
    /// Max hashes bundled into one peer want message.
    pub want_batch_max: AtomicU32,
}

impl Default for PullTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl PullTracker {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            inflight_per_peer: HashMap::new(),
            announcers: HashMap::new(),
            max_inflight_per_peer: 32,
            request_timeout: Duration::from_millis(400),
            max_retries: 64,
            wants_sent: AtomicU64::new(0),
            bodies_served: AtomicU64::new(0),
            retries: AtomicU64::new(0),
            inflight_peak: AtomicU32::new(0),
            want_batch_max: AtomicU32::new(0),
        }
    }

    fn key(kind: PullKind, hash: &str) -> String {
        format!("{}:{}", kind.as_str(), hash)
    }

    /// Note that `peer` announced `hash`. If we lack it, open a pending pull.
    pub fn on_inv(&mut self, kind: PullKind, hash: &str, peer: &str, we_have: bool) {
        if we_have || hash.is_empty() {
            return;
        }
        let k = Self::key(kind, hash);
        self.announcers
            .entry(k.clone())
            .or_default()
            .push_back(peer.to_string());
        self.pending.entry(k).or_insert_with(|| PendingPull {
            kind,
            hash: hash.into(),
            first_seen: Instant::now(),
            last_request: Instant::now()
                .checked_sub(Duration::from_secs(3600))
                .unwrap_or_else(Instant::now),
            peers_tried: HashSet::new(),
            retries: 0,
            inflight_peer: None,
        });
    }

    /// Body received — clear pending + inflight.
    pub fn on_body(&mut self, kind: PullKind, hash: &str) {
        let k = Self::key(kind, hash);
        if let Some(p) = self.pending.remove(&k) {
            if let Some(peer) = p.inflight_peer {
                if let Some(c) = self.inflight_per_peer.get_mut(&peer) {
                    *c = c.saturating_sub(1);
                }
            }
        }
        self.announcers.remove(&k);
    }

    /// Produce want messages: list of (peer_id, kind, hashes) to send.
    pub fn drain_wants(&mut self) -> Vec<(String, PullKind, Vec<String>)> {
        let now = Instant::now();
        let mut by_peer: HashMap<(String, PullKind), Vec<String>> = HashMap::new();
        let keys: Vec<_> = self.pending.keys().cloned().collect();
        for k in keys {
            let Some(p) = self.pending.get_mut(&k) else {
                continue;
            };
            // Still within timeout of last request?
            if p.inflight_peer.is_some() && now.duration_since(p.last_request) < self.request_timeout
            {
                continue;
            }
            // Timed out — free inflight slot and pick next peer
            if let Some(old) = p.inflight_peer.take() {
                if let Some(c) = self.inflight_per_peer.get_mut(&old) {
                    *c = c.saturating_sub(1);
                }
                p.retries += 1;
                self.retries.fetch_add(1, Ordering::Relaxed);
                // Do not permanently drop: under multipack, the holder may only
                // reseed after several seconds. Reset peer trial set so we cycle
                // again (announcers refilled by inv / multi-peer on_inv).
                if p.retries > self.max_retries {
                    p.retries = 0;
                    p.peers_tried.clear();
                }
            }
            // Choose announcer we haven't exhausted / under inflight cap
            let announcers = self.announcers.get(&k).cloned().unwrap_or_default();
            let mut chosen: Option<String> = None;
            for peer in announcers.iter().chain(
                // also try any peer we've seen if announcers empty
                std::iter::empty(),
            ) {
                if p.peers_tried.contains(peer)
                    && p.retries == 0
                    && p.inflight_peer.is_none()
                {
                    // allow retry of same peer after timeout (retries > 0)
                }
                let inflight = self.inflight_per_peer.get(peer).copied().unwrap_or(0);
                if inflight >= self.max_inflight_per_peer {
                    continue;
                }
                // Prefer peers not tried yet, else any under cap
                if !p.peers_tried.contains(peer) || p.retries > 0 {
                    chosen = Some(peer.clone());
                    break;
                }
            }
            // Round-robin: if all tried, pick first announcer under cap
            if chosen.is_none() {
                for peer in &announcers {
                    let inflight = self.inflight_per_peer.get(peer).copied().unwrap_or(0);
                    if inflight < self.max_inflight_per_peer {
                        chosen = Some(peer.clone());
                        break;
                    }
                }
            }
            let Some(peer) = chosen else {
                continue;
            };
            p.peers_tried.insert(peer.clone());
            p.inflight_peer = Some(peer.clone());
            p.last_request = now;
            *self.inflight_per_peer.entry(peer.clone()).or_insert(0) += 1;
            self.wants_sent.fetch_add(1, Ordering::Relaxed);
            by_peer
                .entry((peer, p.kind))
                .or_default()
                .push(p.hash.clone());
        }
        // Phase 49d: record parallelism of this flush.
        for hashes in by_peer.values() {
            let n = hashes.len() as u32;
            self.want_batch_max.fetch_max(n, Ordering::Relaxed);
        }
        let peak = self.inflight_total();
        self.inflight_peak.fetch_max(peak, Ordering::Relaxed);

        by_peer
            .into_iter()
            .map(|((peer, kind), hashes)| (peer, kind, hashes))
            .collect()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Sum of per-peer in-flight want slots (for memory / stall diagnostics).
    pub fn inflight_total(&self) -> u32 {
        self.inflight_per_peer.values().copied().sum()
    }

    /// Snapshot for metrics / stall dumps.
    pub fn stats_json(&self) -> serde_json::Value {
        serde_json::json!({
            "pending_pulls": self.pending.len(),
            "inflight_total": self.inflight_total(),
            "inflight_peak": self.inflight_peak.load(Ordering::Relaxed),
            "want_batch_max": self.want_batch_max.load(Ordering::Relaxed),
            "wants_sent": self.wants_sent.load(Ordering::Relaxed),
            "bodies_served": self.bodies_served.load(Ordering::Relaxed),
            "retries": self.retries.load(Ordering::Relaxed),
            "max_inflight_per_peer": self.max_inflight_per_peer,
            "batch_pull_parallelism": if self.want_batch_max.load(Ordering::Relaxed) > 1
                || self.inflight_peak.load(Ordering::Relaxed) > 1
            {
                "parallel_multi_hash_wants"
            } else {
                "serial_or_idle"
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Peer mesh
// ---------------------------------------------------------------------------

/// DA-must wire types: dedicated high-priority route.
/// These messages are required for open completion (body → sign → cert → creator).
/// Nodes must enqueue and process them; they are never silently skipped/dropped
/// for capacity the way chatter is.
fn is_da_critical(msg_type: &str) -> bool {
    matches!(
        msg_type,
        "batch" | "cert" | "sig" | "hello" | "want"
    )
}

fn is_da_must(msg_type: &str) -> bool {
    // Strict open path — hello excluded (bootstrap only).
    matches!(msg_type, "batch" | "cert" | "sig" | "want")
}

/// Per-peer outbound tiers (wire drain order):
///   1) body   — open batch bodies (must land; highest)
///   2) critical — sig/cert/want/hello
///   3) chatter — inv/tx/tip (droppable)
struct PeerWriter {
    addr: String,
    body_tx: mpsc::Sender<Vec<u8>>,
    critical_tx: mpsc::Sender<Vec<u8>>,
    chatter_tx: mpsc::Sender<Vec<u8>>,
}

/// Inbound mux: body > other critical (sig/cert/want) > chatter.
pub struct InboundMux {
    body: mpsc::Receiver<WireMsg>,
    critical: mpsc::Receiver<WireMsg>,
    chatter: mpsc::Receiver<WireMsg>,
}

impl InboundMux {
    /// Enforce: never skip body — drain body lane first, then sig/cert, then chatter.
    pub async fn recv(&mut self) -> Option<WireMsg> {
        tokio::select! {
            biased;
            m = self.body.recv() => m,
            m = self.critical.recv() => m,
            m = self.chatter.recv() => m,
        }
    }
}

pub type InboundRx = InboundMux;

pub struct PeerMesh {
    pub local_id: String,
    pub network_id: u32,
    writers: Arc<Mutex<Vec<PeerWriter>>>,
    /// validator_id → last known TCP addr (for directed want/body).
    peer_addrs: Arc<Mutex<HashMap<String, String>>>,
    /// Configured dial addresses (stable listen ports) for directed send.
    dial_targets: Arc<Mutex<Vec<String>>>,
    inbound_body_tx: mpsc::Sender<WireMsg>,
    inbound_critical_tx: mpsc::Sender<WireMsg>,
    inbound_chatter_tx: mpsc::Sender<WireMsg>,
    seen_batches: Arc<Mutex<HashSet<String>>>,
    seen_certs: Arc<Mutex<HashSet<String>>>,
    seen_txs: Arc<Mutex<HashSet<String>>>,
    pub pull: Arc<Mutex<PullTracker>>,
    pub dropped_inbound: AtomicU64,
    pub dropped_inbound_critical: AtomicU64,
    pub dropped_inbound_chatter: AtomicU64,
    pub dropped_outbound_chatter: AtomicU64,
    pub dropped_outbound_critical: AtomicU64,
    pub dropped_outbound_body: AtomicU64,
    pub body_must_enqueued: AtomicU64,
    /// WireMsg::batch sends (broadcast + directed) for body-DA digs.
    pub batch_bodies_sent: AtomicU64,
    /// Throttled open-body recovery sends (directed to peer_id).
    pub open_reseed_directed: AtomicU64,
    /// Throttled open-body mesh broadcasts (recovery path only).
    pub open_reseed_mesh: AtomicU64,
    /// Reliability: successful / failed wire writes.
    pub wire_write_ok: AtomicU64,
    pub wire_write_err: AtomicU64,
    /// A2: directed PartialSignature → batch creator.
    pub sig_to_creator_sent: AtomicU64,
    pub sig_to_creator_throttled: AtomicU64,
    /// Last directed sig-to-creator send per batch_hash (rate limit).
    sig_to_creator_last: Arc<Mutex<HashMap<String, Instant>>>,
    /// Recent batch bodies for want-serve without taking the engine lock.
    /// Creator-local open bodies were never pulled under continuous load when
    /// want handlers blocked on engine (DA tick holding with_all_mut).
    body_cache: Arc<Mutex<HashMap<String, Batch>>>,
    body_cache_order: Arc<Mutex<VecDeque<String>>>,
}

impl PeerMesh {
    pub fn new(local_id: String, network_id: u32) -> (Self, InboundRx) {
        Self::new_bounded(local_id, network_id, 65_536)
    }

    pub fn new_bounded(local_id: String, network_id: u32, capacity: usize) -> (Self, InboundRx) {
        // Triple inbound: body must > sig/cert/want > chatter.
        let body_cap = 16_384usize.max(64);
        let crit_cap = 32_768usize.max(64);
        let chat_cap = capacity.max(64);
        let (body_tx, body_rx) = mpsc::channel(body_cap);
        let (crit_tx, crit_rx) = mpsc::channel(crit_cap);
        let (chat_tx, chat_rx) = mpsc::channel(chat_cap);
        (
            Self {
                local_id,
                network_id,
                writers: Arc::new(Mutex::new(Vec::new())),
                peer_addrs: Arc::new(Mutex::new(HashMap::new())),
                dial_targets: Arc::new(Mutex::new(Vec::new())),
                inbound_body_tx: body_tx,
                inbound_critical_tx: crit_tx,
                inbound_chatter_tx: chat_tx,
                seen_batches: Arc::new(Mutex::new(HashSet::new())),
                seen_certs: Arc::new(Mutex::new(HashSet::new())),
                seen_txs: Arc::new(Mutex::new(HashSet::new())),
                pull: Arc::new(Mutex::new(PullTracker::new())),
                dropped_inbound: AtomicU64::new(0),
                dropped_inbound_critical: AtomicU64::new(0),
                dropped_inbound_chatter: AtomicU64::new(0),
                dropped_outbound_chatter: AtomicU64::new(0),
                dropped_outbound_critical: AtomicU64::new(0),
                dropped_outbound_body: AtomicU64::new(0),
                body_must_enqueued: AtomicU64::new(0),
                batch_bodies_sent: AtomicU64::new(0),
                open_reseed_directed: AtomicU64::new(0),
                open_reseed_mesh: AtomicU64::new(0),
                wire_write_ok: AtomicU64::new(0),
                wire_write_err: AtomicU64::new(0),
                sig_to_creator_sent: AtomicU64::new(0),
                sig_to_creator_throttled: AtomicU64::new(0),
                sig_to_creator_last: Arc::new(Mutex::new(HashMap::new())),
                body_cache: Arc::new(Mutex::new(HashMap::new())),
                body_cache_order: Arc::new(Mutex::new(VecDeque::new())),
            },
            InboundMux {
                body: body_rx,
                critical: crit_rx,
                chatter: chat_rx,
            },
        )
    }

    pub async fn mark_batch_seen(&self, h: &str) -> bool {
        self.seen_batches.lock().await.insert(h.to_string())
    }

    /// Cache a batch body for pull/want responses (capped LRU-ish).
    pub async fn cache_batch(&self, batch: Batch) {
        const CAP: usize = 128;
        let h = batch.batch_hash.clone();
        let mut cache = self.body_cache.lock().await;
        let mut order = self.body_cache_order.lock().await;
        if !cache.contains_key(&h) {
            order.push_back(h.clone());
            while order.len() > CAP {
                if let Some(old) = order.pop_front() {
                    cache.remove(&old);
                }
            }
        }
        cache.insert(h, batch);
    }

    pub async fn get_cached_batch(&self, h: &str) -> Option<Batch> {
        self.body_cache.lock().await.get(h).cloned()
    }

    pub fn note_batch_body_sent(&self) {
        self.batch_bodies_sent.fetch_add(1, Ordering::Relaxed);
    }

    pub fn note_open_reseed_directed(&self) {
        self.open_reseed_directed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn note_open_reseed_mesh(&self) {
        self.open_reseed_mesh.fetch_add(1, Ordering::Relaxed);
    }

    /// A2: directed PartialSignature (and optional body) to the batch **creator**.
    /// Rate-limited per `batch_hash` (default 200ms) so reseed noise cannot drown
    /// the creator, while first-sign and periodic re-announce still get through.
    pub async fn try_directed_sig_to_creator(
        &self,
        creator: &str,
        from: &str,
        sig: &PartialSignature,
        body: Option<&Batch>,
        min_interval: Duration,
    ) -> bool {
        if creator.is_empty() || creator == self.local_id {
            return false;
        }
        let h = sig.batch_hash.as_str();
        {
            let mut last = self.sig_to_creator_last.lock().await;
            if let Some(t0) = last.get(h) {
                if t0.elapsed() < min_interval {
                    self.sig_to_creator_throttled
                        .fetch_add(1, Ordering::Relaxed);
                    return false;
                }
            }
            last.insert(h.to_string(), Instant::now());
            // Cap map size (sticky opens under continuous load).
            if last.len() > 4096 {
                let cutoff = Instant::now()
                    .checked_sub(Duration::from_secs(30))
                    .unwrap_or_else(Instant::now);
                last.retain(|_, t| *t >= cutoff);
            }
        }
        // Body first: creator A1 merge path can take foreign sigs from body even
        // if the following PartialSignature frame is reordered/lost.
        // DA-must: body+sig to creator — never skip (PEER_SIGNED path).
        if let Some(b) = body {
            self.cache_batch(b.clone()).await;
            self.send_da_must(creator, &WireMsg::batch(from, b.clone()))
                .await;
        }
        self.send_da_must(creator, &WireMsg::sig(from, sig.clone()))
            .await;
        self.sig_to_creator_sent.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Body-DA / mesh forensic snapshot (RPC metrics).
    pub async fn mesh_stats_json(&self) -> serde_json::Value {
        let peers = self.peer_count().await;
        let cache_len = self.body_cache.lock().await.len();
        let pull = self.pull.lock().await.stats_json();
        serde_json::json!({
            "p2p_peers": peers,
            "dropped_inbound": self.dropped_inbound.load(Ordering::Relaxed),
            "dropped_inbound_critical": self.dropped_inbound_critical.load(Ordering::Relaxed),
            "dropped_inbound_chatter": self.dropped_inbound_chatter.load(Ordering::Relaxed),
            "dropped_outbound_critical": self.dropped_outbound_critical.load(Ordering::Relaxed),
            "dropped_outbound_body": self.dropped_outbound_body.load(Ordering::Relaxed),
            "body_must_enqueued": self.body_must_enqueued.load(Ordering::Relaxed),
            "dropped_outbound_chatter": self.dropped_outbound_chatter.load(Ordering::Relaxed),
            "batch_bodies_sent": self.batch_bodies_sent.load(Ordering::Relaxed),
            "open_reseed_directed": self.open_reseed_directed.load(Ordering::Relaxed),
            "open_reseed_mesh": self.open_reseed_mesh.load(Ordering::Relaxed),
            "wire_write_ok": self.wire_write_ok.load(Ordering::Relaxed),
            "wire_write_err": self.wire_write_err.load(Ordering::Relaxed),
            "sig_to_creator_sent": self.sig_to_creator_sent.load(Ordering::Relaxed),
            "sig_to_creator_throttled": self.sig_to_creator_throttled.load(Ordering::Relaxed),
            "body_cache_len": cache_len,
            "pull": pull,
        })
    }


    pub async fn mark_cert_seen(&self, h: &str) -> bool {
        self.seen_certs.lock().await.insert(h.to_string())
    }

    pub async fn mark_tx_seen(&self, h: &str) -> bool {
        self.seen_txs.lock().await.insert(h.to_string())
    }

    pub async fn has_seen_batch(&self, h: &str) -> bool {
        self.seen_batches.lock().await.contains(h)
    }

    pub async fn has_seen_cert(&self, h: &str) -> bool {
        self.seen_certs.lock().await.contains(h)
    }

    pub async fn note_peer_id(&self, peer_id: &str, addr: &str) {
        // Reliability: dual-dial maps peer to listen vs ephemeral. Prefer a
        // configured dial target (listen port) that still has a live writer.
        let live: Vec<String> = {
            let writers = self.writers.lock().await;
            writers.iter().map(|p| p.addr.clone()).collect()
        };
        let dials = self.dial_targets.lock().await.clone();
        let new_is_dial = dials.iter().any(|d| d == addr);
        let new_live = live.iter().any(|a| a == addr);
        let mut map = self.peer_addrs.lock().await;
        if let Some(cur) = map.get(peer_id) {
            if cur == addr {
                return;
            }
            let cur_is_dial = dials.iter().any(|d| d == cur);
            let cur_live = live.iter().any(|a| a == cur);
            // Never replace a live dial-target mapping with ephemeral.
            if cur_live && cur_is_dial && !new_is_dial {
                return;
            }
            if cur_live && !new_live {
                return;
            }
            // Upgrade ephemeral -> dial target when available.
            if !cur_is_dial && new_is_dial && new_live {
                map.insert(peer_id.to_string(), addr.to_string());
                return;
            }
            if cur_live {
                return;
            }
        }
        if new_live || !map.contains_key(peer_id) {
            map.insert(peer_id.to_string(), addr.to_string());
        }
    }

    /// Validator ids we have mapped to a dial address (for directed pull).
    pub async fn peer_ids(&self) -> Vec<String> {
        self.peer_addrs.lock().await.keys().cloned().collect()
    }


    pub async fn listen(self: &Arc<Self>, addr: SocketAddr) -> Result<()> {
        let listener = TcpListener::bind(addr).await.context("p2p bind")?;
        info!("p2p listen {} (pull-path inv/want)", addr);
        let mesh = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer)) => {
                        let m = Arc::clone(&mesh);
                        tokio::spawn(async move {
                            if let Err(e) = m.handle_stream(stream, peer.to_string()).await {
                                debug!("p2p inbound {}: {e}", peer);
                            }
                        });
                    }
                    Err(e) => {
                        warn!("p2p accept error: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                }
            }
        });
        Ok(())
    }

    pub fn dial_peers(self: &Arc<Self>, peers: Vec<SocketAddr>) {
        let targets: Vec<String> = peers.iter().map(|a| a.to_string()).collect();
        // Best-effort sync seed of dial targets (try_lock; dial tasks also refresh).
        if let Ok(mut g) = self.dial_targets.try_lock() {
            *g = targets.clone();
        } else {
            let mesh0 = Arc::clone(self);
            tokio::spawn(async move {
                *mesh0.dial_targets.lock().await = targets;
            });
        }
        for addr in peers {
            let mesh = Arc::clone(self);
            tokio::spawn(async move {
                loop {
                    match TcpStream::connect(addr).await {
                        Ok(stream) => {
                            info!("p2p connected {}", addr);
                            if let Err(e) = mesh.handle_stream(stream, addr.to_string()).await {
                                debug!("p2p outbound {}: {e}", addr);
                            }
                        }
                        Err(e) => debug!("p2p dial {} failed: {e}", addr),
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            });
        }
    }

    async fn push_inbound(&self, msg: WireMsg) {
        // CRITICAL: never .await long sends on the TCP read path.
        // Blocking here freezes the peer reader → peer write buffers fill →
        // body "must" send blocks DA tick → cluster freeze (BODY_BUT_NO_PEER_SIG).
        // Body/critical: try_send; if full, spawn enqueue so reader keeps running.
        if msg.msg_type == "batch" {
            match self.inbound_body_tx.try_send(msg) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(m)) => {
                    let tx = self.inbound_body_tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(m).await;
                    });
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.dropped_inbound.fetch_add(1, Ordering::Relaxed);
                    self.dropped_inbound_critical
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
            return;
        }
        if is_da_critical(&msg.msg_type) {
            match self.inbound_critical_tx.try_send(msg) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(m)) => {
                    let tx = self.inbound_critical_tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(m).await;
                    });
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.dropped_inbound.fetch_add(1, Ordering::Relaxed);
                    self.dropped_inbound_critical
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
            return;
        }
        match self.inbound_chatter_tx.try_send(msg) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped_inbound.fetch_add(1, Ordering::Relaxed);
                self.dropped_inbound_chatter.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
    }

    async fn handle_stream(&self, stream: TcpStream, name: String) -> Result<()> {
        let (read, write) = stream.into_split();
        // Outbound 3-tier: body (must) > sig/cert/want > chatter.
        let (body_tx, mut body_rx) = mpsc::channel::<Vec<u8>>(512);
        let (crit_tx, mut crit_rx) = mpsc::channel::<Vec<u8>>(2048);
        let (chat_tx, mut chat_rx) = mpsc::channel::<Vec<u8>>(1024);
        let mut write = write;
        let writer_name = name.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    b = body_rx.recv() => {
                        match b {
                            Some(data) => {
                                if write.write_all(&data).await.is_err() {
                                    break;
                                }
                                let _ = write.flush().await;
                            }
                            None => {
                                // body closed — drain crit then chat
                                while let Some(data) = crit_rx.recv().await {
                                    if write.write_all(&data).await.is_err() {
                                        return;
                                    }
                                    let _ = write.flush().await;
                                }
                                while let Some(data) = chat_rx.recv().await {
                                    if write.write_all(&data).await.is_err() {
                                        return;
                                    }
                                }
                                break;
                            }
                        }
                    }
                    c = crit_rx.recv() => {
                        match c {
                            Some(data) => {
                                if write.write_all(&data).await.is_err() {
                                    break;
                                }
                                let _ = write.flush().await;
                            }
                            None => {
                                // keep body+chat until all closed handled below via chat arm
                                while let Some(data) = body_rx.recv().await {
                                    if write.write_all(&data).await.is_err() {
                                        return;
                                    }
                                    let _ = write.flush().await;
                                }
                                while let Some(data) = chat_rx.recv().await {
                                    if write.write_all(&data).await.is_err() {
                                        return;
                                    }
                                }
                                break;
                            }
                        }
                    }
                    ch = chat_rx.recv() => {
                        match ch {
                            Some(data) => {
                                if write.write_all(&data).await.is_err() {
                                    break;
                                }
                            }
                            None => {
                                while let Some(data) = body_rx.recv().await {
                                    if write.write_all(&data).await.is_err() {
                                        return;
                                    }
                                    let _ = write.flush().await;
                                }
                                while let Some(data) = crit_rx.recv().await {
                                    if write.write_all(&data).await.is_err() {
                                        return;
                                    }
                                    let _ = write.flush().await;
                                }
                                break;
                            }
                        }
                    }
                }
            }
            debug!("p2p writer exit {}", writer_name);
        });
        {
            let mut w = self.writers.lock().await;
            w.retain(|p| p.addr != name);
            w.push(PeerWriter {
                addr: name.clone(),
                body_tx,
                critical_tx: crit_tx,
                chatter_tx: chat_tx,
            });
        }
        self.send_to_addr(&name, &WireMsg::hello(&self.local_id, self.network_id))
            .await;

        let mut reader = BufReader::new(read);
        loop {
            let mut first = [0u8; 1];
            let n = reader.read(&mut first).await?;
            if n == 0 {
                break;
            }
            // ALWAYS length-prefixed (u32 LE + JSON). Never branch on first byte looking
            // like JSON: body frames with len%256 in {0x09,0x20,0x7b} were mis-read as
            // line-JSON, desynced the TCP stream, and peers stopped ingest_new forever
            // (dig: NO_BODY_ON_PEERS while body_must_enqueued climbed, drop_body=0).
            let mut len_buf = [0u8; 4];
            len_buf[0] = first[0];
            reader.read_exact(&mut len_buf[1..]).await?;
            let len = u32::from_le_bytes(len_buf) as usize;
            if len == 0 || len > 16 * 1024 * 1024 {
                continue;
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).await?;
            let msg: WireMsg = match serde_json::from_slice(&body) {
                Ok(m) => m,
                Err(_) => continue,
            };

            if let Some(ref pid) = msg.from {
                self.note_peer_id(pid, &name).await;
            }
            if msg.msg_type == "hello" {
                if let Some(nid) = msg.network_id {
                    if nid != self.network_id {
                        warn!("peer {} network_id {} != {}", name, nid, self.network_id);
                    }
                }
                if let Some(ref pid) = msg.from {
                    self.note_peer_id(pid, &name).await;
                }
                continue;
            }
            if msg.from.as_deref() == Some(self.local_id.as_str()) {
                continue;
            }
            self.push_inbound(msg).await;
        }
        self.writers.lock().await.retain(|p| p.addr != name);
        Ok(())
    }

    async fn send_to_addr(&self, addr: &str, msg: &WireMsg) {
        let data = match msg.encode_frame() {
            Ok(d) => d,
            Err(_) => return,
        };
        let is_body = msg.msg_type == "batch";
        if is_body {
            self.note_batch_body_sent();
        }
        let critical = is_da_critical(&msg.msg_type);
        // Clone sender without holding lock across await.
        // Body uses dedicated highest-priority lane.
        let tx = {
            let writers = self.writers.lock().await;
            writers.iter().find(|p| p.addr == addr).map(|p| {
                if is_body {
                    p.body_tx.clone()
                } else if critical {
                    p.critical_tx.clone()
                } else {
                    p.chatter_tx.clone()
                }
            })
        };
        let Some(tx) = tx else {
            return;
        };
        if is_body || critical {
            // Prefer enqueue; if full wait briefly then give up so DA tick lives.
            // Infinite await here deadlocked the cluster when peer TCP window=0.
            const BODY_ENQ_MS: u64 = 200;
            const CRIT_ENQ_MS: u64 = 500;
            let wait = if is_body { BODY_ENQ_MS } else { CRIT_ENQ_MS };
            match tx.try_send(data) {
                Ok(()) => {
                    self.wire_write_ok.fetch_add(1, Ordering::Relaxed);
                    if is_body {
                        self.body_must_enqueued.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(mpsc::error::TrySendError::Full(d)) => {
                    match tokio::time::timeout(
                        std::time::Duration::from_millis(wait),
                        tx.send(d),
                    )
                    .await
                    {
                        Ok(Ok(())) => {
                            self.wire_write_ok.fetch_add(1, Ordering::Relaxed);
                            if is_body {
                                self.body_must_enqueued.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        _ => {
                            self.wire_write_err.fetch_add(1, Ordering::Relaxed);
                            if is_body {
                                self.dropped_outbound_body
                                    .fetch_add(1, Ordering::Relaxed);
                            } else {
                                self.dropped_outbound_critical
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.wire_write_err.fetch_add(1, Ordering::Relaxed);
                }
            }
        } else {
            match tx.try_send(data) {
                Ok(()) => {
                    self.wire_write_ok.fetch_add(1, Ordering::Relaxed);
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.dropped_outbound_chatter
                        .fetch_add(1, Ordering::Relaxed);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.wire_write_err.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// Directed send by validator id.
    /// Reliability: dual-dial can map peer_id → inbound ephemeral; if that writer
    /// is gone, write would silently no-op. Fall back to any live writer for that
    /// id if we can, else mesh broadcast so body/sig/cert still land.
    pub async fn send_to_peer(&self, peer_id: &str, msg: &WireMsg) {
        let addr = self.peer_addrs.lock().await.get(peer_id).cloned();
        if let Some(ref a) = addr {
            let has = {
                let writers = self.writers.lock().await;
                writers.iter().any(|p| p.addr == *a)
            };
            if has {
                self.send_to_addr(a, msg).await;
                return;
            }
        }
        // Stale map: full mesh so body/sig/cert still land.
        self.broadcast(msg).await;
    }

    pub async fn broadcast(&self, msg: &WireMsg) {
        let data = match msg.encode_frame() {
            Ok(d) => d,
            Err(e) => {
                warn!("encode wire msg: {e}");
                return;
            }
        };
        if msg.msg_type == "batch" {
            self.note_batch_body_sent();
        }
        let is_body = msg.msg_type == "batch";
        let critical = is_da_critical(&msg.msg_type);
        // Body: ALL live body lanes (every dual-dial path).
        let txs: Vec<mpsc::Sender<Vec<u8>>> = {
            let writers = self.writers.lock().await;
            writers
                .iter()
                .map(|p| {
                    if is_body {
                        p.body_tx.clone()
                    } else if critical {
                        p.critical_tx.clone()
                    } else {
                        p.chatter_tx.clone()
                    }
                })
                .collect()
        };
        for tx in txs {
            if is_body || critical {
                const BODY_ENQ_MS: u64 = 200;
                const CRIT_ENQ_MS: u64 = 500;
                let wait = if is_body { BODY_ENQ_MS } else { CRIT_ENQ_MS };
                match tx.try_send(data.clone()) {
                    Ok(()) => {
                        self.wire_write_ok.fetch_add(1, Ordering::Relaxed);
                        if is_body {
                            self.body_must_enqueued.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(mpsc::error::TrySendError::Full(d)) => {
                        match tokio::time::timeout(
                            std::time::Duration::from_millis(wait),
                            tx.send(d),
                        )
                        .await
                        {
                            Ok(Ok(())) => {
                                self.wire_write_ok.fetch_add(1, Ordering::Relaxed);
                                if is_body {
                                    self.body_must_enqueued
                                        .fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            _ => {
                                self.wire_write_err.fetch_add(1, Ordering::Relaxed);
                                if is_body {
                                    self.dropped_outbound_body
                                        .fetch_add(1, Ordering::Relaxed);
                                } else {
                                    self.dropped_outbound_critical
                                        .fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        self.wire_write_err.fetch_add(1, Ordering::Relaxed);
                    }
                }
            } else {
                match tx.try_send(data.clone()) {
                    Ok(()) => {
                        self.wire_write_ok.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        self.dropped_outbound_chatter
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        self.wire_write_err.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    /// DA-must directed send: body/sig/cert/want only. Blocks until enqueued.
    /// Use for open completion path — never skip.
    pub async fn send_da_must(&self, peer_id: &str, msg: &WireMsg) {
        debug_assert!(
            is_da_must(&msg.msg_type) || msg.msg_type == "hello",
            "send_da_must for non-DA type {}",
            msg.msg_type
        );
        self.send_to_peer(peer_id, msg).await;
    }

    /// DA-must mesh flood: body/sig/cert. Blocks until enqueued on every live peer.
    pub async fn broadcast_da_must(&self, msg: &WireMsg) {
        debug_assert!(
            is_da_must(&msg.msg_type) || msg.msg_type == "hello",
            "broadcast_da_must for non-DA type {}",
            msg.msg_type
        );
        self.broadcast(msg).await;
    }

    /// Body-only must-deliver mesh flood (highest wire priority).
    pub async fn broadcast_body_must(&self, batch_from: &str, batch: Batch) {
        self.cache_batch(batch.clone()).await;
        self.broadcast_da_must(&WireMsg::batch(batch_from, batch)).await;
    }

    /// Body-only must-deliver directed (highest wire priority).
    pub async fn send_body_must(&self, peer_id: &str, batch_from: &str, batch: Batch) {
        self.cache_batch(batch.clone()).await;
        self.send_da_must(peer_id, &WireMsg::batch(batch_from, batch))
            .await;
    }

    /// Seed open body to all peers: one mesh + directed + inv (body lane only).
    /// No double-mesh flood (that filled body queues and starved progress).
    pub async fn seed_body_must_all(&self, from: &str, batch: Batch) {
        let h = batch.batch_hash.clone();
        self.cache_batch(batch.clone()).await;
        // Mode A: body only on dial-target writers (broadcast) + one directed per peer.
        self.broadcast_da_must(&WireMsg::batch(from, batch.clone()))
            .await;
        let peer_ids = self.peer_ids().await;
        for peer in &peer_ids {
            self.send_da_must(peer, &WireMsg::batch(from, batch.clone()))
                .await;
        }
        // inv so peers can want if body still missing
        self.broadcast(&WireMsg::inv(from, "batch", vec![h])).await;
    }

    pub async fn peer_count(&self) -> usize {
        self.writers.lock().await.len()
    }

    pub async fn pull_stats_json(&self) -> serde_json::Value {
        self.pull.lock().await.stats_json()
    }

    /// Drain pull tracker and send want messages.
    pub async fn flush_wants(&self) {
        let wants = self.pull.lock().await.drain_wants();
        for (peer, kind, hashes) in wants {
            if hashes.is_empty() {
                continue;
            }
            self.send_to_peer(
                &peer,
                &WireMsg::want(&self.local_id, kind.as_str(), hashes),
            )
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn two_node_exchange_tx() {
        let (m1, mut _rx1) = PeerMesh::new("val1".into(), 9002);
        let (m2, mut rx2) = PeerMesh::new("val2".into(), 9002);
        let m1 = Arc::new(m1);
        let m2 = Arc::new(m2);

        m1.listen("127.0.0.1:29111".parse().unwrap()).await.unwrap();
        m2.listen("127.0.0.1:29112".parse().unwrap()).await.unwrap();
        m1.dial_peers(vec!["127.0.0.1:29112".parse().unwrap()]);
        m2.dial_peers(vec!["127.0.0.1:29111".parse().unwrap()]);

        for _ in 0..50 {
            if m1.peer_count().await > 0 && m2.peer_count().await > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let tx = Transaction::create("acct1", 1, "acct2", 1, 10, 0.0);
        m1.broadcast(&WireMsg::tx("val1", tx.clone())).await;
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), rx2.recv())
            .await
            .expect("timeout")
            .expect("channel");
        assert_eq!(got.msg_type, "tx");
        assert_eq!(got.tx.unwrap().tx_id, tx.tx_id);
    }

    #[test]
    fn pull_tracker_want_and_body() {
        let mut t = PullTracker::new();
        t.on_inv(PullKind::Batch, "h1", "val2", false);
        let wants = t.drain_wants();
        assert_eq!(wants.len(), 1);
        assert_eq!(wants[0].0, "val2");
        assert_eq!(wants[0].2, vec!["h1".to_string()]);
        t.on_body(PullKind::Batch, "h1");
        assert_eq!(t.pending_count(), 0);
        assert_eq!(t.inflight_total(), 0);
        assert!(t.drain_wants().is_empty());
    }

    #[test]
    fn pull_tracker_frees_inflight_on_timeout_and_retry() {
        let mut t = PullTracker::new();
        t.request_timeout = Duration::from_millis(1);
        t.on_inv(PullKind::Batch, "h1", "val2", false);
        t.on_inv(PullKind::Batch, "h1", "val3", false);
        let wants = t.drain_wants();
        assert_eq!(wants.len(), 1);
        assert_eq!(t.inflight_total(), 1);
        // Still in timeout window → no re-want
        assert!(t.drain_wants().is_empty());
        std::thread::sleep(Duration::from_millis(5));
        // Timed out → free inflight, retry (possibly different announcer)
        let wants2 = t.drain_wants();
        assert_eq!(wants2.len(), 1);
        assert_eq!(t.inflight_total(), 1, "exactly one inflight after retry");
        t.on_body(PullKind::Batch, "h1");
        assert_eq!(t.pending_count(), 0);
        assert_eq!(t.inflight_total(), 0, "body delivery frees inflight");
    }

    #[test]
    fn pull_tracker_resets_after_max_retries_keeps_pending() {
        // Multipack: never permanently abandon a needed body pull.
        let mut t = PullTracker::new();
        t.request_timeout = Duration::from_millis(1);
        t.max_retries = 1;
        t.on_inv(PullKind::Batch, "h1", "val2", false);
        let w1 = t.drain_wants();
        assert_eq!(w1.len(), 1);
        std::thread::sleep(Duration::from_millis(2));
        // timeout frees inflight, retries=1 == max → will reset on next timeout cycle
        let _w2 = t.drain_wants();
        std::thread::sleep(Duration::from_millis(2));
        let _w3 = t.drain_wants();
        assert_eq!(
            t.pending_count(),
            1,
            "pending must survive max_retries via reset"
        );
    }

}
