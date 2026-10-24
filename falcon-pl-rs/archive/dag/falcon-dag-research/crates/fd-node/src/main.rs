//! falcon-dagd — Rust Falcon DAG node (phase 6: wallet CLI + Prometheus metrics).

use anyhow::{bail, Context, Result};
use clap::Parser;
use fd_crypto::{
    make_crypto_for_validator, make_wallet, Falcon512KeyRing, Falcon512Wallet,
};
use fd_net::{PeerMesh, PullKind, WireMsg};
use fd_node::{engine::GossipEvents, EngineParts, NodeEngine, SubmitResult};
use fd_store;
use fd_types::{
    architecture_contract_json, BatchDaGapClass, BridgeProof, NodeProfile, NodeRole,
    ProtocolConfig, Transaction, ARCHITECTURE_STRING,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "falcon-dagd", about = "Falcon DAG Rust node")]
struct Args {
    /// Validator id (e.g. val1)
    #[arg(long, default_value = "val1")]
    id: String,
    /// RPC listen address
    #[arg(long, default_value = "127.0.0.1:18001")]
    rpc: String,
    /// P2P listen address (empty = P2P off)
    #[arg(long, default_value = "")]
    p2p: String,
    /// Comma-separated peer addresses host:port
    #[arg(long, default_value = "")]
    peers: String,
    /// Comma-separated UNL ids
    #[arg(long, default_value = "val1,val2,val3,val4,val5")]
    unl: String,
    /// Worker index
    #[arg(long, default_value_t = 0)]
    worker_index: usize,
    /// Worker count
    #[arg(long, default_value_t = 5)]
    worker_count: usize,
    /// Network id
    #[arg(long, default_value_t = 9002)]
    network_id: u32,
    /// Batch interval seconds (packing / "close" cadence for local batches)
    #[arg(long, default_value_t = 0.05)]
    batch_interval: f64,
    /// Max transactions packed into one local batch
    #[arg(long, default_value_t = 256)]
    max_batch_txs: usize,
    /// Open sequence window per account (mempool admit range)
    #[arg(long, default_value_t = 512)]
    sequence_window: u64,
    /// Enable adaptive batch interval (low mempool → slower, high → faster)
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    adaptive_batch: bool,
    /// Enable worker sharding (only for multi-worker same-validator setups).
    /// Default off: every UNL validator may pack any ready tx (correct multi-node mode).
    #[arg(long, default_value_t = false)]
    worker_sharding: bool,
    /// UNL certificate threshold (fraction of weight). 0.6 ≈ 3/5 equal vals; 0.8 ≈ 4/5.
    #[arg(long, default_value_t = 0.6)]
    unl_threshold: f64,
    /// Max parents per batch (3 = real DAG width; 1 = linear tip chain)
    #[arg(long, default_value_t = 3)]
    max_batch_parents: usize,
    /// Narwhal role: da-worker | order-exec | full
    /// da-worker = pack/sign/cert only; order-exec = order certs + execute only; full = both (separate paths)
    #[arg(long, default_value = "full")]
    role: String,
    /// Operator deployment profile: validator | archival | rpc (orthogonal to --role).
    /// PR1 freezes profile names; use --force-node-profile to apply profile knob defaults.
    #[arg(long, default_value = "validator")]
    node_profile: String,
    /// When set, apply profile-suggested defaults (retention, admit cap, multipack knobs).
    #[arg(long, default_value_t = false)]
    force_node_profile: bool,
    /// Crypto scheme: hmac-sha256-dev | falcon-512
    #[arg(long, default_value = "hmac-sha256-dev")]
    crypto: String,
    /// Directory with unl_public.json + {id}.json (required for multi-node falcon-512)
    #[arg(long, default_value = "")]
    keys_dir: String,
    /// Generate Falcon-512 UNL keys into --keys-dir and exit
    #[arg(long, default_value_t = false)]
    gen_keys: bool,
    /// Data directory for snapshot.json (empty = no persist)
    #[arg(long, default_value = "")]
    data_dir: String,
    /// Snapshot interval seconds (0 = only on RPC snapshot method / disabled periodic)
    #[arg(long, default_value_t = 5.0)]
    snapshot_interval: f64,
    /// Require wallet signatures on user transactions
    #[arg(long, default_value_t = true)]
    require_tx_sigs: bool,
    /// Auto-sign unsigned faucet submits with local wallet (regtest convenience)
    #[arg(long, default_value_t = true)]
    auto_sign: bool,
    /// Disable faucet auto-sign (clients must pre-sign with falcon-dag-wallet)
    #[arg(long, default_value_t = false)]
    no_auto_sign: bool,
    /// Wallet key directory (Falcon wallet_public.json + acctN.json)
    #[arg(long, default_value = "")]
    wallet_dir: String,
    /// Generate Falcon-512 faucet wallet keys into --wallet-dir and exit
    #[arg(long, default_value_t = false)]
    gen_wallet_keys: bool,
    /// Number of faucet accounts when generating wallet keys
    #[arg(long, default_value_t = 20)]
    faucet_accounts: u32,
    /// Optional packer skip-hole after N seconds (0 = disabled). Abandons missing
    /// sequences so packing/apply can pass aged holes (Phase 49c liveness).
    #[arg(long, default_value_t = 0.0)]
    pack_skip_hole_after: f64,
    /// Max open uncertified batches per creator (DA pipeline depth).
    #[arg(long, default_value_t = 4)]
    max_uncertified_per_creator: usize,
    /// Own open batches while multipacking. 0 = auto (1 for multipack). Dig hypothesis: raise to 2+.
    #[arg(long, default_value_t = 0)]
    pack_open_cap: usize,
    /// When true (default), only sorted_unl[0] packs. Set false for multi-packer tests.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    single_packer: bool,
    /// Adaptive batch interval floor (seconds).
    #[arg(long, default_value_t = 0.02)]
    batch_interval_min: f64,
    /// Adaptive batch interval ceiling (seconds).
    #[arg(long, default_value_t = 0.4)]
    batch_interval_max: f64,
    /// Adaptive: mempool depth treated as "low" (slow packing).
    #[arg(long, default_value_t = 16)]
    adaptive_mempool_low: usize,
    /// Adaptive: mempool depth treated as "high" (fast packing / backlog force).
    #[arg(long, default_value_t = 256)]
    adaptive_mempool_high: usize,
    /// Pack-ahead cap: refuse to pack if DA cursor is this many sequences ahead of exec_next.
    /// 0 = disabled. Recommended: 32 for multi-pack (prevents cursor racing past exec holes).
    #[arg(long, default_value_t = 0)]
    pack_ahead_cap: u64,
    /// Uncertified batch TTL seconds. 0 = never prune open batches (default).
    #[arg(long, default_value_t = 0.0)]
    uncertified_ttl: f64,
    /// Pack/check half-cycle seconds (default 0.25 → 2 pack windows/s).
    #[arg(long, default_value_t = 0.25)]
    pack_check_slot: f64,
    /// Enable conf-pool + pack/check duty (default true).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    conf_pool: bool,
    /// Abandon own open batches with 0 foreign UNL sigs after N seconds and
    /// re-insert txs into mempool (0 = disabled). Default 2.5s.
    #[arg(long, default_value_t = 3.0)]
    open_body_repool_after: f64,
    /// Admit-ahead backlog cap (local_admits_ok - executed). 0 = default 64.
    /// Raise for flood tests so submit is not terBUSY while DA catches up.
    #[arg(long, default_value_t = 0)]
    admit_ahead_cap: u64,
    /// Reject local admits when any uncertified batch is older than this (s).
    /// 0 = disable age trip. Default 8 can kill steady TPS under multipack.
    #[arg(long, default_value_t = -1.0)]
    admit_busy_uncert_age: f64,
    /// PR-G4: Global mempool URL (e.g. http://192.0.2.1:18500). Empty = disabled.
    #[arg(long, default_value = "")]
    gmp_url: String,
    /// PR-G4: max txs to lease per pull.
    #[arg(long, default_value_t = 4)]
    gmp_lease_max: usize,
    /// PR-G4: advertised memory budget (MB) for capacity-weighted leases.
    #[arg(long, default_value_t = 4096)]
    gmp_budget_mb: u64,
    /// PR-G4: max in-flight leased txs for this validator.
    #[arg(long, default_value_t = 32)]
    gmp_max_in_flight: u64,
    /// PR-G4: lease soft-affinity to owned lanes (fnv % worker_count == worker_index).
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    gmp_affinity: bool,
    /// PR-G5 light: GCP registry URL (optional). Empty = disabled.
    #[arg(long, default_value = "")]
    gcp_url: String,
    /// Multipack packer set (comma-separated). May include non-UNL pack workers.
    /// Empty = all UNL members pack (legacy). Cert weight still only from --unl.
    /// Example: --unl val1,val2,val3 --packers val1,val2,val3,pack1,pack2,pack3
    #[arg(long, default_value = "")]
    packers: String,
}



