//! SSZ hash_tree_root + Merkle branch (Ethereum consensus types).
//! SHA256, little-endian basic types. Not keccak (that's execution).

use sha2::{Digest, Sha256};

pub fn sha256_pair(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(a);
    h.update(b);
    h.finalize().into()
}

pub fn chunk_u64(v: u64) -> [u8; 32] {
    let mut c = [0u8; 32];
    c[..8].copy_from_slice(&v.to_le_bytes());
    c
}

pub fn chunk_bytes32(b: &[u8; 32]) -> [u8; 32] {
    *b
}

/// ByteVector[N] packed into 32-byte chunks (right-padded in the last chunk).
pub fn chunks_byte_vector(data: &[u8]) -> Vec<[u8; 32]> {
    if data.is_empty() {
        return vec![[0u8; 32]];
    }
    let n = (data.len() + 31) / 32;
    let mut out = vec![[0u8; 32]; n];
    for (i, chunk) in out.iter_mut().enumerate() {
        let s = i * 32;
        let e = (s + 32).min(data.len());
        chunk[..e - s].copy_from_slice(&data[s..e]);
    }
    out
}

pub fn merkleize(chunks: &[[u8; 32]]) -> [u8; 32] {
    if chunks.is_empty() {
        return [0u8; 32];
    }
    let mut n = chunks.len().next_power_of_two();
    let mut layer = vec![[0u8; 32]; n];
    layer[..chunks.len()].copy_from_slice(chunks);
    while n > 1 {
        let mut next = vec![[0u8; 32]; n / 2];
        for i in 0..n / 2 {
            next[i] = sha256_pair(&layer[2 * i], &layer[2 * i + 1]);
        }
        layer = next;
        n /= 2;
    }
    layer[0]
}

pub fn mix_in_length(root: [u8; 32], len: u64) -> [u8; 32] {
    sha256_pair(&root, &chunk_u64(len))
}

pub fn hash_tree_root_uint64(v: u64) -> [u8; 32] {
    chunk_u64(v)
}

pub fn hash_tree_root_bytes32(b: &[u8; 32]) -> [u8; 32] {
    *b
}

/// ByteList[limit]: merkleize packed bytes, mix in actual length.
pub fn hash_tree_root_byte_list(data: &[u8], limit: usize) -> [u8; 32] {
    let max_chunks = (limit + 31) / 32;
    let mut chunks = chunks_byte_vector(data);
    while chunks.len() < max_chunks.max(1) {
        chunks.push([0u8; 32]);
    }
    if chunks.len() > max_chunks && max_chunks > 0 {
        chunks.truncate(max_chunks);
    }
    mix_in_length(merkleize(&chunks), data.len() as u64)
}

pub fn hash_tree_root_byte_vector(data: &[u8]) -> [u8; 32] {
    merkleize(&chunks_byte_vector(data))
}

pub fn beacon_header_root(
    slot: u64,
    proposer_index: u64,
    parent_root: &[u8; 32],
    state_root: &[u8; 32],
    body_root: &[u8; 32],
) -> [u8; 32] {
    merkleize(&[
        chunk_u64(slot),
        chunk_u64(proposer_index),
        *parent_root,
        *state_root,
        *body_root,
    ])
}

/// SSZ Merkle branch. `index` is the generalized index (not subtree index).
pub fn verify_merkle_gindex(
    leaf: &[u8; 32],
    branch: &[[u8; 32]],
    gindex: u64,
    root: &[u8; 32],
) -> bool {
    let mut value = *leaf;
    let mut idx = gindex;
    for sib in branch {
        if idx % 2 == 1 {
            value = sha256_pair(sib, &value);
        } else {
            value = sha256_pair(&value, sib);
        }
        idx /= 2;
    }
    idx == 1 && value == *root
}

pub fn parse32(hex_s: &str) -> Result<[u8; 32], String> {
    let h = hex_s.trim().trim_start_matches("0x");
    let b = hex::decode(h).map_err(|e| e.to_string())?;
    if b.len() != 32 {
        return Err(format!("want 32 bytes got {}", b.len()));
    }
    let mut o = [0u8; 32];
    o.copy_from_slice(&b);
    Ok(o)
}

pub fn parse_n(hex_s: &str, n: usize) -> Result<Vec<u8>, String> {
    let h = hex_s.trim().trim_start_matches("0x");
    let b = hex::decode(h).map_err(|e| e.to_string())?;
    if b.len() != n {
        return Err(format!("want {n} bytes got {}", b.len()));
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sepolia_finalized_beacon_header_root() {
        // From /eth/v1/beacon/headers/finalized on 2026-08-26 (slot 11000288).
        let parent = parse32("0xb5f8e81b959d63c8b7ef2c71fffe0e15834294898f3c150e7cddfc23ca8dbfb1").unwrap();
        let state = parse32("0xbd47b5a0977051570c4538984611f37623af08be014866f32caa56cd79aab6a2").unwrap();
        let body = parse32("0x7f8cc6fac8fe3569f8a48338414397cc885c0e8dab25d6bf2c4c98d155f80fd9").unwrap();
        let root = beacon_header_root(11_000_288, 1180, &parent, &state, &body);
        let want = parse32("0x15b7004cc5a9d13f74485e05f517c805c451e6fe9204ec94dc91d7f8f093553f").unwrap();
        assert_eq!(hex::encode(root), hex::encode(want));
    }
}
