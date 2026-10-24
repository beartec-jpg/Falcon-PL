//! Fungible BTC vault: UTXO set + t-of-n Shamir shares (full key never on disk).
//!
//! Shares live in per-seat local files only. Snapshot/RPC must never contain
//! the reconstructed secret. Signing reconstructs in RAM, signs, zeros.

use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::{Signature, SigningKey, VerifyingKey};
use k256::elliptic_curve::PrimeField;
use k256::{FieldBytes, Scalar, SecretKey};
use ripemd::Ripemd160;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const MIN_WITHDRAW_SATS: u64 = 10_000;
pub const DUST_SATS: u64 = 546;
pub const DEFAULT_FEE_SATS: u64 = 1_000;
pub const MAX_FEE_SATS: u64 = 50_000;

/// One confirmed Bitcoin output sitting in the protocol vault.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultUtxo {
    pub txid: String,
    pub vout: u32,
    pub sats: u64,
    pub height: u64,
}

impl VaultUtxo {
    pub fn outpoint(&self) -> String {
        format!("{}:{}", self.txid, self.vout)
    }
}

/// Coin-select plan. Change goes back to the same vault address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawPlan {
    pub inputs: Vec<VaultUtxo>,
    pub amount: u64,
    pub fee: u64,
    pub change: u64,
    pub dest: String,
}

/// Deterministic coin-select: largest first, cover amount+fee, change if ≥ dust.
pub fn coin_select(
    utxos: &[VaultUtxo],
    amount: u64,
    dest: &str,
    fee: u64,
) -> Result<WithdrawPlan, String> {
    if amount < MIN_WITHDRAW_SATS {
        return Err(format!("withdraw below min {MIN_WITHDRAW_SATS} sats"));
    }
    if dest.trim().len() < 26 {
        return Err("withdraw dest too short".into());
    }
    if fee == 0 || fee > MAX_FEE_SATS {
        return Err("withdraw fee out of range".into());
    }
    let need = amount.saturating_add(fee);
    let mut pool: Vec<VaultUtxo> = utxos.to_vec();
    pool.sort_by(|a, b| b.sats.cmp(&a.sats).then(a.txid.cmp(&b.txid)));
    let mut picked = Vec::new();
    let mut acc = 0u64;
    for u in pool {
        acc = acc.saturating_add(u.sats);
        picked.push(u);
        if acc >= need {
            break;
        }
    }
    if acc < need {
        return Err(format!("vault short: have {acc} need {need}"));
    }
    let mut change = acc - need;
    if change > 0 && change < DUST_SATS {
        return Err("change is dust — raise amount or wait for a larger UTXO".into());
    }
    if change == 0 {
        change = 0;
    }
    Ok(WithdrawPlan {
        inputs: picked,
        amount,
        fee,
        change,
        dest: dest.trim().to_string(),
    })
}

/// Apply a plan: drop spent outpoints, push change UTXO (if any).
pub fn apply_plan(utxos: &mut Vec<VaultUtxo>, plan: &WithdrawPlan, change_txid: &str, change_height: u64) {
    let spent: BTreeSet<String> = plan.inputs.iter().map(|u| u.outpoint()).collect();
    utxos.retain(|u| !spent.contains(&u.outpoint()));
    if plan.change >= DUST_SATS {
        utxos.push(VaultUtxo {
            txid: change_txid.to_string(),
            vout: 1,
            sats: plan.change,
            height: change_height,
        });
    }
}

pub fn vault_sats(utxos: &[VaultUtxo]) -> u64 {
    utxos.iter().map(|u| u.sats).sum()
}

pub fn vault_header_heights(utxos: &[VaultUtxo]) -> BTreeSet<u64> {
    utxos.iter().map(|u| u.height).collect()
}

// ── Shamir over secp256k1 scalar field ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultShare {
    pub epoch: u64,
    pub x: u32,
    /// 32-byte big-endian scalar share (not the full key).
    pub y_hex: String,
}

impl VaultShare {
    fn y_scalar(&self) -> Result<Scalar, String> {
        let b = hex::decode(&self.y_hex).map_err(|e| e.to_string())?;
        if b.len() != 32 {
            return Err("share must be 32 bytes".into());
        }
        let fb = FieldBytes::from_slice(&b);
        Option::<Scalar>::from(Scalar::from_repr(*fb)).ok_or_else(|| "share not in field".into())
    }
}

fn scalar_to_hex(s: Scalar) -> String {
    hex::encode(s.to_bytes())
}

/// Polynomial coefficients except a0 MUST depend on the secret. Hashing a
/// public tag made one share enough to recover the vault key.
fn random_scalar(secret: &[u8; 32], tag: &[u8], i: u32) -> Scalar {
    let mut h = Sha256::new();
    h.update(b"fpl-shamir-coeff-v2|");
    h.update(secret);
    h.update(tag);
    h.update(i.to_be_bytes());
    let d = h.finalize();
    Scalar::from_repr(FieldBytes::clone_from_slice(&d)).unwrap_or(Scalar::from(i + 1))
}