#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let unl: Vec<String> = args
        .unl
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if args.gen_keys {
        if args.keys_dir.is_empty() {
            bail!("--gen-keys requires --keys-dir");
        }
        let ring = Falcon512KeyRing::generate_unl(unl.iter().cloned())?;
        ring.save_dir(&args.keys_dir)?;
        println!(
            "wrote Falcon-512 keys for {} validators to {}",
            unl.len(),
            args.keys_dir
        );
        return Ok(());
    }

    if args.gen_wallet_keys {
        if args.wallet_dir.is_empty() {
            bail!("--gen-wallet-keys requires --wallet-dir");
        }
        let w = Falcon512Wallet::faucet(args.faucet_accounts);
        w.save_dir(&args.wallet_dir)?;
        println!(
            "wrote Falcon-512 faucet wallet for {} accounts to {}",
            args.faucet_accounts, args.wallet_dir
        );
        return Ok(());
    }

    let mut config = ProtocolConfig::default();
    config.network_id = args.network_id;
    config.crypto_scheme = args.crypto.clone();
    config.worker_sharding = args.worker_sharding;
    config.shard_orphan_age_s = if args.worker_sharding { 0.25 } else { 0.0 };
    config.target_batch_interval_s = args.batch_interval;
    config.max_batch_txs = args.max_batch_txs.max(1);
    config.max_batch_parents = args.max_batch_parents.max(1);
    config.account_sequence_window = args.sequence_window.max(1);
    config.adaptive_interval = args.adaptive_batch;
    config.unl_threshold = args.unl_threshold.clamp(0.51, 1.0);
    config.require_tx_sigs = args.require_tx_sigs;
    config.pack_skip_hole_after_s = args.pack_skip_hole_after.max(0.0);
    config.max_uncertified_per_creator = args.max_uncertified_per_creator.max(1);
    config.pack_open_cap = args.pack_open_cap;
    config.single_packer_per_round = args.single_packer;
    config.batch_interval_min_s = args.batch_interval_min.max(0.001);
    config.batch_interval_max_s = args.batch_interval_max.max(config.batch_interval_min_s);
    config.adaptive_mempool_low = args.adaptive_mempool_low.max(1);
    config.adaptive_mempool_high = args.adaptive_mempool_high.max(config.adaptive_mempool_low);
    config.pack_ahead_cap = args.pack_ahead_cap;
    config.uncertified_ttl_s = args.uncertified_ttl.max(0.0);
    config.pack_check_slot_s = args.pack_check_slot.max(0.05);
    config.conf_pool_enabled = args.conf_pool;
    config.open_body_repool_after_s = args.open_body_repool_after.max(0.0);
    if args.admit_ahead_cap > 0 {
        config.admit_ahead_cap = args.admit_ahead_cap;
    }
    if args.admit_busy_uncert_age >= 0.0 {
        config.admit_busy_uncert_age_s = args.admit_busy_uncert_age;
    }
    let node_profile = NodeProfile::parse(&args.node_profile).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid --node-profile {} (use validator | archival | rpc)",
            args.node_profile
        )
    })?;
    config.apply_node_profile(node_profile, args.force_node_profile);

    let keys_path = if args.keys_dir.is_empty() {
        None
    } else {
        Some(PathBuf::from(&args.keys_dir))
    };

    if args.crypto.starts_with("falcon") && keys_path.is_none() && args.worker_count > 1 {
        warn!(
            "falcon-512 without --keys-dir generates ephemeral keys (single-process only)"
        );
    }

    let crypto = make_crypto_for_validator(
        &args.crypto,
        &unl,
        &args.id,
        keys_path.as_deref(),
    )
    .context("crypto init")?;

    let mut engine_inner = NodeEngine::with_crypto(
        args.id.clone(),
        unl.clone(),
        config,
        args.worker_index,
        args.worker_count,
        crypto,
    );
    let role = NodeRole::parse(&args.role).ok_or_else(|| {
        anyhow::anyhow!("invalid --role {} (use da-worker | order-exec | full)", args.role)
    })?;
    engine_inner.set_role(role);
    let packer_ids: Vec<String> = args
        .packers
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !packer_ids.is_empty() {
        engine_inner.set_packer_ids(packer_ids.clone());
        info!(
            "packer set (multipack lanes)={:?} unl_cert={:?} local_is_packer={} local_is_unl={}",
            packer_ids,
            unl,
            engine_inner.is_packer(),
            engine_inner.is_unl_member()
        );
    }
    info!(
        "narwhal role={} node_profile={} runs_da={} runs_ledger={} is_unl={} is_packer={}",
        role.as_str(),
        node_profile.as_str(),
        role.runs_da(),
        role.runs_ledger(),
        engine_inner.is_unl_member(),
        engine_inner.is_packer(),
    );

    let wallet_path = if args.wallet_dir.is_empty() {
        None
    } else {
        Some(PathBuf::from(&args.wallet_dir))
    };
    let wallet = make_wallet(&args.crypto, wallet_path.as_deref()).context("wallet init")?;
    let auto_sign = args.auto_sign && !args.no_auto_sign;
    engine_inner.set_wallet(wallet, auto_sign);

    let data_dir = if args.data_dir.is_empty() {
        None
    } else {
        let p = PathBuf::from(&args.data_dir);
        std::fs::create_dir_all(&p).context("create data_dir")?;
        match engine_inner.load_snapshot_file(&p) {
            Ok(true) => info!(
                "loaded snapshot from {} (executed={})",
                p.join("snapshot.json").display(),
                engine_inner.execution.executed_txs.len()
            ),
            Ok(false) => info!("no snapshot at {}, starting fresh", p.display()),
            Err(e) => warn!("snapshot load failed: {e}"),
        }
        Some(p)
    };

    // Hub: subsystem locks (mempool|dag|ordering|execution). No outer mutex.
    // DA tick / ledger tick / inbound use EngineParts concurrently; RPC uses with_engine.
    let engine = EngineParts::from_sequential(engine_inner);

    let mesh: Option<Arc<PeerMesh>> = if !args.p2p.is_empty() {
        // Phase C: bounded inbound queue (backpressure / drop under flood).
        let (mesh, inbound) = PeerMesh::new_bounded(args.id.clone(), args.network_id, 65_536);
        let mesh = Arc::new(mesh);
        let p2p_addr: SocketAddr = args.p2p.parse()?;
        mesh.listen(p2p_addr).await?;
        let peers: Vec<SocketAddr> = args
            .peers
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        mesh.dial_peers(peers);

        let eng = engine.clone();
        let m = mesh.clone();
        tokio::spawn(async move {
            handle_inbound(eng, m, inbound).await;
        });
        let m_pull = mesh.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                m_pull.flush_wants().await;
            }
        });
        Some(mesh)
    } else {
        None
    };

    // PR-G4: in-engine GMP lease agent (paused via RPC gmp_control for digs).
    let gmp_enabled = Arc::new(std::sync::atomic::AtomicBool::new(!args.gmp_url.is_empty()));
    let gmp_stats = Arc::new(parking_lot::Mutex::new(serde_json::json!({
        "enabled": !args.gmp_url.is_empty(),
        "url": args.gmp_url,
        "leases_ok": 0u64,
        "leases_empty": 0u64,
        "admit_ok": 0u64,
        "admit_err": 0u64,
        "complete_ok": 0u64,
        "nack": 0u64,
        "last_error": null,
    })));
    // PR-G5: shared GCP registry client (pack/sig/cert publish).
    let gcp_client: Option<Arc<fd_gmp::GcpClient>> = if args.gcp_url.is_empty() {
        None
    } else {
        match fd_gmp::GcpClient::parse(&args.gcp_url) {
            Ok(c) => {
                info!("PR-G5 GCP client enabled url={}", args.gcp_url);
                Some(Arc::new(c))
            }
            Err(e) => {
                warn!("invalid --gcp-url {}: {e:#}", args.gcp_url);
                None
            }
        }
    };
    let gcp_stats = Arc::new(parking_lot::Mutex::new(serde_json::json!({
        "enabled": gcp_client.is_some(),
        "url": args.gcp_url,
        "batches": 0u64,
        "sigs": 0u64,
        "certs": 0u64,
        "errors": 0u64,
        "last_error": null,
    })));
    if !args.gmp_url.is_empty() && role.runs_da() {
        match fd_gmp::GmpClient::parse(&args.gmp_url) {
            Ok(client) => {
                let eng = engine.clone();
                let mesh_g = mesh.clone();
                let vid = args.id.clone();
                let en = gmp_enabled.clone();
                let st = gmp_stats.clone();
                let lease_max = args.gmp_lease_max.max(1);
                let budget = args.gmp_budget_mb.max(64);
                let max_if = args.gmp_max_in_flight.max(1);
                let aff = args.gmp_affinity;
                let widx = args.worker_index;
                let wcnt = args.worker_count.max(1);
                info!(
                    "PR-G4 GMP agent enabled url={} lease_max={} budget_mb={} affinity={}",
                    args.gmp_url, lease_max, budget, aff
                );
                tokio::spawn(async move {
                    gmp_lease_agent(
                        client, eng, mesh_g, vid, en, st, lease_max, budget, max_if, aff, widx,
                        wcnt,
                    )
                    .await;
                });
            }
            Err(e) => warn!("invalid --gmp-url {}: {e:#}", args.gmp_url),
        }
    }

    let addr: SocketAddr = args.rpc.parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!(
        "falcon-dagd id={} rpc={} p2p={} crypto={} data_dir={} network_id={} hub=EngineParts gmp={} gcp={}",
        args.id,
        args.rpc,
        if args.p2p.is_empty() {
            "off"
        } else {
            &args.p2p
        },
        engine.crypto_scheme(),
        data_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "off".into()),
        args.network_id,
        if args.gmp_url.is_empty() {
            "off"
        } else {
            &args.gmp_url
        },
        if args.gcp_url.is_empty() {
            "off"
        } else {
            &args.gcp_url
        }
    );

    let _data_tick = data_dir.clone();
    let snap_iv = args.snapshot_interval;
    // ---- DA worker tick: pack / certify availability only (never order/exec) ----
    {
        let eng_da = engine.clone();
        let mesh_da = mesh.clone();
        let vid_da = args.id.clone();
        let gcp_da = gcp_client.clone();
        let gcp_st = gcp_stats.clone();
        tokio::spawn(async move {
            let mut batches_n = 0u64;
            let mut tick_n = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let now = now_s();
                let mut events_out: Option<GossipEvents> = None;
                if !eng_da.role().runs_da() {
                    continue;
                }
                tick_n = tick_n.saturating_add(1);
                // Hub: advance_da only takes dag write; create_batch/prune via with_all_mut.
                eng_da.advance_da();
                // Body-DA escape hatch: repool aged opens with zero foreign UNL sigs.
                let repooled = eng_da.recover_stale_open_bodies(now);
                if repooled > 0 {
                    let abandoned = eng_da.take_abandoned_open_hashes();
                    info!(
                        "open-body-repool abandoned={repooled} hashes={:?} (0 foreign UNL sigs)",
                        abandoned.iter().map(|h| &h[..12.min(h.len())]).collect::<Vec<_>>()
                    );
                    // Peers may hold abandoned body; inv current opens after pack will refresh.
                }
                let mut force_bodies = false;
                // Conf-pool check phase: scheduled multi-sig window for open bodies.
                let conf_ev = eng_da.advance_da_conf(now);
                if !conf_ev.is_empty() {
                    force_bodies = true;
                    events_out = Some(conf_ev);
                }
                // One pack per tick so peers can sign/cert before the next tip advances.
                // Multi-lane DA tick: pack this node's lane; seed body on first send.
                if let Some((b, events)) = eng_da.create_batch(now, false) {
                    force_bodies = true; // first seed of this batch
                    batches_n += 1;
                    if batches_n % 10 == 1 {
                        info!(
                            "da-batch {} txs={} certs={} role={}",
                            &b.batch_hash[..12.min(b.batch_hash.len())],
                            b.tx_ids.len(),
                            eng_da.cert_count(),
                            eng_da.role().as_str()
                        );
                    }
                    // Extend — do not drop conf-pool sigs from earlier in this tick.
                    // PR-G5 GCP publish happens once when events_out is flushed.
                    if let Some(ref mut out) = events_out {
                        out.extend(events);
                    } else {
                        events_out = Some(events);
                    }
                    // Body-only must-deliver on pack (highest wire priority).
                    // Sig rides critical lane after body is seeded.
                    if let Some(ref m) = mesh_da {
                        m.seed_body_must_all(&vid_da, b.clone()).await;
                        if let Some(s) = b.signatures.get(&vid_da).cloned() {
                            m.broadcast_da_must(&WireMsg::sig(
                                &vid_da,
                                fd_types::PartialSignature {
                                    batch_hash: b.batch_hash.clone(),
                                    validator_id: vid_da.clone(),
                                    signature: s,
                                    timestamp: b.timestamp,
                                },
                            ))
                            .await;
                        }
                    }
                }
                // Step 2: open body-DA recovery — inv/want first, throttled directed
                // body, rare mesh body. Prior every-tick double flood (events force_bodies
                // + directed broadcast) saturated inbound under continuous load and
                // left opens creator-local even after hash_mismatch was fixed.
                let need_seed = eng_da.open_needs_body_seed();
                let waiting = eng_da.waiting_open_conf();
                if let Some(ref m) = mesh_da {
                    // A3: if peers are serving bodies or foreign sigs are merging,
                    // suppress body reseed flood (inv still runs).
                    let bodies_served = m
                        .pull
                        .lock()
                        .await
                        .stats_json()
                        .get("bodies_served")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    eng_da.note_body_da_progress(now, bodies_served);

                    // Always announce open hashes so peers can want (no body bytes).
                    if need_seed || waiting {
                        let open_hs = eng_da.own_open_batch_hashes();
                        if !open_hs.is_empty() {
                            m.broadcast(&WireMsg::inv(&vid_da, "batch", open_hs))
                                .await;
                        }
                    }
                    // Body reseed only when should_reseed_open_body says stalled
                    // (progress suppress + 200ms thrash cap). Age still escalates
                    // mesh vs directed cadence when reseed is allowed.
                    // Reliability seed: when need_seed, reseed body ~every 500ms
                    // (directed + mesh + inv). Not every-tick flood (melts mesh).
                    // Steady continuous on small N > peak TPS on 3 nodes.
                    // Reliability: calm ~500ms body reseed when need_seed.
                    // Faster than every-tick flood; slower than 200ms thrash.
                    // Body-must reseed ~250ms when need_seed (body lane isolated).
                    let (directed_every, mesh_every) = if need_seed {
                        (10u64, 10u64) // 10 * 50ms = 500ms body-must
                    } else if waiting {
                        (20u64, 40u64)
                    } else {
                        (0u64, 0u64)
                    };
                    if directed_every > 0 && tick_n % directed_every == 0 {
                        let opens = eng_da.own_open_batches();
                        if !opens.is_empty() {
                            for b in &opens {
                                // Reliability gate: 500ms min between body reseeds.
                                if !eng_da.should_reseed_open_body(&b.batch_hash, now) {
                                    continue;
                                }
                                // Body-only must reseed (highest priority + double mesh).
                                m.seed_body_must_all(&vid_da, b.clone()).await;
                                m.note_open_reseed_mesh();
                                m.note_open_reseed_directed();
                                // body_must: pull cert from peers who may already have certified us
                                m.pull.lock().await.on_inv(
                                    fd_net::PullKind::Cert,
                                    &b.batch_hash,
                                    "any",
                                    false,
                                );
                                if let Some(s) = b.signatures.get(&vid_da).cloned() {
                                    m.broadcast_da_must(&WireMsg::sig(
                                        &vid_da,
                                        fd_types::PartialSignature {
                                            batch_hash: b.batch_hash.clone(),
                                            validator_id: vid_da.clone(),
                                            signature: s,
                                            timestamp: b.timestamp,
                                        },
                                    ))
                                    .await;
                                }
                            }
                            // flush_wants after body reseed (cert pull for PEER_SIGNED)
                            m.flush_wants().await;
                        }
                    }
                }
                // Pull missing bodies for parked certs; inv only *open uncert* (not all certs).
                if let Some(ref m) = mesh_da {
                    let uncert = eng_da.local_uncertified_batch_hashes();
                    if !uncert.is_empty() {
                        m.broadcast(&WireMsg::inv(&vid_da, "batch", uncert))
                            .await;
                    }
                    // Phase C: do NOT pretend every peer announced the hash (that
                    // caused want thrash to empty holders). Use "any" so want is
                    // broadcast; nodes that have the body serve it.
                    let missing = eng_da.missing_batch_bodies_to_pull();
                    let peer_ids = m.peer_ids().await;
                    for h in missing {
                        m.pull.lock().await.on_inv(
                            PullKind::Batch,
                            &h,
                            "any",
                            false,
                        );
                        for pid in &peer_ids {
                            m.pull.lock().await.on_inv(
                                PullKind::Batch,
                                &h,
                                pid,
                                false,
                            );
                        }
                    }
                    // Parent cert holes: want certs for parents of local uncertified batches.
                    for h in eng_da.missing_cert_hashes_to_pull() {
                        m.pull.lock().await.on_inv(
                            PullKind::Cert,
                            &h,
                            "any",
                            false,
                        );
                        for pid in &peer_ids {
                            m.pull.lock().await.on_inv(
                                PullKind::Cert,
                                &h,
                                pid,
                                false,
                            );
                        }
                    }
                    // Throttled inv of recent finalized bodies we hold so late peers
                    // learn real announcers (not full body flood).
                    if tick_n % 20 == 0 {
                        let hold = eng_da.local_finalized_batch_hashes_capped(8);
                        if !hold.is_empty() {
                            m.broadcast(&WireMsg::inv(&vid_da, "batch", hold)).await;
                        }
                    }
                    // Fix 2: periodic cert-set inv so peers can want missing certs.
                    if tick_n % 10 == 0 {
                        let certs = eng_da.local_cert_hashes_capped(16);
                        if !certs.is_empty() {
                            m.broadcast(&WireMsg::inv(&vid_da, "cert", certs)).await;
                        }
                    }
                    // Finalized heal: rare body reseed only (inv path above is primary).
                    // Every-10-tick force_bodies flooded mesh under continuous load.
                    if tick_n % 40 == 0 {
                        let rb_fin = eng_da.rebroadcast_recent_finalized(4);
                        if !rb_fin.is_empty() {
                            if let Some(ref mut out) = events_out {
                                out.extend(rb_fin);
                            } else {
                                events_out = Some(rb_fin);
                            }
                            force_bodies = true;
                        }
                    }
                    // Fix 1: announce certs we just applied (+ body piggyback).
                    let announce = eng_da.take_cert_announcements();
                    if !announce.is_empty() {
                        m.broadcast(&WireMsg::inv(&vid_da, "cert", announce.clone()))
                            .await;
                        m.broadcast(&WireMsg::inv(&vid_da, "batch", announce.clone()))
                            .await;
                        for h in &announce {
                            if let Some(b) = eng_da.get_batch(h) {
                                // Fix 5: body with cert announce (late peers can ingest without want race).
                                m.broadcast_da_must(&WireMsg::batch(&vid_da, b)).await;
                            }
                            if let Some(c) = eng_da.get_cert_for_batch(h) {
                                m.broadcast_da_must(&WireMsg::cert(&vid_da, c)).await;
                            }
                        }
                    }
                    m.flush_wants().await;
                }
                // Phase B multi-lane: every tick direct non-owned mempool txs to lane owners.
                if tick_n % 1 == 0 {
                    let rb_mp = eng_da.rebroadcast_mempool_txs(256);
                    if !rb_mp.is_empty() {
                        if let Some(ref m) = mesh_da {
                            broadcast_events(m, &vid_da, rb_mp).await;
                        }
                    }
                }
                let pruned = eng_da.prune(now);
                if pruned > 0 {
                    info!("pruned {pruned} uncertified batches");
                }
                // Reliability: when need_seed, inv our open hashes as *cert* so peers
                // that already certified can serve the cert back (PEER_SIGNED path).
                if need_seed && tick_n % 20 == 0 {
                    if let Some(ref m) = mesh_da {
                        let opens = eng_da.own_open_batch_hashes();
                        if !opens.is_empty() {
                            m.broadcast(&WireMsg::inv(&vid_da, "cert", opens.clone()))
                                .await;
                            // Also mark ourselves as wanting so flush_wants peers respond
                            // if they announced first — pull path uses inv/want.
                            for h in opens {
                                m.pull.lock().await.on_inv(
                                    fd_net::PullKind::Cert,
                                    &h,
                                    "any",
                                    false,
                                );
                            }
                            m.flush_wants().await;
                        }
                    }
                }
                // Sticky dig: peer certified foreign batch but creator never applied
                // — directed cert+body+sig to creator (not mesh-only).
                // Reliability: also re-queue foreign certs periodically (one-shot miss).
                if let Some(ref m) = mesh_da {
                    eng_da.queue_foreign_cert_reannounce(now);
                    let ann = eng_da.take_creator_cert_announce();
                    for (creator, bh) in ann {
                        if creator == vid_da {
                            continue;
                        }
                        // Reliability: light directed+mesh. Sig+cert first (small);
                        // body only directed once so creator can A1-merge if needed.
                        if let Some(b) = eng_da.get_batch(&bh) {
                            if let Some(s) = b.signatures.get(&vid_da).cloned() {
                                let ps = fd_types::PartialSignature {
                                    batch_hash: bh.clone(),
                                    validator_id: vid_da.clone(),
                                    signature: s,
                                    timestamp: b.timestamp,
                                };
                                m.send_da_must(&creator, &WireMsg::sig(&vid_da, ps.clone()))
                                    .await;
                                m.broadcast_da_must(&WireMsg::sig(&vid_da, ps)).await;
                            }
                            m.send_da_must(&creator, &WireMsg::batch(&vid_da, b)).await;
                        }
                        if let Some(c) = eng_da.get_cert_for_batch(&bh) {
                            // Directed + mesh so dual-dial / stale map cannot drop cert.
                            m.send_da_must(&creator, &WireMsg::cert(&vid_da, c.clone()))
                                .await;
                            m.broadcast_da_must(&WireMsg::cert(&vid_da, c)).await;
                        }
                    }
                }
                if let Some(events) = events_out {
                    // PR-G5: conf-pool / cert events not only from create_batch.
                    if let Some(ref gcp) = gcp_da {
                        gcp_publish_events(gcp, &events, &gcp_st).await;
                    }
                    if let Some(ref m) = mesh_da {
                        // A2 directed-to-creator is inside broadcast_events_ex
                        // (also covers inbound first-sign egress).
                        broadcast_events_ex(m, &vid_da, events, force_bodies).await;
                    }
                }
            }
        });
    }

    // ---- Light ledger tick: tip-BFT + expand certs + execute (never pack) ----
    {
        let eng_led = engine.clone();
        let mesh_led = mesh.clone();
        let vid_led = args.id.clone();
        let data_led = data_dir.clone();
        let gcp_led = gcp_client.clone();
        let gcp_st_led = gcp_stats.clone();
        tokio::spawn(async move {
            let mut last_snap = now_s();
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let now = now_s();
                let mut events_out: Option<GossipEvents> = None;
                if !eng_led.role().runs_ledger() {
                    continue;
                }
                let before = eng_led.executed_len();
                let (before_commits, _, _) = eng_led.tip_bft_snapshot();
                let ev = eng_led.advance_ledger();
                let after = eng_led.executed_len();
                if after > before && after % 50 == 0 {
                    let (commits, tip_round, _) = eng_led.tip_bft_snapshot();
                    info!(
                        "ledger ordered={} executed={} tip_commits={} tip_round={} role={}",
                        eng_led.ordered_len(),
                        after,
                        commits,
                        tip_round,
                        eng_led.role().as_str()
                    );
                }
                {
                    let (commits, _, last) = eng_led.tip_bft_snapshot();
                    if commits > before_commits {
                        info!(
                            "tip-bft commit tip={} total_commits={}",
                            last.chars().take(12).collect::<String>(),
                            commits
                        );
                    }
                }
                if !ev.is_empty() {
                    events_out = Some(ev);
                }
                // Phase D: snapshot off hot path.
                if let Some(ref dir) = data_led {
                    if snap_iv > 0.0 && (now - last_snap) >= snap_iv {
                        last_snap = now;
                        let snap = eng_led.to_snapshot();
                        let path = dir.clone();
                        let n_exec = eng_led.executed_len();
                        tokio::task::spawn_blocking(move || {
                            let p = fd_store::snapshot_path(&path);
                            match fd_store::save_snapshot(p, &snap) {
                                Ok(()) => info!("snapshot saved executed={n_exec}"),
                                Err(e) => warn!("snapshot save failed: {e}"),
                            }
                        });
                    }
                }
                if let Some(events) = events_out {
                    if let Some(ref gcp) = gcp_led {
                        gcp_publish_events(gcp, &events, &gcp_st_led).await;
                    }
                    if let Some(ref m) = mesh_led {
                        broadcast_events(m, &vid_led, events).await;
                    }
                }
            }
        });
    }

    loop {
        let (sock, peer) = listener.accept().await?;
        let eng = engine.clone();
        let mesh_rpc = mesh.clone();
        let vid = args.id.clone();
        let data_rpc = data_dir.clone();
        let gmp_en = gmp_enabled.clone();
        let gmp_st = gmp_stats.clone();
        let gcp_st_rpc = gcp_stats.clone();
        tokio::spawn(async move {
            if let Err(e) =
                handle_rpc(sock, eng, mesh_rpc, vid, data_rpc, gmp_en, gmp_st, gcp_st_rpc).await
            {
                warn!("rpc {peer}: {e}");
            }
        });
    }
}

