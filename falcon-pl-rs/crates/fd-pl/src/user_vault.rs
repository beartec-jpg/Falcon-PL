//! Non-custodial 1:1 BTC vault: only the user's key can spend.
//!
//! ```text
//! OP_IF
//!   OP_SHA256 <take_commit> OP_EQUALVERIFY
//!   OP_DUP OP_HASH160 <user_h160> OP_EQUALVERIFY OP_CHECKSIG
//! OP_ELSE
//!   <csv> OP_CSV OP_DROP
//!   OP_DUP OP_HASH160 <user_h160> OP_EQUALVERIFY OP_CHECKSIG
//! OP_ENDIF
//! ```
//!
//! - Protocol / watch / n-of-n cannot satisfy CHECKSIG.
//! - Take path needs the preimage FPL publishes on burn (user still signs).
//! - Refund path is CSV + user sig (mint never happened, or user aborts).
//! - Anyone can audit the P2WSH and the FALC memo.

use crate::bitvm::{p2wsh_spk, push_csv, sha256_32};
use crate::btc_tx::{self, TxIn, TxOut, SIGHASH_ALL};
use crate::vault::{self, DUST_SATS, MAX_FEE_SATS};
use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::{Signature, SigningKey};
use sha2::{Digest, Sha256};

pub const USER_VAULT_CSV: u32 = 6;
pub const FALC_USER_VAULT_LEN: usize = 76; // FALC + dest20 + h160 + nonce32
pub const TAKE_DOMAIN: &[u8] = b"fpl-user-take-v1|";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserVault {
    pub take_commit: [u8; 32],
    pub user_h160: [u8; 20],
    pub csv_blocks: u32,
    pub nonce: [u8; 32],
}

pub fn take_preimage(seed: &[u8; 32], fpl_dest20: &[u8; 20], user_h160: &[u8; 20], nonce: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(TAKE_DOMAIN);
    h.update(seed);
    h.update(fpl_dest20);
    h.update(user_h160);
    h.update(nonce);
    h.finalize().into()
}

pub fn take_commit(preimage: &[u8; 32]) -> [u8; 32] {
    sha256_32(preimage)
}

pub fn witness_script(v: &UserVault) -> Result<Vec<u8>, String> {
    let mut s = Vec::with_capacity(90);
    s.push(0x63); // IF
    s.push(0xa8); // SHA256
    s.push(0x20);
    s.extend_from_slice(&v.take_commit);
    s.push(0x88); // EQUALVERIFY
    s.push(0x76); // DUP
    s.push(0xa9); // HASH160
    s.push(0x14);
    s.extend_from_slice(&v.user_h160);
    s.push(0x88); // EQUALVERIFY
    s.push(0xac); // CHECKSIG
    s.push(0x67); // ELSE
    s.extend_from_slice(&push_csv(v.csv_blocks)?);
    s.push(0xb2); // CSV
    s.push(0x75); // DROP
    s.push(0x76); // DUP
    s.push(0xa9);
    s.push(0x14);
    s.extend_from_slice(&v.user_h160);
    s.push(0x88);
    s.push(0xac);
    s.push(0x68); // ENDIF
    Ok(s)
}

pub fn spk(v: &UserVault) -> Result<Vec<u8>, String> {
    Ok(p2wsh_spk(&witness_script(v)?))
}

pub fn testnet_address(v: &UserVault) -> Result<String, String> {
    vault::p2wsh_testnet_address(&witness_script(v)?)
}

/// FALC‖dest20‖h160‖nonce (76 bytes).
pub fn falc_payload(fpl_account: &str, user_h160: &[u8; 20], nonce: &[u8; 32]) -> Vec<u8> {
    let dest = crate::btc_spv::dest_id20(fpl_account);
    let mut p = Vec::with_capacity(FALC_USER_VAULT_LEN);
    p.extend_from_slice(crate::btc_spv::FALC_MAGIC);
    p.extend_from_slice(&dest);
    p.extend_from_slice(user_h160);
    p.extend_from_slice(nonce);
    p
}

pub struct UserVaultDeposit {
    pub value_sats: u64,
    pub vout: u32,
    pub fpl_dest20: [u8; 20],
    pub user_h160: [u8; 20],
    pub nonce: [u8; 32],
}

