//! Atomic JSON snapshot persistence (v4: DeFi + SPV rails light-client state).

use fd_types::{AnchorCommit, Batch, Certificate, HashHex, TipCommit, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Current snapshot schema (v4 adds SPV rails; older versions still load).
pub const SNAPSHOT_VERSION: u32 = 4;
pub const SNAPSHOT_FILENAME: &str = "snapshot.json";
/// Sidecar written on save/restore for operator visibility (PR7).
pub const SNAPSHOT_META_FILENAME: &str = "snapshot.meta.json";

// ---------------------------------------------------------------------------
// Hot vs cold retention (PR7) — explicit, measurable
// ---------------------------------------------------------------------------

/// Retention guidance for validator (hot) vs archival (cold) profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Max hard-final tip hashes kept on validator hot path.
    pub hot_hard_final_max: u64,
    /// Soft target for uncertified batch TTL (0 = prune disabled).
    pub hot_uncertified_ttl_s: u64,
    /// Archival cold history: retain full body+cert history (boolean intent).
    pub cold_retain_full_bodies: bool,
    /// Archival cold history: retain all certificates.
    pub cold_retain_all_certs: bool,
}

impl RetentionPolicy {
    pub fn validator_hot() -> Self {
        Self {
            hot_hard_final_max: 10_000,
            hot_uncertified_ttl_s: 0, // disabled by default (avoid pack/prune spiral)
            cold_retain_full_bodies: false,
            cold_retain_all_certs: false,
        }
    }

    pub fn archival_cold() -> Self {
        Self {
            hot_hard_final_max: 100_000,
            hot_uncertified_ttl_s: 0,
            cold_retain_full_bodies: true,
            cold_retain_all_certs: true,
        }
    }

    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "hot_hard_final_max": self.hot_hard_final_max,
            "hot_uncertified_ttl_s": self.hot_uncertified_ttl_s,
            "cold_retain_full_bodies": self.cold_retain_full_bodies,
            "cold_retain_all_certs": self.cold_retain_all_certs,
            "pruning_rules": [
                "never prune below hard_finality_h for correctness",
                "validator may prune hard_final beyond max_hard_final_history",
                "archival retains full body+cert history for replay/query",
                "uncertified_ttl_s=0 disables uncertified prune by default"
            ],
        })
    }
}

/// Operator metadata about last snapshot save/restore (PR7).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SnapshotMeta {
    pub last_save_unix_s: f64,
    pub last_restore_unix_s: f64,
    pub last_save_path: String,
    pub last_restore_ok: bool,
    pub last_network_id: u32,
    pub last_validator_id: String,
    pub last_batches_created: u64,
    pub last_executed_txs: u64,
}

impl SnapshotMeta {
    pub fn age_s(&self, now: f64) -> Option<f64> {
        if self.last_save_unix_s <= 0.0 {
            None
        } else {
            Some((now - self.last_save_unix_s).max(0.0))
        }
    }

    pub fn to_json(&self, now: f64) -> serde_json::Value {
        serde_json::json!({
            "last_save_unix_s": self.last_save_unix_s,
            "last_restore_unix_s": self.last_restore_unix_s,
            "last_save_path": self.last_save_path,
            "last_restore_ok": self.last_restore_ok,
            "last_network_id": self.last_network_id,
            "last_validator_id": self.last_validator_id,
            "last_batches_created": self.last_batches_created,
            "last_executed_txs": self.last_executed_txs,
            "snapshot_age_s": self.age_s(now),
        })
    }
}

fn wall_now_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn meta_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join(SNAPSHOT_META_FILENAME)
}

pub fn save_snapshot_meta(path: impl AsRef<Path>, meta: &SnapshotMeta) -> Result<(), StoreError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| StoreError::Io(e.to_string()))?;
    }
    let data = serde_json::to_vec_pretty(meta).map_err(|e| StoreError::Json(e.to_string()))?;
    fs::write(path, data).map_err(|e| StoreError::Io(e.to_string()))?;
    Ok(())
}