fn now_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

/// PR-G5: publish a newly packed batch (and creator local sig if present) to GCP-B.
async fn gcp_publish_batch(
    gcp: &fd_gmp::GcpClient,
    b: &fd_types::Batch,
    stats: &parking_lot::Mutex<serde_json::Value>,
) {
    let parents: Vec<String> = b.parents.clone();
    match gcp
        .put_batch(&b.batch_hash, &b.creator, b.tx_ids.len() as u64, &parents)
        .await
    {
        Ok(_) => {
            let mut s = stats.lock();
            let n = s.get("batches").and_then(|x| x.as_u64()).unwrap_or(0);
            s["batches"] = serde_json::json!(n + 1);
        }
        Err(e) => {
            let mut s = stats.lock();
            let n = s.get("errors").and_then(|x| x.as_u64()).unwrap_or(0);
            s["errors"] = serde_json::json!(n + 1);
            s["last_error"] = serde_json::json!(format!("batch: {e:#}"));
        }
    }
    for (vid, sig) in &b.signatures {
        match gcp.put_sig(&b.batch_hash, vid, sig).await {
            Ok(_) => {
                let mut s = stats.lock();
                let n = s.get("sigs").and_then(|x| x.as_u64()).unwrap_or(0);
                s["sigs"] = serde_json::json!(n + 1);
            }
            Err(e) => {
                let mut s = stats.lock();
                let n = s.get("errors").and_then(|x| x.as_u64()).unwrap_or(0);
                s["errors"] = serde_json::json!(n + 1);
                s["last_error"] = serde_json::json!(format!("sig: {e:#}"));
            }
        }
    }
}

