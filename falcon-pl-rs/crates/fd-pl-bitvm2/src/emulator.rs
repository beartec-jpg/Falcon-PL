//! In-process BitVM2 optimistic engine (Phase C).
//!
//! Verifies `FplWithdrawValid` Groth16 off-chain in place of the future
//! on-chain Assert/Disprove tapscript game.

use crate::abi::N_PUBLIC;
use crate::groth16::{self, proof_from_bytes, public_from_be_hex};
use crate::protocol::{
    claim_id, statement_digest, ClaimPhase, ClaimRecord, InstanceConfig,
};
use ark_bn254::Fr;
use ark_groth16::VerifyingKey;
use ark_bn254::Bn254;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("{0}")]
    Msg(String),
}

impl From<String> for EngineError {
    fn from(s: String) -> Self {
        EngineError::Msg(s)
    }
}

impl From<&str> for EngineError {
    fn from(s: &str) -> Self {
        EngineError::Msg(s.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct InstanceState {
    pub config: InstanceConfig,
    pub bond_remaining: u64,
    pub claims: HashMap<String, ClaimRecord>,
    /// Wall clock for the emulator (unix secs). Advance with [`Emulator::set_now`].
    pub now: u64,
}

pub struct Emulator {
    pub state: InstanceState,
    vk: VerifyingKey<Bn254>,
}

impl Emulator {
    pub fn new(config: InstanceConfig, vk: VerifyingKey<Bn254>, now: u64) -> Self {
        let bond = config.bond_sats;
        Self {
            state: InstanceState {
                config,
                bond_remaining: bond,
                claims: HashMap::new(),
                now,
            },
            vk,
        }
    }

    pub fn set_now(&mut self, now: u64) {
        self.state.now = now;
    }

    pub fn now(&self) -> u64 {
        self.state.now
    }

    /// Operator posts a Claim with a Groth16 proof of `FplWithdrawValid`.
    pub fn open_claim(
        &mut self,
        operator: &str,
        note_id: &str,
        amount_sats: u64,
        pegout_txid_hex: &str,
        proof_bytes: &[u8],
        public: &[Fr],
    ) -> Result<String, EngineError> {
        if public.len() != N_PUBLIC {
            return Err(format!("need {N_PUBLIC} public inputs").into());
        }
        if amount_sats == 0 {
            return Err("amount".into());
        }
        if amount_sats > self.state.bond_remaining {
            return Err("bond insufficient for claim amount".into());
        }
        let id = claim_id(operator, note_id, pegout_txid_hex);
        if self.state.claims.contains_key(&id) {
            return Err("claim exists".into());
        }
        let public_hex: Vec<String> = public
            .iter()
            .map(|f| {
                use ark_ff::{BigInteger, PrimeField};
                let mut le = f.into_bigint().to_bytes_le();
                le.resize(32, 0);
                le.reverse();
                hex::encode(le)
            })
            .collect();
        let public_be: Vec<Vec<u8>> = public_hex
            .iter()
            .map(|h| hex::decode(h).unwrap_or_default())
            .collect();
        let digest = statement_digest(proof_bytes, &public_be);
        let rec = ClaimRecord {
            claim_id: id.clone(),
            operator: operator.to_string(),
            amount_sats,
            note_id: note_id.to_string(),
            pegout_txid_hex: pegout_txid_hex.to_string(),
            statement_digest_hex: hex::encode(digest),
            proof_hex: hex::encode(proof_bytes),
            public_hex,
            phase: ClaimPhase::Open,
            opened_at: self.state.now,
            challenged_at: None,
            asserted_at: None,
            challenger: None,
        };
        self.state.claims.insert(id.clone(), rec);
        Ok(id)
    }

    pub fn challenge(&mut self, claim_id: &str, challenger: &str) -> Result<(), EngineError> {
        let c = self
            .state
            .claims
            .get_mut(claim_id)
            .ok_or_else(|| "no claim".to_string())?;
        if c.phase != ClaimPhase::Open {
            return Err("not open".into());
        }
        let deadline = c.opened_at + self.state.config.timeouts.challenge_secs;
        if self.state.now > deadline {
            return Err("challenge window closed".into());
        }
        c.phase = ClaimPhase::Challenged;
        c.challenged_at = Some(self.state.now);
        c.challenger = Some(challenger.to_string());
        Ok(())
    }

    /// Operator re-posts the proof material (Phase C: same as Claim payload).
    pub fn assert_claim(&mut self, claim_id: &str) -> Result<(), EngineError> {
        let c = self
            .state
            .claims
            .get_mut(claim_id)
            .ok_or_else(|| "no claim".to_string())?;
        if c.phase != ClaimPhase::Challenged {
            return Err("not challenged".into());
        }
        let challenged_at = c.challenged_at.ok_or_else(|| "no challenge time".to_string())?;
        let deadline = challenged_at + self.state.config.timeouts.assert_secs;
        if self.state.now > deadline {
            c.phase = ClaimPhase::AssertTimedOut;
            return Err("assert timed out".into());
        }
        c.phase = ClaimPhase::Asserted;
        c.asserted_at = Some(self.state.now);
        Ok(())
    }

    /// Challenger attempts Disprove by re-verifying Groth16. Wins if proof invalid.
    pub fn disprove(&mut self, claim_id: &str) -> Result<bool, EngineError> {
        let (phase, asserted_at, proof_hex, public_hex, amount) = {
            let c = self
                .state
                .claims
                .get(claim_id)
                .ok_or_else(|| "no claim".to_string())?;
            (
                c.phase,
                c.asserted_at,
                c.proof_hex.clone(),
                c.public_hex.clone(),
                c.amount_sats,
            )
        };
        if phase != ClaimPhase::Asserted {
            return Err("not asserted".into());
        }
        let asserted_at = asserted_at.ok_or_else(|| "no assert time".to_string())?;
        let deadline = asserted_at + self.state.config.timeouts.disprove_secs;
        if self.state.now > deadline {
            return Err("disprove window closed".into());
        }
        let proof_bytes = hex::decode(&proof_hex).map_err(|e| e.to_string())?;
        let proof = proof_from_bytes(&proof_bytes)?;
        let public = public_from_be_hex(&public_hex)?;
        let ok = groth16::verify(&self.vk, &public, &proof)?;
        let c = self.state.claims.get_mut(claim_id).unwrap();
        if ok {
            // Valid proof — Disprove fails; claim stays Asserted until grace ends.
            return Ok(false);
        }
        c.phase = ClaimPhase::Disproved;
        // Bond slash: forfeit claim amount from remaining bond.
        self.state.bond_remaining = self.state.bond_remaining.saturating_sub(amount);
        Ok(true)
    }

    /// After Δ with no challenge, or after failed disprove grace — pay operator.
    pub fn payout_optimistic(&mut self, claim_id: &str) -> Result<u64, EngineError> {
        let (phase, opened_at, asserted_at, amount) = {
            let c = self
                .state
                .claims
                .get(claim_id)
                .ok_or_else(|| "no claim".to_string())?;
            (c.phase, c.opened_at, c.asserted_at, c.amount_sats)
        };
        match phase {
            ClaimPhase::Open => {
                let deadline = opened_at + self.state.config.timeouts.challenge_secs;
                if self.state.now < deadline {
                    return Err("challenge window still open".into());
                }
            }
            ClaimPhase::Asserted => {
                let asserted_at = asserted_at.ok_or_else(|| "no assert".to_string())?;
                let deadline = asserted_at + self.state.config.timeouts.disprove_secs;
                if self.state.now < deadline {
                    return Err("disprove window still open".into());
                }
            }
            ClaimPhase::PaidOptimistic => return Ok(amount),
            _ => return Err(format!("cannot payout from {phase:?}").into()),
        }
        if amount > self.state.bond_remaining {
            return Err("bond insolvent".into());
        }
        self.state.bond_remaining -= amount;
        let c = self.state.claims.get_mut(claim_id).unwrap();
        c.phase = ClaimPhase::PaidOptimistic;
        Ok(amount)
    }

    /// Tick timeouts: Open past Δ → eligible for payout; Challenged past assert → AssertTimedOut.
    pub fn tick(&mut self) {
        let now = self.state.now;
        let t = self.state.config.timeouts;
        for c in self.state.claims.values_mut() {
            if c.phase == ClaimPhase::Challenged {
                if let Some(ca) = c.challenged_at {
                    if now > ca + t.assert_secs {
                        c.phase = ClaimPhase::AssertTimedOut;
                    }
                }
            }
        }
    }
}
