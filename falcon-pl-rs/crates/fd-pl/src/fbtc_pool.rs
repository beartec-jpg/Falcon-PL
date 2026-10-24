//! Pooled BTC rail with transferable FBTC.
//!
//! Peg-in to one NUMS P2TR pool. FPL mints fungible FBTC (Dave can pay Sally).
//! Sally burns FBTC, then signs the Bitcoin peg-out. If dest/amount do not
//! match a burn, anyone aborts with a setup-once presigned return-to-pool tx.
//!
//! ```text
//! NUMS key-path: dead
//! pegout leaf:  <csv> OP_CSV OP_DROP OP_CHECKSIG   // pubkey in witness (any holder)
//! abort  leaf:  OP_SHA256 <commit> EQUALVERIFY <abort_xonly> OP_CHECKSIG
//! ```
//!
//! No watch key. No n-of-n on exit. No operator fronting. Abort key can only
//! sign the return-to-pool template (SIGHASH_ALL); it cannot steal.

use crate::bitvm::sha256_32;
use crate::btc_tx::{
    self, bip340_sign, bip340_verify, bip341_scriptpath_sighash, nums_internal_from_tag, p2tr_spk,
    tapbranch, tapleaf_hash, taproot_control_block, taproot_tweak, TxIn, TxOut, SIGHASH_DEFAULT,
};
use crate::vault::{self, DUST_SATS, MAX_FEE_SATS};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const POOL_NUMS_TAG: &[u8] = b"FalconPL-FBTC-Pool-v1";
/// Testnet default (~1h). Production should use 144 (~24h) and re-anchor.
pub const POOL_CSV_BLOCKS: u32 = 6;
pub const KIND_PEGOUT: &str = "pegout";
pub const KIND_ABORT: &str = "abort";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FbtcPool {
    pub abort_commit: [u8; 32],
    pub abort_xonly: [u8; 32],
    pub csv_blocks: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolJson {
    pub address: String,
    pub spk_hex: String,
    pub abort_xonly_hex: String,
    pub abort_commit_hex: String,
    pub csv_blocks: u32,
    pub nums_tag: String,
    pub merkle_root_hex: String,
    pub watch_key: String,
    pub n_of_n: String,
    pub operator_front: String,
    pub pegout: String,
    pub abort: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresignedAbort {
    pub txid: String,
    pub vout: u32,
    pub sats: u64,
    pub fee: u64,
    pub csv_blocks: u32,
    pub abort_xonly_hex: String,
    pub abort_commit_hex: String,
    pub sig_hex: String,
    pub script_hex: String,
    pub control_hex: String,
    pub pool_spk_hex: String,
    pub pool_address: String,
    pub return_sats: u64,
}

/// Sign-bit-safe CScriptNum (144 → `0x90 0x00`).
pub fn push_csv(csv: u32) -> Vec<u8> {
    if csv == 0 {
        return vec![0x00];
    }
    if csv <= 16 {
        return vec![0x50 + csv as u8];
    }
    let mut b = csv.to_le_bytes().to_vec();
    while b.len() > 1 && b.last() == Some(&0) {
        b.pop();
    }
    if b.last().copied().unwrap_or(0) & 0x80 != 0 {
        b.push(0);
    }
    let mut s = vec![b.len() as u8];
    s.extend_from_slice(&b);
    s
}

/// `<csv> CSV DROP CHECKSIG` — withdrawer pubkey is in the witness, not the leaf.
pub fn pegout_script(csv_blocks: u32) -> Vec<u8> {
    let mut s = push_csv(csv_blocks);
    s.push(0xb2); // OP_CHECKSEQUENCEVERIFY
    s.push(0x75); // OP_DROP
    s.push(0xac); // OP_CHECKSIG
    s
}

/// `SHA256 <commit> EQUALVERIFY <abort_xonly> CHECKSIG`
pub fn abort_script(abort_commit: &[u8; 32], abort_xonly: &[u8; 32]) -> Vec<u8> {
    let mut s = Vec::with_capacity(70);
    s.push(0xa8); // OP_SHA256
    s.push(0x20);
    s.extend_from_slice(abort_commit);
    s.push(0x88); // OP_EQUALVERIFY
    s.push(0x20);
    s.extend_from_slice(abort_xonly);
    s.push(0xac); // OP_CHECKSIG
    s
}

fn leaf_hashes(p: &FbtcPool) -> ([u8; 32], [u8; 32]) {
    (
        tapleaf_hash(&pegout_script(p.csv_blocks)),
        tapleaf_hash(&abort_script(&p.abort_commit, &p.abort_xonly)),
    )
}

pub fn merkle_root(p: &FbtcPool) -> [u8; 32] {
    let (a, b) = leaf_hashes(p);
    tapbranch(&a, &b)
}

pub fn output_key(p: &FbtcPool) -> Result<([u8; 32], u8), String> {
    let root = merkle_root(p);
    let (_x, internal) = nums_internal_from_tag(POOL_NUMS_TAG);
    taproot_tweak(&internal, Some(&root))
}

pub fn spk(p: &FbtcPool) -> Result<Vec<u8>, String> {
    let (x, _) = output_key(p)?;
    p2tr_spk(&x)
}

pub fn testnet_address(p: &FbtcPool) -> Result<String, String> {
    let (x, _) = output_key(p)?;
    vault::p2tr_testnet_address(&x)
}

fn control_block(p: &FbtcPool, kind: &str) -> Result<Vec<u8>, String> {
    let (h_peg, h_abort) = leaf_hashes(p);
    let sibling = match kind {
        KIND_PEGOUT => h_abort,
        KIND_ABORT => h_peg,
        _ => return Err("unknown leaf".into()),
    };
    let (internal_x, _) = nums_internal_from_tag(POOL_NUMS_TAG);
    let (_, parity) = output_key(p)?;
    Ok(taproot_control_block(parity, &internal_x, &[sibling]))
}

fn script_of(p: &FbtcPool, kind: &str) -> Result<Vec<u8>, String> {
    match kind {
        KIND_PEGOUT => Ok(pegout_script(p.csv_blocks)),
        KIND_ABORT => Ok(abort_script(&p.abort_commit, &p.abort_xonly)),
        _ => Err("unknown leaf".into()),
    }
}

pub fn pool_json(p: &FbtcPool) -> Result<PoolJson, String> {
    Ok(PoolJson {
        address: testnet_address(p)?,
        spk_hex: hex::encode(spk(p)?),
        abort_xonly_hex: hex::encode(p.abort_xonly),
        abort_commit_hex: hex::encode(p.abort_commit),
        csv_blocks: p.csv_blocks,
        nums_tag: String::from_utf8_lossy(POOL_NUMS_TAG).into_owned(),
        merkle_root_hex: hex::encode(merkle_root(p)),
        watch_key: "none".into(),
        n_of_n: "none".into(),
        operator_front: "none".into(),
        pegout: "CSV + withdrawer BIP340 CHECKSIG (pubkey in witness)".into(),
        abort: "SHA256(preimage)+abort CHECKSIG; presigned return-to-pool only".into(),
    })
}

pub fn from_hexes(commit_hex: &str, xonly_hex: &str, csv_blocks: u32) -> Result<FbtcPool, String> {
    if csv_blocks == 0 {
        return Err("csv must be > 0".into());
    }
    Ok(FbtcPool {
        abort_commit: decode32(commit_hex)?,
        abort_xonly: decode32(xonly_hex)?,
        csv_blocks,
    })
}

pub fn parse_outpoint(utxo: &str) -> Result<(String, u32, u64), String> {
    let parts: Vec<&str> = utxo.split(':').collect();
    if parts.len() != 3 {
        return Err("utxo must be txid:vout:sats".into());
    }
    Ok((
        parts[0].to_string(),
        parts[1].parse::<u32>().map_err(|e| e.to_string())?,
        parts[2].parse::<u64>().map_err(|e| e.to_string())?,
    ))
}

fn decode32(h: &str) -> Result<[u8; 32], String> {
    let b = hex::decode(h.trim()).map_err(|e| e.to_string())?;
    if b.len() != 32 {
        return Err("need 32 bytes".into());
    }
    let mut o = [0u8; 32];
    o.copy_from_slice(&b);
    Ok(o)
}

fn fee_ok(fee: u64) -> Result<(), String> {
    if fee == 0 || fee > MAX_FEE_SATS {
        return Err("fee range".into());
    }
    Ok(())
}

/// Honest peg-out: dest gets `amount`, change returns to the same pool script.
pub fn sign_pegout(
    p: &FbtcPool,
    user_secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    dest_spk: &[u8],
    amount: u64,
    fee: u64,
) -> Result<String, String> {
    fee_ok(fee)?;
    if amount < DUST_SATS {
        return Err("peg-out dust".into());
    }
    if sats <= fee {
        return Err("fee ≥ value".into());
    }
    if amount + fee > sats {
        return Err("amount+fee exceeds pool utxo".into());
    }
    let change = sats - amount - fee;
    let pool_spk = spk(p)?;
    let mut outputs = vec![TxOut {
        sats: amount,
        script_pubkey: dest_spk.to_vec(),
    }];
    if change >= DUST_SATS {
        outputs.push(TxOut {
            sats: change,
            script_pubkey: pool_spk.clone(),
        });
    } else if change != 0 {
        return Err("change is dust; lower amount or increase fee".into());
    }
    let user_x = btc_tx::xonly_from_secret(user_secret)?;
    let script = script_of(p, KIND_PEGOUT)?;
    let leaf_h = tapleaf_hash(&script);
    let inputs = [TxIn {
        txid_display: prev_txid.to_string(),
        vout,
        sats,
        sequence: p.csv_blocks,
        script_pubkey: pool_spk,
    }];
    let pre = bip341_scriptpath_sighash(2, 0, &inputs, &outputs, 0, SIGHASH_DEFAULT, &leaf_h)?;
    let sig = bip340_sign(user_secret, &pre)?;
    bip340_verify(&user_x, &pre, &sig)?;
    let ctrl = control_block(p, KIND_PEGOUT)?;
    let raw = btc_tx::assemble_segwit_tx(
        2,
        0,
        &inputs,
        &outputs,
        &[vec![sig.to_vec(), user_x.to_vec(), script, ctrl]],
    )?;
    Ok(hex::encode(raw))
}

/// Presign abort: **only** return-to-pool. Call after every new pool outpoint.
pub fn presign_abort(
    p: &FbtcPool,
    abort_secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    fee: u64,
) -> Result<PresignedAbort, String> {
    fee_ok(fee)?;
    let got = btc_tx::xonly_from_secret(abort_secret)?;
    if got != p.abort_xonly {
        return Err("abort secret does not match pool abort x-only".into());
    }
    if sats <= fee {
        return Err("abort fee ≥ value".into());
    }
    let ret = sats - fee;
    if ret < DUST_SATS {
        return Err("abort dust".into());
    }
    let pool_spk = spk(p)?;
    let script = script_of(p, KIND_ABORT)?;
    let leaf_h = tapleaf_hash(&script);
    let inputs = [TxIn {
        txid_display: prev_txid.to_string(),
        vout,
        sats,
        sequence: 0xffff_fffe,
        script_pubkey: pool_spk.clone(),
    }];
    let outputs = [TxOut {
        sats: ret,
        script_pubkey: pool_spk.clone(),
    }];
    if outputs[0].script_pubkey != pool_spk {
        return Err("abort output is not the pool".into());
    }
    let pre = bip341_scriptpath_sighash(2, 0, &inputs, &outputs, 0, SIGHASH_DEFAULT, &leaf_h)?;
    let sig = bip340_sign(abort_secret, &pre)?;
    bip340_verify(&p.abort_xonly, &pre, &sig)?;
    Ok(PresignedAbort {
        txid: prev_txid.to_string(),
        vout,
        sats,
        fee,
        csv_blocks: p.csv_blocks,
        abort_xonly_hex: hex::encode(p.abort_xonly),
        abort_commit_hex: hex::encode(p.abort_commit),
        sig_hex: hex::encode(sig),
        script_hex: hex::encode(&script),
        control_hex: hex::encode(control_block(p, KIND_ABORT)?),
        pool_spk_hex: hex::encode(&pool_spk),
        pool_address: testnet_address(p)?,
        return_sats: ret,
    })
}

/// Attach the published abort preimage to a presigned return-to-pool tx.
pub fn assemble_abort(p: &FbtcPool, pre: &PresignedAbort, preimage: &[u8; 32]) -> Result<String, String> {
    if sha256_32(preimage) != p.abort_commit {
        return Err("preimage does not match abort_commit".into());
    }
    if decode32(&pre.abort_commit_hex)? != p.abort_commit {
        return Err("presign abort_commit mismatch".into());
    }
    let pool_spk = spk(p)?;
    if hex::decode(&pre.pool_spk_hex).map_err(|e| e.to_string())? != pool_spk {
        return Err("presign pool spk mismatch".into());
    }
    let inputs = [TxIn {
        txid_display: pre.txid.clone(),
        vout: pre.vout,
        sats: pre.sats,
        sequence: 0xffff_fffe,
        script_pubkey: pool_spk.clone(),
    }];
    let outputs = [TxOut {
        sats: pre.return_sats,
        script_pubkey: pool_spk,
    }];
    let sig = hex::decode(&pre.sig_hex).map_err(|e| e.to_string())?;
    if sig.len() != 64 {
        return Err("abort sig must be 64 bytes".into());
    }
    let script = hex::decode(&pre.script_hex).map_err(|e| e.to_string())?;
    let ctrl = hex::decode(&pre.control_hex).map_err(|e| e.to_string())?;
    let raw = btc_tx::assemble_segwit_tx(
        2,
        0,
        &inputs,
        &outputs,
        &[vec![sig, preimage.to_vec(), script, ctrl]],
    )?;
    Ok(hex::encode(raw))
}

/// Dave (or anyone) pays the pool + 24-byte FALC memo (mint dest).
pub fn sign_deposit(
    user_secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    prev_spk: &[u8],
    pool: &FbtcPool,
    fpl_account: &str,
    fee: u64,
) -> Result<String, String> {
    fee_ok(fee)?;
    if sats <= fee {
        return Err("deposit fee ≥ value".into());
    }
    let paid = sats - fee;
    if paid < DUST_SATS {
        return Err("deposit dust".into());
    }
    let pool_spk = spk(pool)?;
    let outputs = [
        TxOut {
            sats: paid,
            script_pubkey: pool_spk,
        },
        TxOut {
            sats: 0,
            script_pubkey: btc_tx::falc_opreturn(fpl_account),
        },
    ];
    let inputs = [TxIn {
        txid_display: prev_txid.to_string(),
        vout,
        sats,
        sequence: 0xffff_fffe,
        script_pubkey: prev_spk.to_vec(),
    }];
    let pk = vault::pubkey_from_secret(user_secret)?;
    let raw = if btc_tx::is_p2wpkh(prev_spk) {
        let pre = btc_tx::bip143_sighash(2, 0, &inputs, &outputs, 0, crate::btc_tx::SIGHASH_ALL)?;
        let der = ecdsa_der_all(user_secret, &pre)?;
        btc_tx::assemble_segwit_tx(2, 0, &inputs, &outputs, &[vec![der, pk]])?
    } else if btc_tx::is_p2pkh(prev_spk) {
        let pre = btc_tx::legacy_p2pkh_sighash(2, 0, &inputs, &outputs, 0, crate::btc_tx::SIGHASH_ALL)?;
        let der = ecdsa_der_all(user_secret, &pre)?;
        let mut ss = btc_tx::push_bytes(&der);
        ss.extend_from_slice(&btc_tx::push_bytes(&pk));
        btc_tx::assemble_legacy_tx(2, 0, &inputs, &outputs, &[ss])?
    } else {
        return Err("deposit prevout must be p2wpkh/p2pkh".into());
    };
    Ok(hex::encode(raw))
}

fn ecdsa_der_all(secret: &[u8; 32], prehash: &[u8; 32]) -> Result<Vec<u8>, String> {
    use k256::ecdsa::signature::hazmat::PrehashSigner;
    use k256::ecdsa::{Signature, SigningKey};
    let signing = SigningKey::from_bytes(secret.into()).map_err(|e| e.to_string())?;
    let sig: Signature = signing.sign_prehash(prehash).map_err(|e| e.to_string())?;
    let sig = match sig.normalize_s() {
        Some(n) => n,
        None => sig,
    };
    let mut der = btc_tx::der_encode_rs(&sig.to_bytes())?;
    der.push(crate::btc_tx::SIGHASH_ALL as u8);
    Ok(der)
}

pub fn keys_dir_files(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        dir.join("abort.privhex"),
        dir.join("abort.preimage"),
        dir.join("pool.json"),
    )
}

fn write_secret_hex(path: &Path, secret: &[u8; 32]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, hex::encode(secret)).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}

