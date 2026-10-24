//! BitVM2 verifier-split (Phase C.1).
//!
//! Splits `FplWithdrawValid` checking into hash-linked chunks. Scriptable
//! chunks use BIP341 tapscript (`OP_SHA256`) so a challenger can Disprove a
//! bad Assert transition on Bitcoin. The Groth16/BN254 pairing chunk uses the
//! BitVM upstream script pack via [`bn254_pack`] (C.1b) — constants + Disprove
//! bind leaf here; full tapscript blobs from `vendor/BitVM` / export tool.

mod bn254_pack;
mod script;
mod taptree;
mod trace;

pub use bn254_pack::{
    default_manifest, effective_manifest, load_scripts_cache, pack_commitment, pack_disprove_preimage,
    pack_disprove_script, vendor_bitvm_present, verify_vendor_constants, BitvmEntrypoints,
    BitvmPackManifest, BitvmScriptsCache, BITVM_NUM_HASH, BITVM_NUM_PUBS, BITVM_NUM_TAPS,
    BITVM_NUM_U256, PACK_ID, UPSTREAM_COMMIT, UPSTREAM_REPO, VENDOR_BITVM_REL,
};
pub use script::{disprove_preimage, disprove_script, push_bytes, Op};
pub use taptree::{
    build_disprove_taptree, merkle_path, merkle_root, tapbranch, tapleaf_hash, DisproveTapTree,
    TapLeaf,
};
pub use trace::{
    build_verify_trace, evaluate_chunk, is_fraudulent_transition, ChunkBackend, ChunkId, ChunkSpec,
    TraceState, VerifyTrace, NUM_CHUNKS, NUM_SCRIPTABLE_CHUNKS,
};
