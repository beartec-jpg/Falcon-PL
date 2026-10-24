//! M4 ceremony primitives: each participant holds its key in RAM only, signs the
//! graph sighashes, then wipes the key and emits a signed deletion attestation.
//! Testnet: a single operator may play all n participants (logged; labelled
//! "testnet single-party setup", never "non-custodial").

use crate::{secp, sha256_hex};
use bitcoin::secp256k1::{Keypair, Message, SecretKey, XOnlyPublicKey};
use serde::{Deserialize, Serialize};

/// One participant. The secret lives only in this struct (RAM) and is wiped.
pub struct Participant {
    pub id: String,
    kp: Option<Keypair>,
    pub xonly: XOnlyPublicKey,
}

impl Participant {
    /// Fresh key from OS randomness, RAM only.
    pub fn new(id: &str) -> Self {
        let s = secp();
        let kp = Keypair::new(&s, &mut bitcoin::secp256k1::rand::thread_rng());
        let (xonly, _) = kp.x_only_public_key();
        Self { id: id.into(), kp: Some(kp), xonly }
    }

    pub fn sign(&self, msg: &Message) -> Result<[u8; 64], String> {
        let kp = self.kp.as_ref().ok_or("key already wiped")?;
        let sig = secp().sign_schnorr_no_aux_rand(msg, kp);
        Ok(sig.serialize())
    }

    /// Overwrite and drop the secret. Returns a deletion attestation signed with
    /// the key *before* it is wiped (proves the holder had it, then destroyed it).
    pub fn wipe(&mut self, transcript_hash: &str) -> DeletionAttestation {
        let stmt = format!("fpl-ceremony-deleted|{}|{}|{}", self.id, hex::encode(self.xonly.serialize()), transcript_hash);
        let digest = bitcoin::hashes::sha256::Hash::hash(stmt.as_bytes());
        let msg = Message::from_digest(bitcoin::hashes::Hash::to_byte_array(digest));
        let sig = self.sign(&msg).map(hex::encode).unwrap_or_default();
        if let Some(mut kp) = self.kp.take() {
            let mut sk = SecretKey::from_keypair(&kp);
            sk.non_secure_erase();
            kp.non_secure_erase();
        }
        DeletionAttestation { participant: self.id.clone(), xonly: hex::encode(self.xonly.serialize()), statement: stmt, sig }
    }

    pub fn is_wiped(&self) -> bool {
        self.kp.is_none()
    }
}

use bitcoin::hashes::Hash;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletionAttestation {
    pub participant: String,
    pub xonly: String,
    pub statement: String,
    pub sig: String,
}

impl DeletionAttestation {
    pub fn verify(&self) -> bool {
        let Ok(pk) = hex::decode(&self.xonly) else { return false };
        let Ok(xo) = XOnlyPublicKey::from_slice(&pk) else { return false };
        let Ok(sigb) = hex::decode(&self.sig) else { return false };
        let Ok(sig) = bitcoin::secp256k1::schnorr::Signature::from_slice(&sigb) else { return false };
        let digest = bitcoin::hashes::sha256::Hash::hash(self.statement.as_bytes());
        let msg = Message::from_digest(digest.to_byte_array());
        secp().verify_schnorr(&sig, &msg, &xo).is_ok()
    }
}

/// Hash of the full ceremony transcript (pubkeys + presigned txids), recorded
/// in the deletion attestations and the public log.
pub fn transcript_hash(pubkeys: &[XOnlyPublicKey], txids: &[String]) -> String {
    let mut s = String::from("fpl-ceremony-v1");
    for k in pubkeys {
        s.push('|');
        s.push_str(&hex::encode(k.serialize()));
    }
    for t in txids {
        s.push('|');
        s.push_str(t);
    }
    sha256_hex(s.as_bytes())
}