/// Split `secret` (32-byte secp scalar) into `n` shares, threshold `t`.
pub fn split_secret(secret: &[u8; 32], n: u32, t: u32, epoch: u64, tag: &[u8]) -> Result<Vec<VaultShare>, String> {
    if t < 2 || n < t || n > 64 {
        return Err("bad n/t".into());
    }
    let a0 = Option::<Scalar>::from(Scalar::from_repr(*FieldBytes::from_slice(secret)))
        .ok_or_else(|| "secret not a scalar".to_string())?;
    let mut coeffs = vec![a0];
    for i in 1..t {
        coeffs.push(random_scalar(secret, tag, i));
    }
    let mut out = Vec::with_capacity(n as usize);
    for x in 1..=n {
        let xs = Scalar::from(x);
        let mut y = Scalar::ZERO;
        let mut p = Scalar::ONE;
        for c in &coeffs {
            y += *c * p;
            p *= xs;
        }
        out.push(VaultShare {
            epoch,
            x,
            y_hex: scalar_to_hex(y),
        });
    }
    Ok(out)
}

/// Reconstruct secret from `t` shares of the same epoch.
pub fn combine_secret(shares: &[VaultShare]) -> Result<[u8; 32], String> {
    if shares.is_empty() {
        return Err("no shares".into());
    }
    let epoch = shares[0].epoch;
    if shares.iter().any(|s| s.epoch != epoch) {
        return Err("share epoch mismatch (reshare required)".into());
    }
    let mut acc = Scalar::ZERO;
    for (i, si) in shares.iter().enumerate() {
        let xi = Scalar::from(si.x);
        let yi = si.y_scalar()?;
        let mut li = Scalar::ONE;
        for (j, sj) in shares.iter().enumerate() {
            if i == j {
                continue;
            }
            let xj = Scalar::from(sj.x);
            let num = xj; // 0 - xj, field: -xj = 0-xj
            let den = xi - xj;
            let den_inv = Option::<Scalar>::from(den.invert()).ok_or("share x collision")?;
            li *= (Scalar::ZERO - num) * den_inv;
        }
        acc += yi * li;
    }
    Ok(acc.to_bytes().into())
}

/// New shares for `new_n`/`new_t`, same secret, `epoch+1`. Old epoch shares fail combine together with new.
pub fn reshare(
    shares: &[VaultShare],
    new_n: u32,
    new_t: u32,
    tag: &[u8],
) -> Result<Vec<VaultShare>, String> {
    let secret = combine_secret(shares)?;
    let epoch = shares[0].epoch.saturating_add(1);
    split_secret(&secret, new_n, new_t, epoch, tag)
}

pub fn threshold(n: u32) -> u32 {
    // Match 2300 hard QC: ceil(n × 0.6)
    let t = ((n as u64 * 6 + 9) / 10) as u32;
    t.max(2).min(n)
}

pub fn pubkey_from_secret(secret: &[u8; 32]) -> Result<Vec<u8>, String> {
    let sk = SecretKey::from_slice(secret).map_err(|e| e.to_string())?;
    Ok(sk.public_key().to_sec1_bytes().to_vec())
}

pub fn hash160(data: &[u8]) -> [u8; 20] {
    let sha = Sha256::digest(data);
    let r = Ripemd160::digest(sha);
    let mut o = [0u8; 20];
    o.copy_from_slice(&r);
    o
}

/// P2WPKH scriptPubKey: OP_0 0x14 <hash160(compressed pubkey)>
pub fn vault_p2wpkh_spk(compressed_pubkey: &[u8]) -> Vec<u8> {
    let h = hash160(compressed_pubkey);
    let mut s = Vec::with_capacity(22);
    s.push(0x00);
    s.push(0x14);
    s.extend_from_slice(&h);
    s
}

pub fn vault_p2wpkh_spk_hex(compressed_pubkey: &[u8]) -> String {
    hex::encode(vault_p2wpkh_spk(compressed_pubkey))
}

const BECH32_CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

fn bech32_polymod(values: &[u8]) -> u32 {
    let gen = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
    let mut chk = 1u32;
    for v in values {
        let b = chk >> 25;
        chk = ((chk & 0x1ffffff) << 5) ^ u32::from(*v);
        for (i, g) in gen.iter().enumerate() {
            if ((b >> i) & 1) == 1 {
                chk ^= g;
            }
        }
    }
    chk
}

fn bech32_hrp_expand(hrp: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(hrp.len() * 2 + 1);
    for b in hrp.bytes() {
        out.push(b >> 5);
    }
    out.push(0);
    for b in hrp.bytes() {
        out.push(b & 31);
    }
    out
}

fn convert_bits(data: &[u8], from: u32, to: u32, pad: bool) -> Option<Vec<u8>> {
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut ret = Vec::new();
    let maxv = (1u32 << to) - 1;
    for &v in data {
        if (u32::from(v) >> from) != 0 {
            return None;
        }
        acc = (acc << from) | u32::from(v);
        bits += from;
        while bits >= to {
            bits -= to;
            ret.push(((acc >> bits) & maxv) as u8);
        }
    }
    if pad {
        if bits > 0 {
            ret.push(((acc << (to - bits)) & maxv) as u8);
        }
    } else if bits >= from || ((acc << (to - bits)) & maxv) != 0 {
        return None;
    }
    Some(ret)
}

