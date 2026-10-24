//! Local LC Schnorr after native Falcon QC. Secrets stay in `keys/lc/<id>.json`.

use crate::bridge::BridgeState;
use fd_pl_snark::schnorr::{statement_digest, LcSecret, LcSig};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LcBridgeCfg {
    #[serde(default)]
    pub start_height: u64,
    #[serde(default = "default_nid")]
    pub network_id: u32,
    #[serde(default)]
    pub key_root: String,
    /// 3 = pre-fork v3 headers. 4 = lottery-6 outer seal (PR 1).
    #[serde(default = "default_qc_header_version")]
    pub qc_header_version: u32,
    /// First v4 ledger height. 0 = v4 not armed.
    #[serde(default)]
    pub qc_start_height: u64,
    /// v2.10.0 round-scoped vote activation (0 = unset → CLI/env/network
    /// constant). See `resolve_vote_rounds_from_height`.
    #[serde(default)]
    pub vote_rounds_from_height: u64,
    /// v2.9.48 Phase 2 pause window `[from, resume_at)` (0/0 = unset →
    /// CLI/env/network constant). See `resolve_vote_rounds_pause`.
    #[serde(default)]
    pub vote_rounds_pause_from: u64,
    #[serde(default)]
    pub vote_rounds_resume_at: u64,
}

fn default_qc_header_version() -> u32 {
    3
}

