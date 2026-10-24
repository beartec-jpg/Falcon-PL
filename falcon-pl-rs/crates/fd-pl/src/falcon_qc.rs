//! Falcon-512 dest-lock attestations with the seat's **node** Falcon key.
//!
//! Not the BabyJub LC secret in `keys/lc/`. Same statement the Groth16 circuit
//! hashes. Sidecar / status only — does not change tip rules.

use crate::bridge::BridgeState;
use fd_pl_snark::falcon::native::{
    statement_bytes, statement_bytes_v2, FalconPublic, FalconSig,
};
use pqcrypto_falcon::falcon512;
use pqcrypto_traits::sign::{DetachedSignature as _, PublicKey as _, SecretKey as _};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FalconQcAttestation {
    pub height: u64,
    pub header: String,
    pub parent: String,
    pub claim_root: String,
    pub sig: FalconSig,
    #[serde(default)]
    pub pk_hex: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub inner_hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub current_committee_root: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub next_committee_root: String,
    /// "v1" (default) or "v2".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub domain: String,
    /// Sealed C(h) ids in lottery order. Prover must not use live qc_committee.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_committee: Vec<String>,
    /// Sealed C(h+1) ids in lottery order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_committee: Vec<String>,
}

/// Two v2 QC statements at one height from the same seat. Deterrence only —
/// does not roll back Sepolia. Applied like OK-equivocation (jail + burn).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct QcAttestSlash {
    pub offender: String,
    pub height: u64,
    pub stmt_a: String,
    pub sig_a: String,
    pub stmt_b: String,
    pub sig_b: String,
}

impl QcAttestSlash {
    pub fn to_slash_evidence(&self) -> crate::SlashEvidence {
        crate::SlashEvidence {
            offender: self.offender.clone(),
            height: self.height,
            hash_a: self.stmt_a.clone(),
            sig_a: self.sig_a.clone(),
            hash_b: self.stmt_b.clone(),
            sig_b: self.sig_b.clone(),
            kind: "qc-attest".into(),
            round: 0,
        }
    }
}

/// Extra v4 header fields for dual-write / mismatch checks.
#[derive(Debug, Clone, Default)]
pub struct AttestV4Fields {
    pub claim_root: String,
    pub inner_hash: String,
    pub current_committee_root: String,
    pub next_committee_root: String,
    pub current_committee: Vec<String>,
    pub next_committee: Vec<String>,
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

pub fn qc_dir(keys_dir: &Path) -> PathBuf {
    keys_dir.join("falcon-qc")
}

pub fn sigs_dir(keys_dir: &Path) -> PathBuf {
    qc_dir(keys_dir).join("sigs")
}

pub fn sigs_v2_dir(keys_dir: &Path) -> PathBuf {
    qc_dir(keys_dir).join("sigs-v2")
}

fn seat_key_path(keys_dir: &Path, seat: &str) -> PathBuf {
    keys_dir.join(format!("{seat}.json"))
}

fn load_seat_falcon(keys_dir: &Path, seat: &str) -> Result<(String, falcon512::SecretKey, String), String> {
    let p = seat_key_path(keys_dir, seat);
    let raw: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&p).map_err(|_| format!("no node key {}", p.display()))?,
    )
    .map_err(|e| e.to_string())?;
    let scheme = raw
        .get("scheme")
        .and_then(|v| v.as_str())
        .unwrap_or("falcon-512");
    if !scheme.to_ascii_lowercase().contains("falcon") {
        return Err(format!("seat {seat} scheme {scheme} is not falcon"));
    }
    let id = raw
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or(seat)
        .to_string();
    let sk_hex = raw
        .get("secret_key")
        .and_then(|v| v.as_str())
        .ok_or("secret_key")?;
    let pk_hex = raw
        .get("public_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let bytes = hex::decode(sk_hex.trim().trim_start_matches("0x")).map_err(|e| e.to_string())?;
    let sk = falcon512::SecretKey::from_bytes(&bytes).map_err(|e| format!("falcon sk: {e}"))?;
    Ok((id, sk, pk_hex))
}