pub fn extract_deposit(tx: &crate::btc_spv::ParsedTx, seed: &[u8; 32], csv: u32) -> Result<UserVaultDeposit, String> {
    let mut memo: Option<([u8; 20], [u8; 20], [u8; 32])> = None;
    for (_v, spk) in &tx.outputs {
        if spk.first() == Some(&0x6a) {
            let payload = crate::btc_spv::op_return_payload(spk)?;
            if payload.len() == FALC_USER_VAULT_LEN && payload.starts_with(crate::btc_spv::FALC_MAGIC) {
                memo = Some(parse_falc_payload(&payload)?);
            }
        }
    }
    let (dest, h160, nonce) = memo.ok_or("missing user-vault FALC memo")?;
    let pre = take_preimage(seed, &dest, &h160, &nonce);
    let vault = UserVault {
        take_commit: take_commit(&pre),
        user_h160: h160,
        csv_blocks: csv,
        nonce,
    };
    let want = spk(&vault)?;
    let mut paid = 0u64;
    let mut vout = 0u32;
    let mut found = false;
    for (i, (value, spk)) in tx.outputs.iter().enumerate() {
        if spk.as_slice() == want.as_slice() && *value > 0 {
            paid = *value;
            vout = i as u32;
            found = true;
            break;
        }
    }
    if !found {
        return Err("no output to user-vault P2WSH (script mismatch — not protocol watch)".into());
    }
    Ok(UserVaultDeposit {
        value_sats: paid,
        vout,
        fpl_dest20: dest,
        user_h160: h160,
        nonce,
    })
}

pub fn parse_falc_payload(p: &[u8]) -> Result<([u8; 20], [u8; 20], [u8; 32]), String> {
    if p.len() != FALC_USER_VAULT_LEN || !p.starts_with(crate::btc_spv::FALC_MAGIC) {
        return Err("not a user-vault FALC memo".into());
    }
    let mut dest = [0u8; 20];
    dest.copy_from_slice(&p[4..24]);
    let mut h160 = [0u8; 20];
    h160.copy_from_slice(&p[24..44]);
    let mut nonce = [0u8; 32];
    nonce.copy_from_slice(&p[44..76]);
    Ok((dest, h160, nonce))
}

pub fn build_for_deposit(
    seed: &[u8; 32],
    fpl_account: &str,
    user_h160: &[u8; 20],
    nonce: &[u8; 32],
    csv_blocks: u32,
) -> Result<UserVault, String> {
    let dest = crate::btc_spv::dest_id20(fpl_account);
    let pre = take_preimage(seed, &dest, user_h160, nonce);
    Ok(UserVault {
        take_commit: take_commit(&pre),
        user_h160: *user_h160,
        csv_blocks,
        nonce: *nonce,
    })
}

/// Script contains user h160, not a protocol/watch key. No OP_TRUE take.
pub fn audit_non_custodial(script: &[u8], user_h160: &[u8; 20], watch_xonly: &[u8; 32]) -> Result<(), String> {
    if !script.windows(20).any(|w| w == user_h160.as_slice()) {
        return Err("script missing user HASH160".into());
    }
    if script.windows(32).any(|w| w == watch_xonly.as_slice()) {
        return Err("script commits watch key".into());
    }
    let checksigs = script.iter().filter(|&&b| b == 0xac).count();
    if checksigs < 2 {
        return Err("both take and refund must CHECKSIG".into());
    }
    if !script.contains(&0xa8) {
        return Err("take path missing SHA256(preimage)".into());
    }
    if !script.contains(&0xb2) {
        return Err("refund path missing CSV".into());
    }
    Ok(())
}

fn ecdsa_der_all(secret: &[u8; 32], prehash: &[u8; 32]) -> Result<Vec<u8>, String> {
    let signing = SigningKey::from_bytes(secret.into()).map_err(|e| e.to_string())?;
    let sig: Signature = signing.sign_prehash(prehash).map_err(|e| e.to_string())?;
    let sig = match sig.normalize_s() {
        Some(n) => n,
        None => sig,
    };
    let mut der = btc_tx::der_encode_rs(&sig.to_bytes())?;
    der.push(SIGHASH_ALL as u8);
    Ok(der)
}

