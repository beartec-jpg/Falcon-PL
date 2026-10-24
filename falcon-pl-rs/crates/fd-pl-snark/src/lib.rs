//! Groth16 light-client attestations for Falcon PL headers on Ethereum.
//!
//! Two circuits share the same public IO:
//!
//! - [`FplLcCircuit`] — k-of-n BabyJubJub Schnorr. **Live** on Sepolia 2300.
//!   Extra LC keys; same 4-of-6 operators. Not a Falcon light client.
//! - [`falcon::FplFalconQcCircuit`] — k Falcon-512 signatures (HashToPoint +
//!   ring equation + L2 bound) under the validator Falcon public keys.
//!   This is the light-client cut. Not deployed (`lcKeyRoot` is immutable).

pub mod ceremony;
pub mod circuit;
pub mod codec;
pub mod falcon;
pub mod groth16;
pub mod mimc;
pub mod schnorr;

pub use circuit::FplLcCircuit;
pub use falcon::{FplFalconQcCircuit, FplFalconQcCircuitV2};
pub use codec::{
    fr_from_be_hex, fr_to_be_hex, public_inputs, public_inputs_v2, split_hash, N_PUBLIC, N_PUBLIC_V2,
};
pub use groth16::{
    export_proof, export_vk, load_pk, load_vk_bin, prove, prove_falcon, prove_falcon_open,
    prove_falcon_v2, save_pk, save_vk_bin, save_vk_json, setup, setup_falcon, setup_falcon_open,
    setup_falcon_v2, verify, SolidityProof, SolidityVk,
};
pub use schnorr::{
    committee_key_root, generate, pad_committee, statement_digest, LcPublic, LcSecret, LcSig,
    MAX_LEAVES,
};