/// PR-G5: publish gossip-emitted batches / sigs / certs to GCP-B registry.
async fn gcp_publish_events(
    gcp: &fd_gmp::GcpClient,
    events: &GossipEvents,
    stats: &parking_lot::Mutex<serde_json::Value>,
) {
    for b in &events.batches {
        gcp_publish_batch(gcp, b, stats).await;
    }
    for sig in &events.sigs {
        match gcp
            .put_sig(&sig.batch_hash, &sig.validator_id, &sig.signature)
            .await
        {
            Ok(_) => {
                let mut s = stats.lock();
                let n = s.get("sigs").and_then(|x| x.as_u64()).unwrap_or(0);
                s["sigs"] = serde_json::json!(n + 1);
            }
            Err(e) => {
                let mut s = stats.lock();
                let n = s.get("errors").and_then(|x| x.as_u64()).unwrap_or(0);
                s["errors"] = serde_json::json!(n + 1);
                s["last_error"] = serde_json::json!(format!("sig: {e:#}"));
            }
        }
    }
    for cert in &events.certs {
        // ensure batch meta exists before cert mark
        let _ = gcp
            .put_batch(
                &cert.batch_hash,
                &cert.creator,
                0,
                &[],
            )
            .await;
        match gcp.put_cert(&cert.batch_hash).await {
            Ok(_) => {
                let mut s = stats.lock();
                let n = s.get("certs").and_then(|x| x.as_u64()).unwrap_or(0);
                s["certs"] = serde_json::json!(n + 1);
            }
            Err(e) => {
                let mut s = stats.lock();
                let n = s.get("errors").and_then(|x| x.as_u64()).unwrap_or(0);
                s["errors"] = serde_json::json!(n + 1);
                s["last_error"] = serde_json::json!(format!("cert: {e:#}"));
            }
        }
    }
}

/// PR-G4: packers pull capacity-weighted leases from GMP and admit into local mempool.
/// Non-owned txs are handed off via existing multipack rebroadcast each DA tick.
async fn gmp_lease_agent(
    client: fd_gmp::GmpClient,
    engine: Arc<EngineParts>,
    mesh: Option<Arc<PeerMesh>>,
    validator_id: String,
    enabled: Arc<std::sync::atomic::AtomicBool>,
    stats: Arc<parking_lot::Mutex<serde_json::Value>>,
    lease_max: usize,
    budget_mb: u64,
    max_in_flight: u64,
    affinity: bool,
    worker_index: usize,
    worker_count: usize,
) {
    use std::sync::atomic::Ordering;
    let mut weight = 0.5f64;
    let mut tick: u64 = 0;
    let mut last_exec: u64 = 0;
    let mut last_exec_t = now_s();
    let mut clearance_ema = 5.0f64;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        tick = tick.saturating_add(1);
        if !enabled.load(Ordering::Relaxed) {
            continue;
        }
        if !engine.role().runs_da() {
            continue;
        }

        // Capacity advert from live gauges: mempool + open + observed clearance TPS.
        let (mempool_n, open_n, exec_n) = engine.with_engine(|g| {
            let open = g
                .metrics_json()
                .get("pack_stall")
                .and_then(|p| p.get("own_open_n"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            (g.mempool.len() as u64, open, g.execution.executed_txs.len() as u64)
        });
        let now = now_s();
        let dt = (now - last_exec_t).max(0.05);
        if exec_n >= last_exec {
            let inst = (exec_n - last_exec) as f64 / dt;
            clearance_ema = 0.7 * clearance_ema + 0.3 * inst;
        }
        last_exec = exec_n;
        last_exec_t = now;
        let used = (mempool_n.saturating_mul(2) + open_n.saturating_mul(20))
            .min(budget_mb.saturating_sub(1));
        let clearance = clearance_ema.max(0.1);
        let adv = fd_gmp::CapacityAdvert {
            validator_id: validator_id.clone(),
            mem_budget_mb: budget_mb,
            mem_used_mb: used,
            open_uncertified: open_n,
            max_in_flight_leases: max_in_flight,
            recent_clearance_tps: clearance,
            lease_expiry_rate: 0.0,
            ts: now,
        };
        match client.capacity(&adv).await {
            Ok(v) => {
                if let Some(w) = v.get("weight").and_then(|x| x.as_f64()) {
                    weight = w.max(0.02);
                }
            }
            Err(e) => {
                let mut s = stats.lock();
                s["last_error"] = serde_json::json!(format!("capacity: {e:#}"));
                continue;
            }
        }

        // Weight-paced pull: low capacity sleeps more (share of global work).
        // base 2ms at weight>=1, up to ~60ms when weight tiny.
        let pace_ms = (2.0 + 58.0 * (1.0 - (weight / (weight + 0.5)).min(1.0))) as u64;
        if pace_ms > 5 {
            tokio::time::sleep(std::time::Duration::from_millis(pace_ms)).await;
        }

        let (aff_mod, aff_idx) = if affinity && worker_count > 0 {
            (Some(worker_count), Some(worker_index))
        } else {
            (None, None)
        };

        let lease = match client
            .lease(&validator_id, lease_max, aff_mod, aff_idx)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                let mut s = stats.lock();
                s["last_error"] = serde_json::json!(format!("lease: {e:#}"));
                continue;
            }
        };
        if lease.get("error").is_some() {
            let mut s = stats.lock();
            s["last_error"] = lease.get("error").cloned().unwrap_or(serde_json::Value::Null);
            continue;
        }
        let lid = lease
            .get("lease_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let txs = lease
            .get("txs")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if lid.is_empty() || txs.is_empty() {
            let mut s = stats.lock();
            let n = s.get("leases_empty").and_then(|x| x.as_u64()).unwrap_or(0);
            s["leases_empty"] = serde_json::json!(n + 1);
            continue;
        }
        {
            let mut s = stats.lock();
            let n = s.get("leases_ok").and_then(|x| x.as_u64()).unwrap_or(0);
            s["leases_ok"] = serde_json::json!(n + 1);
        }

        // Admit one-at-a-time with complete only if all succeed / stale.
        // Prefer atomic: process all, complete if none retryable.
        let mut ok_n = 0u64;
        let mut stale_n = 0u64;
        let mut retryable = false;
        let mut events_all = GossipEvents::default();
        let now = now_s();
        for txv in &txs {
            let account = txv
                .get("account")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let sequence = txv.get("sequence").and_then(|x| x.as_u64()).unwrap_or(0);
            let destination = txv
                .get("destination")
                .and_then(|x| x.as_str())
                .unwrap_or("acct40")
                .to_string();
            let amount = txv.get("amount").and_then(|x| x.as_u64()).unwrap_or(1);
            let fee = txv.get("fee").and_then(|x| x.as_u64()).unwrap_or(10);
            if account.is_empty() || sequence == 0 {
                retryable = true;
                continue;
            }
            let tx = Transaction::create(account, sequence, destination, amount, fee, now);
            let tx_id = tx.tx_id.clone();
            let (result, events) = engine.with_engine(|g| {
                let result = g.submit_tx(tx);
                let mut events = GossipEvents::default();
                if result.is_ok() {
                    if let Some(full) = g.dag.tx_index.get(&tx_id) {
                        let multi = !g.config.single_packer_per_round;
                        if multi {
                            if g.owns_tx(full, now) {
                                // keep local for pack
                            } else if let Some(owner) = g.account_owner_id(&full.account) {
                                events.directed_txs.push((owner, full.clone()));
                                g.mempool.remove(&tx_id);
                                g.shard_flow.directed_to_owner =
                                    g.shard_flow.directed_to_owner.saturating_add(1);
                            } else {
                                events.txs.push(full.clone());
                            }
                        }
                    }
                }
                (result, events)
            });
            events_all.directed_txs.extend(events.directed_txs);
            events_all.txs.extend(events.txs);
            match result {
                SubmitResult::Accepted => {
                    ok_n += 1;
                    let mut s = stats.lock();
                    let n = s.get("admit_ok").and_then(|x| x.as_u64()).unwrap_or(0);
                    s["admit_ok"] = serde_json::json!(n + 1);
                }
                SubmitResult::SeqPast | SubmitResult::Duplicate => {
                    stale_n += 1;
                }
                r if r.is_retryable() => {
                    retryable = true;
                    let mut s = stats.lock();
                    let n = s.get("admit_err").and_then(|x| x.as_u64()).unwrap_or(0);
                    s["admit_err"] = serde_json::json!(n + 1);
                }
                _ => {
                    // non-retryable protocol error: drop from GMP to avoid stuck lease
                    stale_n += 1;
                    let mut s = stats.lock();
                    let n = s.get("admit_err").and_then(|x| x.as_u64()).unwrap_or(0);
                    s["admit_err"] = serde_json::json!(n + 1);
                }
            }
        }
        if let Some(ref m) = mesh {
            if !events_all.is_empty() {
                broadcast_events(m, &validator_id, events_all).await;
            }
        }

        // Complete if everything admitted or already past; else nack for reissue.
        let total = txs.len() as u64;
        if !retryable && (ok_n + stale_n) >= total && total > 0 {
            if let Err(e) = client.complete(&lid).await {
                let mut s = stats.lock();
                s["last_error"] = serde_json::json!(format!("complete: {e:#}"));
            } else {
                let mut s = stats.lock();
                let n = s.get("complete_ok").and_then(|x| x.as_u64()).unwrap_or(0);
                s["complete_ok"] = serde_json::json!(n + 1);
            }
        } else {
            let reason = if retryable {
                "retryable_admit"
            } else {
                "partial"
            };
            if let Err(e) = client.nack(&lid, reason).await {
                let mut s = stats.lock();
                s["last_error"] = serde_json::json!(format!("nack: {e:#}"));
            } else {
                let mut s = stats.lock();
                let n = s.get("nack").and_then(|x| x.as_u64()).unwrap_or(0);
                s["nack"] = serde_json::json!(n + 1);
            }
        }
    }
}

/// Pull-path gossip: seed full body once + inv announce; no mesh-wide body rebroadcast.
/// When `force_bodies` is true (open-batch reseed under stall), re-send batch bodies even
/// if already marked seen so peers that missed the first seed can still want/sign.
async fn broadcast_events(mesh: &PeerMesh, from: &str, events: GossipEvents) {
    broadcast_events_ex(mesh, from, events, false).await;
}

