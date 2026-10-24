//! Core types for Falcon DAG (hashes, txs, batches, certs, config, economics).

mod architecture;

pub use architecture::{
    architecture_contract_json, BatchDaGapClass, DigReliabilityBaseline, MetricKey, MetricTier,
    NodeProfile, ProfileSubsystems, ARCHITECTURE_STRING, DIG_BASELINES, FROZEN_METRIC_KEYS,
    IMPLEMENTATION_GUIDE_PATH,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

pub type HashHex = String;
pub type NodeId = String;
pub type AccountId = String;
pub type AssetId = String;

/// Process role in a real Narwhal-style split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NodeRole {
    /// Data-availability worker only: mempool → batches → signatures → availability certs.
    /// Does **not** order certificates or execute transactions.
    DaWorker,
    /// Light ledger: order **certificates only**, then execute payloads from the DA store.
    /// Does **not** pack batches from the mempool.
    OrderExec,
    /// Both roles in one process (separate code paths / ticks). Default for simple deploys.
    #[default]
    Full,
}

impl NodeRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DaWorker => "da-worker",
            Self::OrderExec => "order-exec",
            Self::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "da" | "da-worker" | "worker" | "narwhal-da" => Some(Self::DaWorker),
            "order" | "order-exec" | "ledger" | "exec" => Some(Self::OrderExec),
            "full" | "both" | "all" => Some(Self::Full),
            _ => None,
        }
    }

    pub fn runs_da(self) -> bool {
        matches!(self, Self::DaWorker | Self::Full)
    }

    pub fn runs_ledger(self) -> bool {
        matches!(self, Self::OrderExec | Self::Full)
    }
}

pub const GENESIS_BATCH_LABEL: &str = "falcon-dag-genesis-batch-v1";
pub const GENESIS_CERT_LABEL: &str = "falcon-dag-genesis-cert-v1";

/// Native chain asset (fees paid in this). Substrate symbol (engineering).
pub const NATIVE_ASSET: &str = "FDAG";
/// Public-network token name (PR9 freeze): same unit as FDAG substrate until
/// a hard rename migration; operators and docs use **FALC** publicly.
pub const PUBLIC_TOKEN_SYMBOL: &str = "FALC";
/// Fixed public supply target (NETWORK_ECONOMICS_AND_BOOTSTRAP).
pub const PUBLIC_TOKEN_SUPPLY: u64 = 200_000_000_000; // 200B
/// Protocol-controlled treasury share of fixed supply (98%).
pub const TREASURY_SUPPLY_BPS: u16 = 9_800;
/// System treasury account receiving fee share + emissions.
pub const TREASURY_ACCOUNT: &str = "treasury";

// ---------------------------------------------------------------------------
// Cross-chain rails (protocol SPV bridge — not federated custody)
// ---------------------------------------------------------------------------

/// Genesis rail asset codes (wrapped claims; minted only via verified deposit proofs).
pub const RAIL_BTC: &str = "BTC";
pub const RAIL_ETH: &str = "ETH";
pub const RAIL_BNB: &str = "BNB";
pub const RAIL_XRP: &str = "XRP";

/// All genesis SPV rails.
pub const GENESIS_RAILS: &[&str] = &[RAIL_BTC, RAIL_ETH, RAIL_BNB, RAIL_XRP];

/// Sealed bridge accounting account for a rail (`bridge:BTC`, …).
pub fn bridge_account(rail: &str) -> AccountId {
    format!("bridge:{rail}")
}

/// Protocol issuer id for rail assets (no human key can mint).
pub const BRIDGE_ISSUER: &str = "protocol:spv-bridge";

/// External chain family for light-client rules.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RailFamily {
    /// Bitcoin-style headers + tx merkle (BTC).
    Bitcoin,
    /// EVM block headers + receipt/tx trie root (ETH, BNB).
    Evm,
    /// XRPL ledger headers + tx inclusion (XRP).
    XrpLedger,
}

impl RailFamily {
    pub fn for_rail(rail: &str) -> Option<Self> {
        match rail {
            RAIL_BTC => Some(Self::Bitcoin),
            RAIL_ETH | RAIL_BNB => Some(Self::Evm),
            RAIL_XRP => Some(Self::XrpLedger),
            _ => None,
        }
    }
}

/// SPV / light-client proof payload attached to bridge txs (consensus-critical when present).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BridgeProof {
    /// External chain transaction / payment id (hex or base58 as string).
    #[serde(default)]
    pub external_txid: String,
    /// Block / ledger hash that includes the lock tx.
    #[serde(default)]
    pub block_hash: String,
    /// Height or ledger index of `block_hash`.
    #[serde(default)]
    pub height: u64,
    /// Merkle (or receipt) branch: sibling hashes from leaf toward root.
    #[serde(default)]
    pub merkle_path: Vec<String>,
    /// Leaf index among txs in the block/ledger.
    #[serde(default)]
    pub merkle_index: u32,
    /// Protocol lock commitment: vault script hash, contract address, or escrow id.
    #[serde(default)]
    pub lock_id: String,
    /// Parent header hash (SubmitRailHeader).
    #[serde(default)]
    pub parent_hash: String,
    /// Merkle / deposit-tree root (BTC: tx merkle; EVM deposit proofs: deposit tree root).
    /// On SubmitRailHeader for EVM, this is the **bind root** sha256(tx_root‖deposit_root).
    #[serde(default)]
    pub merkle_root: String,
    /// EVM transactionsRoot (hex); empty for BTC. Used to bind deposit trees to real blocks.
    #[serde(default)]
    pub tx_root: String,
    /// Cumulative work / total difficulty (decimal string, compared as u128).
    #[serde(default)]
    pub work: String,
    /// External recipient address for withdrawals (BTC/ETH/… address string).
    #[serde(default)]
    pub external_to: String,
}

impl BridgeProof {
    pub fn work_u128(&self) -> u128 {
        self.work.parse().unwrap_or(0)
    }
}

pub fn sha256_hex(data: impl AsRef<[u8]>) -> HashHex {
    let mut h = Sha256::new();
    h.update(data.as_ref());
    hex::encode(h.finalize())
}

pub fn genesis_batch_hash() -> HashHex {
    sha256_hex(GENESIS_BATCH_LABEL)
}

pub fn genesis_cert_id() -> HashHex {
    sha256_hex(GENESIS_CERT_LABEL)
}

// ---------------------------------------------------------------------------
// Economics
// ---------------------------------------------------------------------------

/// Fee split + genesis native distribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EconomicConfig {
    pub native_asset: AssetId,
    pub treasury: AccountId,
    /// Basis points of each fee burned (0..=10000).
    pub fee_burn_bps: u16,
    /// Basis points of each fee to treasury; burn+treasury should be <= 10000.
    pub fee_treasury_bps: u16,
    /// Faucet accounts acct1..N at genesis.
    pub genesis_accounts: u32,
    /// Native balance per faucet account at genesis.
    pub genesis_native_per_account: u64,
    /// Optional emission schedule (applied by height index in ordered non-genesis batches).
    pub emission_schedule: Vec<EmissionEntry>,
}

impl Default for EconomicConfig {
    fn default() -> Self {
        // PR9 freeze: 50% burn / 30% treasury / 20% residual (service-node pathway deferred;
        // residual held as burn-equivalent until service rewards land — no admin spend).
        Self {
            native_asset: NATIVE_ASSET.into(),
            treasury: TREASURY_ACCOUNT.into(),
            fee_burn_bps: 5_000,     // 50% burn
            fee_treasury_bps: 3_000, // 30% treasury
            genesis_accounts: 100,
            genesis_native_per_account: 1_000_000_000,
            emission_schedule: vec![
                // demo: small mint to treasury every 10 ordered non-genesis batches
                EmissionEntry {
                    every_n_batches: 10,
                    amount: 1_000,
                    to: TREASURY_ACCOUNT.into(),
                },
            ],
        }
    }
}