pub fn load_snapshot_meta(path: impl AsRef<Path>) -> Result<Option<SnapshotMeta>, StoreError> {
    let path = path.as_ref();
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(|e| StoreError::Io(e.to_string()))?;
    let meta: SnapshotMeta =
        serde_json::from_str(&text).map_err(|e| StoreError::Json(e.to_string()))?;
    Ok(Some(meta))
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(String),
    #[error("json: {0}")]
    Json(String),
    #[error("unsupported snapshot version {0}")]
    BadVersion(u32),
    #[error("network_id mismatch: snap={got} local={want}")]
    NetworkMismatch { got: u32, want: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub network_id: u32,
    pub validator_id: String,
    pub batches: Vec<Batch>,
    pub certificates: Vec<Certificate>,
    pub ordered: Vec<HashHex>,
    pub hard_final: Vec<HashHex>,
    pub executed_txs: Vec<HashHex>,
    pub soft_final_txs: Vec<HashHex>,
    pub mempool: Vec<Transaction>,
    /// Multi-asset balances: account -> asset -> amount
    pub balances: BTreeMap<String, BTreeMap<String, u64>>,
    pub sequences: BTreeMap<String, u64>,
    /// asset_code -> {issuer, total_supply, max_supply}
    #[serde(default)]
    pub assets: BTreeMap<String, AssetSnap>,
    #[serde(default)]
    pub burned_native: u64,
    #[serde(default)]
    pub treasury_fees: u64,
    #[serde(default)]
    pub emissions_minted: u64,
    #[serde(default)]
    pub state_root: String,
    pub batches_created: u64,
    // --- v3: protocol account guards + DeFi + oracle ---
    #[serde(default)]
    pub policies: BTreeMap<String, PolicySnap>,
    #[serde(default)]
    pub pools: BTreeMap<String, PoolSnap>,
    #[serde(default)]
    pub markets: BTreeMap<String, MarketSnap>,
    /// supplied[user][market] = supply shares
    #[serde(default)]
    pub supplied: BTreeMap<String, BTreeMap<String, u64>>,
    /// borrowed[user][market] = debt shares
    #[serde(default)]
    pub borrowed: BTreeMap<String, BTreeMap<String, u64>>,
    /// collateral[user][market][asset] = locked amount
    #[serde(default)]
    pub collateral: BTreeMap<String, BTreeMap<String, BTreeMap<String, u64>>>,
    /// Legacy v1 collateral (user→market→native amount); migrated on load.
    #[serde(default)]
    pub collateral_legacy: BTreeMap<String, BTreeMap<String, u64>>,
    #[serde(default)]
    pub applied_user_txs: u64,
    #[serde(default)]
    pub last_emission_height: u64,
    /// Final oracle prices: asset → price in 1e6 native units per 1 asset unit
    #[serde(default)]
    pub oracle_prices: BTreeMap<String, u64>,
    /// oracle_votes[asset][voter] = price
    #[serde(default)]
    pub oracle_votes: BTreeMap<String, BTreeMap<String, u64>>,
    #[serde(default)]
    pub protocol_fees_skimmed: u64,
    // --- v4: protocol SPV rails (no federated mint) ---
    #[serde(default)]
    pub rails: BTreeMap<String, RailSnap>,
    /// rail -> height -> header
    #[serde(default)]
    pub rail_headers: BTreeMap<String, BTreeMap<u64, RailHeaderSnap>>,
    /// rail -> spent external deposit txids
    #[serde(default)]
    pub rail_spent_deposits: BTreeMap<String, BTreeSet<String>>,
    /// withdrawal note_id -> note
    #[serde(default)]
    pub rail_withdrawals: BTreeMap<String, WithdrawalSnap>,
    // --- v5: tip-BFT light ordering state ---
    /// Next tip-BFT round (1-based). 0 = legacy snapshot without tip-BFT.
    #[serde(default)]
    pub tip_bft_round: u64,
    #[serde(default)]
    pub tip_bft_last_commit: String,
    #[serde(default)]
    pub tip_commits: Vec<TipCommit>,
    #[serde(default)]
    pub ledger_commits: u64,
    // --- v6: Bullshark anchors ---
    #[serde(default)]
    pub bullshark_anchors: Vec<AnchorCommit>,
    #[serde(default)]
    pub bullshark_last_anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RailSnap {
    pub asset: String,
    pub family: String,
    pub min_confirmations: u64,
    pub bridge_account: String,
    pub lock_id: String,
    pub total_minted: u64,
    pub total_burned: u64,
    pub tip_height: u64,
    pub tip_hash: String,
    pub tip_work: String,
    #[serde(default)]
    pub claim_root: String,
    #[serde(default)]
    pub claim_note_count: u64,
    #[serde(default)]
    pub claim_height: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RailHeaderSnap {
    pub height: u64,
    pub hash: String,
    pub parent_hash: String,
    pub merkle_root: String,
    #[serde(default)]
    pub tx_root: String,
    pub work: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawalSnap {
    pub note_id: String,
    pub rail: String,
    pub amount: u64,
    pub from: String,
    pub external_to: String,
    pub falcon_height: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetSnap {
    pub issuer: String,
    pub total_supply: u64,
    pub max_supply: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PolicySnap {
    pub account_type: String,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default)]
    pub hard_locked: bool,
    #[serde(default)]
    pub max_per_tx: u64,
    #[serde(default)]
    pub max_per_window: u64,
    #[serde(default)]
    pub window_spent: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolSnap {
    pub asset_a: String,
    pub asset_b: String,
    pub lp_asset: String,
    pub fee_bps: u16,
    #[serde(default = "default_proto_share")]
    pub protocol_fee_share_bps: u16,
}

fn default_proto_share() -> u16 {
    1_667
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnap {
    pub asset: String,
    pub ltv_bps: u16,
    pub liq_bonus_bps: u16,
    #[serde(default = "default_index")]
    pub supply_index: u64,
    #[serde(default = "default_index")]
    pub borrow_index: u64,
    #[serde(default)]
    pub last_accrual_txs: u64,
    #[serde(default = "default_base_rate")]
    pub base_rate_bps: u16,
    #[serde(default = "default_slope")]
    pub slope_bps: u16,
    #[serde(default)]
    pub allowed_collateral: Vec<String>,
}

fn default_index() -> u64 {
    1_000_000_000
}
fn default_base_rate() -> u16 {
    50
}
fn default_slope() -> u16 {
    200
}

impl Snapshot {
    pub fn validate_for(&self, network_id: u32) -> Result<(), StoreError> {
        if self.version > SNAPSHOT_VERSION || self.version == 0 {
            return Err(StoreError::BadVersion(self.version));
        }
        if self.network_id != network_id {
            return Err(StoreError::NetworkMismatch {
                got: self.network_id,
                want: network_id,
            });
        }
        Ok(())
    }
}

pub fn snapshot_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join(SNAPSHOT_FILENAME)
}

pub fn save_snapshot(path: impl AsRef<Path>, snap: &Snapshot) -> Result<(), StoreError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| StoreError::Io(e.to_string()))?;
    }
    let data = serde_json::to_vec(snap).map_err(|e| StoreError::Json(e.to_string()))?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(
        ".snap-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    {
        let mut f = fs::File::create(&tmp).map_err(|e| StoreError::Io(e.to_string()))?;
        f.write_all(&data).map_err(|e| StoreError::Io(e.to_string()))?;
        f.write_all(b"\n").map_err(|e| StoreError::Io(e.to_string()))?;
        f.sync_all().map_err(|e| StoreError::Io(e.to_string()))?;
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        StoreError::Io(e.to_string())
    })?;
    // PR7: write sidecar meta for age / restore observability.
    let meta = SnapshotMeta {
        last_save_unix_s: wall_now_s(),
        last_restore_unix_s: 0.0,
        last_save_path: path.display().to_string(),
        last_restore_ok: false,
        last_network_id: snap.network_id,
        last_validator_id: snap.validator_id.clone(),
        last_batches_created: snap.batches_created,
        last_executed_txs: snap.executed_txs.len() as u64,
    };
    let _ = save_snapshot_meta(parent.join(SNAPSHOT_META_FILENAME), &meta);
    Ok(())
}

/// Record a successful restore into snapshot.meta.json (PR7).
pub fn note_restore_ok(data_dir: impl AsRef<Path>, snap: &Snapshot) -> Result<(), StoreError> {
    let path = meta_path(&data_dir);
    let mut meta = load_snapshot_meta(&path)?.unwrap_or_default();
    meta.last_restore_unix_s = wall_now_s();
    meta.last_restore_ok = true;
    meta.last_network_id = snap.network_id;
    meta.last_validator_id = snap.validator_id.clone();
    meta.last_batches_created = snap.batches_created;
    meta.last_executed_txs = snap.executed_txs.len() as u64;
    if meta.last_save_path.is_empty() {
        meta.last_save_path = snapshot_path(&data_dir).display().to_string();
    }
    save_snapshot_meta(path, &meta)
}

pub fn load_snapshot(path: impl AsRef<Path>) -> Result<Option<Snapshot>, StoreError> {
    let path = path.as_ref();
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(|e| StoreError::Io(e.to_string()))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| StoreError::Json(e.to_string()))?;
    let ver = v.get("version").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
    if ver == 1 {
        let mut snap: Snapshot =
            serde_json::from_value(v.clone()).map_err(|e| StoreError::Json(e.to_string()))?;
        if let Some(bal) = v.get("balances").and_then(|b| b.as_object()) {
            let mut nested = BTreeMap::new();
            for (acct, val) in bal {
                if let Some(n) = val.as_u64() {
                    let mut m = BTreeMap::new();
                    m.insert("FDAG".into(), n);
                    nested.insert(acct.clone(), m);
                } else if let Ok(m) =
                    serde_json::from_value::<BTreeMap<String, u64>>(val.clone())
                {
                    nested.insert(acct.clone(), m);
                }
            }
            snap.balances = nested;
        }
        snap.version = SNAPSHOT_VERSION;
        migrate_collateral(&mut snap);
        return Ok(Some(snap));
    }
    let mut snap: Snapshot =
        serde_json::from_str(&text).map_err(|e| StoreError::Json(e.to_string()))?;
    // Accept older versions by bumping on load
    if snap.version < SNAPSHOT_VERSION {
        snap.version = SNAPSHOT_VERSION;
    }
    migrate_collateral(&mut snap);
    Ok(Some(snap))
}

