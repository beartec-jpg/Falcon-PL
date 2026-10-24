//! v4 two-stage ledger seal: inner packer-sig hash + outer sha256 opening.
//!
//! Outer preimage is fixed-width for Solidity `sha256(abi.encodePacked(...))`.
//! See `docs/LOTTERY6_SEPOLIA_LC.md`.

use crate::crypto::{sha256_hex, sha256_raw, KeyRing};
use fd_pl_snark::codec::fr_to_be_hex;
use fd_pl_snark::falcon::native::{committee_key_root, decode_pk, pk_leaf, N};

/// ASCII prefix. Solidity: `bytes("pl-ledger:v4-seal")` (17 bytes, not 16).
pub const SEAL_PREFIX: &[u8] = b"pl-ledger:v4-seal";

pub fn parse32(hex_s: &str) -> Result<[u8; 32], String> {
    let h = hex_s.trim().trim_start_matches("0x").trim_start_matches("0X");
    let b = hex::decode(h).map_err(|e| e.to_string())?;
    if b.len() != 32 {
        return Err(format!("expected 32 bytes, got {}", b.len()));
    }
    let mut o = [0u8; 32];
    o.copy_from_slice(&b);
    Ok(o)
}

pub fn header_msg_v4(
    height: u64,
    parent_hash: &str,
    network_id: u32,
    packer: &str,
    tx_root: &str,
    state_root_after: &str,
    skipped_predecessors: &[String],
    parent_cert_hash: &str,
    claim_root_hex: &str,
    current_committee_root_hex: &str,
) -> String {
    let skips = skipped_predecessors.join(",");
    format!(
        "pl-ledger:v4|{height}|{parent_hash}|{network_id}|{packer}|{tx_root}|{state_root_after}|skips:{skips}|cert:{parent_cert_hash}|claim:{claim_root_hex}|cur6:{current_committee_root_hex}"
    )
}

pub fn inner_hash_hex(msg_v4: &str, packer_sig: &str) -> String {
    sha256_hex(format!("{msg_v4}|{packer_sig}").as_bytes())
}

/// Outer `ledger_hash` (32 raw bytes, returned as lowercase hex).
pub fn outer_hash_hex(inner: &[u8; 32], next_root: &[u8; 32], claim_root: &[u8; 32]) -> String {
    let mut buf = Vec::with_capacity(17 + 96);
    buf.extend_from_slice(SEAL_PREFIX);
    buf.extend_from_slice(inner);
    buf.extend_from_slice(next_root);
    buf.extend_from_slice(claim_root);
    debug_assert_eq!(buf.len(), 113);
    hex::encode(sha256_raw(&buf))
}

fn pk_to_poly(pk_hex: &str) -> Result<[u16; N], String> {
    let h = pk_hex.trim().trim_start_matches("0x").trim_start_matches("0X");
    if let Ok(raw) = hex::decode(h) {
        if let Ok(poly) = decode_pk(&raw) {
            return Ok(poly);
        }
    }
    // hmac-dev / non-Falcon: fold utf-8 or hex into a 512-coeff poly.
    let d = sha256_raw(pk_hex.as_bytes());
    let mut poly = [0u16; N];
    for (i, b) in d.iter().enumerate() {
        poly[i] = u16::from(*b);
    }
    Ok(poly)
}

/// MiMC merkle8 of up to 6 Falcon (or hmac stand-in) pks, dummy-padded to 8.
/// Result is Fr big-endian 64 hex chars (no 0x), same as v1 `falconKeyRoot`.
pub fn committee_root_hex(pk_hexes: &[String]) -> Result<String, String> {
    if pk_hexes.len() > 6 {
        return Err("committee root expects at most 6 pks".into());
    }
    let mut polys = Vec::with_capacity(pk_hexes.len());
    for h in pk_hexes {
        polys.push(pk_to_poly(h)?);
    }
    let fr = committee_key_root(&polys)?;
    Ok(fr_to_be_hex(fr))
}

pub fn committee_root_for_ids(ids: &[String], keys: &KeyRing) -> Result<String, String> {
    let mut pks = Vec::with_capacity(ids.len());
    for id in ids {
        pks.push(
            keys.public_hex(id)
                .ok_or_else(|| format!("no public key for {id}"))?,
        );
    }
    committee_root_hex(&pks)
}

/// pk_leaf Fr as 32-byte BE hex (no 0x). Solidity `currentLeaves` / `nextLeaves`.
pub fn committee_leaf_hexes(pk_hexes: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::with_capacity(pk_hexes.len());
    for h in pk_hexes {
        let poly = pk_to_poly(h)?;
        out.push(fr_to_be_hex(pk_leaf(&poly)));
    }
    Ok(out)
}

pub fn committee_leaf_hexes_for_ids(ids: &[String], keys: &KeyRing) -> Result<Vec<String>, String> {
    let mut pks = Vec::with_capacity(ids.len());
    for id in ids {
        pks.push(
            keys.public_hex(id)
                .ok_or_else(|| format!("no public key for {id}"))?,
        );
    }
    committee_leaf_hexes(&pks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_prefix_len() {
        assert_eq!(SEAL_PREFIX.len(), 17);
        assert_eq!(SEAL_PREFIX, b"pl-ledger:v4-seal");
    }

    #[test]
    fn outer_hash_encoding_vector() {
        let inner = [0x11u8; 32];
        let next = [0x22u8; 32];
        let claim = [0x33u8; 32];
        let got = outer_hash_hex(&inner, &next, &claim);
        let mut buf = Vec::with_capacity(112);
        buf.extend_from_slice(SEAL_PREFIX);
        buf.extend_from_slice(&inner);
        buf.extend_from_slice(&next);
        buf.extend_from_slice(&claim);
        assert_eq!(buf.len(), 113);
        assert_eq!(got, hex::encode(sha256_raw(&buf)));
        assert_eq!(got.len(), 64);
    }

    #[test]
    fn committee_root_hmac_standin_is_stable() {
        let a = committee_root_hex(&["aa".into(), "bb".into()]).unwrap();
        let b = committee_root_hex(&["aa".into(), "bb".into()]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        let c = committee_root_hex(&["aa".into(), "cc".into()]).unwrap();
        assert_ne!(a, c);
    }

    #[test]
    fn committee_leaves_match_root_arity() {
        let pks = vec!["aa".into(), "bb".into(), "cc".into(), "dd".into(), "ee".into(), "ff".into()];
        let leaves = committee_leaf_hexes(&pks).unwrap();
        assert_eq!(leaves.len(), 6);
        for l in &leaves {
            assert_eq!(l.len(), 64);
        }
        assert_ne!(leaves[0], leaves[1]);
    }
}