impl EconomicConfig {
    pub fn genesis_native_supply(&self) -> u64 {
        self.genesis_accounts as u64 * self.genesis_native_per_account
    }

    /// Residual bps after burn+treasury (≤ 10000). PR9: service-node path deferred.
    pub fn fee_residual_bps(&self) -> u16 {
        10_000u16
            .saturating_sub(self.fee_burn_bps)
            .saturating_sub(self.fee_treasury_bps)
    }

    /// Split a fee into burn / treasury / residual (deterministic, no admin).
    pub fn split_fee(&self, fee: u64) -> FeeSplit {
        let burn = fee.saturating_mul(self.fee_burn_bps as u64) / 10_000;
        let treasury = fee.saturating_mul(self.fee_treasury_bps as u64) / 10_000;
        let residual = fee.saturating_sub(burn).saturating_sub(treasury);
        FeeSplit {
            burn,
            treasury,
            residual,
        }
    }

    /// No-admin invariant: treasury is protocol account id only (string check).
    pub fn treasury_is_protocol_controlled(&self) -> bool {
        self.treasury == TREASURY_ACCOUNT
    }

    /// Emissions due when `ordered_non_genesis` advances to `height` (1-based count).
    pub fn emissions_at_height(&self, height: u64) -> Vec<(AccountId, u64)> {
        if height == 0 {
            return vec![];
        }
        let mut out = Vec::new();
        for e in &self.emission_schedule {
            if e.every_n_batches > 0 && height % e.every_n_batches == 0 {
                out.push((e.to.clone(), e.amount));
            }
        }
        out
    }

    /// Public freeze blob for RPC / digs (PR9).
    pub fn freeze_json(&self) -> serde_json::Value {
        serde_json::json!({
            "public_token": PUBLIC_TOKEN_SYMBOL,
            "substrate_asset": NATIVE_ASSET,
            "public_supply": PUBLIC_TOKEN_SUPPLY,
            "treasury_supply_bps": TREASURY_SUPPLY_BPS,
            "treasury_account": self.treasury,
            "treasury_protocol_controlled": self.treasury_is_protocol_controlled(),
            "fee_burn_bps": self.fee_burn_bps,
            "fee_treasury_bps": self.fee_treasury_bps,
            "fee_residual_bps": self.fee_residual_bps(),
            "service_node_rewards": "deferred",
            "admin_spend": false,
            "genesis_native_supply": self.genesis_native_supply(),
            "emission_entries": self.emission_schedule.len(),
        })
    }
}

/// Deterministic fee accounting result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeSplit {
    pub burn: u64,
    pub treasury: u64,
    pub residual: u64,
}

/// Recurring emission: every N hard-ordered non-genesis batches, mint `amount` native to `to`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmissionEntry {
    pub every_n_batches: u64,
    pub amount: u64,
    pub to: AccountId,
}

// ---------------------------------------------------------------------------
// Protocol config
// ---------------------------------------------------------------------------

/// Consensus-critical + local defaults (mirrors Python ProtocolConfig).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolConfig {
    pub unl_threshold: f64,
    pub max_batch_parents: usize,
    pub target_batch_interval_s: f64,
    pub max_batch_txs: usize,
    pub hard_finality_h: u64,
    pub starvation_boost_per_s: f64,
    pub network_id: u32,
    pub adaptive_interval: bool,
    pub batch_interval_min_s: f64,
    pub batch_interval_max_s: f64,
    pub adaptive_mempool_low: usize,
    pub adaptive_mempool_high: usize,
    pub worker_sharding: bool,
    pub shard_orphan_age_s: f64,
    pub require_sig_verify: bool,
    /// Require user-tx signatures (wallet Falcon/HMAC).
    pub require_tx_sigs: bool,
    pub crypto_scheme: String,
    pub max_known_batches: usize,
    pub max_uncertified_batches: usize,
    pub max_uncertified_per_creator: usize,
    /// Own open batches allowed while multipacking (0 = auto: 1 multipack / max_uncert single).
    /// Raising this bypasses open_cap=1 deadlock if body-sig loop stalls.
    pub pack_open_cap: usize,
    pub max_mempool_txs: usize,
    /// Max open sequences per account in the mempool beyond the next-to-apply.
    /// Admit allows `expected .. expected+window` (inclusive of expected, exclusive of end).
    /// Apply still requires exact `expected` sequence (ordered, no holes).
    pub account_sequence_window: u64,
    pub max_sigs_per_validator_per_s: f64,
    pub sig_rate_window_s: f64,
    /// Seconds before dropping uncertified batches. 0 disables prune entirely
    /// (preferred while multi-lane cert/body delivery is flaky).
    pub uncertified_ttl_s: f64,
    pub max_hard_final_history: usize,
    /// When true (default), only `sorted_unl[0]` creates batches (stable primary).
    /// Prevents multi-sibling same-seq forks. Off for partition / multi-packer tests.
    /// Note: must not key packer off local max_cert_round (mesh lag → zero packers).
    pub single_packer_per_round: bool,
    /// If the assigned packer's open work is stale this long (seconds) and tips
    /// still have not advanced, the next-in-ring secondary may pack. Set high
    /// enough that normal mesh certify finishes first; used only for liveness.
    pub packer_stall_timeout_s: f64,
    /// Packing backpressure gate on ordered-lag (certified-but-unordered work).
    /// `0` (default) DISABLES the gate; when `> 0`, `create_batch` returns `None`
    /// while `ordered_lag` exceeds this threshold. Measurement/liveness only —
    /// does not affect consensus, ordering, or execution semantics.
    pub pack_backpressure_max: usize,
    /// Optional packer skip-hole liveness (Phase 49c). `0` = disabled (default).
    /// When `> 0`, if the next pack sequence for an account is missing from the
    /// mempool but higher sequences have been waiting at least this many seconds,
    /// advance pack cursor **and** execution next-seq watermark past the hole so
    /// packing/apply can proceed (abandons missing sequences; they are treated as
    /// never arriving). Off by default — enable only when holes are expected
    /// (e.g. after RPC flakiness) or for load-test liveness.
    pub pack_skip_hole_after_s: f64,
    /// Pack-ahead cap: refuse to pack if `pack_cursor - exec_next > cap` for any account.
    /// Prevents the DA cursor racing far ahead of ledger apply under multi-pack mesh lag.
    /// `0` = disabled (default). Recommended: 32 for multi-pack setups.
    pub pack_ahead_cap: u64,
    /// Admit circuit breaker (MCB): when true, local/RPC admits fail with terBUSY
    /// when unprocessed work exceeds a window — i.e. sit at max sustainable load.
    /// Mesh handoffs bypass so transfer-to-owner is not dropped.
    pub admit_circuit_breaker: bool,
    /// Soft mempool high that trips the breaker. `0` = auto
    /// (`max(adaptive_mempool_high * 2, 256)`).
    pub admit_busy_mempool: usize,
    /// Age (seconds) of oldest non-genesis uncertified batch that trips the breaker.
    /// `0` = disabled. Default 8s — open batch not certifying under load.
    pub admit_busy_uncert_age_s: f64,
    /// Own open uncertified count that trips the breaker. `0` = disabled for count
    /// (age gate above still applies). Set >0 to also trip on open count alone.
    pub admit_busy_open_uncert: usize,
    /// **Primary throttle:** max local/RPC admits that may sit ahead of `executed_txs`.
    /// lag = local_admits_ok - executed; if lag >= cap → terBUSY.
    /// This is the "max TPS the node will queue" window (not a rate, a backlog cap).
    /// `0` = default 64.
    pub admit_ahead_cap: u64,
    /// Hysteresis low watermark: once open, stay busy until lag <= this.
    /// `0` = ahead_cap / 2.
    pub admit_ahead_resume: u64,
    /// Conf-pool + pack/check duty cycle. When true (default), multi-lane nodes
    /// alternate Pack/Check slots of `pack_check_slot_s` and deposit open batches
    /// into a logical conf pool for scheduled signing.
    pub conf_pool_enabled: bool,
    /// Half-cycle duration seconds (pack | check). 0.25 → 2 pack windows/s.
    pub pack_check_slot_s: f64,
    /// Abandon own open batches with zero foreign UNL sigs after this many
    /// seconds and re-insert their txs into the mempool (0 = disabled).
    /// Prevents stranded work when body DA never reaches peers.
    pub open_body_repool_after_s: f64,
    /// Operator node profile (validator | archival | rpc). Orthogonal to process
    /// [`NodeRole`]. PR1 freezes profiles; later PRs specialize transport/retention.
    #[serde(default)]
    pub node_profile: NodeProfile,
    pub economics: EconomicConfig,
}