/// If only legacy flat collateral was present, fold into multi-asset map under FDAG.
fn migrate_collateral(snap: &mut Snapshot) {
    if snap.collateral.is_empty() && !snap.collateral_legacy.is_empty() {
        let legacy = std::mem::take(&mut snap.collateral_legacy);
        for (user, markets) in legacy {
            for (mkt, amt) in markets {
                if amt > 0 {
                    snap.collateral
                        .entry(user.clone())
                        .or_default()
                        .entry(mkt)
                        .or_default()
                        .insert("FDAG".into(), amt);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fd_types::{genesis_batch_hash, Transaction};

    #[test]
    fn roundtrip_file() {
        let dir = std::env::temp_dir().join(format!("fd-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = snapshot_path(&dir);
        let tx = Transaction::create("acct1", 1, "acct2", 1, 10, 0.0);
        let mut bal = BTreeMap::new();
        bal.insert(
            "acct1".into(),
            BTreeMap::from([("FDAG".into(), 999u64)]),
        );
        let mut pools = BTreeMap::new();
        pools.insert(
            "pool1".into(),
            PoolSnap {
                asset_a: "FDAG".into(),
                asset_b: "GOLD".into(),
                lp_asset: "LP-pool1".into(),
                fee_bps: 30,
                protocol_fee_share_bps: 1_667,
            },
        );
        let snap = Snapshot {
            version: SNAPSHOT_VERSION,
            network_id: 9002,
            validator_id: "val1".into(),
            batches: vec![],
            certificates: vec![],
            ordered: vec![genesis_batch_hash()],
            hard_final: vec![genesis_batch_hash()],
            executed_txs: vec![tx.tx_id.clone()],
            soft_final_txs: vec![tx.tx_id.clone()],
            mempool: vec![tx],
            balances: bal,
            sequences: BTreeMap::from([("acct1".into(), 2)]),
            assets: BTreeMap::new(),
            burned_native: 0,
            treasury_fees: 0,
            emissions_minted: 0,
            state_root: "ab".into(),
            batches_created: 3,
            policies: BTreeMap::new(),
            pools,
            markets: BTreeMap::new(),
            supplied: BTreeMap::new(),
            borrowed: BTreeMap::new(),
            collateral: BTreeMap::new(),
            collateral_legacy: BTreeMap::new(),
            applied_user_txs: 0,
            last_emission_height: 0,
            oracle_prices: BTreeMap::new(),
            oracle_votes: BTreeMap::new(),
            protocol_fees_skimmed: 0,
            rails: BTreeMap::new(),
            rail_headers: BTreeMap::new(),
            rail_spent_deposits: BTreeMap::new(),
            rail_withdrawals: BTreeMap::new(),
            tip_bft_round: 0,
            tip_bft_last_commit: String::new(),
            tip_commits: vec![],
            ledger_commits: 0,
            bullshark_anchors: vec![],
            bullshark_last_anchor: String::new(),
        };
        save_snapshot(&path, &snap).unwrap();
        let loaded = load_snapshot(&path).unwrap().unwrap();
        assert_eq!(loaded.batches_created, 3);
        note_restore_ok(&dir, &loaded).unwrap();
        let meta = load_snapshot_meta(meta_path(&dir)).unwrap().unwrap();
        assert!(meta.last_save_unix_s > 0.0);
        assert!(meta.last_restore_ok);
        assert!(meta.age_s(wall_now_s() + 1.0).unwrap() >= 0.0);
        assert!(RetentionPolicy::archival_cold().cold_retain_full_bodies);
        assert!(!RetentionPolicy::validator_hot().cold_retain_full_bodies);
        assert_eq!(
            loaded.balances.get("acct1").unwrap().get("FDAG"),
            Some(&999)
        );
        assert!(loaded.pools.contains_key("pool1"));
        let _ = fs::remove_dir_all(&dir);
    }
}