fn default_nid() -> u32 {
    2300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LcAttestation {
    pub height: u64,
    pub header: String,
    pub parent: String,
    pub claim_root: String,
    pub sig: LcSig,
}

fn parse32(hex_s: &str) -> Result<[u8; 32], String> {
    let h = hex_s.trim().trim_start_matches("0x").trim_start_matches("0X");
    let b = hex::decode(h).map_err(|e| e.to_string())?;
    if b.len() != 32 {
        return Err("expected 32-byte hex".into());
    }
    let mut o = [0u8; 32];
    o.copy_from_slice(&b);
    Ok(o)
}

pub fn lc_dir(keys_dir: &Path) -> PathBuf {
    keys_dir.join("lc")
}

pub fn sigs_dir(keys_dir: &Path) -> PathBuf {
    lc_dir(keys_dir).join("sigs")
}

pub fn load_bridge_cfg(keys_dir: &Path) -> LcBridgeCfg {
    let p = lc_dir(keys_dir).join("bridge.json");
    fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(LcBridgeCfg {
            start_height: 0,
            network_id: 2300,
            key_root: String::new(),
            qc_header_version: 3,
            qc_start_height: 0,
            vote_rounds_from_height: 0,
            vote_rounds_pause_from: 0,
            vote_rounds_resume_at: 0,
        })
}

/// Fingerprint of `keys/lc/bridge.json` for status / soak drift checks.
///
/// `sha256` is over the raw file bytes (empty if missing/unreadable).
/// `qc_cfg_digest` is over the consensus-relevant fields only (whitespace /
/// key order independent), so two seats with the same effective config agree
/// even if the files are formatted differently. No secrets live in this file.
pub fn bridge_cfg_fingerprint(keys_dir: &Path) -> serde_json::Value {
    use sha2::{Digest, Sha256};
    let p = lc_dir(keys_dir).join("bridge.json");
    let raw = fs::read(&p).ok();
    let parsed: Option<LcBridgeCfg> = raw.as_ref().and_then(|b| serde_json::from_slice(b).ok());
    let raw_sha = raw.as_ref().map(|b| hex::encode(Sha256::digest(b))).unwrap_or_default();
    let cfg = parsed.clone().unwrap_or_else(|| load_bridge_cfg(keys_dir));
    serde_json::json!({
        "present": raw.is_some(),
        "parsed": parsed.is_some(),
        "sha256": raw_sha,
        "qc_cfg_digest": qc_cfg_digest(&cfg),
        "network_id": cfg.network_id,
        "start_height": cfg.start_height,
        "qc_header_version": cfg.qc_header_version,
        "qc_start_height": cfg.qc_start_height,
        "vote_rounds_from_height": cfg.vote_rounds_from_height,
        "vote_rounds_pause_from": cfg.vote_rounds_pause_from,
        "vote_rounds_resume_at": cfg.vote_rounds_resume_at,
        "key_root": cfg.key_root,
    })
}

/// Canonical digest of the consensus-relevant `bridge.json` fields.
///
/// The pause window is appended only when set, so files without it keep the
/// exact pre-v2.9.48 digest.
pub fn qc_cfg_digest(cfg: &LcBridgeCfg) -> String {
    use sha2::{Digest, Sha256};
    let mut s = format!(
        "falcon-pl-qc-cfg:v1|nid={}|start={}|qcv={}|qcs={}|vr={}|root={}",
        cfg.network_id,
        cfg.start_height,
        cfg.qc_header_version,
        cfg.qc_start_height,
        cfg.vote_rounds_from_height,
        cfg.key_root.trim().trim_start_matches("0x").to_ascii_lowercase()
    );
    if cfg.vote_rounds_pause_from != 0 || cfg.vote_rounds_resume_at != 0 {
        s.push_str(&format!(
            "|vp={}..{}",
            cfg.vote_rounds_pause_from, cfg.vote_rounds_resume_at
        ));
    }
    hex::encode(Sha256::digest(s.as_bytes()))
}

pub fn evm_claim_leaves(bridge: &BridgeState) -> Vec<[u8; 32]> {
    let mut leaves = Vec::new();
    for asset in [crate::bridge::RAIL_ETH, crate::bridge::RAIL_USDC] {
        let Some(r) = bridge.rails.get(asset) else { continue };
        for h in &r.claim_leaves {
            if let Ok(b) = hex::decode(h.trim()) {
                if b.len() == 32 {
                    let mut a = [0u8; 32];
                    a.copy_from_slice(&b);
                    leaves.push(a);
                }
            }
        }
    }
    leaves
}

pub fn evm_header_claim_root(bridge: &BridgeState) -> [u8; 32] {
    let leaves = evm_claim_leaves(bridge);
    if leaves.is_empty() {
        return [0u8; 32];
    }
    match crate::evm_spv::build_tree(&leaves, 0) {
        Ok((root, _)) => parse32(&root).unwrap_or([0u8; 32]),
        Err(_) => [0u8; 32],
    }
}

pub fn attest_committed(
    keys_dir: &Path,
    seat: &str,
    height: u64,
    parent_hex: &str,
    header_hex: &str,
    bridge: &BridgeState,
) -> Result<PathBuf, String> {
    let cfg = load_bridge_cfg(keys_dir);
    if cfg.start_height > 0 && height <= cfg.start_height {
        return Err("below lc start_height".into());
    }
    let sec_path = lc_dir(keys_dir).join(format!("{seat}.json"));
    let raw = fs::read_to_string(&sec_path).map_err(|_| "no LC secret on this seat".to_string())?;
    let sec: LcSecret = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let parent = parse32(parent_hex)?;
    let header = parse32(header_hex)?;
    let claim = evm_header_claim_root(bridge);
    let stmt = statement_digest(height, &parent, &header, &claim, cfg.network_id);
    let sig = sec.sign_stmt(stmt, &mut OsRng).map_err(|e| e.to_string())?;
    let att = LcAttestation {
        height,
        header: header_hex.trim().to_ascii_lowercase(),
        parent: parent_hex.trim().to_ascii_lowercase(),
        claim_root: hex::encode(claim),
        sig,
    };
    let dir = sigs_dir(keys_dir);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{height:010}-{seat}.json"));
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_string(&att).map_err(|e| e.to_string())? + "\n")
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644));
    }
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    prune_sigs(&dir, 4096);
    Ok(path)
}

fn prune_sigs(dir: &Path, keep: usize) {
    let mut files: Vec<_> = fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort_by_key(|e| e.file_name());
    let drop_n = files.len() - keep;
    for e in files.into_iter().take(drop_n) {
        let _ = fs::remove_file(e.path());
    }
}

pub fn recent_attestations(keys_dir: &Path, limit: usize) -> Vec<LcAttestation> {
    // Cached + O(n) newest-N selection (v2.9.49, see falcon_qc::newest_json_cached).
    crate::falcon_qc::newest_json_cached(&sigs_dir(keys_dir), limit)
        .iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect()
}

pub fn backfill_chain(
    keys_dir: &Path,
    seat: &str,
    chain: &[crate::Ledger],
    bridge: &BridgeState,
) -> usize {
    let cfg = load_bridge_cfg(keys_dir);
    let mut n = 0usize;
    for led in chain {
        if cfg.start_height > 0 && led.height <= cfg.start_height {
            continue;
        }
        let p = sigs_dir(keys_dir).join(format!("{:010}-{seat}.json", led.height));
        if p.exists() {
            continue;
        }
        if attest_committed(
            keys_dir,
            seat,
            led.height,
            &led.parent_hash,
            &led.ledger_hash,
            bridge,
        )
        .is_ok()
        {
            n += 1;
        }
    }
    n
}