impl ProtocolConfig {
    /// Apply conservative profile defaults without forcing transport extraction (PR1).
    /// Does not override knobs already set non-default unless `force` is true.
    pub fn apply_node_profile(&mut self, profile: NodeProfile, force: bool) {
        self.node_profile = profile;
        match profile {
            NodeProfile::Validator => {
                // Consensus participant: multipack + conf-pool; protect from public RPC load.
                if force {
                    self.single_packer_per_round = false;
                    self.conf_pool_enabled = true;
                    // Keep admit_ahead_cap modest so public flood does not bury DA.
                    self.admit_ahead_cap = 64;
                    self.admit_circuit_breaker = true;
                }
            }
            NodeProfile::Archival => {
                // Deep store + ledger; no pack duty. Pair with NodeRole::OrderExec.
                if force || self.max_hard_final_history < 100_000 {
                    self.max_hard_final_history = self.max_hard_final_history.max(100_000);
                }
                if force {
                    self.single_packer_per_round = true;
                    self.pack_open_cap = 0;
                    self.conf_pool_enabled = true; // still track/sign mesh honesty
                }
            }
            NodeProfile::Rpc => {
                // Client-facing admit + query; owner-route without pretending to pack.
                if force || self.admit_ahead_cap < 128 {
                    self.admit_ahead_cap = 128;
                }
                if force {
                    self.admit_circuit_breaker = true;
                    self.single_packer_per_round = true; // never multipack on RPC profile
                    self.pack_open_cap = 1;
                }
            }
        }
    }

    /// Operator-facing profile + subsystem map (PR6 server_info).
    pub fn profile_status_json(&self) -> serde_json::Value {
        let subs = self.node_profile.enabled_subsystems();
        serde_json::json!({
            "node_profile": self.node_profile.as_str(),
            "suggested_role": self.node_profile.suggested_node_role().as_str(),
            "subsystems": subs.to_json(),
            "notes": match self.node_profile {
                NodeProfile::Validator => "packs, signs, orders, executes; protect from public RPC",
                NodeProfile::Archival => "deep retention + replay; order/exec without pack duty",
                NodeProfile::Rpc => "admit + query + owner-route; do not take pack/order load",
            },
        })
    }
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            // 0.6 with 5 equal validators = 3-of-5 (faster certs than 4-of-5 at 0.8).
            unl_threshold: 0.60,
            // Phase B: real DAG width (amortize Falcon-512; was 1 = linear chain).
            max_batch_parents: 3,
            target_batch_interval_s: 0.05,
            // Fat batches amortize PQ signatures (core throughput thesis).
            max_batch_txs: 256,
            hard_finality_h: 6,
            starvation_boost_per_s: 0.01,
            network_id: 9002,
            // Phase B: adaptive interval on by default so fat batches form under load.
            adaptive_interval: true,
            batch_interval_min_s: 0.02,
            batch_interval_max_s: 0.4,
            adaptive_mempool_low: 16,
            adaptive_mempool_high: 256,
            worker_sharding: false,
            shard_orphan_age_s: 0.0,
            require_sig_verify: true,
            require_tx_sigs: false,
            crypto_scheme: "hmac-sha256-dev".into(),
            max_known_batches: 50_000,
            max_uncertified_batches: 2_000,
            // Pipeline DA: several open certs-in-flight on advancing tips.
            max_uncertified_per_creator: 4,
            // 0 = auto (multipack hard-capped at 1 until continuous body DA is solid).
            pack_open_cap: 0,
            max_mempool_txs: 100_000,
            account_sequence_window: 512,
            max_sigs_per_validator_per_s: 50.0,
            sig_rate_window_s: 1.0,
            // 0 = disable uncertified prune (avoids pack/prune/seq-hole spiral).
            uncertified_ttl_s: 0.0,
            max_hard_final_history: 10_000,
            // Single packer keeps multi-node agreement; sharding remains opt-in.
            single_packer_per_round: true,
            // ~8 DA ticks at 50ms — long enough for 3/5 cert under honest mesh.
            packer_stall_timeout_s: 0.4,
            // Disabled by default: no packing backpressure unless explicitly set > 0.
            pack_backpressure_max: 0,
            // Disabled by default: do not abandon sequence holes.
            pack_skip_hole_after_s: 0.0,
            // Disabled by default: no pack-ahead cap.
            pack_ahead_cap: 0,
            // MCB on by default: backlog window around executed (not open-count spam).
            admit_circuit_breaker: true,
            admit_busy_mempool: 0,  // auto soft full
            admit_busy_uncert_age_s: 0.0,
            admit_busy_open_uncert: 0,  // disabled count trip (use age / ahead cap)
            admit_ahead_cap: 64,  // ~max unexecuted local admits
            admit_ahead_resume: 0,  // auto = cap/2
            conf_pool_enabled: true,
            pack_check_slot_s: 0.25,
            // 8s: allow multipack/Falcon seed+cert; still unsticks stranded opens.
            open_body_repool_after_s: 8.0,
            node_profile: NodeProfile::Validator,
            economics: EconomicConfig::default(),
        }
    }
}

#[cfg(test)]
mod economics_tests {
    use super::*;

    #[test]
    fn fee_split_and_emission_deterministic() {
        let eco = EconomicConfig::default();
        assert_eq!(eco.fee_burn_bps, 5_000);
        assert_eq!(eco.fee_treasury_bps, 3_000);
        assert_eq!(eco.fee_residual_bps(), 2_000);
        let s = eco.split_fee(1000);
        assert_eq!(s.burn, 500);
        assert_eq!(s.treasury, 300);
        assert_eq!(s.residual, 200);
        assert!(eco.treasury_is_protocol_controlled());
        assert!(eco.emissions_at_height(10).len() >= 1);
        assert!(eco.emissions_at_height(9).is_empty());
        let j = eco.freeze_json();
        assert_eq!(j["public_token"], PUBLIC_TOKEN_SYMBOL);
        assert_eq!(j["admin_spend"], false);
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn node_profile_serde_and_apply() {
        let mut cfg = ProtocolConfig::default();
        assert_eq!(cfg.node_profile, NodeProfile::Validator);
        cfg.apply_node_profile(NodeProfile::Archival, false);
        assert_eq!(cfg.node_profile, NodeProfile::Archival);
        assert!(cfg.max_hard_final_history >= 100_000);

        let mut rpc = ProtocolConfig::default();
        rpc.apply_node_profile(NodeProfile::Rpc, true);
        assert_eq!(rpc.node_profile, NodeProfile::Rpc);
        assert_eq!(rpc.admit_ahead_cap, 128);
        assert!(rpc.admit_circuit_breaker);
        let subs = NodeProfile::Rpc.enabled_subsystems();
        assert!(subs.admit && subs.public_rpc);
        assert!(!subs.pack && !subs.order);
        let arch = NodeProfile::Archival.enabled_subsystems();
        assert!(arch.deep_store && arch.order && !arch.pack);
        let st = rpc.profile_status_json();
        assert_eq!(st["node_profile"], "rpc");
        assert!(st["subsystems"]["public_rpc"].as_bool().unwrap());

        let d = ProtocolConfig::default();
        let v = serde_json::to_value(&d).expect("ser");
        assert_eq!(v["node_profile"], "validator");
        let back: ProtocolConfig = serde_json::from_value(v).expect("de");
        assert_eq!(back.node_profile, NodeProfile::Validator);
    }
}

// ---------------------------------------------------------------------------
// Account types & guards (protocol-enforced)
// ---------------------------------------------------------------------------

/// Protocol account class. Set at open/conversion; Vault/Pool/Lending are hard-constrained.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountType {
    /// Normal spendable account (no destination lock).
    #[default]
    Hot,
    /// Configurable guard: allowlist + optional velocity; changes may be time-locked later.
    Timed,
    /// Cold/treasury: receive-only until destinations set; permanent hard lock after `vault_lock`.
    Vault,
    /// AMM / LP vault: outflows only to allowlisted program destinations (LP withdraw / swap out).
    Pool,
    /// Lending market vault: outflows only via protocol market ops (repay/withdraw/liquidate paths).
    LendingMarket,
}