fn sign_p2wsh(
    secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    sequence: u32,
    dest_spk: &[u8],
    fee: u64,
    witness_script: &[u8],
    extra: Vec<Vec<u8>>,
) -> Result<String, String> {
    if sats <= fee || sats - fee < DUST_SATS {
        return Err("user-vault fee".into());
    }
    if fee == 0 || fee > MAX_FEE_SATS {
        return Err("fee range".into());
    }
    let pk = vault::pubkey_from_secret(secret)?;
    let h160 = vault::hash160(&pk);
    let mut script_h160 = [0u8; 20];
    // last HASH160 push in script is user
    let spk = p2wsh_spk(witness_script);
    let outputs = [TxOut {
        sats: sats - fee,
        script_pubkey: dest_spk.to_vec(),
    }];
    let inputs = [TxIn {
        txid_display: prev_txid.to_string(),
        vout,
        sats,
        sequence,
        script_pubkey: spk,
    }];
    let pre = btc_tx::bip143_p2wsh_sighash(
        2,
        0,
        &inputs,
        &outputs,
        0,
        SIGHASH_ALL,
        witness_script,
    )?;
    let der = ecdsa_der_all(secret, &pre)?;
    let mut wit = vec![der, pk];
    wit.extend(extra);
    wit.push(witness_script.to_vec());
    let raw = btc_tx::assemble_segwit_tx(2, 0, &inputs, &outputs, &[wit])?;
    let _ = h160;
    let _ = script_h160;
    Ok(hex::encode(raw))
}

/// Deposit: user P2WPKH/P2PKH → user-vault P2WSH + 76-byte FALC memo.
pub fn sign_deposit(
    secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    prev_spk: &[u8],
    fpl_account: &str,
    nonce: &[u8; 32],
    seed: &[u8; 32],
    fee: u64,
) -> Result<(String, UserVault), String> {
    let pk = vault::pubkey_from_secret(secret)?;
    let h160 = vault::hash160(&pk);
    let v = build_for_deposit(seed, fpl_account, &h160, nonce, USER_VAULT_CSV)?;
    let vault_spk = spk(&v)?;
    if sats <= fee {
        return Err("deposit fee".into());
    }
    let vault_amt = sats - fee;
    if vault_amt < DUST_SATS {
        return Err("deposit dust".into());
    }
    let payload = falc_payload(fpl_account, &h160, nonce);
    let outputs = [
        TxOut {
            sats: vault_amt,
            script_pubkey: vault_spk,
        },
        TxOut {
            sats: 0,
            script_pubkey: btc_tx::op_return_script(&payload),
        },
    ];
    let inputs = [TxIn {
        txid_display: prev_txid.to_string(),
        vout,
        sats,
        sequence: 0xffff_fffe,
        script_pubkey: prev_spk.to_vec(),
    }];
    let raw = if btc_tx::is_p2wpkh(prev_spk) {
        let pre = btc_tx::bip143_sighash(2, 0, &inputs, &outputs, 0, SIGHASH_ALL)?;
        let der = ecdsa_der_all(secret, &pre)?;
        btc_tx::assemble_segwit_tx(2, 0, &inputs, &outputs, &[vec![der, pk]])?
    } else if btc_tx::is_p2pkh(prev_spk) {
        let pre = btc_tx::legacy_p2pkh_sighash(2, 0, &inputs, &outputs, 0, SIGHASH_ALL)?;
        let der = ecdsa_der_all(secret, &pre)?;
        let mut ss = btc_tx::push_bytes(&der);
        ss.extend_from_slice(&btc_tx::push_bytes(&pk));
        btc_tx::assemble_legacy_tx(2, 0, &inputs, &outputs, &[ss])?
    } else {
        return Err("deposit prevout must be p2wpkh/p2pkh".into());
    };
    Ok((hex::encode(raw), v))
}

/// Take: IF branch. Witness: sig, pk, preimage, 0x01, script.
pub fn sign_take(
    secret: &[u8; 32],
    v: &UserVault,
    preimage: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    dest_spk: &[u8],
    fee: u64,
) -> Result<String, String> {
    if take_commit(preimage) != v.take_commit {
        return Err("preimage does not match take_commit".into());
    }
    let pk = vault::pubkey_from_secret(secret)?;
    if vault::hash160(&pk) != v.user_h160 {
        return Err("secret is not the vault user key".into());
    }
    let ws = witness_script(v)?;
    sign_p2wsh(
        secret,
        prev_txid,
        vout,
        sats,
        0xffff_fffe,
        dest_spk,
        fee,
        &ws,
        vec![preimage.to_vec(), vec![0x01]],
    )
}