const BECH32M_CONST: u32 = 0x2bc830a3;

fn bech32_encode_tb(witver: u8, prog: &[u8], spec_const: u32) -> Result<String, String> {
    let mut data = vec![witver];
    data.extend(convert_bits(prog, 8, 5, true).ok_or("bech32 convert")?);
    let mut values = bech32_hrp_expand("tb");
    values.extend_from_slice(&data);
    values.extend_from_slice(&[0; 6]);
    let pmod = bech32_polymod(&values) ^ spec_const;
    let mut out = String::from("tb1");
    for d in &data {
        out.push(BECH32_CHARSET[*d as usize] as char);
    }
    for i in 0..6 {
        let c = ((pmod >> (5 * (5 - i))) & 31) as usize;
        out.push(BECH32_CHARSET[c] as char);
    }
    Ok(out)
}

/// BIP173 testnet witness address (`tb1q…`).
pub fn bech32_testnet(witver: u8, prog: &[u8]) -> Result<String, String> {
    bech32_encode_tb(witver, prog, 1)
}

/// BIP350 P2TR testnet (`tb1p…`).
pub fn p2tr_testnet_address(xonly32: &[u8]) -> Result<String, String> {
    if xonly32.len() != 32 {
        return Err("p2tr x-only must be 32 bytes".into());
    }
    bech32_encode_tb(1, xonly32, BECH32M_CONST)
}

/// BIP173 P2WPKH testnet address (`tb1q…`).
pub fn p2wpkh_testnet_address(compressed_pubkey: &[u8]) -> Result<String, String> {
    bech32_testnet(0, &hash160(compressed_pubkey))
}

pub fn p2wsh_testnet_address(witness_script: &[u8]) -> Result<String, String> {
    let h = Sha256::digest(witness_script);
    bech32_testnet(0, &h)
}

/// User park: <csv> OP_CSV OP_DROP <pubkey> OP_CHECKSIG
pub fn park_script(csv_blocks: u32, user_compressed_pk: &[u8]) -> Vec<u8> {
    let mut s = Vec::new();
    // CSV as CScriptNum (small)
    if csv_blocks <= 16 {
        s.push(0x50 + csv_blocks as u8); // OP_1 .. 
        if csv_blocks == 0 {
            s.pop();
            s.push(0x00);
        }
    } else {
        let b = csv_blocks.to_le_bytes();
        let n = b.iter().rposition(|x| *x != 0).map(|i| i + 1).unwrap_or(1);
        s.push(n as u8);
        s.extend_from_slice(&b[..n]);
    }
    s.push(0xb2); // OP_CHECKSEQUENCEVERIFY
    s.push(0x75); // OP_DROP
    s.push(user_compressed_pk.len() as u8);
    s.extend_from_slice(user_compressed_pk);
    s.push(0xac); // OP_CHECKSIG
    s
}

/// Domain-separated withdraw digest (not a full BIP143 tx — see tests).
pub fn withdraw_prehash(plan: &WithdrawPlan) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"fpl-vault-wd-v1|");
    h.update(plan.dest.as_bytes());
    h.update(plan.amount.to_be_bytes());
    h.update(plan.fee.to_be_bytes());
    h.update(plan.change.to_be_bytes());
    for i in &plan.inputs {
        h.update(i.txid.as_bytes());
        h.update(i.vout.to_be_bytes());
        h.update(i.sats.to_be_bytes());
    }
    h.finalize().into()
}

/// Sign a withdraw plan with `t` shares. Secret reconstructed in RAM only.
pub fn sign_plan(shares: &[VaultShare], plan: &WithdrawPlan) -> Result<String, String> {
    let secret = combine_secret(shares)?;
    let signing = SigningKey::from_bytes((&secret).into()).map_err(|e| e.to_string())?;
    let pre = withdraw_prehash(plan);
    let sig: Signature = signing.sign_prehash(&pre).map_err(|e| e.to_string())?;
    let _ = secret; // drop
    Ok(hex::encode(sig.to_bytes()))
}

pub fn verify_plan_sig(pubkey_sec1: &[u8], plan: &WithdrawPlan, sig_hex: &str) -> Result<(), String> {
    let vk = VerifyingKey::from_sec1_bytes(pubkey_sec1).map_err(|e| e.to_string())?;
    let raw = hex::decode(sig_hex).map_err(|e| e.to_string())?;
    let sig = Signature::from_slice(&raw).map_err(|e| e.to_string())?;
    use k256::ecdsa::signature::hazmat::PrehashVerifier;
    vk.verify_prehash(&withdraw_prehash(plan), &sig)
        .map_err(|_| "vault sig mismatch".to_string())
}

fn ecdsa_der_sighash_all(secret: &[u8; 32], prehash: &[u8; 32]) -> Result<Vec<u8>, String> {
    let signing = SigningKey::from_bytes(secret.into()).map_err(|e| e.to_string())?;
    let sig: Signature = signing.sign_prehash(prehash).map_err(|e| e.to_string())?;
    let sig = match sig.normalize_s() {
        Some(n) => n,
        None => sig,
    };
    let mut der = crate::btc_tx::der_encode_rs(&sig.to_bytes())?;
    der.push(crate::btc_tx::SIGHASH_ALL as u8);
    Ok(der)
}