pub const VAULT_MAX_DESTINATIONS: usize = 5;

// ---------------------------------------------------------------------------
// Typed transactions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TxKind {
    /// Native asset payment (legacy-compatible hashing for amount/fee).
    #[default]
    Payment,
    /// Transfer any asset (including native) amount to destination; fee in native.
    Transfer,
    /// Create a new asset code; issuer = account; initial mint `amount` to destination.
    Issue,
    /// Issuer mints `amount` of existing asset to destination.
    Mint,
    /// Holder burns `amount` of asset from their balance.
    Burn,
    /// Convert Hot → Vault/Timed/Pool/LendingMarket (once). `asset` unused; type in `account_type`.
    OpenAccountType,
    /// Set destination allowlist (Vault: only while unlocked; max 5). Destinations in `destinations`.
    SetAllowlist,
    /// Permanently hard-lock a Vault (or finalize Timed hard mode). Irreversible.
    VaultLock,
    /// Timed only: update velocity limits (max_per_tx, max_per_window in amount/max_supply fields).
    SetLimits,
    // --- Protocol DeFi (debit/credit Pool & LendingMarket under fixed rules; no free payment) ---
    /// Register AMM pool on a Pool-type account: asset/asset_b, initial amount/amount_b from user.
    CreatePool,
    /// Add liquidity to pool (destination=pool); amount/amount_b of asset/asset_b; mints LP to user.
    AddLiquidity,
    /// Remove liquidity: amount = LP tokens to burn; returns both assets to user.
    RemoveLiquidity,
    /// Swap: amount of `asset` into pool (destination=pool), min_out of other side.
    Swap,
    /// Register lending market on LendingMarket account for `asset`.
    CreateMarket,
    /// Supply `amount` of `asset` to market (destination=market).
    Supply,
    /// Withdraw supplied `amount` of `asset` from market.
    WithdrawSupply,
    /// Borrow `amount` of market asset; collateral = amount_b of native (fixed LTV).
    Borrow,
    /// Repay `amount` of borrow.
    Repay,
    /// Liquidate borrower (destination=borrower): repay amount, seize collateral.
    Liquidate,
    /// Post an oracle price vote: `asset` priced at `amount` (scale 1e6 native per unit).
    /// Median of votes becomes the effective price used by lending LTV.
    OracleVote,
    // --- Protocol SPV bridge (proof-gated; no federated mint authority) ---
    /// Advance light-client tip for a rail (`asset` = BTC|ETH|BNB|XRP). Header fields in `bridge`.
    SubmitRailHeader,
    /// Mint rail asset to `destination` after verifying lock SPV proof in `bridge` (amount = units).
    RailDeposit,
    /// Burn rail asset from `account`; open protocol withdrawal note for external claim.
    RailWithdraw,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Transaction {
    pub tx_id: HashHex,
    #[serde(default)]
    pub kind: TxKind,
    pub account: AccountId,
    pub sequence: u64,
    pub destination: AccountId,
    pub amount: u64,
    pub fee: u64,
    /// Asset for Transfer/Mint/Burn; for Issue this is the new asset code.
    /// AMM: token A / swap input asset. Lending: market asset.
    #[serde(default = "default_native")]
    pub asset: AssetId,
    /// Optional max supply on Issue (None = unlimited). Also used for Timed limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_supply: Option<u64>,
    /// Account type for OpenAccountType.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_type: Option<AccountType>,
    /// Allowlist destinations for SetAllowlist (1..=5 for Vault).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub destinations: Vec<AccountId>,
    /// Second asset (AMM pair) or collateral asset override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_b: Option<AssetId>,
    /// Second amount (AMM amount_b, borrow collateral, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_b: Option<u64>,
    /// Minimum output for Swap / RemoveLiquidity (slippage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_out: Option<u64>,
    /// SPV / light-client proof for bridge txs (SubmitRailHeader, RailDeposit, RailWithdraw).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<BridgeProof>,
    /// Local arrival time (not consensus-critical for tx_id).
    pub first_seen: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
}

fn default_native() -> AssetId {
    NATIVE_ASSET.into()
}

impl Transaction {
    /// Legacy payment constructor (native FDAG) — preserves golden tx_id for Python PoC.
    pub fn create(
        account: impl Into<String>,
        sequence: u64,
        destination: impl Into<String>,
        amount: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        Self::payment(account, sequence, destination, amount, fee, first_seen)
    }

    pub fn payment(
        account: impl Into<String>,
        sequence: u64,
        destination: impl Into<String>,
        amount: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = account.into();
        let destination = destination.into();
        // Golden-compatible payload (no kind/asset suffix).
        let payload = format!("{account}:{sequence}:{destination}:{amount}:{fee}");
        Self {
            tx_id: sha256_hex(payload),
            kind: TxKind::Payment,
            account,
            sequence,
            destination,
            amount,
            fee,
            asset: NATIVE_ASSET.into(),
            max_supply: None,
            account_type: None,
            destinations: vec![],
            asset_b: None,
            amount_b: None,
            min_out: None,
            bridge: None,
            first_seen,
            signature: None,
            public_key: None,
        }
    }

    pub fn transfer(
        account: impl Into<String>,
        sequence: u64,
        destination: impl Into<String>,
        asset: impl Into<String>,
        amount: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = account.into();
        let destination = destination.into();
        let asset = asset.into();
        let payload =
            format!("transfer:{account}:{sequence}:{destination}:{asset}:{amount}:{fee}");
        Self {
            tx_id: sha256_hex(payload),
            kind: TxKind::Transfer,
            account,
            sequence,
            destination,
            amount,
            fee,
            asset,
            max_supply: None,
            account_type: None,
            destinations: vec![],
            asset_b: None,
            amount_b: None,
            min_out: None,
            bridge: None,
            first_seen,
            signature: None,
            public_key: None,
        }
    }

    pub fn issue(
        issuer: impl Into<String>,
        sequence: u64,
        asset_code: impl Into<String>,
        initial_amount: u64,
        to: impl Into<String>,
        fee: u64,
        max_supply: Option<u64>,
        first_seen: f64,
    ) -> Self {
        let account = issuer.into();
        let destination = to.into();
        let asset = asset_code.into();
        let max_s = max_supply.map(|m| m.to_string()).unwrap_or_else(|| "none".into());
        let payload =
            format!("issue:{account}:{sequence}:{asset}:{destination}:{initial_amount}:{fee}:{max_s}");
        Self {
            tx_id: sha256_hex(payload),
            kind: TxKind::Issue,
            account,
            sequence,
            destination,
            amount: initial_amount,
            fee,
            asset,
            max_supply,
            account_type: None,
            destinations: vec![],
            asset_b: None,
            amount_b: None,
            min_out: None,
            bridge: None,
            first_seen,
            signature: None,
            public_key: None,
        }
    }

