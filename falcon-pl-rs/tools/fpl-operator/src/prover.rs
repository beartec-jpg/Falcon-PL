//! Prover wiring: the Groth16 proof/VK the kit hands to fpl_pregen, the ceremony and the
//! Disprove watcher (FPL_PROOF_DIR), plus the M1 withdraw VK used by the attest hook.
//! On testnet4/mainnet there is no fallback to the upstream BitVM mock VK: if the proof dir
//! is missing or its vk.bin does not hash to the pinned deployment VK, pregen is refused.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Testnet VK setup 2026-10-19 (docs/bridge/ceremony/testnet-vk-2026-10-19).
pub const TESTNET_DISPROVE_VK_SHA256: &str = "c6d3509b1e64bc4e22fe73503901070e5b9717c4cd40869ac5fac80401787843";
pub const TESTNET_WITHDRAW_VK_SHA256: &str = "4a1cabbccc33907e7bb20aa9a4ca3681920daab53a3a0e781e80ff403d5b4763";
pub const PROOF_FILES: [&str; 3] = ["vk.bin", "proof.bin", "public.bin"];

pub fn sha256_file(p: &Path) -> Result<String, String> {
    let b = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(hex::encode(Sha256::digest(&b)))
}

/// Pinned disprove VK hash for a network ("" = no pin, regtest only).
pub fn expected_disprove_vk(network: &str, override_hex: &str) -> String {
    if !override_hex.is_empty() {
        return override_hex.to_ascii_lowercase();
    }
    if network == "regtest" { String::new() } else { TESTNET_DISPROVE_VK_SHA256.into() }
}

/// Resolve and check the proof dir. `explicit` wins; else `vk_dir` is used when it holds the
/// full proof set. Ok(None) = no proof dir and none required (regtest without a pin).
pub fn resolve_proof_dir(explicit: &str, vk_dir: &str, network: &str, expect_sha: &str) -> Result<Option<(PathBuf, String)>, String> {
    let pick = |d: &str| !d.is_empty() && PROOF_FILES.iter().all(|f| Path::new(d).join(f).is_file());
    let dir = if !explicit.is_empty() {
        if !pick(explicit) {
            return Err(format!("FPL_OP_PROOF_DIR {explicit} lacks vk.bin/proof.bin/public.bin"));
        }
        explicit
    } else if pick(vk_dir) {
        vk_dir
    } else if expect_sha.is_empty() && network == "regtest" {
        return Ok(None);
    } else {
        return Err(format!("no proof dir for {network} (set FPL_OP_PROOF_DIR); refusing upstream mock VK"));
    };
    let sha = sha256_file(&Path::new(dir).join("vk.bin"))?;
    if !expect_sha.is_empty() && sha != expect_sha {
        return Err(format!("vk.bin sha256 {sha} != pinned {expect_sha}"));
    }
    Ok(Some((PathBuf::from(dir), sha)))
}

/// sha256 of the M1 withdraw VK bytes given as hex (BITVM2_WITHDRAW_VK_HEX form) or a file path.
pub fn withdraw_vk_sha(vk_hex_or_path: &str) -> Result<String, String> {
    let s = vk_hex_or_path.trim();
    let bytes = if Path::new(s).is_file() {
        let t = std::fs::read(s).map_err(|e| e.to_string())?;
        match hex::decode(String::from_utf8_lossy(&t).trim()) { Ok(b) => b, Err(_) => t }
    } else {
        hex::decode(s).map_err(|e| format!("attest VK hex: {e}"))?
    };
    Ok(hex::encode(Sha256::digest(&bytes)))
}