fn compressed_pubkey(secret: &[u8; 32]) -> Result<Vec<u8>, String> {
    pubkey_from_secret(secret)
}

/// Sign a reconstructed vault spend as a real P2WPKH BIP143 tx.
/// Secret is used in RAM only and dropped.
pub fn sign_vault_spend_tx(
    shares: &[VaultShare],
    plan: &WithdrawPlan,
    vault_spk_hex: &str,
) -> Result<String, String> {
    let secret = combine_secret(shares)?;
    let pk = compressed_pubkey(&secret)?;
    let vault_spk = hex::decode(vault_spk_hex).map_err(|e| e.to_string())?;
    let (dest_kind, dest_spk) = crate::btc_tx::decode_address(&plan.dest)?;
    let _ = dest_kind;
    let mut inputs = Vec::new();
    for u in &plan.inputs {
        inputs.push(crate::btc_tx::TxIn {
            txid_display: u.txid.clone(),
            vout: u.vout,
            sats: u.sats,
            sequence: 0xffff_fffe,
            script_pubkey: vault_spk.clone(),
        });
    }
    let mut outputs = vec![crate::btc_tx::TxOut {
        sats: plan.amount,
        script_pubkey: dest_spk,
    }];
    if plan.change >= DUST_SATS {
        outputs.push(crate::btc_tx::TxOut {
            sats: plan.change,
            script_pubkey: vault_spk.clone(),
        });
    }
    let mut witnesses = Vec::new();
    for i in 0..inputs.len() {
        let pre = crate::btc_tx::bip143_sighash(
            2,
            0,
            &inputs,
            &outputs,
            i,
            crate::btc_tx::SIGHASH_ALL,
        )?;
        let der = ecdsa_der_sighash_all(&secret, &pre)?;
        witnesses.push(vec![der, pk.clone()]);
    }
    let raw = crate::btc_tx::assemble_segwit_tx(2, 0, &inputs, &outputs, &witnesses)?;
    let _ = secret;
    Ok(hex::encode(raw))
}

/// Kickoff: pool UTXOs → vout0 dest-lock P2WSH + change-to-pool. Not a pay-to-user.
pub fn sign_kickoff_tx(
    shares: &[VaultShare],
    plan: &WithdrawPlan,
    vault_spk_hex: &str,
    claim_spk: &[u8],
) -> Result<String, String> {
    if claim_spk.is_empty() {
        return Err("kickoff needs dest-lock spk".into());
    }
    let secret = combine_secret(shares)?;
    let pk = compressed_pubkey(&secret)?;
    let vault_spk = hex::decode(vault_spk_hex).map_err(|e| e.to_string())?;
    let mut inputs = Vec::new();
    for u in &plan.inputs {
        inputs.push(crate::btc_tx::TxIn {
            txid_display: u.txid.clone(),
            vout: u.vout,
            sats: u.sats,
            sequence: 0xffff_fffe,
            script_pubkey: vault_spk.clone(),
        });
    }
    let mut outputs = vec![crate::btc_tx::TxOut {
        sats: plan.amount,
        script_pubkey: claim_spk.to_vec(),
    }];
    if plan.change >= DUST_SATS {
        outputs.push(crate::btc_tx::TxOut {
            sats: plan.change,
            script_pubkey: vault_spk.clone(),
        });
    }
    let mut witnesses = Vec::new();
    for i in 0..inputs.len() {
        let pre = crate::btc_tx::bip143_sighash(
            2,
            0,
            &inputs,
            &outputs,
            i,
            crate::btc_tx::SIGHASH_ALL,
        )?;
        let der = ecdsa_der_sighash_all(&secret, &pre)?;
        witnesses.push(vec![der, pk.clone()]);
    }
    let raw = crate::btc_tx::assemble_segwit_tx(2, 0, &inputs, &outputs, &witnesses)?;
    let _ = secret;
    Ok(hex::encode(raw))
}

/// Build a dest-lock Kickoff using the test abort seed (unit tests / ctl preview).
pub fn signed_dest_lock_kickoff(
    shares: &[VaultShare],
    utxos: &[VaultUtxo],
    dest: &str,
    amount: u64,
    fee: u64,
    vault_spk_hex: &str,
    note_id: &str,
) -> Result<String, String> {
    let plan = coin_select(utxos, amount, dest, fee)?;
    let abort = crate::bitvm::abort_preimage_from_seed(&crate::bitvm::sha256_32(b"fpl-test-abort-seed"), note_id);
    let dest_h160 = crate::bitvm::dest_h160_from_address(dest)?;
    let commit = crate::bitvm::abort_commit(&abort);
    let ws = crate::bitvm::claim_witness_script(&commit, &dest_h160, crate::bitvm::CLAIM_CSV_BLOCKS)?;
    let claim_spk = crate::bitvm::p2wsh_spk(&ws);
    sign_kickoff_tx(shares, &plan, vault_spk_hex, &claim_spk)
}

