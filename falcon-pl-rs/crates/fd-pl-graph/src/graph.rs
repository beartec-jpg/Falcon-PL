//! Presigned peg-out graph templates.
//!
//! deposit (NUMS + n-of-n leaf only)
//!   └─ Claim  (presigned n-of-n)  → claim output, leaves:
//!        - "payout":   <T> CSV DROP <n-of-n>   → Payout to operator (timeout path, presigned)
//!        - "disprove": equivocation leaf       → anyone (operator Asserted two traces)
//!   └─ Payout (presigned n-of-n, nSequence = T)
//!   └─ Disprove (unsigned; witness = the two revealed preimages)

use crate::*;
use bitcoin::secp256k1::XOnlyPublicKey;

pub fn nofn_csv_script(blocks: u16, keys: &[XOnlyPublicKey]) -> ScriptBuf {
    let mut b = bitcoin::script::Builder::new()
        .push_int(blocks as i64)
        .push_opcode(op::OP_CSV)
        .push_opcode(op::OP_DROP);
    for (i, k) in keys.iter().enumerate() {
        b = b.push_x_only_key(k);
        b = if i + 1 == keys.len() { b.push_opcode(op::OP_CHECKSIG) } else { b.push_opcode(op::OP_CHECKSIGVERIFY) };
    }
    b.into_script()
}

pub struct GraphParams {
    pub keys: Vec<XOnlyPublicKey>,
    pub payout_csv: u16,
    /// Lamport pair committed by the operator for one Assert bit (split verifier).
    pub equiv_pk0: [u8; 32],
    pub equiv_pk1: [u8; 32],
    pub fee_sats: u64,
    /// v3 (TRUC) presigned txs: may carry zero fee and be CPFP-bumped via the P2A anchor.
    pub truc: bool,
}

pub fn deposit_output(p: &GraphParams) -> Result<TapOutput, String> {
    // No single-key leaf: only the n-of-n.
    TapOutput::build(vec![("nofn".into(), nofn_script(&p.keys))])
}

pub fn claim_output(p: &GraphParams) -> Result<TapOutput, String> {
    TapOutput::build(vec![
        ("payout".into(), nofn_csv_script(p.payout_csv, &p.keys)),
        ("disprove".into(), equivocation_leaf(&p.equiv_pk0, &p.equiv_pk1)),
    ])
}

fn base_tx(input: OutPoint, seq: Sequence, outs: Vec<TxOut>) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn { previous_output: input, script_sig: ScriptBuf::new(), sequence: seq, witness: Witness::new() }],
        output: outs,
    }
}

/// Claim: deposit → claim output (+anchor). Presigned n-of-n.
pub fn claim_tx(p: &GraphParams, deposit: OutPoint, deposit_value: u64) -> Result<Transaction, String> {
    let co = claim_output(p)?;
    let v = deposit_value.checked_sub(p.fee_sats + ANCHOR_SATS).ok_or("deposit too small")?;
    Ok(maybe_truc(p, base_tx(deposit, Sequence::ENABLE_RBF_NO_LOCKTIME, vec![
        TxOut { value: Amount::from_sat(v), script_pubkey: co.spk() },
        TxOut { value: Amount::from_sat(ANCHOR_SATS), script_pubkey: anchor_spk() },
    ])))
}

fn maybe_truc(p: &GraphParams, mut tx: Transaction) -> Transaction {
    if p.truc {
        tx.version = transaction::Version(3);
    }
    tx
}

/// Payout: claim output → operator after `payout_csv` blocks. Presigned n-of-n.
pub fn payout_tx(p: &GraphParams, claim: &Transaction, operator_spk: ScriptBuf) -> Result<Transaction, String> {
    let v = claim.output[0].value.to_sat().checked_sub(p.fee_sats + ANCHOR_SATS).ok_or("claim too small")?;
    Ok(base_tx(
        OutPoint { txid: claim.compute_txid(), vout: 0 },
        Sequence::from_height(p.payout_csv),
        vec![
            TxOut { value: Amount::from_sat(v), script_pubkey: operator_spk },
            TxOut { value: Amount::from_sat(ANCHOR_SATS), script_pubkey: anchor_spk() },
        ],
    ))
}

/// Disprove: claim output → challenger, via the equivocation leaf.
pub fn disprove_tx(p: &GraphParams, claim: &Transaction, challenger_spk: ScriptBuf, s0: &[u8; 32], s1: &[u8; 32]) -> Result<Transaction, String> {
    let co = claim_output(p)?;
    let leaf = co.leaf("disprove").unwrap().clone();
    let v = claim.output[0].value.to_sat().checked_sub(p.fee_sats).ok_or("claim too small")?;
    let mut tx = base_tx(OutPoint { txid: claim.compute_txid(), vout: 0 }, Sequence::ENABLE_RBF_NO_LOCKTIME, vec![TxOut { value: Amount::from_sat(v), script_pubkey: challenger_spk }]);
    let mut w = Witness::new();
    // Script pops s1 first? Leaf: SHA256 <pk0> EQV SHA256 <pk1> EQV — top of stack is checked against pk0.
    w.push(s1);
    w.push(s0);
    w.push(leaf.as_bytes());
    w.push(control_block(&co.info, &leaf)?);
    tx.input[0].witness = w;
    Ok(tx)
}

/// Witness for an n-of-n leaf: signatures in reverse key order (last key's sig deepest is consumed last).
pub fn nofn_witness(sigs_in_key_order: &[[u8; 64]], leaf: &ScriptBuf, cb: Vec<u8>) -> Witness {
    let mut w = Witness::new();
    for s in sigs_in_key_order.iter().rev() {
        w.push(s);
    }
    w.push(leaf.as_bytes());
    w.push(cb);
    w
}
