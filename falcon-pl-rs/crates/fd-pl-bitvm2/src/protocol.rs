//! BitVM2 optimistic protocol types (Phase C core).
//!
//! Full on-chain Groth16 verifier tapscript split is **not** included yet —
//! the emulator verifies `FplWithdrawValid` off-chain as the Assert/Disprove
//! oracle. Bitcoin testnet scaffolding commits claim digests for later wiring.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Testnet demo defaults (seconds). Production BitVM2 uses multi-day windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timeouts {
    /// Challenge window after Claim (Δ).
    pub challenge_secs: u64,
    /// Operator must Assert within this after Challenge.
    pub assert_secs: u64,
    /// Challenger Disprove grace after Assert.
    pub disprove_secs: u64,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            challenge_secs: 24 * 3600,
            assert_secs: 12 * 3600,
            disprove_secs: 12 * 3600,
        }
    }
}

impl Timeouts {
    /// Fast windows for unit/emulator demos.
    pub fn demo() -> Self {
        Self {
            challenge_secs: 60,
            assert_secs: 30,
            disprove_secs: 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InstanceConfig {
    pub network_id: u32,
    pub bond_sats: u64,
    pub timeouts: Timeouts,
}

impl Default for InstanceConfig {
    fn default() -> Self {
        Self {
            network_id: 2300,
            bond_sats: 100_000,
            timeouts: Timeouts::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimPhase {
    /// Operator posted Claim; waiting for Challenge or Δ.
    Open,
    /// Challenged; waiting for Assert.
    Challenged,
    /// Asserted; waiting for Disprove or grace.
    Asserted,
    /// No successful dispute — operator reimbursed.
    PaidOptimistic,
    /// Challenger won — bond forfeited.
    Disproved,
    /// Operator missed Assert deadline.
    AssertTimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimRecord {
    pub claim_id: String,
    pub operator: String,
    pub amount_sats: u64,
    pub note_id: String,
    pub pegout_txid_hex: String,
    /// SHA256 of (proof_bytes || public_fr_be_concat).
    pub statement_digest_hex: String,
    pub proof_hex: String,
    pub public_hex: Vec<String>,
    pub phase: ClaimPhase,
    pub opened_at: u64,
    pub challenged_at: Option<u64>,
    pub asserted_at: Option<u64>,
    pub challenger: Option<String>,
}

/// Domain-separated digest committed in a Claim (and later on-chain OP_RETURN).
pub fn statement_digest(proof_bytes: &[u8], public_fr_be: &[Vec<u8>]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"fpl-bitvm2-claim-v1|");
    h.update(proof_bytes);
    for p in public_fr_be {
        h.update(p);
    }
    h.finalize().into()
}

pub fn claim_id(operator: &str, note_id: &str, pegout_txid_hex: &str) -> String {
    let h = Sha256::digest(format!("fpl-bitvm2-id|{operator}|{note_id}|{pegout_txid_hex}").as_bytes());
    hex::encode(h)
}