    pub fn mint(
        issuer: impl Into<String>,
        sequence: u64,
        asset: impl Into<String>,
        to: impl Into<String>,
        amount: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = issuer.into();
        let destination = to.into();
        let asset = asset.into();
        let payload = format!("mint:{account}:{sequence}:{destination}:{asset}:{amount}:{fee}");
        Self {
            tx_id: sha256_hex(payload),
            kind: TxKind::Mint,
            account,
            sequence,
            destination,
            amount,
            fee,
            asset,
            max_supply: None,
            account_type: None,
            destinations: vec![],
            asset_b: None,
            amount_b: None,
            min_out: None,
            bridge: None,
            first_seen,
            signature: None,
            public_key: None,
        }
    }

    pub fn burn(
        account: impl Into<String>,
        sequence: u64,
        asset: impl Into<String>,
        amount: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = account.into();
        let asset = asset.into();
        let payload = format!("burn:{account}:{sequence}:{asset}:{amount}:{fee}");
        Self {
            tx_id: sha256_hex(payload),
            kind: TxKind::Burn,
            account: account.clone(),
            sequence,
            destination: account,
            amount,
            fee,
            asset,
            max_supply: None,
            account_type: None,
            destinations: vec![],
            asset_b: None,
            amount_b: None,
            min_out: None,
            bridge: None,
            first_seen,
            signature: None,
            public_key: None,
        }
    }

    /// Convert Hot → Vault / Timed / Pool / LendingMarket.
    pub fn open_account_type(
        account: impl Into<String>,
        sequence: u64,
        account_type: AccountType,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = account.into();
        let ty = format!("{:?}", account_type).to_lowercase();
        let payload = format!("open_type:{account}:{sequence}:{ty}:{fee}");
        Self {
            tx_id: sha256_hex(payload),
            kind: TxKind::OpenAccountType,
            account: account.clone(),
            sequence,
            destination: account,
            amount: 0,
            fee,
            asset: NATIVE_ASSET.into(),
            max_supply: None,
            account_type: Some(account_type),
            destinations: vec![],
            asset_b: None,
            amount_b: None,
            min_out: None,
            bridge: None,
            first_seen,
            signature: None,
            public_key: None,
        }
    }

    /// Set allowlist (Vault: 1..=5 destinations while unlocked).
    pub fn set_allowlist(
        account: impl Into<String>,
        sequence: u64,
        destinations: Vec<impl Into<String>>,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = account.into();
        let destinations: Vec<String> = destinations.into_iter().map(|d| d.into()).collect();
        let mut sorted = destinations.clone();
        sorted.sort();
        let payload = format!("set_allowlist:{account}:{sequence}:{}:{fee}", sorted.join(","));
        Self {
            tx_id: sha256_hex(payload),
            kind: TxKind::SetAllowlist,
            account: account.clone(),
            sequence,
            destination: destinations.first().cloned().unwrap_or_default(),
            amount: 0,
            fee,
            asset: NATIVE_ASSET.into(),
            max_supply: None,
            account_type: None,
            destinations,
            asset_b: None,
            amount_b: None,
            min_out: None,
            bridge: None,
            first_seen,
            signature: None,
            public_key: None,
        }
    }

    /// Permanent hard lock (Vault). Irreversible.
    pub fn vault_lock(account: impl Into<String>, sequence: u64, fee: u64, first_seen: f64) -> Self {
        let account = account.into();
        let payload = format!("vault_lock:{account}:{sequence}:{fee}");
        Self {
            tx_id: sha256_hex(payload),
            kind: TxKind::VaultLock,
            account: account.clone(),
            sequence,
            destination: account,
            amount: 0,
            fee,
            asset: NATIVE_ASSET.into(),
            max_supply: None,
            account_type: None,
            destinations: vec![],
            asset_b: None,
            amount_b: None,
            min_out: None,
            bridge: None,
            first_seen,
            signature: None,
            public_key: None,
        }
    }

    /// Timed: set max per-tx (`amount`) and max per window (`max_supply`).
    pub fn set_limits(
        account: impl Into<String>,
        sequence: u64,
        max_per_tx: u64,
        max_per_window: Option<u64>,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = account.into();
        let payload = format!(
            "set_limits:{account}:{sequence}:{max_per_tx}:{}:{fee}",
            max_per_window
                .map(|m| m.to_string())
                .unwrap_or_else(|| "none".into())
        );
        Self {
            tx_id: sha256_hex(payload),
            kind: TxKind::SetLimits,
            account: account.clone(),
            sequence,
            destination: account,
            amount: max_per_tx,
            fee,
            asset: NATIVE_ASSET.into(),
            max_supply: max_per_window,
            account_type: None,
            destinations: vec![],
            asset_b: None,
            amount_b: None,
            min_out: None,
            bridge: None,
            first_seen,
            signature: None,
            public_key: None,
        }
    }

    fn defi_base(
        kind: TxKind,
        account: String,
        sequence: u64,
        destination: String,
        amount: u64,
        fee: u64,
        asset: String,
        asset_b: Option<String>,
        amount_b: Option<u64>,
        min_out: Option<u64>,
        first_seen: f64,
        payload: String,
    ) -> Self {
        Self {
            tx_id: sha256_hex(payload),
            kind,
            account,
            sequence,
            destination,
            amount,
            fee,
            asset,
            max_supply: None,
            account_type: None,
            destinations: vec![],
            asset_b,
            amount_b,
            min_out,
            bridge: None,
            first_seen,
            signature: None,
            public_key: None,
        }
    }

    /// Submit an external chain header to the protocol light client (`asset` = rail code).
    pub fn submit_rail_header(
        account: impl Into<String>,
        sequence: u64,
        rail: impl Into<String>,
        proof: BridgeProof,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = account.into();
        let asset = rail.into();
        let payload = format!(
            "rail_hdr:{account}:{sequence}:{asset}:{}:{}:{}:{}:{}",
            proof.height, proof.block_hash, proof.parent_hash, proof.merkle_root, proof.work
        );
        let mut t = Self::defi_base(
            TxKind::SubmitRailHeader,
            account,
            sequence,
            "bridge".into(),
            proof.height,
            fee,
            asset,
            None,
            None,
            None,
            first_seen,
            payload,
        );
        t.bridge = Some(proof);
        t
    }

    /// Deposit: SPV proof of external lock → mint `amount` of rail to `destination`.
    pub fn rail_deposit(
        account: impl Into<String>,
        sequence: u64,
        rail: impl Into<String>,
        destination: impl Into<String>,
        amount: u64,
        proof: BridgeProof,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = account.into();
        let asset = rail.into();
        let destination = destination.into();
        let payload = format!(
            "rail_dep:{account}:{sequence}:{asset}:{destination}:{amount}:{}:{}:{}",
            proof.external_txid, proof.block_hash, proof.height
        );
        let mut t = Self::defi_base(
            TxKind::RailDeposit,
            account,
            sequence,
            destination,
            amount,
            fee,
            asset,
            None,
            None,
            None,
            first_seen,
            payload,
        );
        t.bridge = Some(proof);
        t
    }

    /// Withdraw: burn rail asset; `external_to` is the destination on the source chain.
    pub fn rail_withdraw(
        account: impl Into<String>,
        sequence: u64,
        rail: impl Into<String>,
        amount: u64,
        external_to: impl Into<String>,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = account.into();
        let asset = rail.into();
        let external_to = external_to.into();
        let payload = format!(
            "rail_wd:{account}:{sequence}:{asset}:{amount}:{external_to}:{fee}"
        );
        let mut t = Self::defi_base(
            TxKind::RailWithdraw,
            account,
            sequence,
            bridge_account(&asset),
            amount,
            fee,
            asset,
            None,
            None,
            None,
            first_seen,
            payload,
        );
        t.bridge = Some(BridgeProof {
            external_to,
            ..Default::default()
        });
        t
    }

