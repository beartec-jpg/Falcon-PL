//! Bitcoin testnet scaffolding for a BitVM2 instance (Phase C).
//!
//! Does **not** yet emit the full Groth16-verifier tapscript tree. It defines
//! the commit formats and funding metadata so Phase D can broadcast real txs.

use crate::protocol::InstanceConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestnetInstance {
    pub network: String,
    pub config: InstanceConfig,
    /// Placeholder funding outpoint once funded on testnet.
    pub funding_txid_hex: Option<String>,
    pub funding_vout: Option<u32>,
    pub funding_sats: u64,
    /// Setup signer x-only pubkeys (hex), 1-of-n honesty at ceremony.
    pub setup_xonly_hex: Vec<String>,
    pub notes: String,
}

impl TestnetInstance {
    pub fn skeleton(config: InstanceConfig, setup_xonly_hex: Vec<String>) -> Self {
        Self {
            network: "bitcoin-testnet".into(),
            funding_sats: config.bond_sats,
            config,
            funding_txid_hex: None,
            funding_vout: None,
            setup_xonly_hex,
            notes: "Phase D.1: NUMS instance + CSV operator payout + setup n-of-n Challenge template. Groth16 Disprove still emulator / BitVM pack.".into(),
        }
    }

    /// OP_RETURN / commit payload for a Claim statement digest (32 bytes).
    pub fn claim_commit_script_data(statement_digest: &[u8; 32]) -> Vec<u8> {
        let mut v = Vec::with_capacity(40);
        v.extend_from_slice(b"FPL2"); // 4-byte tag
        v.extend_from_slice(statement_digest);
        v
    }

    pub fn instance_id_hint(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"fpl-bitvm2-instance|");
        h.update(self.network.as_bytes());
        h.update(self.funding_sats.to_le_bytes());
        for pk in &self.setup_xonly_hex {
            h.update(pk.as_bytes());
        }
        hex::encode(h.finalize())
    }
}
