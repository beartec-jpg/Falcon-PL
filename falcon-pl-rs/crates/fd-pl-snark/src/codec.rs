//! Field / hash encoding shared with the Solidity verifier.

use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};

pub fn hex_to_bytes32(s: &str) -> Result<[u8; 32], String> {
    let h = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    let b = hex::decode(h).map_err(|e| e.to_string())?;
    if b.len() != 32 {
        return Err(format!("expected 32 bytes, got {}", b.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&b);
    Ok(out)
}

pub fn fr_to_be_hex(f: Fr) -> String {
    let mut le = f.into_bigint().to_bytes_le();
    le.resize(32, 0);
    le.reverse();
    hex::encode(le)
}

pub fn fr_from_be_hex(s: &str) -> Result<Fr, String> {
    let h = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    let raw = hex::decode(h).map_err(|e| e.to_string())?;
    if raw.len() > 32 {
        return Err("field hex too long".into());
    }
    let mut be = [0u8; 32];
    be[32 - raw.len()..].copy_from_slice(&raw);
    Ok(Fr::from_be_bytes_mod_order(&be))
}

/// Split a 32-byte BE hash into two 128-bit field elements (high, low).
/// Matches `uint256(bytes32) >> 128` and `& ((1<<128)-1)` in Solidity.
pub fn split_hash(h: &[u8; 32]) -> (Fr, Fr) {
    let mut hi = [0u8; 32];
    let mut lo = [0u8; 32];
    hi[16..].copy_from_slice(&h[..16]);
    lo[16..].copy_from_slice(&h[16..]);
    (
        Fr::from_be_bytes_mod_order(&hi),
        Fr::from_be_bytes_mod_order(&lo),
    )
}

pub fn public_inputs(
    height: u64,
    parent: &[u8; 32],
    header: &[u8; 32],
    claim_root: &[u8; 32],
    key_root: Fr,
    threshold: u64,
    network_id: u32,
) -> Vec<Fr> {
    let (p_hi, p_lo) = split_hash(parent);
    let (h_hi, h_lo) = split_hash(header);
    let (c_hi, c_lo) = split_hash(claim_root);
    vec![
        Fr::from(height),
        p_hi,
        p_lo,
        h_hi,
        h_lo,
        c_hi,
        c_lo,
        key_root,
        Fr::from(threshold),
        Fr::from(network_id),
    ]
}

pub const N_PUBLIC: usize = 10;

/// v2: height, parent hi/lo, header hi/lo, currentRoot, nextRoot, threshold, nid, signerIndex, nextLeaf0..5
pub const N_PUBLIC_V2: usize = 16;

pub fn public_inputs_v2(
    height: u64,
    parent: &[u8; 32],
    header: &[u8; 32],
    current_root: Fr,
    next_root: Fr,
    threshold: u64,
    network_id: u32,
    signer_index: u64,
    next_leaves: &[Fr; 6],
) -> Vec<Fr> {
    let (p_hi, p_lo) = split_hash(parent);
    let (h_hi, h_lo) = split_hash(header);
    let mut v = vec![
        Fr::from(height),
        p_hi,
        p_lo,
        h_hi,
        h_lo,
        current_root,
        next_root,
        Fr::from(threshold),
        Fr::from(network_id),
        Fr::from(signer_index),
    ];
    v.extend_from_slice(next_leaves);
    debug_assert_eq!(v.len(), N_PUBLIC_V2);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ff::{BigInteger, PrimeField};

    #[test]
    fn split_hash_matches_solidity_shift() {
        let mut h = [0u8; 32];
        h[0] = 0xaa;
        h[15] = 0xbb;
        h[16] = 0xcc;
        h[31] = 0xdd;
        let (hi, lo) = split_hash(&h);
        let mut hi_bytes = hi.into_bigint().to_bytes_be();
        let mut lo_bytes = lo.into_bigint().to_bytes_be();
        while hi_bytes.len() < 32 {
            hi_bytes.insert(0, 0);
        }
        while lo_bytes.len() < 32 {
            lo_bytes.insert(0, 0);
        }
        assert_eq!(&hi_bytes[16..], &h[..16]);
        assert_eq!(&lo_bytes[16..], &h[16..]);
    }

    #[test]
    fn public_inputs_len() {
        assert_eq!(
            public_inputs(1, &[0u8; 32], &[1u8; 32], &[2u8; 32], Fr::from(3u64), 4, 2300)
                .len(),
            N_PUBLIC
        );
    }

    #[test]
    fn public_inputs_v2_len() {
        assert_eq!(
            public_inputs_v2(
                1,
                &[0u8; 32],
                &[1u8; 32],
                Fr::from(3u64),
                Fr::from(4u64),
                1,
                2300,
                2,
                &[Fr::from(10u64); 6],
            )
            .len(),
            N_PUBLIC_V2
        );
    }
}