pub fn export_pub(keys_dir: &Path, seat: &str) -> Result<PathBuf, String> {
    let (id, _sk, pk_hex) = load_seat_falcon(keys_dir, seat)?;
    if pk_hex.is_empty() {
        return Err("no public_key".into());
    }
    let dir = qc_dir(keys_dir);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let pk = FalconPublic {
        id,
        pk_hex: pk_hex.trim().trim_start_matches("0x").to_string(),
    };
    let path = dir.join(format!("{seat}.pub.json"));
    fs::write(&path, serde_json::to_string_pretty(&pk).map_err(|e| e.to_string())? + "\n")
        .map_err(|e| e.to_string())?;
    Ok(path)
}

fn write_attestation(dir: &Path, height: u64, seat: &str, att: &FalconQcAttestation) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{height:010}-{seat}.json"));
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_string(att).map_err(|e| e.to_string())? + "\n")
        .map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Dual-write v1 (live prover) and, when v4 is armed, v2 (outer-hash statement).
/// `v4` is the committed ledger's header fields; refuse claim/header mismatch.
pub fn attest_committed(
    keys_dir: &Path,
    seat: &str,
    height: u64,
    parent_hex: &str,
    header_hex: &str,
    bridge: &BridgeState,
    v4: Option<&AttestV4Fields>,
) -> Result<(PathBuf, Option<QcAttestSlash>), String> {
    let cfg = crate::lc::load_bridge_cfg(keys_dir);
    if cfg.start_height > 0 && height <= cfg.start_height {
        return Err("below falcon-qc start_height".into());
    }
    let (id, sk, pk_hex) = load_seat_falcon(keys_dir, seat)?;
    let _ = export_pub(keys_dir, seat);
    let parent = parse32(parent_hex)?;
    let header = parse32(header_hex)?;
    let claim_from_bridge = crate::lc::evm_header_claim_root(bridge);
    let claim = if let Some(v4) = v4 {
        if !v4.claim_root.is_empty() {
            let want = parse32(&v4.claim_root)?;
            if want != claim_from_bridge {
                return Err("attest claim_root != bridge claim_root".into());
            }
            want
        } else {
            claim_from_bridge
        }
    } else {
        claim_from_bridge
    };
    let nid = cfg.network_id;
    let msg_v1 = statement_bytes(height, &parent, &header, &claim, nid);
    let sig_v1 = falcon512::detached_sign(&msg_v1, &sk);
    let pk_norm = pk_hex.trim().trim_start_matches("0x").to_string();
    let att_v1 = FalconQcAttestation {
        height,
        header: header_hex.trim().to_ascii_lowercase(),
        parent: parent_hex.trim().to_ascii_lowercase(),
        claim_root: hex::encode(claim),
        sig: FalconSig {
            id: id.clone(),
            sig_hex: hex::encode(sig_v1.as_bytes()),
        },
        pk_hex: pk_norm.clone(),
        inner_hash: String::new(),
        current_committee_root: String::new(),
        next_committee_root: String::new(),
        domain: String::new(),
        current_committee: Vec::new(),
        next_committee: Vec::new(),
    };
    let path = write_attestation(&sigs_dir(keys_dir), height, seat, &att_v1)?;

    let v4_armed = cfg.qc_header_version >= 4 && cfg.qc_start_height > 0 && height >= cfg.qc_start_height;
    if !v4_armed {
        return Ok((path, None));
    }
    let Some(v4) = v4 else {
        return Ok((path, None));
    };
    // A second v2 signature on a different header at this height is itself
    // the qc-attest proof. Do not mint it, do not overwrite the stored
    // attestation, and do not queue a self-slash. A real dual observed from
    // the network is still evidence if both signatures already exist.
    // The same header may be rewritten (replay of the committed ledger).
    let v2_dir = sigs_v2_dir(keys_dir);
    let v2_path = v2_dir.join(format!("{height:010}-{seat}.json"));
    let new_header = header_hex.trim().to_ascii_lowercase();
    if v2_path.exists() {
        let prior = fs::read_to_string(&v2_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<FalconQcAttestation>(&raw).ok());
        match prior {
            Some(prev) => {
                let prev_header = prev.header.trim().to_ascii_lowercase();
                if prev_header != new_header && !prev.sig.sig_hex.is_empty() {
                    eprintln!(
                        "falcon-qc refuse second v2 signature seat={seat} height={height}"
                    );
                    return Ok((path, None));
                }
            }
            None => {
                eprintln!(
                    "falcon-qc refuse v2 rewrite seat={seat} height={height} unreadable prior"
                );
                return Ok((path, None));
            }
        }
    }
    let msg_v2 = statement_bytes_v2(height, &parent, &header, nid);
    let sig_v2 = falcon512::detached_sign(&msg_v2, &sk);
    let att_v2 = FalconQcAttestation {
        height,
        header: header_hex.trim().to_ascii_lowercase(),
        parent: parent_hex.trim().to_ascii_lowercase(),
        claim_root: hex::encode(claim),
        sig: FalconSig {
            id: id.clone(),
            sig_hex: hex::encode(sig_v2.as_bytes()),
        },
        pk_hex: pk_norm,
        inner_hash: v4.inner_hash.clone(),
        current_committee_root: v4.current_committee_root.clone(),
        next_committee_root: v4.next_committee_root.clone(),
        domain: "v2".into(),
        current_committee: v4.current_committee.clone(),
        next_committee: v4.next_committee.clone(),
    };
    write_attestation(&v2_dir, height, seat, &att_v2)?;
    Ok((path, None))
}

