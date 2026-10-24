//! Falcon-512-in-circuit light client (replaces BabyJubJub 4-of-6 LC).
//!
//! Ethereum verifies a Groth16 proof that k Falcon-512 signatures over the
//! dest-lock header statement are valid under public keys whose packed-MiMC
//! merkle root equals `key_root`. Those keys are the 2300 validator Falcon
//! keys, not a side committee.
//!
//! This is a light client of 2300 consensus. It is **not** deployed on the
//! live Sepolia bridge (`lcKeyRoot` is immutable; new verifier + bridge).
//! 2300 is still a 4-of-6 validator set — that is source-chain consensus,
//! not a dest-lock custodian with extra secrets.

pub mod circuit;
pub mod circuit_v2;
pub mod gadget;
pub mod native;
pub mod shake;

pub use circuit::FplFalconQcCircuit;
pub use circuit_v2::FplFalconQcCircuitV2;
pub use native::{
    committee_key_root, hash_to_point_ct, pk_leaf, statement_bytes, statement_bytes_v2,
    verify_detached, FalconPublic, FalconSig, FalconVerifyWitness, L2BOUND, LOGN, N, NONCE_LEN,
    PK_LEN, Q, SIG_MAX_LEN, STATEMENT_DOMAIN, STATEMENT_DOMAIN_V2,
};
