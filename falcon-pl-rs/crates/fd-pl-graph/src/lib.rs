//! M2: BitVM2-style presigned peg-out graph for the Falcon PL BTC bridge.
//!
//! Design rules this crate enforces structurally:
//! - The deposit output has **no single-key leaf**: its taproot internal key is
//!   a NUMS point (provably unspendable) and the only script path is the
//!   n-of-n (all setup participants) key. So no one key, and no one machine's
//!   files, can move the deposit — only the fully presigned graph can.
//! - The graph is a set of **presigned templates**: Kickoff → Claim →
//!   (timeout Payout to operator) | (Challenge → Assert → Disprove-per-chunk |
//!   Assert-timeout). Signatures are produced once at the ceremony and the
//!   keys are wiped; spending later needs no fresh signature.
//! - Disprove leaves are Winternitz/Lamport **equivocation** leaves taken from
//!   the split verifier (`fd-pl-bitvm2`): if the operator Asserts two different
//!   traces for a chunk, anyone can spend the Disprove leaf. No abort branch
//!   (#46 removed): the dest-lock has only the user-payout and the n-of-n
//!   recovery path.
//! - Anchor/CPFP: every presigned tx carries a P2A-style anchor output so fees
//!   can be attached at broadcast time without re-signing.

use bitcoin::hashes::{sha256, Hash};
use bitcoin::key::Secp256k1;
use bitcoin::secp256k1::{Message, XOnlyPublicKey};
use bitcoin::taproot::{LeafVersion, TaprootBuilder, TaprootSpendInfo};
use bitcoin::{
    absolute, transaction, Amount, OutPoint, ScriptBuf, Sequence, TapLeafHash, TapSighashType,
    Transaction, TxIn, TxOut, Witness, Address, Network, opcodes::all as op,
};
use bitcoin::sighash::{Prevouts, SighashCache};
use serde::{Deserialize, Serialize};

pub mod ceremony;
pub mod graph;

/// Provably-unspendable BIP341 NUMS internal key (from BIP341 "H").
pub const NUMS_HEX: &str = "50929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0";

/// Dust-ish anchor value for CPFP (P2A). Keep tiny.
pub const ANCHOR_SATS: u64 = 240;

pub fn secp() -> Secp256k1<bitcoin::secp256k1::All> {
    Secp256k1::new()
}

pub fn nums_xonly() -> XOnlyPublicKey {
    let bytes = hex::decode(NUMS_HEX).unwrap();
    XOnlyPublicKey::from_slice(&bytes).unwrap()
}

/// n-of-n tapscript: `<P0> CHECKSIGVERIFY <P1> CHECKSIGVERIFY ... <Pn-1> CHECKSIG`.
pub fn nofn_script(keys: &[XOnlyPublicKey]) -> ScriptBuf {
    assert!(!keys.is_empty());
    let mut b = bitcoin::script::Builder::new();
    for (i, k) in keys.iter().enumerate() {
        b = b.push_x_only_key(k);
        if i + 1 == keys.len() {
            b = b.push_opcode(op::OP_CHECKSIG);
        } else {
            b = b.push_opcode(op::OP_CHECKSIGVERIFY);
        }
    }
    b.into_script()
}

/// CSV-timelocked operator payout leaf: `<n> CSV DROP <op> CHECKSIG`.
pub fn csv_key_script(blocks: u16, key: &XOnlyPublicKey) -> ScriptBuf {
    bitcoin::script::Builder::new()
        .push_int(blocks as i64)
        .push_opcode(op::OP_CSV)
        .push_opcode(op::OP_DROP)
        .push_x_only_key(key)
        .push_opcode(op::OP_CHECKSIG)
        .into_script()
}