    /// Create AMM pool: destination must already be AccountType::Pool.
    pub fn create_pool(
        user: impl Into<String>,
        sequence: u64,
        pool: impl Into<String>,
        asset_a: impl Into<String>,
        asset_b: impl Into<String>,
        amount_a: u64,
        amount_b: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = user.into();
        let destination = pool.into();
        let asset = asset_a.into();
        let asset_b = asset_b.into();
        let payload = format!(
            "create_pool:{account}:{sequence}:{destination}:{asset}:{asset_b}:{amount_a}:{amount_b}:{fee}"
        );
        Self::defi_base(
            TxKind::CreatePool,
            account,
            sequence,
            destination,
            amount_a,
            fee,
            asset,
            Some(asset_b),
            Some(amount_b),
            None,
            first_seen,
            payload,
        )
    }

    pub fn add_liquidity(
        user: impl Into<String>,
        sequence: u64,
        pool: impl Into<String>,
        amount_a: u64,
        amount_b: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = user.into();
        let destination = pool.into();
        let payload =
            format!("add_liq:{account}:{sequence}:{destination}:{amount_a}:{amount_b}:{fee}");
        Self::defi_base(
            TxKind::AddLiquidity,
            account,
            sequence,
            destination,
            amount_a,
            fee,
            NATIVE_ASSET.into(),
            None,
            Some(amount_b),
            None,
            first_seen,
            payload,
        )
    }

    pub fn remove_liquidity(
        user: impl Into<String>,
        sequence: u64,
        pool: impl Into<String>,
        lp_amount: u64,
        min_a: u64,
        min_b: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = user.into();
        let destination = pool.into();
        let payload =
            format!("rem_liq:{account}:{sequence}:{destination}:{lp_amount}:{min_a}:{min_b}:{fee}");
        Self::defi_base(
            TxKind::RemoveLiquidity,
            account,
            sequence,
            destination,
            lp_amount,
            fee,
            NATIVE_ASSET.into(),
            None,
            Some(min_b),
            Some(min_a),
            first_seen,
            payload,
        )
    }

    pub fn swap(
        user: impl Into<String>,
        sequence: u64,
        pool: impl Into<String>,
        asset_in: impl Into<String>,
        amount_in: u64,
        min_out: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = user.into();
        let destination = pool.into();
        let asset = asset_in.into();
        let payload =
            format!("swap:{account}:{sequence}:{destination}:{asset}:{amount_in}:{min_out}:{fee}");
        Self::defi_base(
            TxKind::Swap,
            account,
            sequence,
            destination,
            amount_in,
            fee,
            asset,
            None,
            None,
            Some(min_out),
            first_seen,
            payload,
        )
    }

    pub fn create_market(
        user: impl Into<String>,
        sequence: u64,
        market: impl Into<String>,
        asset: impl Into<String>,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = user.into();
        let destination = market.into();
        let asset = asset.into();
        let payload = format!("create_mkt:{account}:{sequence}:{destination}:{asset}:{fee}");
        Self::defi_base(
            TxKind::CreateMarket,
            account,
            sequence,
            destination,
            0,
            fee,
            asset,
            None,
            None,
            None,
            first_seen,
            payload,
        )
    }

    pub fn supply(
        user: impl Into<String>,
        sequence: u64,
        market: impl Into<String>,
        amount: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = user.into();
        let destination = market.into();
        let payload = format!("supply:{account}:{sequence}:{destination}:{amount}:{fee}");
        Self::defi_base(
            TxKind::Supply,
            account,
            sequence,
            destination,
            amount,
            fee,
            NATIVE_ASSET.into(),
            None,
            None,
            None,
            first_seen,
            payload,
        )
    }

    pub fn withdraw_supply(
        user: impl Into<String>,
        sequence: u64,
        market: impl Into<String>,
        amount: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = user.into();
        let destination = market.into();
        let payload = format!("withdraw_sup:{account}:{sequence}:{destination}:{amount}:{fee}");
        Self::defi_base(
            TxKind::WithdrawSupply,
            account,
            sequence,
            destination,
            amount,
            fee,
            NATIVE_ASSET.into(),
            None,
            None,
            None,
            first_seen,
            payload,
        )
    }

    /// Borrow with native collateral (backward-compatible helper).
    pub fn borrow(
        user: impl Into<String>,
        sequence: u64,
        market: impl Into<String>,
        amount: u64,
        collateral_native: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        Self::borrow_with_collateral(
            user,
            sequence,
            market,
            amount,
            NATIVE_ASSET,
            collateral_native,
            fee,
            first_seen,
        )
    }

    /// Borrow market asset; lock `collateral_amount` of `collateral_asset` (multi-collateral).
    pub fn borrow_with_collateral(
        user: impl Into<String>,
        sequence: u64,
        market: impl Into<String>,
        amount: u64,
        collateral_asset: impl Into<String>,
        collateral_amount: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = user.into();
        let destination = market.into();
        let coll_asset = collateral_asset.into();
        let payload = format!(
            "borrow:{account}:{sequence}:{destination}:{amount}:{coll_asset}:{collateral_amount}:{fee}"
        );
        Self::defi_base(
            TxKind::Borrow,
            account,
            sequence,
            destination,
            amount,
            fee,
            NATIVE_ASSET.into(),
            Some(coll_asset),
            Some(collateral_amount),
            None,
            first_seen,
            payload,
        )
    }

    /// Oracle price vote: `price_1e6` is native units * 1e6 per 1 unit of `asset`.
    pub fn oracle_vote(
        voter: impl Into<String>,
        sequence: u64,
        asset: impl Into<String>,
        price_1e6: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = voter.into();
        let asset = asset.into();
        let payload = format!("oracle_vote:{account}:{sequence}:{asset}:{price_1e6}:{fee}");
        Self::defi_base(
            TxKind::OracleVote,
            account,
            sequence,
            "oracle".into(),
            price_1e6,
            fee,
            asset,
            None,
            None,
            None,
            first_seen,
            payload,
        )
    }

    pub fn repay(
        user: impl Into<String>,
        sequence: u64,
        market: impl Into<String>,
        amount: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = user.into();
        let destination = market.into();
        let payload = format!("repay:{account}:{sequence}:{destination}:{amount}:{fee}");
        Self::defi_base(
            TxKind::Repay,
            account,
            sequence,
            destination,
            amount,
            fee,
            NATIVE_ASSET.into(),
            None,
            None,
            None,
            first_seen,
            payload,
        )
    }

    pub fn liquidate(
        liquidator: impl Into<String>,
        sequence: u64,
        market: impl Into<String>,
        borrower: impl Into<String>,
        repay_amount: u64,
        fee: u64,
        first_seen: f64,
    ) -> Self {
        let account = liquidator.into();
        let destination = market.into();
        let borrower = borrower.into();
        let payload = format!(
            "liq:{account}:{sequence}:{destination}:{borrower}:{repay_amount}:{fee}"
        );
        let mut t = Self::defi_base(
            TxKind::Liquidate,
            account,
            sequence,
            destination,
            repay_amount,
            fee,
            NATIVE_ASSET.into(),
            None,
            None,
            None,
            first_seen,
            payload,
        );
        // encode borrower in destinations[0]
        t.destinations = vec![borrower];
        t
    }