pub fn recent_attestations(keys_dir: &Path, limit: usize) -> Vec<FalconQcAttestation> {
    recent_from_dir(&sigs_dir(keys_dir), limit)
}

pub fn recent_attestations_v2(keys_dir: &Path, limit: usize) -> Vec<FalconQcAttestation> {
    recent_from_dir(&sigs_v2_dir(keys_dir), limit)
}

fn recent_from_dir(dir: &Path, limit: usize) -> Vec<FalconQcAttestation> {
    newest_json_cached(dir, limit)
        .iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect()
}

/// How long a status-path directory scan is reused (v2.9.49).
pub const RECENT_DIR_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// Contents of the `limit` newest `*.json` files in `dir` (newest first),
/// cached for [`RECENT_DIR_CACHE_TTL`].
///
/// Status calls this under the node lock. On host1 `falcon-qc/sigs` holds
/// ~700k files (+129k in `sigs-v2`): the old per-request `read_dir` + sort by
/// `file_name()` (one allocation per comparison) cost 1–4 s of lock time per
/// status, and the prover/submitter scripts poll status every few seconds.
/// That starved every other task on the hub (v1 fell 100+ blocks behind and
/// its catch-up writer never ran, 2026-10-06). Attestations are informational
/// in status (provers poll the newest 256), so 10 s staleness is harmless.
pub fn newest_json_cached(dir: &Path, limit: usize) -> Vec<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    type Cache = HashMap<(PathBuf, usize), (Instant, std::sync::Arc<Vec<String>>)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let key = (dir.to_path_buf(), limit);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(c) = cache.lock() {
        if let Some((at, v)) = c.get(&key) {
            if at.elapsed() < RECENT_DIR_CACHE_TTL {
                return v.as_ref().clone();
            }
        }
    }
    let fresh: Vec<String> = newest_json_files(dir, limit)
        .into_iter()
        .filter_map(|p| fs::read_to_string(p).ok())
        .collect();
    if let Ok(mut c) = cache.lock() {
        c.insert(key, (Instant::now(), std::sync::Arc::new(fresh.clone())));
    }
    fresh
}