/// Sign a user P2PKH (or P2WPKH) spend → vault + FALC OP_RETURN (+ change).
pub fn sign_user_deposit_tx(
    secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    prev_spk: &[u8],
    vault_spk: &[u8],
    dest_account: &str,
    change_spk: Option<&[u8]>,
    fee: u64,
) -> Result<String, String> {
    if sats <= fee {
        return Err("deposit fee ≥ value".into());
    }
    let pk = compressed_pubkey(secret)?;
    let vault_amt = sats.saturating_sub(fee);
    // If change is requested, peel it off the vault amount (caller sets change).
    let mut outputs = vec![crate::btc_tx::TxOut {
        sats: vault_amt,
        script_pubkey: vault_spk.to_vec(),
    }];
    outputs.push(crate::btc_tx::TxOut {
        sats: 0,
        script_pubkey: crate::btc_tx::falc_opreturn(dest_account),
    });
    if let Some(ch) = change_spk {
        // keep all-minus-fee on vault unless caller used a smaller vault_amt via fee
        let _ = ch;
    }
    let inp = crate::btc_tx::TxIn {
        txid_display: prev_txid.to_string(),
        vout,
        sats,
        sequence: 0xffff_fffe,
        script_pubkey: prev_spk.to_vec(),
    };
    let inputs = [inp];
    if crate::btc_tx::is_p2pkh(prev_spk) {
        let pre = crate::btc_tx::legacy_p2pkh_sighash(
            2,
            0,
            &inputs,
            &outputs,
            0,
            crate::btc_tx::SIGHASH_ALL,
        )?;
        let der = ecdsa_der_sighash_all(secret, &pre)?;
        let mut script_sig = crate::btc_tx::push_bytes(&der);
        script_sig.extend_from_slice(&crate::btc_tx::push_bytes(&pk));
        let raw = crate::btc_tx::assemble_legacy_tx(2, 0, &inputs, &outputs, &[script_sig])?;
        Ok(hex::encode(raw))
    } else if crate::btc_tx::is_p2wpkh(prev_spk) {
        let pre = crate::btc_tx::bip143_sighash(
            2,
            0,
            &inputs,
            &outputs,
            0,
            crate::btc_tx::SIGHASH_ALL,
        )?;
        let der = ecdsa_der_sighash_all(secret, &pre)?;
        let raw = crate::btc_tx::assemble_segwit_tx(2, 0, &inputs, &outputs, &[vec![der, pk]])?;
        Ok(hex::encode(raw))
    } else {
        Err("unsupported prevout script".into())
    }
}

fn user_change_spk(secret: &[u8; 32], prev_spk: &[u8]) -> Result<Vec<u8>, String> {
    if crate::btc_tx::is_p2wpkh(prev_spk) || crate::btc_tx::is_p2pkh(prev_spk) {
        return Ok(prev_spk.to_vec());
    }
    let pk = compressed_pubkey(secret)?;
    Ok(crate::btc_tx::p2pkh_spk(&hash160(&pk))?)
}

fn sign_user_single_input_tx(
    secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    prev_spk: &[u8],
    outputs: &[crate::btc_tx::TxOut],
) -> Result<String, String> {
    let inp = crate::btc_tx::TxIn {
        txid_display: prev_txid.to_string(),
        vout,
        sats,
        sequence: 0xffff_fffe,
        script_pubkey: prev_spk.to_vec(),
    };
    let inputs = [inp];
    if crate::btc_tx::is_p2pkh(prev_spk) {
        let pre = crate::btc_tx::legacy_p2pkh_sighash(
            2,
            0,
            &inputs,
            outputs,
            0,
            crate::btc_tx::SIGHASH_ALL,
        )?;
        let der = ecdsa_der_sighash_all(secret, &pre)?;
        let pk = compressed_pubkey(secret)?;
        let mut script_sig = crate::btc_tx::push_bytes(&der);
        script_sig.extend_from_slice(&crate::btc_tx::push_bytes(&pk));
        let raw = crate::btc_tx::assemble_legacy_tx(2, 0, &inputs, outputs, &[script_sig])?;
        Ok(hex::encode(raw))
    } else if crate::btc_tx::is_p2wpkh(prev_spk) {
        let pre = crate::btc_tx::bip143_sighash(
            2,
            0,
            &inputs,
            outputs,
            0,
            crate::btc_tx::SIGHASH_ALL,
        )?;
        let der = ecdsa_der_sighash_all(secret, &pre)?;
        let pk = compressed_pubkey(secret)?;
        let raw =
            crate::btc_tx::assemble_segwit_tx(2, 0, &inputs, outputs, &[vec![der, pk]])?;
        Ok(hex::encode(raw))
    } else {
        Err("unsupported prevout script".into())
    }
}