    /// Canonical bytes wallets sign (includes network_id + kind + asset).
    pub fn sign_message(&self, network_id: u32) -> String {
        match self.kind {
            TxKind::Payment => format!(
                "FALCON-DAG-TX-V1:{network_id}:{}:{}:{}:{}:{}",
                self.account, self.sequence, self.destination, self.amount, self.fee
            ),
            TxKind::Transfer => format!(
                "FALCON-DAG-TX-V1:{network_id}:transfer:{}:{}:{}:{}:{}:{}",
                self.account,
                self.sequence,
                self.destination,
                self.asset,
                self.amount,
                self.fee
            ),
            TxKind::Issue => format!(
                "FALCON-DAG-TX-V1:{network_id}:issue:{}:{}:{}:{}:{}:{}:{}",
                self.account,
                self.sequence,
                self.asset,
                self.destination,
                self.amount,
                self.fee,
                self.max_supply
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "none".into())
            ),
            TxKind::Mint => format!(
                "FALCON-DAG-TX-V1:{network_id}:mint:{}:{}:{}:{}:{}:{}",
                self.account,
                self.sequence,
                self.destination,
                self.asset,
                self.amount,
                self.fee
            ),
            TxKind::Burn => format!(
                "FALCON-DAG-TX-V1:{network_id}:burn:{}:{}:{}:{}:{}",
                self.account, self.sequence, self.asset, self.amount, self.fee
            ),
            TxKind::OpenAccountType => format!(
                "FALCON-DAG-TX-V1:{network_id}:open_type:{}:{}:{:?}:{}",
                self.account, self.sequence, self.account_type, self.fee
            ),
            TxKind::SetAllowlist => {
                let mut d = self.destinations.clone();
                d.sort();
                format!(
                    "FALCON-DAG-TX-V1:{network_id}:set_allowlist:{}:{}:{}:{}",
                    self.account,
                    self.sequence,
                    d.join(","),
                    self.fee
                )
            }
            TxKind::VaultLock => format!(
                "FALCON-DAG-TX-V1:{network_id}:vault_lock:{}:{}:{}",
                self.account, self.sequence, self.fee
            ),
            TxKind::SetLimits => format!(
                "FALCON-DAG-TX-V1:{network_id}:set_limits:{}:{}:{}:{}:{}",
                self.account,
                self.sequence,
                self.amount,
                self.max_supply
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "none".into()),
                self.fee
            ),
            other => {
                let b = self.bridge.as_ref();
                format!(
                    "FALCON-DAG-TX-V1:{network_id}:{:?}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
                    other,
                    self.account,
                    self.sequence,
                    self.destination,
                    self.asset,
                    self.amount,
                    self.fee,
                    self.asset_b.as_deref().unwrap_or(""),
                    self.amount_b.unwrap_or(0),
                    self.min_out.unwrap_or(0),
                    b.map(|p| p.external_txid.as_str()).unwrap_or(""),
                    b.map(|p| p.block_hash.as_str()).unwrap_or(""),
                    b.map(|p| p.height).unwrap_or(0),
                )
            }
        }
    }

    pub fn fee_density(&self) -> f64 {
        self.fee as f64
    }

    pub fn is_signed(&self) -> bool {
        self.signature
            .as_ref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
            && self
                .public_key
                .as_ref()
                .map(|s| !s.is_empty())
                .unwrap_or(false)
    }
}

/// Message string for signing a legacy payment without a full Transaction object.
pub fn tx_sign_message(
    network_id: u32,
    account: &str,
    sequence: u64,
    destination: &str,
    amount: u64,
    fee: u64,
) -> String {
    format!("FALCON-DAG-TX-V1:{network_id}:{account}:{sequence}:{destination}:{amount}:{fee}")
}

// ---------------------------------------------------------------------------
// Batch / cert
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    pub batch_hash: HashHex,
    pub parents: Vec<HashHex>,
    pub tx_ids: Vec<HashHex>,
    pub creator: NodeId,
    pub timestamp: f64,
    pub signatures: BTreeMap<NodeId, String>,
    pub txs: BTreeMap<HashHex, Transaction>,
}

impl Batch {
    /// JSON f64 is not bit-stable across serde_json round-trips. Hashing the
    /// raw wall-clock float made creator vs peer `compute_hash` disagree
    /// (mass `hash_mismatch` under continuous body reseed). Always hash and
    /// store the JSON-canonical float so wire decode recomputes the same id.
    pub fn canonicalize_timestamp(ts: f64) -> f64 {
        match serde_json::to_string(&ts) {
            Ok(s) => serde_json::from_str(&s).unwrap_or(ts),
            Err(_) => ts,
        }
    }

    pub fn compute_hash(
        tx_ids: &[HashHex],
        parents: &[HashHex],
        creator: &str,
        timestamp: f64,
    ) -> HashHex {
        let timestamp = Self::canonicalize_timestamp(timestamp);
        let mut parents_sorted: Vec<&HashHex> = parents.iter().collect();
        parents_sorted.sort();
        let s = format!(
            r#"{{"creator":{},"parents":{},"timestamp":{},"tx_ids":{}}}"#,
            serde_json::to_string(creator).unwrap(),
            serde_json::to_string(&parents_sorted).unwrap(),
            serde_json::to_string(&timestamp).unwrap(),
            serde_json::to_string(tx_ids).unwrap(),
        );
        sha256_hex(s)
    }

    pub fn create(
        txs: Vec<Transaction>,
        parents: Vec<HashHex>,
        creator: NodeId,
        timestamp: f64,
    ) -> Self {
        // Store wire-stable timestamp so peers recompute the same hash.
        let timestamp = Self::canonicalize_timestamp(timestamp);
        let tx_ids: Vec<_> = txs.iter().map(|t| t.tx_id.clone()).collect();
        let batch_hash = Self::compute_hash(&tx_ids, &parents, &creator, timestamp);
        let mut map = BTreeMap::new();
        for t in txs {
            map.insert(t.tx_id.clone(), t);
        }
        Self {
            batch_hash,
            parents,
            tx_ids,
            creator,
            timestamp,
            signatures: BTreeMap::new(),
            txs: map,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Certificate {
    pub cert_id: HashHex,
    pub batch_hash: HashHex,
    pub signers: Vec<NodeId>,
    pub parents: Vec<HashHex>,
    pub creator: NodeId,
    pub timestamp: f64,
    pub round: u64,
}

impl Certificate {
    pub fn compute_id(batch_hash: &str, signers: &[NodeId]) -> HashHex {
        let mut s = signers.to_vec();
        s.sort();
        let body = format!(
            r#"{{"batch_hash":{},"signers":{}}}"#,
            serde_json::to_string(batch_hash).unwrap(),
            serde_json::to_string(&s).unwrap(),
        );
        sha256_hex(body)
    }

    pub fn from_batch(batch: &Batch, round: u64) -> Self {
        let mut signers: Vec<_> = batch
            .signatures
            .iter()
            .filter(|(_, sig)| !sig.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        signers.sort();
        Self {
            cert_id: Self::compute_id(&batch.batch_hash, &signers),
            batch_hash: batch.batch_hash.clone(),
            signers,
            parents: batch.parents.clone(),
            creator: batch.creator.clone(),
            timestamp: batch.timestamp,
            round,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartialSignature {
    pub batch_hash: HashHex,
    pub validator_id: NodeId,
    pub signature: String,
    pub timestamp: f64,
}

pub fn batch_sign_message(batch_hash: &str) -> String {
    format!("FALCON-DAG-BATCH-V1:{batch_hash}")
}

// ---------------------------------------------------------------------------
// Tip-BFT (light ordering over certificate tips — Bullshark-class for UNL)
// ---------------------------------------------------------------------------

/// Leader nominates a **certified** batch tip for ordering round `round`.
/// Consensus only orders this small tip metadata; the cert DAG is not re-sorted
/// from scratch on every node tick.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TipProposal {
    pub round: u64,
    pub tip_batch_hash: HashHex,
    /// Previous committed tip (genesis hash at round 1).
    pub parent_commit: HashHex,
    pub proposer: NodeId,
    pub signature: String,
}

impl TipProposal {
    /// Identity of the *tip choice* for this round (proposer-independent).
    /// Votes aggregate on this id so concurrent leaders for the same tip merge.
    pub fn proposal_id(&self) -> HashHex {
        sha256_hex(format!(
            "tip-prop:{}:{}:{}",
            self.round, self.tip_batch_hash, self.parent_commit
        ))
    }

    pub fn sign_message(&self) -> String {
        tip_proposal_sign_message(self.round, &self.tip_batch_hash, &self.parent_commit)
    }
}

/// UNL vote for a tip proposal (one vote per round per voter).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TipVote {
    pub round: u64,
    pub tip_batch_hash: HashHex,
    pub proposal_id: HashHex,
    pub voter: NodeId,
    pub signature: String,
}

impl TipVote {
    pub fn sign_message(&self) -> String {
        tip_vote_sign_message(self.round, &self.tip_batch_hash, &self.proposal_id)
    }
}

/// Quorum-certified tip commit: ≥ UNL threshold weight agreed on (round, tip).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TipCommit {
    pub round: u64,
    pub tip_batch_hash: HashHex,
    pub parent_commit: HashHex,
    pub proposal_id: HashHex,
    pub voters: Vec<NodeId>,
    pub commit_id: HashHex,
}

impl TipCommit {
    pub fn compute_id(
        round: u64,
        tip: &str,
        parent: &str,
        proposal_id: &str,
        voters: &[NodeId],
    ) -> HashHex {
        let mut v = voters.to_vec();
        v.sort();
        sha256_hex(format!(
            "tip-commit:{}:{}:{}:{}:{}",
            round,
            tip,
            parent,
            proposal_id,
            v.join(",")
        ))
    }
}

pub fn tip_proposal_sign_message(round: u64, tip: &str, parent: &str) -> String {
    format!("FALCON-DAG-TIP-PROPOSAL-V1:{round}:{tip}:{parent}")
}

pub fn tip_vote_sign_message(round: u64, tip: &str, proposal_id: &str) -> String {
    format!("FALCON-DAG-TIP-VOTE-V1:{round}:{tip}:{proposal_id}")
}

// ---------------------------------------------------------------------------
// Bullshark anchor commits (DAG-derived; no extra consensus messages)
// ---------------------------------------------------------------------------

/// A committed even-round leader certificate (Bullshark anchor).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnchorCommit {
    /// Even DAG round of the leader certificate.
    pub round: u64,
    pub leader: NodeId,
    pub batch_hash: HashHex,
    /// Creators of round `round+1` certificates that parent this anchor (votes).
    pub supporters: Vec<NodeId>,
    pub commit_id: HashHex,
}

impl AnchorCommit {
    pub fn compute_id(round: u64, batch_hash: &str, leader: &str, supporters: &[NodeId]) -> HashHex {
        let mut s = supporters.to_vec();
        s.sort();
        sha256_hex(format!(
            "bullshark-anchor:{}:{}:{}:{}",
            round,
            batch_hash,
            leader,
            s.join(",")
        ))
    }
}

impl fmt::Display for Transaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tx:{}:{:?}:{}->{}",
            &self.tx_id[..12.min(self.tx_id.len())],
            self.kind,
            self.account,
            self.destination
        )
    }
}