/// Refund: ELSE branch after CSV. Witness: sig, pk, empty, script (OP_0).
pub fn sign_refund(
    secret: &[u8; 32],
    v: &UserVault,
    prev_txid: &str,
    vout: u32,
    sats: u64,
    dest_spk: &[u8],
    fee: u64,
) -> Result<String, String> {
    let pk = vault::pubkey_from_secret(secret)?;
    if vault::hash160(&pk) != v.user_h160 {
        return Err("secret is not the vault user key".into());
    }
    let ws = witness_script(v)?;
    sign_p2wsh(
        secret,
        prev_txid,
        vout,
        sats,
        v.csv_blocks,
        dest_spk,
        fee,
        &ws,
        vec![Vec::new()],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitvm::watch_xonly;
    use crate::btc_spv::{extract_witnesses, parse_tx};

    fn seed() -> [u8; 32] {
        sha256_32(b"test-take-seed")
    }

    fn user_secret() -> [u8; 32] {
        sha256_32(b"user-btc-key")
    }

    fn vault() -> (UserVault, [u8; 32]) {
        let pk = vault::pubkey_from_secret(&user_secret()).unwrap();
        let h160 = vault::hash160(&pk);
        let nonce = sha256_32(b"nonce-1");
        let v = build_for_deposit(&seed(), "sally", &h160, &nonce, 6).unwrap();
        let dest = crate::btc_spv::dest_id20("sally");
        let pre = take_preimage(&seed(), &dest, &h160, &nonce);
        (v, pre)
    }

    #[test]
    fn script_is_user_only_no_watch_no_true() {
        let (v, _) = vault();
        let s = witness_script(&v).unwrap();
        audit_non_custodial(&s, &v.user_h160, &watch_xonly()).unwrap();
        assert!(s.contains(&0xac));
        assert!(!s.windows(32).any(|w| w == watch_xonly().as_slice()));
    }

    #[test]
    fn take_requires_matching_preimage_and_user_key() {
        let (v, pre) = vault();
        let dest_spk = crate::btc_tx::p2wpkh_spk(&v.user_h160).unwrap();
        sign_take(
            &user_secret(),
            &v,
            &pre,
            &"aa".repeat(32),
            0,
            50_000,
            &dest_spk,
            1_000,
        )
        .unwrap();
        let bad = sha256_32(b"nope");
        assert!(sign_take(
            &user_secret(),
            &v,
            &bad,
            &"aa".repeat(32),
            0,
            50_000,
            &dest_spk,
            1_000
        )
        .is_err());
        let proto = sha256_32(b"protocol-key");
        assert!(sign_take(&proto, &v, &pre, &"aa".repeat(32), 0, 50_000, &dest_spk, 1_000).is_err());
    }

    #[test]
    fn take_tx_is_script_path_with_preimage_not_op_true() {
        let (v, pre) = vault();
        let dest_spk = crate::btc_tx::p2wpkh_spk(&v.user_h160).unwrap();
        let raw = sign_take(
            &user_secret(),
            &v,
            &pre,
            &"bb".repeat(32),
            0,
            20_000,
            &dest_spk,
            1_000,
        )
        .unwrap();
        let bytes = hex::decode(&raw).unwrap();
        let parsed = parse_tx(&bytes).unwrap();
        assert_eq!(parsed.outputs[0].0, 19_000);
        assert_eq!(parsed.outputs[0].1, dest_spk);
        let wits = extract_witnesses(&bytes).unwrap();
        // sig, pk, preimage, 0x01, script
        assert_eq!(wits[0].len(), 5);
        assert_eq!(wits[0][2], pre);
        assert_eq!(wits[0][3], vec![0x01]);
        assert_ne!(wits[0][3], vec![0x51]);
    }

    #[test]
    fn refund_uses_csv_and_user_key_no_preimage() {
        let (v, _) = vault();
        let dest_spk = crate::btc_tx::p2wpkh_spk(&v.user_h160).unwrap();
        let raw = sign_refund(
            &user_secret(),
            &v,
            &"cc".repeat(32),
            0,
            20_000,
            &dest_spk,
            1_000,
        )
        .unwrap();
        let wits = extract_witnesses(&hex::decode(&raw).unwrap()).unwrap();
        assert_eq!(wits[0].len(), 4); // sig, pk, OP_0, script
        assert!(wits[0][2].is_empty());
    }

    #[test]
    fn falc_payload_roundtrip() {
        let (v, _) = vault();
        let p = falc_payload("sally", &v.user_h160, &v.nonce);
        assert_eq!(p.len(), FALC_USER_VAULT_LEN);
        let (d, h, n) = parse_falc_payload(&p).unwrap();
        assert_eq!(d, crate::btc_spv::dest_id20("sally"));
        assert_eq!(h, v.user_h160);
        assert_eq!(n, v.nonce);
    }
}
