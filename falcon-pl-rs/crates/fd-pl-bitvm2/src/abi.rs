//! Public-input ABI for `FplWithdrawValid` (`fpl-bitvm2-wd-v1`).
//!
//! Matches [BITVM2_ENGINE.md](../../fd-pl/docs/BITVM2_ENGINE.md) §5.1.

use ark_bn254::Fr;
use ark_ff::PrimeField;
use fd_pl_snark::codec::split_hash;
use sha2::{Digest, Sha256};

/// Domain tag for the SNARK-friendly withdraw leaf (MiMC tree).
pub const DOMAIN: &[u8] = b"fpl-bitvm2-wd-v1";

/// Circuit / ABI version committed as public `vk_hash` when using test setup.
pub const CIRCUIT_VERSION: u64 = 1;

/// Merkle depth for Phase B fixtures (2^DEPTH leaves max).
pub const MERKLE_DEPTH: usize = 4;

/// Number of public field elements.
pub const N_PUBLIC: usize = 13;

/// Public input indices.
pub mod idx {
    pub const NETWORK_ID: usize = 0;
    pub const AMOUNT: usize = 1;
    pub const DEST_HI: usize = 2;
    pub const DEST_LO: usize = 3;
    pub const CLAIM_ROOT_HI: usize = 4;
    pub const CLAIM_ROOT_LO: usize = 5;
    pub const HEIGHT: usize = 6;
    pub const HEADER_HI: usize = 7;
    pub const HEADER_LO: usize = 8;
    pub const NOTE_ID: usize = 9;
    pub const PEGOUT_HI: usize = 10;
    pub const PEGOUT_LO: usize = 11;
    pub const VK_HASH: usize = 12;
}

/// Map an arbitrary note id string to a field element (SHA256 → Fr).
pub fn note_id_fr(note_id: &str) -> Fr {
    let h = Sha256::digest(format!("fpl-bitvm2-note|{note_id}").as_bytes());
    Fr::from_be_bytes_mod_order(&h)
}

/// Domain constant as Fr.
pub fn domain_fr() -> Fr {
    let h = Sha256::digest(DOMAIN);
    Fr::from_be_bytes_mod_order(&h)
}

/// Pack public inputs in circuit order.
pub fn public_inputs(
    network_id: u32,
    amount: u64,
    dest: &[u8; 32],
    claim_root: &[u8; 32],
    height: u64,
    header_hash: &[u8; 32],
    note_id: &str,
    pegout_txid: &[u8; 32],
    vk_hash: Fr,
) -> Vec<Fr> {
    let (d_hi, d_lo) = split_hash(dest);
    let (c_hi, c_lo) = split_hash(claim_root);
    let (h_hi, h_lo) = split_hash(header_hash);
    let (p_hi, p_lo) = split_hash(pegout_txid);
    vec![
        Fr::from(network_id),
        Fr::from(amount),
        d_hi,
        d_lo,
        c_hi,
        c_lo,
        Fr::from(height),
        h_hi,
        h_lo,
        note_id_fr(note_id),
        p_hi,
        p_lo,
        vk_hash,
    ]
}

/// Default testnet vk_hash placeholder (Phase B test-seed setups).
pub fn test_vk_hash() -> Fr {
    let h = Sha256::digest(b"fpl-bitvm2-test-vk|v1");
    Fr::from_be_bytes_mod_order(&h)
}

/// Nullifier for double-claim detection (off-circuit spent set): hash(note_id).
pub fn nullifier(note_id: &str) -> [u8; 32] {
    Sha256::digest(format!("fpl-bitvm2-nullifier|{note_id}").as_bytes()).into()
}