/// Pool entries are tagged with the VK they were built for (pregen/vk.sha256). When the VK
/// changes (or an untagged pool exists), ready entries are moved to pregen/retired-<sha>.
/// Returns how many entries were retired.
pub fn retag_pool(pool: &Path, vk_sha: &str) -> Result<usize, String> {
    let tag = pool.join("vk.sha256");
    let cur = std::fs::read_to_string(&tag).unwrap_or_default().trim().to_string();
    if cur == vk_sha {
        return Ok(0);
    }
    let ready = pool.join("ready");
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(&ready) {
        let label = if cur.is_empty() { "untagged".to_string() } else { cur[..cur.len().min(12)].to_string() };
        let dst = pool.join(format!("retired-{label}"));
        std::fs::create_dir_all(&dst).map_err(|e| e.to_string())?;
        for e in rd.flatten() {
            std::fs::rename(e.path(), dst.join(e.file_name())).map_err(|e| e.to_string())?;
            n += 1;
        }
    }
    std::fs::create_dir_all(&ready).map_err(|e| e.to_string())?;
    std::fs::write(&tag, format!("{vk_sha}\n")).map_err(|e| e.to_string())?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fplprov-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn proofset(d: &Path, vk: &[u8]) {
        std::fs::write(d.join("vk.bin"), vk).unwrap();
        std::fs::write(d.join("proof.bin"), b"p").unwrap();
        std::fs::write(d.join("public.bin"), b"x").unwrap();
    }
    #[test]
    fn testnet_refuses_mock_fallback() {
        let e = resolve_proof_dir("", "", "testnet4", &expected_disprove_vk("testnet4", "")).unwrap_err();
        assert!(e.contains("mock"));
        assert_eq!(resolve_proof_dir("", "", "regtest", "").unwrap(), None);
    }
    #[test]
    fn pin_checked_and_vk_dir_used() {
        let d = tmp("pin");
        proofset(&d, b"vk-a");
        let sha = hex::encode(Sha256::digest(b"vk-a"));
        let (p, s) = resolve_proof_dir("", d.to_str().unwrap(), "testnet4", &sha).unwrap().unwrap();
        assert_eq!((p, s), (d.clone(), sha));
        assert!(resolve_proof_dir(d.to_str().unwrap(), "", "testnet4", TESTNET_DISPROVE_VK_SHA256).unwrap_err().contains("pinned"));
    }
    #[test]
    fn real_testnet_vk_matches_pin() {
        let p = Path::new("/workspace/deploy/bridge-int/testnet-vk");
        if !p.join("v3_vk.bin").exists() { return; }
        assert_eq!(sha256_file(&p.join("v3_vk.bin")).unwrap(), TESTNET_DISPROVE_VK_SHA256);
        assert_eq!(sha256_file(&p.join("v1_vk.bin")).unwrap(), TESTNET_WITHDRAW_VK_SHA256);
        assert_eq!(withdraw_vk_sha(p.join("v1_vk.bin").to_str().unwrap()).unwrap(), TESTNET_WITHDRAW_VK_SHA256);
    }
    #[test]
    fn pool_retired_on_vk_change() {
        let d = tmp("pool");
        std::fs::create_dir_all(d.join("ready/g1")).unwrap();
        assert_eq!(retag_pool(&d, "aa").unwrap(), 1);
        assert!(d.join("retired-untagged/g1").exists());
        assert_eq!(retag_pool(&d, "aa").unwrap(), 0);
        std::fs::create_dir_all(d.join("ready/g2")).unwrap();
        assert_eq!(retag_pool(&d, "bb").unwrap(), 1);
        assert!(d.join("retired-aa/g2").exists());
    }
    #[test]
    fn full_untagged_pool_retired_at_startup() {
        // 0.5.0 bug: a full pool (ready == target) never spawned pregen, so mock-VK entries stayed;
        // 0.5.1 calls retag_pool at daemon startup regardless of pool size.
        let d = tmp("full");
        std::fs::create_dir_all(d.join("ready")).unwrap();
        for f in ["a.json", "b.json"] { std::fs::write(d.join("ready").join(f), "{}").unwrap(); }
        assert_eq!(retag_pool(&d, TESTNET_DISPROVE_VK_SHA256).unwrap(), 2);
        assert_eq!(std::fs::read_dir(d.join("ready")).unwrap().count(), 0);
        assert!(d.join("retired-untagged/a.json").exists() && d.join("retired-untagged/b.json").exists());
        assert_eq!(std::fs::read_to_string(d.join("vk.sha256")).unwrap().trim(), TESTNET_DISPROVE_VK_SHA256);
    }
}
