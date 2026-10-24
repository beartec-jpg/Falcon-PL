//! BitVM2 engine — Phases B–D.1.
//!
//! Spec: `fd-pl/docs/BITVM2_ENGINE.md`.

pub mod abi;
pub mod circuit;
pub mod emulator;
pub mod groth16;
pub mod instance;
pub mod leaf;
pub mod protocol;
pub mod snarkjs_io;
pub mod spend;
pub mod verifier_split;
pub mod winternitz;
pub mod withdraw_ops;

pub use abi::{nullifier, public_inputs, test_vk_hash, CIRCUIT_VERSION, DOMAIN, N_PUBLIC};
pub use circuit::{FplWithdrawValidV3, v3_public_input, v3_message, v3_binding_commit, fr_to_be32, public_digest, FplWithdrawValid, FplWithdrawValidV2};
pub use emulator::{Emulator, EngineError, InstanceState};
pub use groth16::{proof_from_bytes, proof_to_bytes, prove, setup, verify};
pub use instance::TestnetInstance;
pub use leaf::{build_tree, verify_path, withdraw_leaf_fr};
pub use protocol::{claim_id, statement_digest, ClaimPhase, ClaimRecord, InstanceConfig, Timeouts};
pub use spend::{
    build_assert_connector_taptree, build_instance_taptree, operator_recover_script,
    kickoff_script, payout_optimistic_script, setup_nofn_script, AssertConnectorTree,
    InstanceTapTree, ASSERT_CSV_BLOCKS, INSTANCE_NUMS_TAG, KIND_KICKOFF, KIND_OPERATOR_RECOVER,
    KIND_PAYOUT, KIND_SETUP, PAYOUT_CSV_BLOCKS,
};
pub use verifier_split::{
    build_disprove_taptree, build_verify_trace, default_manifest, disprove_preimage, evaluate_chunk,
    is_fraudulent_transition, merkle_path, ChunkBackend, ChunkId, DisproveTapTree, VerifyTrace,
    BITVM_NUM_TAPS, PACK_ID,
};