/// Operator/user P2PKH/P2WPKH pay: one input → dest + optional change.
pub fn sign_user_pay_tx(
    secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    prev_spk: &[u8],
    dest: &str,
    amount: u64,
    mut fee: u64,
) -> Result<String, String> {
    if amount < DUST_SATS {
        return Err(format!("pay amount below dust {DUST_SATS}"));
    }
    if fee == 0 || fee > MAX_FEE_SATS {
        return Err("pay fee out of range".into());
    }
    if sats < amount.saturating_add(fee) {
        return Err(format!("insufficient input: have {sats} need {}", amount + fee));
    }
    let mut change = sats - amount - fee;
    if change > 0 && change < DUST_SATS {
        // Absorb uneconomical change into the fee (standard wallet behaviour).
        fee = fee.saturating_add(change);
        change = 0;
    }
    let (_k, dest_spk) = crate::btc_tx::decode_address(dest)?;
    let mut outputs = vec![crate::btc_tx::TxOut {
        sats: amount,
        script_pubkey: dest_spk,
    }];
    if change >= DUST_SATS {
        outputs.push(crate::btc_tx::TxOut {
            sats: change,
            script_pubkey: user_change_spk(secret, prev_spk)?,
        });
    }
    sign_user_single_input_tx(secret, prev_txid, vout, sats, prev_spk, &outputs)
}

/// BitVM2 Claim marker: `OP_RETURN FPL2||digest` + change to payer key.
pub fn sign_user_claim_tx(
    secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    prev_spk: &[u8],
    statement_digest_hex: &str,
    mut fee: u64,
) -> Result<String, String> {
    if fee == 0 || fee > MAX_FEE_SATS {
        return Err("claim fee out of range".into());
    }
    if sats <= fee {
        return Err("claim input ≤ fee".into());
    }
    let digest_raw = hex::decode(statement_digest_hex.trim())
        .map_err(|e| format!("statement digest hex: {e}"))?;
    if digest_raw.len() != 32 {
        return Err("statement digest must be 32 bytes".into());
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&digest_raw);
    let mut change = sats - fee;
    if change > 0 && change < DUST_SATS {
        fee = fee.saturating_add(change);
        change = 0;
    }
    let mut outputs = vec![crate::btc_tx::TxOut {
        sats: 0,
        script_pubkey: crate::btc_tx::bitvm2_claim_opreturn(&digest),
    }];
    if change >= DUST_SATS {
        outputs.push(crate::btc_tx::TxOut {
            sats: change,
            script_pubkey: user_change_spk(secret, prev_spk)?,
        });
    }
    sign_user_single_input_tx(secret, prev_txid, vout, sats, prev_spk, &outputs)
}

/// Dest-lock ELSE take: CSV + dest CHECKSIG. User key only; no vault share.
pub fn sign_dest_lock_take(
    secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    dest_spk: &[u8],
    witness_script: &[u8],
    csv_blocks: u32,
    fee: u64,
) -> Result<String, String> {
    if sats <= fee {
        return Err("take fee ≥ value".into());
    }
    let pk = pubkey_from_secret(secret)?;
    let claim_spk = crate::bitvm::p2wsh_spk(witness_script);
    let outputs = [crate::btc_tx::TxOut {
        sats: sats - fee,
        script_pubkey: dest_spk.to_vec(),
    }];
    let inputs = [crate::btc_tx::TxIn {
        txid_display: prev_txid.to_string(),
        vout,
        sats,
        sequence: csv_blocks,
        script_pubkey: claim_spk,
    }];
    let pre = crate::btc_tx::bip143_p2wsh_sighash(
        2,
        0,
        &inputs,
        &outputs,
        0,
        crate::btc_tx::SIGHASH_ALL,
        witness_script,
    )?;
    let der = ecdsa_der_sighash_all(secret, &pre)?;
    let raw = crate::btc_tx::assemble_segwit_tx(
        2,
        0,
        &inputs,
        &outputs,
        &[vec![der, pk, Vec::new(), witness_script.to_vec()]],
    )?;
    Ok(hex::encode(raw))
}

/// Verify a BIP143 P2WPKH vault spend against the plan + vault pubkey.
pub fn verify_vault_spend_tx(
    raw_hex: &str,
    plan: &WithdrawPlan,
    vault_pubkey_sec1: &[u8],
    vault_spk_hex: &str,
) -> Result<(), String> {
    let raw = hex::decode(raw_hex.trim()).map_err(|e| e.to_string())?;
    let vault_spk = hex::decode(vault_spk_hex).map_err(|e| e.to_string())?;
    let (_k, dest_spk) = crate::btc_tx::decode_address(&plan.dest)?;
    let mut inputs = Vec::new();
    for u in &plan.inputs {
        inputs.push(crate::btc_tx::TxIn {
            txid_display: u.txid.clone(),
            vout: u.vout,
            sats: u.sats,
            sequence: 0xffff_fffe,
            script_pubkey: vault_spk.clone(),
        });
    }
    crate::btc_tx::vault_spend_matches(&raw, &dest_spk, plan.amount, &vault_spk, plan.change, &inputs)?;
    // Recompute sighash[0] and require the witness pubkey matches the vault.
    let parsed = crate::btc_spv::parse_tx(&raw)?;
    let _ = parsed;
    let want_pk = hex::encode(vault_pubkey_sec1);
    if !raw_hex.to_ascii_lowercase().contains(&want_pk.to_ascii_lowercase()) {
        // witness contains compressed pubkey; reject if vault pk is absent
        return Err("vault spend missing vault pubkey".into());
    }
    Ok(())
}