async fn broadcast_events_ex(
    mesh: &PeerMesh,
    from: &str,
    events: GossipEvents,
    force_bodies: bool,
) {
    for tx in events.txs {
        let _ = mesh.mark_tx_seen(&tx.tx_id).await;
        mesh.broadcast(&WireMsg::tx(from, tx)).await;
    }
    // Phase B: direct-to-owner (lane) delivery — not mesh flood lottery.
    for (peer, tx) in events.directed_txs {
        let _ = mesh.mark_tx_seen(&tx.tx_id).await;
        mesh.send_to_peer(&peer, &WireMsg::tx(from, tx)).await;
    }
    // A2: directed sig (+ body) to creator FIRST (all egress: DA + inbound first-sign).
    const SIG_TO_CREATOR_INTERVAL: std::time::Duration =
        std::time::Duration::from_millis(200);
    for sig in &events.sigs {
        if let Some(b) = events.batches.iter().find(|b| b.batch_hash == sig.batch_hash) {
            if b.creator != from {
                let _ = mesh
                    .try_directed_sig_to_creator(
                        &b.creator,
                        from,
                        sig,
                        Some(b),
                        SIG_TO_CREATOR_INTERVAL,
                    )
                    .await;
            }
        }
    }
    let mut batch_hashes = Vec::new();
    let batch_bodies = events.batches.clone();
    for batch in events.batches {
        let h = batch.batch_hash.clone();
        let first = mesh.mark_batch_seen(&h).await;
        if first || force_bodies {
            mesh.cache_batch(batch.clone()).await;
            mesh.broadcast_da_must(&WireMsg::batch(from, batch)).await;
        }
        batch_hashes.push(h);
    }
    if !batch_hashes.is_empty() {
        mesh.broadcast(&WireMsg::inv(from, "batch", batch_hashes))
            .await;
    }
    for sig in &events.sigs {
        mesh.broadcast_da_must(&WireMsg::sig(from, sig.clone())).await;
    }
    let mut cert_hashes = Vec::new();
    for cert in events.certs {
        let h = cert.batch_hash.clone();
        // Fix 5: if this events also carries the body, force-send body again with cert.
        if let Some(b) = batch_bodies.iter().find(|b| b.batch_hash == h) {
            mesh.broadcast_da_must(&WireMsg::batch(from, b.clone())).await;
        }
        // Reliability: directed cert (+ body) to creator FIRST — mesh-only + mark_seen
        // suppressed re-sends and left PEER_SIGNED_CREATOR_MISSING_FOREIGN.
        let creator = cert.creator.clone();
        if creator != from && !creator.is_empty() {
            if let Some(b) = batch_bodies.iter().find(|b| b.batch_hash == h) {
                mesh.send_da_must(&creator, &WireMsg::batch(from, b.clone()))
                    .await;
            }
            mesh.send_da_must(&creator, &WireMsg::cert(from, cert.clone()))
                .await;
        }
        // Always mesh-broadcast cert (do not suppress after first mark_seen).
        let _first_cert = mesh.mark_cert_seen(&cert.cert_id).await;
        mesh.broadcast_da_must(&WireMsg::cert(from, cert)).await;
        cert_hashes.push(h);
    }
    if !cert_hashes.is_empty() {
        mesh.broadcast(&WireMsg::inv(from, "cert", cert_hashes)).await;
    }
    for prop in events.tip_proposals {
        mesh.broadcast(&WireMsg::tip_proposal(from, prop)).await;
    }
    for vote in events.tip_votes {
        mesh.broadcast(&WireMsg::tip_vote(from, vote)).await;
    }
    for commit in events.tip_commits {
        mesh.broadcast(&WireMsg::tip_commit(from, commit)).await;
    }
    mesh.flush_wants().await;
}

async fn handle_inbound(
    engine: Arc<EngineParts>,
    mesh: Arc<PeerMesh>,
    mut inbound: fd_net::InboundRx,
) {
    // InboundMux: critical DA (batch/sig/cert/want) preferred over chatter.
    while let Some(msg) = inbound.recv().await {
        let now = now_s();
        let from = mesh.local_id.clone();
        let mut out = GossipEvents::default();
        let rebroadcast_batch: Option<fd_types::Batch> = None;
        let mut rebroadcast_tx: Option<Transaction> = None;
        let rebroadcast_cert: Option<fd_types::Certificate> = None;

        // Phase A: Falcon-512 verify off the engine lock. Drop bad sigs here so we
        // do not re-enter on_remote_sig / merge thrash for garbage (N=6 SIG_GOSSIP dig).
        if msg.msg_type == "sig" {
            if let Some(ref sig) = msg.sig {
                let crypto = engine.crypto_arc();
                let vid = sig.validator_id.clone();
                let bh = sig.batch_hash.clone();
                let s = sig.signature.clone();
                let ok = tokio::task::spawn_blocking(move || crypto.verify_batch(&vid, &bh, &s))
                    .await
                    .unwrap_or(false);
                if !ok {
                    // Invalid or empty — skip engine path entirely.
                    continue;
                }
                // Valid: add_signature re-checks via cache (hit) under lock.
            }
        }

        // Never hold hub locks across .await (not Send).
        match msg.msg_type.as_str() {
            "tx" => {
                if let Some(tx) = msg.tx {
                    // Always attempt admit (directed handoff may re-deliver after
                    // mark_tx_seen on a prior flood that never submitted).
                    // Mesh path: bypass MCB so transfer-to-owner is not dropped.
                    let _first = mesh.mark_tx_seen(&tx.tx_id).await;
                    let accepted = engine.submit_tx_mesh(tx.clone()).is_ok();
                    // Multi-lane: do NOT mesh-flood. Owner packs; not-owned is
                    // transferred via rebroadcast_mempool_txs directed_txs.
                    // Single-packer legacy: flood first-seen accepts.
                    if accepted && _first && engine.single_packer_mode() {
                        rebroadcast_tx = Some(tx);
                    }
                }
            }
            "batch" => {
                // Body (seed or pull response) — ingest; do not rebroadcast body.
                if let Some(batch) = msg.batch {
                    let h = batch.batch_hash.clone();
                    let _ = mesh.mark_batch_seen(&h).await;
                    mesh.cache_batch(batch.clone()).await;
                    {
                        out = engine.on_remote_batch(batch, now);
                    }
                    mesh.pull.lock().await.on_body(PullKind::Batch, &h);
                    // Only announce inv if we actually store the body (ingest may reject).
                    if engine.has_batch(&h) {
                        mesh.broadcast(&WireMsg::inv(&from, "batch", vec![h])).await;
                    }
                }
            }
            "sig" => {
                if let Some(sig) = msg.sig {
                    out = engine.on_remote_sig(&sig.batch_hash, &sig.validator_id, &sig.signature);
                }
            }
            "cert" => {
                if let Some(cert) = msg.cert {
                    let bh = cert.batch_hash.clone();
                    let _ = mesh.mark_cert_seen(&cert.cert_id).await;
                    let need = engine.on_remote_cert(cert);
                    mesh.pull.lock().await.on_body(PullKind::Cert, &bh);
                    // Phase C: pull body from cert source (honest announcer) + "any"
                    // broadcast fallback. Do not mark every peer as announcer.
                    let source = msg.from.clone().unwrap_or_default();
                    for missing in need {
                        if engine.has_batch(&missing) {
                            continue;
                        }
                        if !source.is_empty() {
                            mesh.pull.lock().await.on_inv(
                                PullKind::Batch,
                                &missing,
                                &source,
                                false,
                            );
                        }
                        mesh.pull.lock().await.on_inv(
                            PullKind::Batch,
                            &missing,
                            "any",
                            false,
                        );
                    }
                    mesh.broadcast(&WireMsg::inv(&from, "cert", vec![bh.clone()])).await;
                    // If we already have the body, inv batch so peers can want from us.
                    if engine.has_batch(&bh) {
                        mesh.broadcast(&WireMsg::inv(&from, "batch", vec![bh.clone()]))
                            .await;
                    }
                    // Fix 1: if apply succeeded, re-announce cert+body.
                    let announce = engine.take_cert_announcements();
                    for h in announce {
                        mesh.broadcast(&WireMsg::inv(&from, "cert", vec![h.clone()]))
                            .await;
                        mesh.broadcast(&WireMsg::inv(&from, "batch", vec![h.clone()]))
                            .await;
                        if let Some(b) = engine.get_batch(&h) {
                            mesh.broadcast_da_must(&WireMsg::batch(&from, b)).await;
                        }
                        if let Some(c) = engine.get_cert_for_batch(&h) {
                            mesh.broadcast_da_must(&WireMsg::cert(&from, c)).await;
                        }
                    }
                }
            }
            "inv" => {
                let kind = msg
                    .inv_kind
                    .as_deref()
                    .and_then(PullKind::parse)
                    .unwrap_or(PullKind::Batch);
                let hashes = msg.inv_hashes.clone().unwrap_or_default();
                let peer = msg.from.clone().unwrap_or_default();
                if !peer.is_empty() {
                    for h in hashes {
                        // Prefer mesh cache so we don't false-negative during engine lock.
                        let we_have = match kind {
                            PullKind::Batch => {
                                engine.has_batch(&h)
                                    || mesh.get_cached_batch(&h).await.is_some()
                            }
                            PullKind::Cert => engine.has_cert_for_batch(&h),
                        };
                        mesh.pull.lock().await.on_inv(kind, &h, &peer, we_have);
                    }
                    // Immediate want flush — do not wait for next DA tick.
                    mesh.flush_wants().await;
                }
            }
            "want" => {
                let kind = msg
                    .inv_kind
                    .as_deref()
                    .and_then(PullKind::parse)
                    .unwrap_or(PullKind::Batch);
                let hashes = msg.inv_hashes.clone().unwrap_or_default();
                let peer = msg.from.clone().unwrap_or_default();
                if !peer.is_empty() {
                    for h in hashes {
                        match kind {
                            PullKind::Batch => {
                                let cached = mesh.get_cached_batch(&h).await;
                                let body = cached.or_else(|| engine.get_batch(&h));
                                if let Some(b) = body {
                                    mesh.pull.lock().await.bodies_served.fetch_add(
                                        1,
                                        std::sync::atomic::Ordering::Relaxed,
                                    );
                                    mesh.cache_batch(b.clone()).await;
                                    mesh.send_da_must(&peer, &WireMsg::batch(&from, b))
                                        .await;
                                    // Flake-hunt: piggyback cert if finalized.
                                    if let Some(c) = engine.get_cert_for_batch(&h) {
                                        mesh.send_da_must(&peer, &WireMsg::cert(&from, c))
                                            .await;
                                    }
                                }
                            }
                            PullKind::Cert => {
                                if let Some(c) = engine.get_cert_for_batch(&h) {
                                    mesh.pull.lock().await.bodies_served.fetch_add(
                                        1,
                                        std::sync::atomic::Ordering::Relaxed,
                                    );
                                    // Flake-hunt: body first so try_apply_parked has it.
                                    if let Some(b) = engine.get_batch(&h) {
                                        mesh.send_da_must(&peer, &WireMsg::batch(&from, b))
                                            .await;
                                    }
                                    mesh.send_da_must(&peer, &WireMsg::cert(&from, c))
                                        .await;
                                }
                            }
                        }
                    }
                }
            }
            "tip_proposal" => {
                if let Some(prop) = msg.tip_proposal {
                    out.extend(engine.on_tip_proposal(prop));
                }
            }
            "tip_vote" => {
                if let Some(vote) = msg.tip_vote {
                    out.extend(engine.on_tip_vote(vote));
                }
            }
            "tip_commit" => {
                if let Some(commit) = msg.tip_commit {
                    out.extend(engine.on_tip_commit(commit));
                }
            }
            _ => {}
        }

        if let Some(tx) = rebroadcast_tx {
            mesh.broadcast(&WireMsg::tx(&from, tx)).await;
        }
        // No body rebroadcast — pull path only.
        let _ = rebroadcast_batch;
        let _ = rebroadcast_cert;
        broadcast_events(&mesh, &from, out).await;
        mesh.flush_wants().await;
    }
}

