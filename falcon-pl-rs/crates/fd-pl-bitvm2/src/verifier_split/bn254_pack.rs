//! Phase C.1b — BitVM BN254/Groth16 script-pack bridge.
//!
//! Upstream [BitVM/BitVM](https://github.com/BitVM/BitVM) owns the pairing tapscripts
//! (`bitvm::chunk::api`, ~364 taps). We do **not** reinvent or commit the ~1GB
//! generated script blobs. This module pins the API contract, commits a pack
//! digest into the Falcon Disprove taptree, and optionally loads an on-disk
//! script cache produced by `tools/bitvm-c1b-export`.
//!
//! Ark versions differ (Falcon `0.4` vs BitVM `0.5`), so BitVM is a **path/submodule
//! dependency of the export tool**, not linked into this crate.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Must match `bitvm::chunk::api` at the pinned upstream commit.
pub const BITVM_NUM_PUBS: usize = 1;
pub const BITVM_NUM_U256: usize = 14;
pub const BITVM_NUM_HASH: usize = 363;
pub const BITVM_VALIDATING_TAPS: usize = 1;
pub const BITVM_NUM_TAPS: usize = BITVM_NUM_HASH + BITVM_VALIDATING_TAPS; // 364

pub const PACK_ID: &str = "fpl-bitvm-bn254-c1b-v1";
pub const UPSTREAM_REPO: &str = "https://github.com/BitVM/BitVM";
/// Shallow tip when vendor/BitVM was wired (update when bumping submodule).
pub const UPSTREAM_COMMIT: &str = "7d1ca3660cac08aab62e76f3aa4daec0d7403ecc";
pub const UPSTREAM_API_PATH: &str = "bitvm/src/chunk/api.rs";

/// Relative to `falcon-pl-rs/`.
pub const VENDOR_BITVM_REL: &str = "vendor/BitVM";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BitvmPackManifest {
    pub pack_id: String,
    pub upstream: String,
    pub upstream_commit: String,
    pub api_path: String,
    pub num_pubs: usize,
    pub num_u256: usize,
    pub num_hash: usize,
    pub num_taps: usize,
    pub entrypoints: BitvmEntrypoints,
    /// SHA256 of concatenated per-tap script digests when a cache was exported.
    pub scripts_root_hex: Option<String>,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BitvmEntrypoints {
    pub generate_partial_scripts: String,
    pub generate_full_tapscripts: String,
    pub validate_assertions: String,
}

impl Default for BitvmPackManifest {
    fn default() -> Self {
        Self {
            pack_id: PACK_ID.into(),
            upstream: UPSTREAM_REPO.into(),
            upstream_commit: UPSTREAM_COMMIT.into(),
            api_path: UPSTREAM_API_PATH.into(),
            num_pubs: BITVM_NUM_PUBS,
            num_u256: BITVM_NUM_U256,
            num_hash: BITVM_NUM_HASH,
            num_taps: BITVM_NUM_TAPS,
            entrypoints: BitvmEntrypoints {
                generate_partial_scripts: "api_generate_partial_script".into(),
                generate_full_tapscripts: "api_generate_full_tapscripts".into(),
                validate_assertions: "validate_assertions".into(),
            },
            scripts_root_hex: None,
            notes: "Full ScriptBuf cache via tools/bitvm-c1b-export; not committed (~1GB).".into(),
        }
    }
}

/// Built-in manifest (constants pinned; no script blobs).
pub fn default_manifest() -> BitvmPackManifest {
    BitvmPackManifest::default()
}

/// Digest committed into the Falcon Disprove tree for the Groth16/BN254 pack.
pub fn pack_commitment(m: &BitvmPackManifest) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"fpl-bitvm2-bn254-pack-v1|");
    h.update(m.pack_id.as_bytes());
    h.update(b"|");
    h.update(m.upstream_commit.as_bytes());
    h.update(b"|");
    h.update((m.num_taps as u32).to_le_bytes());
    h.update((m.num_pubs as u32).to_le_bytes());
    h.update((m.num_u256 as u32).to_le_bytes());
    h.update((m.num_hash as u32).to_le_bytes());
    if let Some(root) = &m.scripts_root_hex {
        h.update(b"|scripts|");
        h.update(root.as_bytes());
    }
    h.finalize().into()
}