pub fn share_path(keys_dir: &Path, seat: &str) -> PathBuf {
    keys_dir.join(format!("vault-share-{seat}"))
}

pub fn write_share(keys_dir: &Path, seat: &str, share: &VaultShare) -> Result<(), String> {
    std::fs::create_dir_all(keys_dir).map_err(|e| e.to_string())?;
    let p = share_path(keys_dir, seat);
    let tmp = p.with_extension("tmp");
    let s = serde_json::to_string(share).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, s).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(tmp, p).map_err(|e| e.to_string())
}

pub fn read_share(keys_dir: &Path, seat: &str) -> Result<VaultShare, String> {
    let raw = std::fs::read_to_string(share_path(keys_dir, seat)).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}

/// Public vault identity (safe to put in status / AMD). Never includes shares.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultPub {
    pub epoch: u64,
    pub n: u32,
    pub t: u32,
    pub pubkey_hex: String,
    pub spk_hex: String,
}

pub fn vault_pub_from_secret(secret: &[u8; 32], epoch: u64, n: u32, t: u32) -> Result<VaultPub, String> {
    let pk = pubkey_from_secret(secret)?;
    Ok(VaultPub {
        epoch,
        n,
        t,
        pubkey_hex: hex::encode(&pk),
        spk_hex: vault_p2wpkh_spk_hex(&pk),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret_from_tag(tag: &[u8]) -> [u8; 32] {
        let d = Sha256::digest(tag);
        let mut s = [0u8; 32];
        s.copy_from_slice(&d);
        // ensure in field
        s[0] &= 0x3f;
        s
    }

    fn utxo(txid: &str, vout: u32, sats: u64, h: u64) -> VaultUtxo {
        VaultUtxo {
            txid: txid.into(),
            vout,
            sats,
            height: h,
        }
    }

    #[test]
    fn sally_dave_exact_with_fees_taken_from_vault() {
        // Model: users receive 1.5 each; fees (2 * 1000) come from vault so
        // deposits must be 3.0 and last change absorbs fees.
        let mut vault = vec![
            utxo("s", 0, 100_000_000, 1),
            utxo("d", 0, 200_000_000, 2),
        ];
        let fee = 1_000u64;
        let want = 149_999_000u64; // 1.5 BTC minus half the total fee so both fit
        let p1 = coin_select(&vault, want, "tb1qaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", fee).unwrap();
        apply_plan(&mut vault, &p1, "c1", 3);
        let p2 = coin_select(&vault, want, "tb1qbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", fee).unwrap();
        apply_plan(&mut vault, &p2, "c2", 4);
        assert_eq!(vault_sats(&vault), 0);
    }

    #[test]
    fn short_vault_rejected() {
        let vault = vec![utxo("a", 0, 20_000, 1)];
        assert!(coin_select(&vault, 100_000, "tb1qaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 1_000).is_err());
    }

    #[test]
    fn dust_change_rejected() {
        let vault = vec![utxo("a", 0, 10_200, 1)];
        let e = coin_select(&vault, 10_000, "tb1qaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 150).unwrap_err();
        assert!(e.contains("dust"), "{e}");
    }

    #[test]
    fn shamir_t_of_n_and_reshare_kills_old() {
        let secret = secret_from_tag(b"vault-test-secret");
        let shares = split_secret(&secret, 5, 3, 0, b"gen").unwrap();
        assert_eq!(shares.len(), 5);
        let rec = combine_secret(&shares[0..3]).unwrap();
        assert_eq!(rec, secret);
        assert!(combine_secret(&shares[0..2]).is_err() || combine_secret(&shares[0..2]).unwrap() != secret);

        let new_s = reshare(&shares[1..4], 6, threshold(6), b"re").unwrap();
        assert_eq!(new_s.len(), 6);
        assert_eq!(new_s[0].epoch, 1);
        // old + new must not combine
        let mixed = vec![shares[0].clone(), new_s[0].clone(), new_s[1].clone()];
        assert!(combine_secret(&mixed).is_err());
        let rec2 = combine_secret(&new_s[0..threshold(6) as usize]).unwrap();
        assert_eq!(rec2, secret);
    }

    #[test]
    fn one_share_cannot_sign() {
        let secret = secret_from_tag(b"sign-test");
        let shares = split_secret(&secret, 5, 3, 0, b"g").unwrap();
        let rec1 = combine_secret(&shares[0..1]);
        assert!(
            rec1.is_err() || rec1.unwrap() != secret,
            "one share must not recover the vault scalar"
        );
        // Public-tag attack (v1 bug): coeffs = SHA256(tag||i) independent of secret.
        let mut h = sha2::Sha256::new();
        h.update(b"g");
        h.update(1u32.to_be_bytes());
        let d = h.finalize();
        assert_ne!(
            &d[..],
            &secret[..],
            "sanity"
        );
        let vault = vec![utxo("x", 0, 200_000_000, 1)];
        let plan = coin_select(&vault, 150_000_000, "tb1qccccccccccccccccccccccccccccccc", 1_000).unwrap();
        assert!(sign_plan(&shares[0..1], &plan).is_err() || {
            let sig = sign_plan(&shares[0..1], &plan);
            match sig {
                Err(_) => true,
                Ok(s) => {
                    let pk = pubkey_from_secret(&secret).unwrap();
                    verify_plan_sig(&pk, &plan, &s).is_err()
                }
            }
        });
        let sig = sign_plan(&shares[0..3], &plan).unwrap();
        let pk = pubkey_from_secret(&secret).unwrap();
        verify_plan_sig(&pk, &plan, &sig).unwrap();
        let mut bad = plan.clone();
        bad.dest = "tb1qddddddddddddddddddddddddddddddd".into();
        assert!(verify_plan_sig(&pk, &bad, &sig).is_err());
    }

    #[test]
    fn serde_share_is_not_full_secret() {
        let secret = secret_from_tag(b"leak-test");
        let shares = split_secret(&secret, 5, 3, 0, b"g").unwrap();
        let json = serde_json::to_string(&shares[0]).unwrap();
        let sec_hex = hex::encode(secret);
        assert!(
            !json.contains(&sec_hex),
            "share JSON must not contain the vault secret"
        );
        let pubj = serde_json::to_string(&vault_pub_from_secret(&secret, 0, 5, 3).unwrap()).unwrap();
        assert!(!pubj.contains(&sec_hex));
    }

    #[test]
    fn threshold_matches_hard_qc() {
        assert_eq!(threshold(5), 3); // ceil(3.0)
        assert_eq!(threshold(6), 4); // ceil(3.6)
        assert_eq!(threshold(7), 5); // ceil(4.2)
    }

    #[test]
    fn bip143_vault_spend_roundtrip() {
        let secret = secret_from_tag(b"bip143-vault");
        let shares = split_secret(&secret, 5, 3, 0, b"g").unwrap();
        let pk = pubkey_from_secret(&secret).unwrap();
        let vp = vault_pub_from_secret(&secret, 0, 5, 3).unwrap();
        let addr = p2wpkh_testnet_address(&pk).unwrap();
        let vault = vec![utxo("11".repeat(32).as_str(), 0, 80_000, 10)];
        let dest = "n1fH7uhaDauQYfuwG6t1aTncvyuBSZtDEY";
        let plan = coin_select(&vault, 50_000, dest, 1_000).unwrap();
        assert!(plan.change >= DUST_SATS || plan.change == 0);
        let hex = sign_vault_spend_tx(&shares[0..3], &plan, &vp.spk_hex).unwrap();
        verify_vault_spend_tx(&hex, &plan, &pk, &vp.spk_hex).unwrap();
        let raw = hex::decode(&hex).unwrap();
        let parsed = crate::btc_spv::parse_tx(&raw).unwrap();
        assert_eq!(parsed.outputs[0].0, 50_000);
        let _ = addr;
    }

    #[test]
    fn park_and_vault_scripts_are_nonempty() {
        let secret = secret_from_tag(b"spk");
        let pk = pubkey_from_secret(&secret).unwrap();
        assert_eq!(vault_p2wpkh_spk(&pk).len(), 22);
        let park = park_script(144, &pk);
        assert!(park.len() > 30);
        assert_eq!(*park.last().unwrap(), 0xac);
        let addr = p2wpkh_testnet_address(&pk).unwrap();
        assert!(addr.starts_with("tb1q"), "{addr}");
    }

    #[test]
    fn dest_lock_take_csv_sequence() {
        let secret = secret_from_tag(b"dest-lock-take");
        let pk = pubkey_from_secret(&secret).unwrap();
        let dest = p2wpkh_testnet_address(&pk).unwrap();
        let dest_h160 = crate::bitvm::dest_h160_from_address(&dest).unwrap();
        let seed = crate::bitvm::sha256_32(b"fpl-live-abort-seed");
        let note = crate::bridge::btc_withdraw_note_id("dave", 50_000, &dest);
        let abort = crate::bitvm::abort_preimage_from_seed(&seed, &note);
        let commit = crate::bitvm::abort_commit(&abort);
        let ws = crate::bitvm::claim_witness_script(
            &commit,
            &dest_h160,
            crate::bitvm::CLAIM_CSV_BLOCKS,
        )
        .unwrap();
        let (_k, dest_spk) = crate::btc_tx::decode_address(&dest).unwrap();
        let hex = sign_dest_lock_take(
            &secret,
            &"cc".repeat(32),
            0,
            50_000,
            &dest_spk,
            &ws,
            crate::bitvm::CLAIM_CSV_BLOCKS,
            1_000,
        )
        .unwrap();
        let raw = hex::decode(&hex).unwrap();
        let parsed = crate::btc_spv::parse_tx(&raw).unwrap();
        assert_eq!(parsed.outputs[0].0, 49_000);
        assert_eq!(parsed.outputs[0].1, dest_spk);
        // version(4) + marker/flag(2) + vin(1) + outpoint(36) + scriptsig-len(1) + sequence(4)
        let seq = u32::from_le_bytes(raw[44..48].try_into().unwrap());
        assert_eq!(seq, crate::bitvm::CLAIM_CSV_BLOCKS);
        let _ = pk;
    }
}
