//! BIP341 taptree over Disprove leaves (C.1a SHA256 + C.1b BitVM pack bind).

use super::bn254_pack::{
    default_manifest, effective_manifest, load_scripts_cache, pack_commitment, pack_disprove_script,
    BitvmPackManifest,
};
use super::script::disprove_script;
use super::trace::{ChunkId, NUM_SCRIPTABLE_CHUNKS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapLeaf {
    pub chunk_id: u8,
    pub script_hex: String,
    pub tapleaf_hash_hex: String,
    /// `sha256` | `bitvm_bn254_pack`
    #[serde(default = "default_leaf_kind")]
    pub kind: String,
}

fn default_leaf_kind() -> String {
    "sha256".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisproveTapTree {
    pub leaves: Vec<TapLeaf>,
    pub merkle_root_hex: String,
    /// NUMS-style tag for a future output key (address derivation Phase D).
    pub nums_tag: String,
    pub notes: String,
    /// C.1b BitVM pack bind (always present when using [`build_disprove_taptree`]).
    pub bn254_pack: Option<BitvmPackManifest>,
    pub bn254_pack_commitment_hex: Option<String>,
}

pub fn tagged_hash(tag: &[u8], msg: &[u8]) -> [u8; 32] {
    let tag_hash = Sha256::digest(tag);
    let mut h = Sha256::new();
    h.update(tag_hash);
    h.update(tag_hash);
    h.update(msg);
    h.finalize().into()
}

fn write_compact(out: &mut Vec<u8>, n: u64) {
    if n < 0xfd {
        out.push(n as u8);
    } else if n <= 0xffff {
        out.push(0xfd);
        out.extend_from_slice(&(n as u16).to_le_bytes());
    } else {
        out.push(0xfe);
        out.extend_from_slice(&(n as u32).to_le_bytes());
    }
}

pub fn tapleaf_hash(script: &[u8]) -> [u8; 32] {
    let mut msg = Vec::with_capacity(1 + 9 + script.len());
    msg.push(0xc0);
    write_compact(&mut msg, script.len() as u64);
    msg.extend_from_slice(script);
    tagged_hash(b"TapLeaf", &msg)
}

pub fn tapbranch(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut msg = [0u8; 64];
    if a.as_slice() < b.as_slice() {
        msg[..32].copy_from_slice(a);
        msg[32..].copy_from_slice(b);
    } else {
        msg[..32].copy_from_slice(b);
        msg[32..].copy_from_slice(a);
    }
    tagged_hash(b"TapBranch", &msg)
}

pub fn merkle_root(leaf_hashes: &[[u8; 32]]) -> [u8; 32] {
    if leaf_hashes.is_empty() {
        return [0u8; 32];
    }
    let mut layer: Vec<[u8; 32]> = leaf_hashes.to_vec();
    while layer.len() > 1 {
        if layer.len() % 2 == 1 {
            layer.push(*layer.last().unwrap());
        }
        let mut next = Vec::with_capacity(layer.len() / 2);
        for pair in layer.chunks(2) {
            next.push(tapbranch(&pair[0], &pair[1]));
        }
        layer = next;
    }
    layer[0]
}

/// Control-block merkle nodes (leaf → root siblings). Matches [`merkle_root`] odd-dup.
pub fn merkle_path(leaf_hashes: &[[u8; 32]], index: usize) -> Result<Vec<[u8; 32]>, String> {
    if leaf_hashes.is_empty() {
        return Err("empty taptree".into());
    }
    if index >= leaf_hashes.len() {
        return Err("leaf index".into());
    }
    let mut layer: Vec<[u8; 32]> = leaf_hashes.to_vec();
    let mut idx = index;
    let mut path = Vec::new();
    while layer.len() > 1 {
        if layer.len() % 2 == 1 {
            layer.push(*layer.last().unwrap());
        }
        let sib = if idx % 2 == 0 {
            layer[idx + 1]
        } else {
            layer[idx - 1]
        };
        path.push(sib);
        let mut next = Vec::with_capacity(layer.len() / 2);
        for pair in layer.chunks(2) {
            next.push(tapbranch(&pair[0], &pair[1]));
        }
        layer = next;
        idx /= 2;
    }
    Ok(path)
}

/// Build Disprove taptree: 6 SHA256 leaves + BitVM BN254 pack-bind leaf (C.1b).
pub fn build_disprove_taptree() -> DisproveTapTree {
    let cache = load_scripts_cache(None).ok().flatten();
    let manifest = effective_manifest(cache.as_ref());
    let pack_c = pack_commitment(&manifest);

    let mut leaves = Vec::with_capacity(NUM_SCRIPTABLE_CHUNKS + 1);
    let mut hashes = Vec::with_capacity(NUM_SCRIPTABLE_CHUNKS + 1);

    for id in ChunkId::all_scriptable() {
        let script = disprove_script(id.index() as u8);
        let th = tapleaf_hash(&script);
        hashes.push(th);
        leaves.push(TapLeaf {
            chunk_id: id.index() as u8,
            script_hex: hex::encode(&script),
            tapleaf_hash_hex: hex::encode(th),
            kind: "sha256".into(),
        });
    }

    // Chunk 6 / pack bind
    let pack_script = pack_disprove_script();
    let pack_th = tapleaf_hash(&pack_script);
    hashes.push(pack_th);
    leaves.push(TapLeaf {
        chunk_id: ChunkId::Groth16Pairing.index() as u8,
        script_hex: hex::encode(&pack_script),
        tapleaf_hash_hex: hex::encode(pack_th),
        kind: "bitvm_bn254_pack".into(),
    });

    let root = merkle_root(&hashes);
    let scripts_note = if manifest.scripts_root_hex.is_some() {
        "scripts cache digests loaded"
    } else {
        "constants-only pack; export scripts with tools/bitvm-c1b-export"
    };
    DisproveTapTree {
        leaves,
        merkle_root_hex: hex::encode(root),
        nums_tag: "FalconPL-BitVM2-Disprove-2300".into(),
        notes: format!(
            "C.1a SHA256 leaves + C.1b BitVM BN254 pack bind ({}; {}).",
            default_manifest().pack_id,
            scripts_note
        ),
        bn254_pack: Some(manifest.clone()),
        bn254_pack_commitment_hex: Some(hex::encode(pack_c)),
    }
}