/// Tapscript leaf that binds Disprove of chunk 6 to the BitVM pack commitment.
///
/// Witness: `preimage` (= pack commitment domain bytes), `claimed` (32).
/// Succeeds iff `SHA256(preimage) != claimed` (same inequality pattern as C.1a).
/// Challenger supplies the expected pack commitment as `claimed` from Assert
/// and a wrong/empty preimage to fail — or uses BitVM validate_assertions for
/// the real pairing fraud proof once scripts are cached.
pub fn pack_disprove_script() -> Vec<u8> {
    // OP_OVER OP_SHA256 OP_SWAP OP_EQUAL OP_NOT OP_VERIFY OP_DROP
    vec![0x78, 0xa8, 0x7c, 0x87, 0x91, 0x69, 0x75]
}

/// Preimage material for [`pack_disprove_script`] when asserting the pack bind.
pub fn pack_disprove_preimage(m: &BitvmPackManifest) -> Vec<u8> {
    let mut v = Vec::from(&b"fpl-bitvm2-bn254-pack-v1|"[..]);
    v.extend_from_slice(m.pack_id.as_bytes());
    v.extend_from_slice(b"|");
    v.extend_from_slice(m.upstream_commit.as_bytes());
    v.extend_from_slice(b"|");
    v.extend_from_slice(&(m.num_taps as u32).to_le_bytes());
    v
}

/// On-disk cache layout produced by `falcon-pl-bitvm2-c1b-export`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitvmScriptsCache {
    pub manifest: BitvmPackManifest,
    /// Per-tap SHA256(script) hex, length == num_taps when present.
    pub tap_script_digests_hex: Vec<String>,
}

impl BitvmScriptsCache {
    pub fn scripts_root(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"fpl-bitvm2-script-digests-v1|");
        for d in &self.tap_script_digests_hex {
            h.update(d.as_bytes());
        }
        h.finalize().into()
    }
}

fn cache_path_from_env() -> Option<PathBuf> {
    std::env::var_os("FPL_BITVM_SCRIPTS_CACHE").map(PathBuf::from)
}

/// Load optional exported script-digest cache (not the full ~1GB scripts).
pub fn load_scripts_cache(path: Option<&Path>) -> Result<Option<BitvmScriptsCache>, String> {
    let p = match path.map(Path::to_path_buf).or_else(cache_path_from_env) {
        Some(p) if p.exists() => p,
        Some(_) => return Ok(None),
        None => return Ok(None),
    };
    let raw = std::fs::read_to_string(&p).map_err(|e| format!("read cache {}: {e}", p.display()))?;
    let cache: BitvmScriptsCache =
        serde_json::from_str(&raw).map_err(|e| format!("parse cache: {e}"))?;
    if cache.tap_script_digests_hex.len() != cache.manifest.num_taps {
        return Err(format!(
            "cache digests {} != num_taps {}",
            cache.tap_script_digests_hex.len(),
            cache.manifest.num_taps
        ));
    }
    Ok(Some(cache))
}

/// Effective manifest: default constants, upgraded with scripts_root if cache present.
pub fn effective_manifest(cache: Option<&BitvmScriptsCache>) -> BitvmPackManifest {
    let mut m = default_manifest();
    if let Some(c) = cache {
        m = c.manifest.clone();
        m.scripts_root_hex = Some(hex::encode(c.scripts_root()));
    }
    m
}

/// True when BitVM vendor tree is present (submodule / clone).
pub fn vendor_bitvm_present(falcon_pl_rs_root: &Path) -> bool {
    falcon_pl_rs_root
        .join(VENDOR_BITVM_REL)
        .join(UPSTREAM_API_PATH)
        .is_file()
}

/// Validate pinned constants against a live checkout of BitVM api.rs (string scan).
pub fn verify_vendor_constants(falcon_pl_rs_root: &Path) -> Result<(), String> {
    let api = falcon_pl_rs_root
        .join(VENDOR_BITVM_REL)
        .join(UPSTREAM_API_PATH);
    if !api.is_file() {
        return Err(format!("missing BitVM api at {}", api.display()));
    }
    let src = std::fs::read_to_string(&api).map_err(|e| e.to_string())?;
    for (name, want) in [
        ("NUM_PUBS", BITVM_NUM_PUBS),
        ("NUM_U256", BITVM_NUM_U256),
        ("NUM_HASH", BITVM_NUM_HASH),
    ] {
        let pat = format!("pub const {name}: usize = ");
        let Some(rest) = src.split(&pat).nth(1) else {
            return Err(format!("constant {name} not found in BitVM api"));
        };
        let got: usize = rest
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .map_err(|_| format!("parse {name}"))?;
        if got != want {
            return Err(format!("{name}: vendor={got} pinned={want}"));
        }
    }
    Ok(())
}