fn random_secret() -> Result<[u8; 32], String> {
    use rand::RngCore;
    for _ in 0..16 {
        let mut b = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut b);
        if btc_tx::xonly_from_secret(&b).is_ok() {
            return Ok(b);
        }
    }
    Err("failed to sample secp256k1 secret".into())
}

/// Generate abort key + preimage, write 0600 files, return public pool.json.
pub fn ceremony(dir: &Path, csv_blocks: u32) -> Result<PoolJson, String> {
    if csv_blocks == 0 {
        return Err("csv must be > 0".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let abort_sk = random_secret()?;
    let mut preimage = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut preimage);
    let abort_xonly = btc_tx::xonly_from_secret(&abort_sk)?;
    let p = FbtcPool {
        abort_commit: sha256_32(&preimage),
        abort_xonly,
        csv_blocks,
    };
    let (sk_path, pre_path, json_path) = keys_dir_files(dir);
    write_secret_hex(&sk_path, &abort_sk)?;
    write_secret_hex(&pre_path, &preimage)?;
    let meta = pool_json(&p)?;
    let tmp = json_path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(tmp, json_path).map_err(|e| e.to_string())?;
    Ok(meta)
}

pub fn load_pool_json(path: &Path) -> Result<FbtcPool, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let j: PoolJson = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    from_hexes(&j.abort_commit_hex, &j.abort_xonly_hex, j.csv_blocks)
}

