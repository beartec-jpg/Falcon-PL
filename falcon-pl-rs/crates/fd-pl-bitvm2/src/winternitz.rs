//! Lamport one-time signatures for BitVM2 Assert / equivocation slash.
//!
//! A 32-byte digest is 256 bits. Each bit has a pair of SHA256 pubkeys.
//! Signing a second, different digest reveals both preimages for some bit;
//! that leaf is a permissionless spend (equivocation Disprove).
//!
//! Does not need OP_CAT. Does not by itself prove a SHA256-step lie (that
//! still needs BitVM pack / a SHA256 circuit). It *does* slash an operator
//! who Asserts two conflicting traces.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const LAMPORT_BITS: usize = 256;

#[derive(Clone)]
pub struct LamportSk {
    pub zero: Vec<[u8; 32]>,
    pub one: Vec<[u8; 32]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LamportPk {
    pub zero_hex: Vec<String>,
    pub one_hex: Vec<String>,
}

impl LamportPk {
    pub fn bit_pks(&self, i: usize) -> Result<([u8; 32], [u8; 32]), String> {
        if i >= LAMPORT_BITS || i >= self.zero_hex.len() || i >= self.one_hex.len() {
            return Err("lamport bit".into());
        }
        Ok((decode32(&self.zero_hex[i])?, decode32(&self.one_hex[i])?))
    }
}

fn decode32(h: &str) -> Result<[u8; 32], String> {
    let b = hex::decode(h.trim()).map_err(|e| e.to_string())?;
    if b.len() != 32 {
        return Err("lamport pk not 32".into());
    }
    let mut o = [0u8; 32];
    o.copy_from_slice(&b);
    Ok(o)
}

fn sha256_32(x: &[u8; 32]) -> [u8; 32] {
    Sha256::digest(x).into()
}

pub fn keygen(fill: impl FnMut(&mut [u8])) -> (LamportSk, LamportPk) {
    let mut fill = fill;
    let mut zero = vec![[0u8; 32]; LAMPORT_BITS];
    let mut one = vec![[0u8; 32]; LAMPORT_BITS];
    for i in 0..LAMPORT_BITS {
        fill(&mut zero[i]);
        fill(&mut one[i]);
    }
    let pk = LamportPk {
        zero_hex: zero.iter().map(sha256_32).map(hex::encode).collect(),
        one_hex: one.iter().map(sha256_32).map(hex::encode).collect(),
    };
    (LamportSk { zero, one }, pk)
}

pub fn sign(sk: &LamportSk, msg: &[u8; 32]) -> Result<Vec<[u8; 32]>, String> {
    if sk.zero.len() != LAMPORT_BITS || sk.one.len() != LAMPORT_BITS {
        return Err("lamport sk size".into());
    }
    let mut sig = Vec::with_capacity(LAMPORT_BITS);
    for i in 0..LAMPORT_BITS {
        let bit = (msg[i / 8] >> (7 - (i % 8))) & 1;
        sig.push(if bit == 1 { sk.one[i] } else { sk.zero[i] });
    }
    Ok(sig)
}

pub fn verify(pk: &LamportPk, msg: &[u8; 32], sig: &[[u8; 32]]) -> Result<(), String> {
    if sig.len() != LAMPORT_BITS || pk.zero_hex.len() != LAMPORT_BITS {
        return Err("lamport size".into());
    }
    for i in 0..LAMPORT_BITS {
        let bit = (msg[i / 8] >> (7 - (i % 8))) & 1;
        let want = if bit == 1 {
            decode32(&pk.one_hex[i])?
        } else {
            decode32(&pk.zero_hex[i])?
        };
        if sha256_32(&sig[i]) != want {
            return Err(format!("lamport bit {i}"));
        }
    }
    Ok(())
}

/// Equivocation leaf: both preimages for bit i. Witness: pre0, pre1 (pre1 top).
pub fn equivocation_script(pk0: &[u8; 32], pk1: &[u8; 32]) -> Vec<u8> {
    let mut s = Vec::with_capacity(70);
    s.push(0xa8); // SHA256
    s.push(0x20);
    s.extend_from_slice(pk1);
    s.push(0x88); // EQUALVERIFY
    s.push(0xa8);
    s.push(0x20);
    s.extend_from_slice(pk0);
    s.push(0x88);
    s
}

/// True if `sig_a` and `sig_b` (two Lamport sigs) reveal both preimages of `bit`.
pub fn equivocation_preimages(
    sk_unused: Option<&LamportSk>,
    sig_a: &[[u8; 32]],
    sig_b: &[[u8; 32]],
    msg_a: &[u8; 32],
    msg_b: &[u8; 32],
    bit: usize,
) -> Option<([u8; 32], [u8; 32])> {
    let _ = sk_unused;
    if bit >= LAMPORT_BITS || sig_a.len() != LAMPORT_BITS || sig_b.len() != LAMPORT_BITS {
        return None;
    }
    let ba = (msg_a[bit / 8] >> (7 - (bit % 8))) & 1;
    let bb = (msg_b[bit / 8] >> (7 - (bit % 8))) & 1;
    if ba == bb {
        return None;
    }
    let (pre0, pre1) = if ba == 0 {
        (sig_a[bit], sig_b[bit])
    } else {
        (sig_b[bit], sig_a[bit])
    };
    Some((pre0, pre1))
}

pub fn vk_root(pk: &LamportPk) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"fpl-bitvm2-lamport-vk-v1|");
    for i in 0..pk.zero_hex.len() {
        h.update(pk.zero_hex[i].as_bytes());
        h.update(pk.one_hex[i].as_bytes());
    }
    h.finalize().into()
}
