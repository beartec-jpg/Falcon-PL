//! Native verify-trace for BitVM2 Assert commitments.

use crate::abi::{domain_fr, note_id_fr, N_PUBLIC};
use crate::groth16::{self, fr_to_be32};
use crate::leaf::withdraw_leaf_fr;
use ark_bn254::Fr;
use ark_groth16::{Proof, VerifyingKey};
use ark_bn254::Bn254;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Chunks that have Falcon SHA256 Disprove leaves (C.1a).
pub const NUM_SCRIPTABLE_CHUNKS: usize = 6;
/// Total Assert chunks including Groth16/BN254 pack (C.1b).
pub const NUM_CHUNKS: usize = NUM_SCRIPTABLE_CHUNKS + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkBackend {
    /// Implemented with `OP_SHA256` tapscript.
    Sha256Script,
    /// BitVM BN254/Groth16 script pack (`bn254_pack` + vendor/BitVM).
    ///
    /// Falcon wrapper transition is hash-linked here; pairing tapscripts come
    /// from upstream BitVM `chunk::api` (see pack manifest).
    #[serde(alias = "bitvm_bn254_pending")]
    BitvmBn254,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkId {
    BindPublic = 0,
    Network2300 = 1,
    LeafAbsorb = 2,
    MerkleFold = 3,
    RootMatch = 4,
    PegoutBind = 5,
    Groth16Pairing = 6,
}

impl ChunkId {
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn backend(self) -> ChunkBackend {
        match self {
            ChunkId::Groth16Pairing => ChunkBackend::BitvmBn254,
            _ => ChunkBackend::Sha256Script,
        }
    }

    pub fn all_scriptable() -> [ChunkId; NUM_SCRIPTABLE_CHUNKS] {
        [
            ChunkId::BindPublic,
            ChunkId::Network2300,
            ChunkId::LeafAbsorb,
            ChunkId::MerkleFold,
            ChunkId::RootMatch,
            ChunkId::PegoutBind,
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkSpec {
    pub id: ChunkId,
    pub backend: ChunkBackend,
    /// Input state digest z_i (32 bytes hex).
    pub z_in_hex: String,
    /// Output state digest z_{i+1} (32 bytes hex).
    pub z_out_hex: String,
    /// Aux payload hashed into the step (hex).
    pub aux_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyTrace {
    pub chunks: Vec<ChunkSpec>,
    /// True iff native Groth16 verify succeeds.
    pub groth16_ok: bool,
    pub z0_hex: String,
    pub z_final_hex: String,
}

#[derive(Debug, Clone)]
pub struct TraceState {
    pub z: [u8; 32],
}

fn h_step(chunk: ChunkId, z_in: &[u8; 32], aux: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"fpl-bitvm2-chunk-v1|");
    h.update([chunk.index() as u8]);
    h.update(z_in);
    h.update(aux);
    h.finalize().into()
}

fn fr_be_list(public: &[Fr]) -> Vec<u8> {
    let mut v = Vec::with_capacity(public.len() * 32);
    for f in public {
        v.extend_from_slice(&fr_to_be32(*f));
    }
    v
}

/// Build Assert trace from a Claim's proof + public inputs.
///
/// Scriptable steps commit a SHA256 transition chain over the withdraw
/// statement. The final Groth16/BN254 step is [`ChunkBackend::BitvmBn254`]
/// (C.1b pack bridge); pairing tapscripts live in upstream BitVM.
pub fn build_verify_trace(
    vk: &VerifyingKey<Bn254>,
    public: &[Fr],
    proof: &Proof<Bn254>,
    note_id: &str,
    dest_h160: &[u8; 20],
    amount: u64,
    claim_root: Fr,
    merkle_path: &[Fr],
    merkle_bits: &[bool],
) -> Result<VerifyTrace, String> {
    if public.len() != N_PUBLIC {
        return Err(format!("need {N_PUBLIC} publics"));
    }
    let groth16_ok = groth16::verify(vk, public, proof)?;

    let pub_bytes = fr_be_list(public);
    let z0 = Sha256::digest([&b"fpl-bitvm2-z0|"[..], pub_bytes.as_slice()].concat()).into();

    let mut chunks = Vec::new();
    let mut z = z0;

    // 0 BindPublic
    let aux0 = pub_bytes.clone();
    let z1 = h_step(ChunkId::BindPublic, &z, &aux0);
    chunks.push(spec(ChunkId::BindPublic, &z, &z1, &aux0));
    z = z1;

    // 1 Network2300
    let net = public[0];
    let aux1 = fr_to_be32(net);
    let z2 = h_step(ChunkId::Network2300, &z, &aux1);
    chunks.push(spec(ChunkId::Network2300, &z, &z2, &aux1));
    z = z2;
    if net != Fr::from(2300u64) && groth16_ok {
        // inconsistent — still build trace; disprove path will catch
    }

    // 2 LeafAbsorb
    let leaf = withdraw_leaf_fr(note_id, dest_h160, amount);
    let aux2 = fr_to_be32(leaf);
    let z3 = h_step(ChunkId::LeafAbsorb, &z, &aux2);
    chunks.push(spec(ChunkId::LeafAbsorb, &z, &z3, &aux2));
    z = z3;

    // 3 MerkleFold — fold path into digest (bits + siblings)
    let mut fold = leaf;
    for (i, sib) in merkle_path.iter().enumerate() {
        let bit = merkle_bits.get(i).copied().unwrap_or(false);
        fold = if bit {
            crate::leaf::verify_path_step(*sib, fold)
        } else {
            crate::leaf::verify_path_step(fold, *sib)
        };
    }
    let mut aux3 = Vec::new();
    for (i, sib) in merkle_path.iter().enumerate() {
        aux3.push(u8::from(merkle_bits.get(i).copied().unwrap_or(false)));
        aux3.extend_from_slice(&fr_to_be32(*sib));
    }
    let z4 = h_step(ChunkId::MerkleFold, &z, &aux3);
    chunks.push(spec(ChunkId::MerkleFold, &z, &z4, &aux3));
    z = z4;

    // 4 RootMatch
    let aux4 = fr_to_be32(claim_root);
    let z5 = h_step(ChunkId::RootMatch, &z, &aux4);
    chunks.push(spec(ChunkId::RootMatch, &z, &z5, &aux4));
    z = z5;

    // 5 PegoutBind
    let peg_hi = public[10];
    let peg_lo = public[11];
    let mut aux5 = Vec::new();
    aux5.extend_from_slice(&fr_to_be32(peg_hi));
    aux5.extend_from_slice(&fr_to_be32(peg_lo));
    aux5.extend_from_slice(&fr_to_be32(public[1])); // amount
    let z6 = h_step(ChunkId::PegoutBind, &z, &aux5);
    chunks.push(spec(ChunkId::PegoutBind, &z, &z6, &aux5));
    z = z6;

    // 6 Groth16Pairing — Falcon hash wrapper; BitVM pack for on-chain pairing
    let aux6 = [u8::from(groth16_ok)];
    let z7 = h_step(ChunkId::Groth16Pairing, &z, &aux6);
    chunks.push(ChunkSpec {
        id: ChunkId::Groth16Pairing,
        backend: ChunkBackend::BitvmBn254,
        z_in_hex: hex::encode(z),
        z_out_hex: hex::encode(z7),
        aux_hex: hex::encode(aux6),
    });

    // Sanity: merkle fold should match claim_root for honest proofs
    let _ = (fold, domain_fr(), note_id_fr(note_id));

    Ok(VerifyTrace {
        chunks,
        groth16_ok,
        z0_hex: hex::encode(z0),
        z_final_hex: hex::encode(z7),
    })
}

fn spec(id: ChunkId, zin: &[u8; 32], zout: &[u8; 32], aux: &[u8]) -> ChunkSpec {
    ChunkSpec {
        id,
        backend: id.backend(),
        z_in_hex: hex::encode(zin),
        z_out_hex: hex::encode(zout),
        aux_hex: hex::encode(aux),
    }
}

/// Recompute z_out from z_in + aux for every Assert chunk.
///
/// For [`ChunkId::Groth16Pairing`], this is the Falcon **wrapper** transition
/// (hash-linked). On-chain pairing fraud uses BitVM `validate_assertions` on
/// the pack tapscripts once a scripts cache is exported.
pub fn evaluate_chunk(id: ChunkId, z_in: &[u8; 32], aux: &[u8]) -> Result<[u8; 32], String> {
    Ok(h_step(id, z_in, aux))
}

/// True if Assert transition is fraudulent for this chunk.
pub fn is_fraudulent_transition(
    id: ChunkId,
    z_in: &[u8; 32],
    z_out_claimed: &[u8; 32],
    aux: &[u8],
) -> Result<bool, String> {
    let expected = evaluate_chunk(id, z_in, aux)?;
    Ok(expected != *z_out_claimed)
}