/// Paths of the `limit` lexicographically largest `*.json` names in `dir`,
/// largest first. O(n) selection on owned names (no per-compare allocation).
pub fn newest_json_files(dir: &Path, limit: usize) -> Vec<PathBuf> {
    if limit == 0 {
        return Vec::new();
    }
    let mut names: Vec<std::ffi::OsString> = fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .filter(|n| Path::new(n).extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    if names.len() > limit {
        let cut = names.len() - limit;
        names.select_nth_unstable(cut);
        names.drain(..cut);
    }
    names.sort_unstable_by(|a, b| b.cmp(a));
    names.into_iter().map(|n| dir.join(n)).collect()
}

pub fn verify_attestation(att: &FalconQcAttestation, network_id: u32) -> Result<(), String> {
    let parent = parse32(&att.parent)?;
    let header = parse32(&att.header)?;
    let claim = parse32(&att.claim_root)?;
    let msg = statement_bytes(att.height, &parent, &header, &claim, network_id);
    let pk_bytes = hex::decode(att.pk_hex.trim().trim_start_matches("0x")).map_err(|e| e.to_string())?;
    let sig_bytes =
        hex::decode(att.sig.sig_hex.trim().trim_start_matches("0x")).map_err(|e| e.to_string())?;
    let pk = falcon512::PublicKey::from_bytes(&pk_bytes).map_err(|e| format!("pk: {e}"))?;
    let sig = falcon512::DetachedSignature::from_bytes(&sig_bytes).map_err(|e| format!("sig: {e}"))?;
    falcon512::verify_detached_signature(&sig, &msg, &pk).map_err(|e| format!("verify: {e}"))
}

pub fn verify_attestation_v2(att: &FalconQcAttestation, network_id: u32) -> Result<(), String> {
    let parent = parse32(&att.parent)?;
    let header = parse32(&att.header)?;
    let msg = statement_bytes_v2(att.height, &parent, &header, network_id);
    let pk_bytes = hex::decode(att.pk_hex.trim().trim_start_matches("0x")).map_err(|e| e.to_string())?;
    let sig_bytes =
        hex::decode(att.sig.sig_hex.trim().trim_start_matches("0x")).map_err(|e| e.to_string())?;
    let pk = falcon512::PublicKey::from_bytes(&pk_bytes).map_err(|e| format!("pk: {e}"))?;
    let sig = falcon512::DetachedSignature::from_bytes(&sig_bytes).map_err(|e| format!("sig: {e}"))?;
    falcon512::verify_detached_signature(&sig, &msg, &pk).map_err(|e| format!("verify: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pqcrypto_traits::sign::{DetachedSignature as _, PublicKey as _};

    #[test]
    fn v2_statement_not_v1() {
        let p = [1u8; 32];
        let h = [2u8; 32];
        let c = [3u8; 32];
        let v1 = statement_bytes(9, &p, &h, &c, 2300);
        let v2 = statement_bytes_v2(9, &p, &h, 2300);
        assert_eq!(v1.len(), 124);
        assert_eq!(v2.len(), 92);
        assert_ne!(&v1[..16], &v2[..16]);
    }

    fn temp_seat(dir: &Path) -> (String, String) {
        let (pk, sk) = falcon512::keypair();
        let pk_hex = hex::encode(pk.as_bytes());
        let sk_hex = hex::encode(sk.as_bytes());
        fs::create_dir_all(dir.join("lc")).unwrap();
        fs::write(
            dir.join("lc/bridge.json"),
            r#"{"start_height":0,"network_id":2300,"key_root":"ab","qc_header_version":4,"qc_start_height":1}"#,
        )
        .unwrap();
        fs::write(
            dir.join("seat.json"),
            serde_json::json!({
                "id": "seat",
                "scheme": "falcon-512",
                "public_key": pk_hex,
                "secret_key": sk_hex,
            })
            .to_string(),
        )
        .unwrap();
        let parent = "11".repeat(32);
        let header_a = "22".repeat(32);
        (parent, header_a)
    }

    #[test]
    fn v2_refuses_second_header_without_signing_or_slashing() {
        let dir = std::env::temp_dir().join(format!("fpl-qc-refuse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (parent, header_a) = temp_seat(&dir);
        let header_b = "33".repeat(32);
        let bridge = crate::bridge::BridgeState::default();
        let v4 = AttestV4Fields::default();
        let (_p, slash) =
            attest_committed(&dir, "seat", 10, &parent, &header_a, &bridge, Some(&v4)).unwrap();
        assert!(slash.is_none());
        let stored = sigs_v2_dir(&dir).join("0000000010-seat.json");
        let first: FalconQcAttestation =
            serde_json::from_str(&fs::read_to_string(&stored).unwrap()).unwrap();
        assert_eq!(first.header, header_a);
        let sig_a = first.sig.sig_hex.clone();
        let (_p2, slash2) =
            attest_committed(&dir, "seat", 10, &parent, &header_b, &bridge, Some(&v4)).unwrap();
        assert!(slash2.is_none(), "a conflicting header must not self-slash");
        let kept: FalconQcAttestation =
            serde_json::from_str(&fs::read_to_string(&stored).unwrap()).unwrap();
        assert_eq!(kept.header, header_a);
        assert_eq!(kept.sig.sig_hex, sig_a);
        let (_p3, slash3) =
            attest_committed(&dir, "seat", 10, &parent, &header_a, &bridge, Some(&v4)).unwrap();
        assert!(slash3.is_none());
        let rewritten: FalconQcAttestation =
            serde_json::from_str(&fs::read_to_string(&stored).unwrap()).unwrap();
        assert_eq!(rewritten.header, header_a);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn v2_verify_roundtrip() {
        let (pk, sk) = falcon512::keypair();
        let parent = [9u8; 32];
        let header = [8u8; 32];
        let msg = statement_bytes_v2(42, &parent, &header, 2300);
        let sig = falcon512::detached_sign(&msg, &sk);
        let att = FalconQcAttestation {
            height: 42,
            header: hex::encode(header),
            parent: hex::encode(parent),
            claim_root: hex::encode([7u8; 32]),
            sig: FalconSig {
                id: "v1".into(),
                sig_hex: hex::encode(sig.as_bytes()),
            },
            pk_hex: hex::encode(pk.as_bytes()),
            inner_hash: "aa".repeat(32),
            current_committee_root: "bb".repeat(32),
            next_committee_root: "cc".repeat(32),
            domain: "v2".into(),
            current_committee: vec!["v1".into()],
            next_committee: vec!["v1".into()],
        };
        verify_attestation_v2(&att, 2300).expect("v2 verify");
        assert!(verify_attestation(&att, 2300).is_err(), "v1 verify must fail on v2 sig");
    }

    #[test]
    fn v2_prover_forbids_sample_gap_jump() {
        let s = include_str!("../scripts/pl-falcon-qc-v2-prover.py");
        assert!(!s.contains("SAMPLE_GAP"), "v2 prover must not name SAMPLE_GAP");
        assert!(!s.contains("pick_on_grid"), "v2 prover must not jump on a sample grid");
        assert!(s.contains("--domain"));
        assert!(s.contains("submitHeaderWithProofs"));
        assert!(s.contains("nextLeaves") || s.contains("next_leaves"));
    }

    #[test]
    fn v2_e2e_forbids_sample_gap_as_success() {
        let s = include_str!("../scripts/pl-eth-sepolia-e2e.py");
        assert!(
            s.contains("FALCON_QC_V2"),
            "e2e must gate sequential V2 (FALCON_QC_V2)"
        );
        assert!(s.contains("forbids FALCON_QC_SAMPLE_GAP"));
        assert!(s.contains("assert_sequential_chain"));
        assert!(!s.contains("kickoff("));
    }

    #[test]
    fn v2_e2e_wait_is_sequential() {
        let s = include_str!("../scripts/pl-bridge-e2e-wait.py");
        assert!(s.contains("FALCON_QC_V2"));
        assert!(s.contains("pl-falcon-qc-v2-catchup.py"));
    }

    #[test]
    fn leftover_notes_strand_v1_and_destlock() {
        let dest = include_str!("../config/falcon-destlock.json");
        assert!(dest.contains("LEFTOVER"));
        assert!(dest.contains("external_to"));
        assert!(!dest.to_ascii_lowercase().contains("self-dest rescue"));
        let v1 = include_str!("../config/sepolia-qc.json");
        assert!(v1.contains("unrecoverable"));
        let v2 = include_str!("../config/sepolia-qc-v2.json");
        assert!(v2.contains("PLACEHOLDER"));
        assert!(v2.contains("Do not reuse v1 pk"));
    }
}