/// Winternitz/Lamport equivocation Disprove leaf: reveal two preimages for one
/// bit (digest A bit=0, digest B bit=1) → anyone can spend. We model the leaf as
/// `OP_SHA256 <pk0> EQUALVERIFY OP_SHA256 <pk1> EQUALVERIFY` over the two
/// revealed preimages; the committed pk pair comes from the split verifier.
pub fn equivocation_leaf(pk0: &[u8; 32], pk1: &[u8; 32]) -> ScriptBuf {
    bitcoin::script::Builder::new()
        .push_opcode(op::OP_SHA256)
        .push_slice(pk0)
        .push_opcode(op::OP_EQUALVERIFY)
        .push_opcode(op::OP_SHA256)
        .push_slice(pk1)
        .push_opcode(op::OP_EQUALVERIFY)
        .push_opcode(op::OP_PUSHNUM_1)
        .into_script()
}

/// A taproot output: NUMS internal key + a set of named script leaves.
#[derive(Clone)]
pub struct TapOutput {
    pub info: TaprootSpendInfo,
    pub leaves: Vec<(String, ScriptBuf)>,
}

impl TapOutput {
    pub fn build(leaves: Vec<(String, ScriptBuf)>) -> Result<Self, String> {
        let s = secp();
        let mut b = TaprootBuilder::new();
        // Balanced tree by depth for small leaf counts.
        let n = leaves.len();
        let depth = if n <= 1 { 0 } else { (usize::BITS - (n - 1).leading_zeros()) as u8 };
        for (i, (_, script)) in leaves.iter().enumerate() {
            let d = if n == 1 { 0 } else if i == n - 1 && n.count_ones() != 1 { depth.saturating_sub(1) } else { depth };
            b = b.add_leaf(d, script.clone()).map_err(|e| format!("add_leaf: {e:?}"))?;
        }
        let info = b.finalize(&s, nums_xonly()).map_err(|e| format!("finalize: {e:?}"))?;
        Ok(Self { info, leaves })
    }

    pub fn spk(&self) -> ScriptBuf {
        ScriptBuf::new_p2tr_tweaked(self.info.output_key())
    }

    pub fn address(&self, network: Network) -> Address {
        Address::p2tr_tweaked(self.info.output_key(), network)
    }

    pub fn leaf(&self, name: &str) -> Option<&ScriptBuf> {
        self.leaves.iter().find(|(n, _)| n == name).map(|(_, s)| s)
    }
}

/// Minimal P2A anchor spk (OP_1 <0x4e73>) per the ephemeral-anchor pattern.
pub fn anchor_spk() -> ScriptBuf {
    bitcoin::script::Builder::new()
        .push_opcode(op::OP_PUSHNUM_1)
        .push_slice([0x4e, 0x73])
        .into_script()
}

/// Sighash for a taproot script-path spend of `input_index`.
pub fn script_path_sighash(
    tx: &Transaction,
    input_index: usize,
    prevouts: &[TxOut],
    leaf: &ScriptBuf,
) -> Result<Message, String> {
    let mut cache = SighashCache::new(tx);
    let leaf_hash = TapLeafHash::from_script(leaf, LeafVersion::TapScript);
    let sh = cache
        .taproot_script_spend_signature_hash(
            input_index,
            &Prevouts::All(prevouts),
            leaf_hash,
            TapSighashType::Default,
        )
        .map_err(|e| format!("sighash: {e}"))?;
    Ok(Message::from_digest(sh.to_byte_array()))
}

/// Control block push for a script-path witness.
pub fn control_block(info: &TaprootSpendInfo, leaf: &ScriptBuf) -> Result<Vec<u8>, String> {
    let cb = info
        .control_block(&(leaf.clone(), LeafVersion::TapScript))
        .ok_or("no control block for leaf")?;
    Ok(cb.serialize())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxTemplate {
    pub name: String,
    pub txid_unsigned: String,
    pub raw_unsigned_hex: String,
}

pub fn sha256_hex(b: &[u8]) -> String {
    hex::encode(sha256::Hash::hash(b).to_byte_array())
}

pub use bitcoin;