/// Read one HTTP/1.1 request from `sock`: full headers + body by Content-Length.
/// Prevents "bad json: EOF" when the first TCP read only contains headers or a
/// partial body under concurrent flood load (Phase 49c).
async fn read_http_request(sock: &mut TcpStream) -> Result<(String /* request-line+headers */, Vec<u8> /* body */)> {
    const MAX_HEADERS: usize = 64 * 1024;
    const MAX_BODY: usize = 4 * 1024 * 1024;
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut tmp = [0u8; 8 * 1024];

    // 1) Accumulate until header terminator or limit.
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() >= MAX_HEADERS {
            bail!("http headers too large");
        }
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok((String::new(), Vec::new()));
            }
            // Incomplete headers on EOF — surface what we have (caller may error).
            break buf.len();
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let header_bytes = &buf[..header_end.min(buf.len())];
    let headers = String::from_utf8_lossy(header_bytes).into_owned();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let line = line.trim();
            let (name, rest) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                rest.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);

    if content_length > MAX_BODY {
        bail!("http body too large: {content_length}");
    }

    // 2) Body may already be partially in `buf` after headers.
    let mut body = if header_end < buf.len() {
        buf[header_end..].to_vec()
    } else {
        Vec::new()
    };

    // 3) Read until Content-Length is satisfied (or no CL and we already have data).
    if content_length > 0 {
        while body.len() < content_length {
            let n = sock.read(&mut tmp).await?;
            if n == 0 {
                // Peer closed early — return partial so dispatch can report incomplete_body
                // rather than silently treating empty as bad json from nowhere.
                break;
            }
            body.extend_from_slice(&tmp[..n]);
            if body.len() > MAX_BODY {
                bail!("http body exceeds max while reading");
            }
        }
        if body.len() > content_length {
            body.truncate(content_length);
        }
    }

    Ok((headers, body))
}

async fn handle_rpc(
    mut sock: TcpStream,
    engine: Arc<EngineParts>,
    mesh: Option<Arc<PeerMesh>>,
    validator_id: String,
    data_dir: Option<PathBuf>,
    gmp_enabled: Arc<std::sync::atomic::AtomicBool>,
    gmp_stats: Arc<parking_lot::Mutex<serde_json::Value>>,
    gcp_stats: Arc<parking_lot::Mutex<serde_json::Value>>,
) -> Result<()> {
    let (headers, body_bytes) = read_http_request(&mut sock).await?;
    if headers.is_empty() && body_bytes.is_empty() {
        return Ok(());
    }
    let first_line = headers.lines().next().unwrap_or("");
    // Prometheus scrape: GET /metrics
    if first_line.starts_with("GET /metrics") || first_line.starts_with("GET /metrics ") {
        let text = engine.metrics_prometheus();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            text.len()
        );
        sock.write_all(header.as_bytes()).await?;
        sock.write_all(text.as_bytes()).await?;
        return Ok(());
    }

    // Prefer declared Content-Length completeness; empty body with CL>0 is incomplete.
    let cl = headers.lines().find_map(|line| {
        let line = line.trim();
        let (name, rest) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            rest.trim().parse::<usize>().ok()
        } else {
            None
        }
    });
    let body_owned = if body_bytes.is_empty() {
        String::new()
    } else {
        String::from_utf8_lossy(&body_bytes).into_owned()
    };
    let body_ref: &str = body_owned.as_str();
    let resp = if let Some(want) = cl {
        if body_bytes.len() < want {
            serde_json::json!({
                "error": format!(
                    "incomplete_body: got {} of {} bytes (retryable)",
                    body_bytes.len(),
                    want
                ),
                "retryable": true,
            })
        } else if body_ref.trim().is_empty() {
            serde_json::json!({
                "error": "empty_body (retryable)",
                "retryable": true,
            })
        } else {
            dispatch(
                body_ref,
                engine,
                mesh,
                validator_id,
                data_dir,
                gmp_enabled,
                gmp_stats,
                gcp_stats,
            )
            .await
        }
    } else if body_ref.trim().is_empty() {
        // No Content-Length and no body — still try header-only GET-like; for POST, retryable.
        if first_line.starts_with("POST") || first_line.starts_with("post") {
            serde_json::json!({
                "error": "empty_body (retryable)",
                "retryable": true,
            })
        } else {
            dispatch(
                body_ref,
                engine,
                mesh,
                validator_id,
                data_dir,
                gmp_enabled,
                gmp_stats,
                gcp_stats,
            )
            .await
        }
    } else {
        dispatch(
            body_ref,
            engine,
            mesh,
            validator_id,
            data_dir,
            gmp_enabled,
            gmp_stats,
            gcp_stats,
        )
        .await
    };

    let payload = serde_json::to_vec(&serde_json::json!({ "result": resp }))?;
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    sock.write_all(header.as_bytes()).await?;
    sock.write_all(&payload).await?;
    Ok(())
}

fn parse_bridge_proof(params: &serde_json::Value) -> BridgeProof {
    let b = params.get("bridge").cloned().unwrap_or_else(|| params.clone());
    let merkle_path = b
        .get("merkle_path")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    BridgeProof {
        external_txid: b
            .get("external_txid")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .into(),
        block_hash: b
            .get("block_hash")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .into(),
        height: b.get("height").and_then(|x| x.as_u64()).unwrap_or(0),
        merkle_path,
        merkle_index: b
            .get("merkle_index")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
        lock_id: b.get("lock_id").and_then(|x| x.as_str()).unwrap_or("").into(),
        parent_hash: b
            .get("parent_hash")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .into(),
        merkle_root: b
            .get("merkle_root")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .into(),
        tx_root: b
            .get("tx_root")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .into(),
        work: b
            .get("work")
            .map(|x| {
                if let Some(s) = x.as_str() {
                    s.to_string()
                } else if let Some(n) = x.as_u64() {
                    n.to_string()
                } else {
                    String::new()
                }
            })
            .unwrap_or_default(),
        external_to: b
            .get("external_to")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .into(),
    }
}