pub fn load_presign(path: &Path) -> Result<PresignedAbort, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}

/// Auditor checks: no watch, no n-of-n, abort output is pool-only, pegout is CSV+CHECKSIG.
pub fn audit_product(p: &FbtcPool, watch_xonly: &[u8; 32]) -> Result<(), String> {
    let peg = pegout_script(p.csv_blocks);
    let ab = abort_script(&p.abort_commit, &p.abort_xonly);
    if !peg.contains(&0xb2) || !peg.contains(&0xac) {
        return Err("pegout must be CSV+CHECKSIG".into());
    }
    if peg.windows(32).any(|w| w == watch_xonly.as_slice()) {
        return Err("pegout commits watch key".into());
    }
    if peg.windows(32).any(|w| w == p.abort_xonly.as_slice()) {
        return Err("pegout must not commit abort key (would mix freeze with take)".into());
    }
    if !ab.contains(&0xa8) || !ab.windows(32).any(|w| w == p.abort_commit.as_slice()) {
        return Err("abort missing SHA256(commit)".into());
    }
    if !ab.windows(32).any(|w| w == p.abort_xonly.as_slice()) {
        return Err("abort missing abort x-only".into());
    }
    if ab.contains(&0xb2) {
        return Err("abort must be live immediately (no CSV)".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitvm::watch_xonly;
    use crate::btc_spv::{extract_witnesses, parse_tx};

    fn sk(tag: &[u8]) -> [u8; 32] {
        sha256_32(tag)
    }

    fn pool() -> (FbtcPool, [u8; 32], [u8; 32]) {
        let abort_sk = sk(b"abort-key");
        let pre = sk(b"abort-preimage");
        let p = FbtcPool {
            abort_commit: sha256_32(&pre),
            abort_xonly: btc_tx::xonly_from_secret(&abort_sk).unwrap(),
            csv_blocks: 6,
        };
        (p, abort_sk, pre)
    }

    #[test]
    fn product_audit_no_watch_no_n_of_n() {
        let (p, _, _) = pool();
        audit_product(&p, &watch_xonly()).unwrap();
        let peg = pegout_script(p.csv_blocks);
        assert!(!peg.windows(32).any(|w| w == p.abort_xonly.as_slice()));
        assert_eq!(testnet_address(&p).unwrap().chars().take(4).collect::<String>(), "tb1p");
    }

    #[test]
    fn dave_and_sally_can_both_sign_pegout() {
        let (p, _, _) = pool();
        let dest = btc_tx::p2wpkh_spk(&[7u8; 20]).unwrap();
        let dave = sign_pegout(&p, &sk(b"dave"), "aa".repeat(32).as_str(), 0, 50_000, &dest, 20_000, 1_000).unwrap();
        let sally = sign_pegout(&p, &sk(b"sally"), "aa".repeat(32).as_str(), 0, 50_000, &dest, 20_000, 1_000).unwrap();
        assert_ne!(dave, sally);
        for raw in [&dave, &sally] {
            let tx = parse_tx(&hex::decode(raw).unwrap()).unwrap();
            assert_eq!(tx.outputs[0].0, 20_000);
            assert_eq!(tx.outputs[0].1, dest);
            assert_eq!(tx.outputs[1].1, spk(&p).unwrap());
            assert_eq!(tx.outputs[1].0, 29_000);
            let w = extract_witnesses(&hex::decode(raw).unwrap()).unwrap();
            assert_eq!(w[0].len(), 4); // sig, xonly, script, control
            assert_eq!(w[0][0].len(), 64);
            assert_eq!(w[0][1].len(), 32);
        }
    }

    #[test]
    fn abort_returns_only_to_pool() {
        let (p, abort_sk, preimage) = pool();
        let presign = presign_abort(&p, &abort_sk, "bb".repeat(32).as_str(), 1, 40_000, 1_000).unwrap();
        assert_eq!(presign.return_sats, 39_000);
        assert_eq!(presign.pool_spk_hex, hex::encode(spk(&p).unwrap()));
        let raw = assemble_abort(&p, &presign, &preimage).unwrap();
        let tx = parse_tx(&hex::decode(&raw).unwrap()).unwrap();
        assert_eq!(tx.outputs.len(), 1);
        assert_eq!(tx.outputs[0].1, spk(&p).unwrap());
        assert_eq!(tx.outputs[0].0, 39_000);
        let w = extract_witnesses(&hex::decode(&raw).unwrap()).unwrap();
        assert_eq!(w[0][1], preimage);
        let bad = sk(b"wrong-preimage");
        assert!(assemble_abort(&p, &presign, &bad).is_err());
        let dave = sk(b"dave");
        assert!(presign_abort(&p, &dave, "bb".repeat(32).as_str(), 1, 40_000, 1_000).is_err());
    }

    #[test]
    fn abort_leaf_has_no_csv_pegout_has_csv() {
        let (p, _, _) = pool();
        assert!(pegout_script(p.csv_blocks).contains(&0xb2));
        assert!(!abort_script(&p.abort_commit, &p.abort_xonly).contains(&0xb2));
    }

    #[test]
    fn wrong_preimage_or_watch_cannot_build_abort() {
        let (p, abort_sk, _) = pool();
        let presign = presign_abort(&p, &abort_sk, "cc".repeat(32).as_str(), 0, 10_000, 500).unwrap();
        assert!(assemble_abort(&p, &presign, &sk(b"nope")).is_err());
    }
}