// ---------------------------------------------------------------------------
// Merkle / state root helpers
// ---------------------------------------------------------------------------

/// Deterministic Merkle root over sorted hex leaf hashes (empty → hash of "empty").
pub fn merkle_root(mut leaves: Vec<HashHex>) -> HashHex {
    if leaves.is_empty() {
        return sha256_hex("empty");
    }
    leaves.sort();
    while leaves.len() > 1 {
        if leaves.len() % 2 == 1 {
            let last = leaves.last().unwrap().clone();
            leaves.push(last);
        }
        let mut next = Vec::with_capacity(leaves.len() / 2);
        for chunk in leaves.chunks(2) {
            next.push(sha256_hex(format!("{}{}", chunk[0], chunk[1])));
        }
        leaves = next;
    }
    leaves.pop().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genesis_hashes_stable() {
        assert_eq!(genesis_batch_hash().len(), 64);
        assert_ne!(genesis_batch_hash(), genesis_cert_id());
    }

    #[test]
    fn tx_id_deterministic() {
        let a = Transaction::create("acct1", 1, "acct2", 1, 10, 0.0);
        let b = Transaction::create("acct1", 1, "acct2", 1, 10, 99.0);
        assert_eq!(a.tx_id, b.tx_id);
        assert_eq!(a.kind, TxKind::Payment);
        assert_eq!(a.asset, NATIVE_ASSET);
    }

    #[test]
    fn golden_match_python_poc() {
        assert_eq!(
            genesis_batch_hash(),
            "2857a9cb0c2467ff9978f27eb0fa25ce0b577fe432c129ad667d7c413d39e52e"
        );
        assert_eq!(genesis_cert_id(), sha256_hex(GENESIS_CERT_LABEL));
        let tx = Transaction::create("acct1", 1, "acct2", 1, 10, 0.0);
        assert_eq!(
            tx.tx_id,
            "4fe44bc9b4309c1ff5d82fe60c1c487da40b804f9ae2d22381a0ef51c51ec2a4"
        );
        let b = Batch::create(vec![tx], vec![genesis_batch_hash()], "val1".into(), 1.0);
        assert_eq!(
            b.batch_hash,
            "76eeacbef7ac4717e7b7b5f947924e97ba14daa52b94f21ee9a1df5398ef7758"
        );
        let cert_id = Certificate::compute_id(&b.batch_hash, &["val1".into(), "val2".into()]);
        let expected_body = format!(
            r#"{{"batch_hash":{},"signers":{}}}"#,
            serde_json::to_string(&b.batch_hash).unwrap(),
            serde_json::to_string(&["val1", "val2"]).unwrap(),
        );
        assert_eq!(cert_id, sha256_hex(expected_body));
    }

    #[test]
    fn issue_tx_id_stable() {
        let a = Transaction::issue("acct1", 1, "USDC", 1000, "acct1", 10, Some(1_000_000), 0.0);
        let b = Transaction::issue("acct1", 1, "USDC", 1000, "acct1", 10, Some(1_000_000), 9.0);
        assert_eq!(a.tx_id, b.tx_id);
        assert_eq!(a.kind, TxKind::Issue);
    }

    #[test]
    fn merkle_root_deterministic() {
        let r1 = merkle_root(vec!["aa".into(), "bb".into()]);
        let r2 = merkle_root(vec!["bb".into(), "aa".into()]);
        assert_eq!(r1, r2);
    }

    #[test]
    fn batch_json_roundtrip_preserves_hash() {
        for i in 0..200 {
            let ts = 1_700_000_000.0 + (i as f64) * 0.000_001 + (i as f64) * 1.23456789e-7;
            let tx = Transaction::create("acct1", i as u64, "acct2", 1, 10, ts);
            let b = Batch::create(vec![tx], vec![genesis_batch_hash()], "val1".into(), ts);
            let j = serde_json::to_string(&b).unwrap();
            let b2: Batch = serde_json::from_str(&j).unwrap();
            let expected = Batch::compute_hash(&b2.tx_ids, &b2.parents, &b2.creator, b2.timestamp);
            assert_eq!(
                b2.batch_hash,
                expected,
                "i={i} bits_in={:x} bits_out={:x}",
                b.timestamp.to_bits(),
                b2.timestamp.to_bits()
            );
            assert_eq!(b.timestamp.to_bits(), b2.timestamp.to_bits());
            let j3 = serde_json::to_vec(&b).unwrap();
            let b3: Batch = serde_json::from_slice(&j3).unwrap();
            assert_eq!(
                b3.batch_hash,
                Batch::compute_hash(&b3.tx_ids, &b3.parents, &b3.creator, b3.timestamp)
            );
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let b = Batch::create(vec![], vec![genesis_batch_hash()], "n0".into(), ts);
        let b2: Batch = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
        assert_eq!(
            b2.batch_hash,
            Batch::compute_hash(&b2.tx_ids, &b2.parents, &b2.creator, b2.timestamp)
        );
        assert_eq!(b.timestamp.to_bits(), b2.timestamp.to_bits());
    }
}