async fn dispatch(
    body: &str,
    engine: Arc<EngineParts>,
    mesh: Option<Arc<PeerMesh>>,
    validator_id: String,
    data_dir: Option<PathBuf>,
    gmp_enabled: Arc<std::sync::atomic::AtomicBool>,
    gmp_stats: Arc<parking_lot::Mutex<serde_json::Value>>,
    gcp_stats: Arc<parking_lot::Mutex<serde_json::Value>>,
) -> serde_json::Value {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return serde_json::json!({ "error": format!("bad json: {e}") }),
    };
    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = v
        .get("params")
        .and_then(|p| p.as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(serde_json::json!({}));

    match method {
        "ping" => engine.with_engine(|g| {
            serde_json::json!({ "pong": true, "validator_id": g.validator_id })
        }),
        "gmp_control" => {
            // Dig control: pause/resume in-engine GMP lease agent without killing the node.
            if let Some(en) = params.get("enabled").and_then(|v| v.as_bool()) {
                gmp_enabled.store(en, std::sync::atomic::Ordering::Relaxed);
            }
            let cur = gmp_enabled.load(std::sync::atomic::Ordering::Relaxed);
            {
                let mut s = gmp_stats.lock();
                s["enabled"] = serde_json::json!(cur);
            }
            let snap = gmp_stats.lock().clone();
            serde_json::json!({ "ok": true, "enabled": cur, "stats": snap })
        }
        "gmp_stats" => {
            let cur = gmp_enabled.load(std::sync::atomic::Ordering::Relaxed);
            let mut snap = gmp_stats.lock().clone();
            snap["enabled"] = serde_json::json!(cur);
            snap
        }
        "gcp_stats" => gcp_stats.lock().clone(),
        "server_info" => {
            let mesh_opt = if let Some(ref mesh) = mesh {
                Some(mesh.mesh_stats_json().await)
            } else {
                None
            };
            // Fold live pull peaks into mesh_diag, then snapshot metrics.
            let mut m = engine.with_engine(|g| {
                if let Some(ref mesh_stats) = mesh_opt {
                    if let Some(pull) = mesh_stats.get("pull") {
                        if let Some(p) = pull.get("inflight_peak").and_then(|x| x.as_u64()) {
                            g.mesh_diag.note_pull_inflight(p as u32);
                        }
                        if let Some(p) = pull.get("want_batch_max").and_then(|x| x.as_u64()) {
                            g.mesh_diag.note_want_batch(p as u32);
                        }
                    }
                }
                g.metrics_json()
            });
            if let Some(mesh_stats) = mesh_opt {
                if let Some(peers) = mesh_stats.get("p2p_peers") {
                    m["p2p_peers"] = peers.clone();
                }
                if let Some(pull) = mesh_stats.get("pull") {
                    m["pull"] = pull.clone();
                }
                m["mesh_net"] = mesh_stats;
            }
            serde_json::json!({
                "info": {
                    "server_state": "proposing",
                    "validator_id": m.get("validator_id").cloned().unwrap_or(serde_json::json!("")),
                    "network_id": m.get("network_id").cloned().unwrap_or(serde_json::json!(0)),
                    "build_version": "falcon-dagd/0.48.0-rust",
                    "architecture": ARCHITECTURE_STRING,
                    "node_profile": m.get("node_profile").cloned().unwrap_or(serde_json::json!("validator")),
                    "profile_status": m.get("profile_status").cloned().unwrap_or(serde_json::json!({})),
                    "metrics": m,
                }
            })
        }
        "architecture" => {
            // PR1: frozen architecture / metrics / gap-class contract for digs & ops.
            let mut contract = architecture_contract_json();
            if let serde_json::Value::Object(ref mut map) = contract {
                map.insert(
                    "node".into(),
                    engine.with_engine(|g| {
                        serde_json::json!({
                            "validator_id": g.validator_id,
                            "role": g.role.as_str(),
                            "node_profile": g.config.node_profile.as_str(),
                            "profile_status": g.config.profile_status_json(),
                            "single_packer_per_round": g.config.single_packer_per_round,
                            "conf_pool_enabled": g.config.conf_pool_enabled,
                        })
                    }),
                );
            }
            contract
        }
        "batch_status" => {
            let h = params
                .get("batch_hash")
                .or_else(|| params.get("hash"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            if h.is_empty() {
                serde_json::json!({ "error": "batch_hash required" })
            } else {
                // Optional dig cross-check inputs (when omitted, use local view).
                let peer_body_param = params
                    .get("any_peer_has_body")
                    .and_then(|x| x.as_bool());
                let peer_signed_param = params
                    .get("any_peer_signed")
                    .and_then(|x| x.as_bool());
                engine.with_engine(|g| {
                    let has_body = g.dag.batches.contains_key(h);
                    let certified = g.dag.certificates.contains_key(h);
                    let conf_pool = g.conf_pool.contains(h);
                    if let Some(b) = g.dag.batches.get(h) {
                        let nsigs = b.signatures.len();
                        let foreign_n = b
                            .signatures
                            .keys()
                            .filter(|s| *s != &b.creator)
                            .count();
                        let any_peer_has_body = peer_body_param.unwrap_or(has_body);
                        // Local approximation: foreign sigs imply peers signed.
                        let any_peer_signed =
                            peer_signed_param.unwrap_or(foreign_n >= 1);
                        let gap = BatchDaGapClass::classify(
                            has_body,
                            certified,
                            nsigs,
                            any_peer_has_body,
                            any_peer_signed,
                        );
                        serde_json::json!({
                            "batch_hash": h,
                            "has_body": true,
                            "certified": certified,
                            "conf_pool": conf_pool,
                            "creator": b.creator,
                            "nsigs": nsigs,
                            "foreign_sigs": foreign_n,
                            "signers": b.signatures.keys().cloned().collect::<Vec<_>>(),
                            "my_sig": b.signatures.contains_key(&g.validator_id),
                            "ntx": b.tx_ids.len(),
                            "gap_class": gap.as_str(),
                            "gap_class_inputs": {
                                "any_peer_has_body": any_peer_has_body,
                                "any_peer_signed": any_peer_signed,
                            },
                        })
                    } else {
                        let any_peer_has_body = peer_body_param.unwrap_or(false);
                        let any_peer_signed = peer_signed_param.unwrap_or(false);
                        let gap = BatchDaGapClass::classify(
                            false,
                            certified,
                            0,
                            any_peer_has_body,
                            any_peer_signed,
                        );
                        serde_json::json!({
                            "batch_hash": h,
                            "has_body": false,
                            "certified": certified,
                            "conf_pool": conf_pool,
                            "gap_class": gap.as_str(),
                            "gap_class_inputs": {
                                "any_peer_has_body": any_peer_has_body,
                                "any_peer_signed": any_peer_signed,
                            },
                        })
                    }
                })
            }
        }
        "snapshot" => {

            let path = params
                .get("path")
                .and_then(|x| x.as_str())
                .map(PathBuf::from)
                .or(data_dir);
            engine.with_engine(|g| {
                match path {
                    Some(p) => match g.save_snapshot(&p) {
                        Ok(()) => serde_json::json!({
                            "ok": true,
                            "path": p.join("snapshot.json").display().to_string(),
                            "executed_txs": g.execution.executed_txs.len(),
                            "batches_created": g.batches_created,
                        }),
                        Err(e) => serde_json::json!({ "error": e.to_string() }),
                    },
                    None => serde_json::json!({
                        "ok": false,
                        "error": "no data_dir; pass --data-dir or params.path",
                        "executed_txs": g.execution.executed_txs.len(),
                    }),
                }
            })
        }
        "submit" => {
            let account = params.get("account").and_then(|x| x.as_str()).unwrap_or("");
            let sequence = params.get("sequence").and_then(|x| x.as_u64()).unwrap_or(0);
            let destination = params
                .get("destination")
                .and_then(|x| x.as_str())
                .unwrap_or("acct1");
            let amount = params.get("amount").and_then(|x| x.as_u64()).unwrap_or(1);
            let fee = params.get("fee").and_then(|x| x.as_u64()).unwrap_or(10);
            let asset = params
                .get("asset")
                .and_then(|x| x.as_str())
                .unwrap_or(fd_types::NATIVE_ASSET);
            let kind = params
                .get("kind")
                .and_then(|x| x.as_str())
                .unwrap_or("payment");
            let max_supply = params.get("max_supply").and_then(|x| x.as_u64());
            let now = now_s();
            let mut tx = match kind {
                "transfer" => Transaction::transfer(
                    account, sequence, destination, asset, amount, fee, now,
                ),
                "issue" => Transaction::issue(
                    account,
                    sequence,
                    asset,
                    amount,
                    destination,
                    fee,
                    max_supply,
                    now,
                ),
                "mint" => {
                    Transaction::mint(account, sequence, asset, destination, amount, fee, now)
                }
                "burn" => Transaction::burn(account, sequence, asset, amount, fee, now),
                "open_account_type" | "open_type" => {
                    let ty = match params
                        .get("account_type")
                        .and_then(|x| x.as_str())
                        .unwrap_or("vault")
                    {
                        "timed" => fd_types::AccountType::Timed,
                        "pool" => fd_types::AccountType::Pool,
                        "lending" | "lending_market" => fd_types::AccountType::LendingMarket,
                        _ => fd_types::AccountType::Vault,
                    };
                    Transaction::open_account_type(account, sequence, ty, fee, now)
                }
                "set_allowlist" => {
                    let dests: Vec<String> = params
                        .get("destinations")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_else(|| {
                            if destination.is_empty() {
                                vec![]
                            } else {
                                vec![destination.to_string()]
                            }
                        });
                    Transaction::set_allowlist(account, sequence, dests, fee, now)
                }
                "vault_lock" | "hard_lock" => {
                    Transaction::vault_lock(account, sequence, fee, now)
                }
                "set_limits" => Transaction::set_limits(
                    account,
                    sequence,
                    amount,
                    max_supply,
                    fee,
                    now,
                ),
                "create_pool" => {
                    let asset_b = params
                        .get("asset_b")
                        .and_then(|x| x.as_str())
                        .unwrap_or("GOLD");
                    let amount_b = params.get("amount_b").and_then(|x| x.as_u64()).unwrap_or(0);
                    Transaction::create_pool(
                        account, sequence, destination, asset, asset_b, amount, amount_b, fee, now,
                    )
                }
                "add_liquidity" => {
                    let amount_b = params.get("amount_b").and_then(|x| x.as_u64()).unwrap_or(0);
                    Transaction::add_liquidity(
                        account, sequence, destination, amount, amount_b, fee, now,
                    )
                }
                "remove_liquidity" => {
                    let min_a = params.get("min_out").and_then(|x| x.as_u64()).unwrap_or(0);
                    let min_b = params.get("amount_b").and_then(|x| x.as_u64()).unwrap_or(0);
                    Transaction::remove_liquidity(
                        account, sequence, destination, amount, min_a, min_b, fee, now,
                    )
                }
                "swap" => {
                    let min_out = params.get("min_out").and_then(|x| x.as_u64()).unwrap_or(0);
                    Transaction::swap(
                        account, sequence, destination, asset, amount, min_out, fee, now,
                    )
                }
                "supply" => Transaction::supply(account, sequence, destination, amount, fee, now),
                "withdraw_supply" => {
                    Transaction::withdraw_supply(account, sequence, destination, amount, fee, now)
                }
                "borrow" => {
                    let coll = params.get("amount_b").and_then(|x| x.as_u64()).unwrap_or(0);
                    let coll_asset = params
                        .get("asset_b")
                        .or_else(|| params.get("collateral_asset"))
                        .and_then(|x| x.as_str())
                        .unwrap_or(fd_types::NATIVE_ASSET);
                    Transaction::borrow_with_collateral(
                        account, sequence, destination, amount, coll_asset, coll, fee, now,
                    )
                }
                "repay" => Transaction::repay(account, sequence, destination, amount, fee, now),
                "liquidate" => {
                    let borrower = params
                        .get("borrower")
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    Transaction::liquidate(
                        account, sequence, destination, borrower, amount, fee, now,
                    )
                }
                "oracle_vote" | "oracle" => {
                    let price = params
                        .get("price")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(amount);
                    Transaction::oracle_vote(account, sequence, asset, price, fee, now)
                }
                "create_market" => {
                    let mut t =
                        Transaction::create_market(account, sequence, destination, asset, fee, now);
                    // Optional multi-collateral allowlist: collateral_assets: ["FDAG","GOLD"]
                    if let Some(arr) = params.get("collateral_assets").and_then(|x| x.as_array()) {
                        t.destinations = arr
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect();
                    }
                    t
                }
                "submit_rail_header" | "rail_header" => {
                    let proof = parse_bridge_proof(&params);
                    Transaction::submit_rail_header(account, sequence, asset, proof, fee, now)
                }
                "rail_deposit" => {
                    let proof = parse_bridge_proof(&params);
                    Transaction::rail_deposit(
                        account, sequence, asset, destination, amount, proof, fee, now,
                    )
                }
                "rail_withdraw" => {
                    let external_to = params
                        .get("external_to")
                        .or_else(|| params.get("to"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    Transaction::rail_withdraw(
                        account, sequence, asset, amount, external_to, fee, now,
                    )
                }
                _ => Transaction::payment(account, sequence, destination, amount, fee, now),
            };
            if let Some(sig) = params.get("signature").and_then(|x| x.as_str()) {
                tx.signature = Some(sig.to_string());
            }
            if let Some(pk) = params.get("public_key").and_then(|x| x.as_str()) {
                tx.public_key = Some(pk.to_string());
            }
            let tx_id = tx.tx_id.clone();
            let (result, events) = engine.with_engine(|g| {
                let result = g.submit_tx(tx);
                let mut events = GossipEvents::default();
                if result.is_ok() {
                    if let Some(full) = g.dag.tx_index.get(&tx_id) {
                        let multi = !g.config.single_packer_per_round;
                        if multi {
                            if g.owns_tx(full, now) {
                                // Owner already has it — pack on tick, no flood.
                            } else if let Some(owner) = g.account_owner_id(&full.account) {
                                // Immediate transfer handoff to lane owner.
                                events.directed_txs.push((owner, full.clone()));
                                g.mempool.remove(&tx_id);
                                g.shard_flow.directed_to_owner =
                                    g.shard_flow.directed_to_owner.saturating_add(1);
                            } else {
                                events.txs.push(full.clone());
                            }
                        } else {
                            events.txs.push(full.clone());
                        }
                    }
                    // Do NOT force-pack on every submit (that yields 1-tx batches).
                    // Narwhal-style: tick loop / remote cert path packs asynchronously.
                    // Only pack here if many ready txs already queued (fat DA batch).
                    if g.mempool.len() >= 16 {
                        if let Some((_, more)) = g.create_batch(now, false) {
                            events.batches.extend(more.batches);
                            events.sigs.extend(more.sigs);
                            events.certs.extend(more.certs);
                        }
                    }
                }
                g.advance();
                (result, events)
            });
            if let Some(ref m) = mesh {
                broadcast_events(m, &validator_id, events).await;
            }
            serde_json::json!({
                "engine_result": result.engine_code(),
                "engine_detail": result.reason_key(),
                "retryable": result.is_retryable(),
                "tx_id": tx_id,
                "kind": kind,
            })
        }
        "account_info" => {
            let account = params
                .get("account")
                .and_then(|x| x.as_str())
                .unwrap_or("acct1");
            engine.with_engine(|g| {
                let bals = g
                    .execution
                    .state
                    .balances
                    .get(account)
                    .cloned()
                    .unwrap_or_default();
                let seq = g
                    .execution
                    .state
                    .sequences
                    .get(account)
                    .copied()
                    .unwrap_or(1);
                let pol = g.execution.state.policy(account);
                serde_json::json!({
                    "account": account,
                    "sequence": seq,
                    "balances": bals,
                    "native": g.execution.state.native_balance(account),
                    "account_type": format!("{:?}", pol.account_type).to_lowercase(),
                    "hard_locked": pol.hard_locked,
                    "allowlist": pol.allowlist,
                    "max_per_tx": pol.max_per_tx,
                    "max_per_window": pol.max_per_window,
                })
            })
        }
        "asset_info" => {
            let asset = params
                .get("asset")
                .and_then(|x| x.as_str())
                .unwrap_or(fd_types::NATIVE_ASSET);
            engine.with_engine(|g| {
                match g.execution.state.assets.get(asset) {
                    Some(meta) => serde_json::json!({
                        "asset": asset,
                        "issuer": meta.issuer,
                        "total_supply": meta.total_supply,
                        "max_supply": meta.max_supply,
                    }),
                    None => serde_json::json!({ "error": "unknown asset", "asset": asset }),
                }
            })
        }
        "economy" => {
            engine.with_engine(|g| {
                let st = &g.execution.state;
                serde_json::json!({
                    "native_asset": g.config.economics.native_asset,
                    "native_supply": st.total_supply(&g.config.economics.native_asset),
                    "burned_native": st.burned_native,
                    "treasury_fees": st.treasury_fees,
                    "emissions_minted": st.emissions_minted,
                    "treasury_balance": st.native_balance(&g.config.economics.treasury),
                    "fee_burn_bps": g.config.economics.fee_burn_bps,
                    "fee_treasury_bps": g.config.economics.fee_treasury_bps,
                    "protocol_fees_skimmed": st.protocol_fees_skimmed,
                    "oracle_prices": st.oracle_prices,
                    "rails": st.rails,
                    "assets": st.assets,
                    "pools": st.pools,
                    "markets": st.markets,
                    "state_root": st.state_root(),
                })
            })
        }
        "rail_info" => {
            let rail = params
                .get("rail")
                .or_else(|| params.get("asset"))
                .and_then(|x| x.as_str())
                .unwrap_or("BTC");
            engine.with_engine(|g| {
                match g.execution.state.rails.get(rail) {
                    Some(r) => {
                        let notes = fd_exec::sorted_withdrawals(
                            &g.execution.state.rail_withdrawals,
                            Some(rail),
                        );
                        let live_root = fd_exec::withdrawal_claim_root(&notes);
                        serde_json::json!({
                            "rail": rail,
                            "family": format!("{:?}", r.family).to_lowercase(),
                            "min_confirmations": r.min_confirmations,
                            "bridge_account": r.bridge_account,
                            "lock_id": r.lock_id,
                            "tip_height": r.tip_height,
                            "tip_hash": r.tip_hash,
                            "tip_work": r.tip_work.to_string(),
                            "total_minted": r.total_minted,
                            "total_burned": r.total_burned,
                            "supply": g.execution.state.total_supply(rail),
                            "open_withdrawals": notes.len(),
                            "claim_root": r.claim_root,
                            "claim_root_live": live_root,
                            "claim_root_matches": r.claim_root == live_root || (r.claim_root.is_empty() && notes.is_empty()),
                            "claim_note_count": r.claim_note_count,
                            "claim_height": r.claim_height,
                            "mint_path": "spv_deposit_only",
                            "federated_mint": false,
                            "committee": null,
                            "key_can_payment_bridge": false,
                        })
                    }
                    None => serde_json::json!({ "error": "unknown rail", "rail": rail }),
                }
            })
        }
        "withdrawal_notes" => {
            let rail = params.get("rail").and_then(|x| x.as_str());
            engine.with_engine(|g| {
                let notes = fd_exec::sorted_withdrawals(&g.execution.state.rail_withdrawals, rail);
                let claim_root = fd_exec::withdrawal_claim_root(&notes);
                let list: Vec<_> = notes
                    .iter()
                    .map(|n| {
                        serde_json::json!({
                            "note_id": n.note_id,
                            "rail": n.rail,
                            "amount": n.amount,
                            "from": n.from,
                            "external_to": n.external_to,
                            "falcon_height": n.falcon_height,
                            "claim_leaf": fd_exec::withdrawal_claim_leaf(n),
                        })
                    })
                    .collect();
                serde_json::json!({
                    "rail": rail,
                    "count": list.len(),
                    "claim_root": claim_root,
                    "notes": list,
                    "committee": null,
                })
            })
        }
        "withdrawal_info" | "claim_export" => {
            let note_id = params
                .get("note_id")
                .or_else(|| params.get("id"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            engine.with_engine(|g| {
                let Some(note) = g.execution.state.rail_withdrawals.get(note_id) else {
                    return serde_json::json!({ "error": "unknown note", "note_id": note_id });
                };
                let rail = note.rail.as_str();
                let notes = fd_exec::sorted_withdrawals(&g.execution.state.rail_withdrawals, Some(rail));
                let (path, idx, root) = fd_exec::withdrawal_claim_proof(&notes, note_id)
                    .unwrap_or_else(|| (vec![], 0u32, String::new()));
                let committed = g
                    .execution
                    .state
                    .rails
                    .get(rail)
                    .map(|r| r.claim_root.clone())
                    .unwrap_or_default();
                let state_root = g.execution.state.state_root();
                serde_json::json!({
                    "note_id": note.note_id,
                    "rail": note.rail,
                    "amount": note.amount,
                    "from": note.from,
                    "external_to": note.external_to,
                    "falcon_height": note.falcon_height,
                    "claim_leaf": fd_exec::withdrawal_claim_leaf(note),
                    "claim_root": root,
                    "claim_root_committed": committed,
                    "claim_root_matches_commit": root == committed,
                    "merkle_path": path,
                    "merkle_index": idx,
                    "state_root": state_root,
                    "applied_user_txs": g.execution.state.applied_user_txs,
                    "executed_txs": g.execution.executed_txs.len(),
                    "hard_final_batches": g.ordering.hard_final.len(),
                    "family": g.execution.state.rails.get(rail).map(|r| format!("{:?}", r.family).to_lowercase()),
                    "lock_id": g.execution.state.rails.get(rail).map(|r| r.lock_id.clone()),
                    "finality": {
                        "note_height": note.falcon_height,
                        "applied_user_txs": g.execution.state.applied_user_txs,
                        "hard_final_batches": g.ordering.hard_final.len(),
                        "state_root": state_root,
                        "ready_for_foreign_claim": root == committed && !committed.is_empty(),
                    },
                    "evm_claim_hint": {
                        "submitFalconRoot": root,
                        "height": note.falcon_height,
                        "noteId_bytes32": format!("0x{}", note.note_id),
                        "to": note.external_to,
                        "amount": note.amount,
                        "pathIndex": idx,
                        "path": path,
                        "codec": "falcon_claim_sha256_v1",
                        "tool": "scripts/rail_indexers/evm_outbound_claim.py",
                        "bind_state_root": state_root,
                    },
                    "committee": null,
                })
            })
        }
        "bridge_status" => {
            engine.with_engine(|g| {
                let st = &g.execution.state;
                let mut rails = serde_json::Map::new();
                for (name, r) in &st.rails {
                    let notes =
                        fd_exec::sorted_withdrawals(&st.rail_withdrawals, Some(name.as_str()));
                    let live = fd_exec::withdrawal_claim_root(&notes);
                    rails.insert(
                        name.clone(),
                        serde_json::json!({
                            "family": format!("{:?}", r.family).to_lowercase(),
                            "tip_height": r.tip_height,
                            "total_minted": r.total_minted,
                            "total_burned": r.total_burned,
                            "supply": st.total_supply(name),
                            "claim_root": r.claim_root,
                            "claim_root_live": live,
                            "claim_root_matches": r.claim_root == live
                                || (r.claim_root.is_empty() && notes.is_empty()),
                            "claim_note_count": r.claim_note_count,
                            "claim_height": r.claim_height,
                            "open_withdrawals": notes.len(),
                            "lock_id": r.lock_id,
                            "federated_mint": false,
                            "committee": null,
                        }),
                    );
                }
                serde_json::json!({
                    "state_root": st.state_root(),
                    "applied_user_txs": st.applied_user_txs,
                    "executed_txs": g.execution.executed_txs.len(),
                    "hard_final_batches": g.ordering.hard_final.len(),
                    "soft_final_txs": g.execution.soft_final_txs.len(),
                    "ordered_batches": g.ordering.ordered.len(),
                    "rails": rails,
                    "finality_hint": "Agents should wait until withdraw is in executed_txs and hard_final_batches has advanced before submitting claim roots on foreign chains",
                    "committee": null,
                })
            })
        }
        "oracle_info" => {
            let asset = params
                .get("asset")
                .and_then(|x| x.as_str())
                .unwrap_or(fd_types::NATIVE_ASSET);
            engine.with_engine(|g| {
                let st = &g.execution.state;
                let votes = st.oracle_votes.get(asset).cloned().unwrap_or_default();
                serde_json::json!({
                    "asset": asset,
                    "price_1e6": st.oracle_price(asset),
                    "votes": votes,
                    "vote_count": votes.len(),
                    "scale": fd_exec::ORACLE_PRICE_SCALE,
                })
            })
        }
        "pool_info" => {
            let pool = params
                .get("pool")
                .or_else(|| params.get("account"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            engine.with_engine(|g| {
                match g.execution.state.pools.get(pool) {
                    Some(p) => serde_json::json!({
                        "pool": pool,
                        "asset_a": p.asset_a,
                        "asset_b": p.asset_b,
                        "lp_asset": p.lp_asset,
                        "fee_bps": p.fee_bps,
                        "protocol_fee_share_bps": p.protocol_fee_share_bps,
                        "reserve_a": g.execution.state.balance(pool, &p.asset_a),
                        "reserve_b": g.execution.state.balance(pool, &p.asset_b),
                        "lp_supply": g.execution.state.total_supply(&p.lp_asset),
                        "account_type": "pool",
                        "key_can_payment": false,
                    }),
                    None => serde_json::json!({ "error": "unknown pool", "pool": pool }),
                }
            })
        }
        "market_info" => {
            let market = params
                .get("market")
                .or_else(|| params.get("account"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            engine.with_engine(|g| {
                match g.execution.state.markets.get(market) {
                    Some(m) => serde_json::json!({
                        "market": market,
                        "asset": m.asset,
                        "ltv_bps": m.ltv_bps,
                        "liq_bonus_bps": m.liq_bonus_bps,
                        "supply_index": m.supply_index,
                        "borrow_index": m.borrow_index,
                        "base_rate_bps": m.base_rate_bps,
                        "slope_bps": m.slope_bps,
                        "allowed_collateral": m.allowed_collateral,
                        "total_supplied": g.execution.state.total_supplied_underlying(market),
                        "total_borrowed": g.execution.state.total_borrowed_underlying(market),
                        "oracle_price_1e6": g.execution.state.oracle_price(&m.asset),
                        "reserve": g.execution.state.balance(market, &m.asset),
                        "account_type": "lending_market",
                        "key_can_payment": false,
                    }),
                    None => serde_json::json!({ "error": "unknown market", "market": market }),
                }
            })
        }
        "canonical" => {
            engine.with_engine(|g| {
                serde_json::json!({
                    "ordered_batches": g.ordering.ordered,
                    "txs": g.execution.executed_txs,
                    "hard_final": g.ordering.hard_final.iter().collect::<Vec<_>>(),
                })
            })
        }
        "dag_status" => {
            let mut m = engine.metrics_json();
            if let Some(ref mesh) = mesh {
                m["p2p_peers"] = serde_json::json!(mesh.peer_count().await);
            }
            m
        }
        // Re-gossip all non-genesis batches (catch-up for rejoining peers).
        "catchup" => {
            let Some(ref m) = mesh else {
                return serde_json::json!({ "error": "p2p off" });
            };
            let (batches, n_sigs) = engine.with_engine(|g| {
                let mut batches = Vec::new();
                for (bh, batch) in &g.dag.batches {
                    if bh == &fd_types::genesis_batch_hash() {
                        continue;
                    }
                    batches.push(batch.clone());
                }
                let n_sigs = batches.iter().map(|b| b.signatures.len()).sum::<usize>();
                (batches, n_sigs)
            });
            let n = batches.len();
            for batch in batches {
                m.broadcast_da_must(&WireMsg::batch(&validator_id, batch)).await;
            }
            serde_json::json!({
                "ok": true,
                "batches_broadcast": n,
                "signature_entries": n_sigs,
            })
        }
        _ => serde_json::json!({ "error": format!("unknown method: {method}") }),
    }
}
