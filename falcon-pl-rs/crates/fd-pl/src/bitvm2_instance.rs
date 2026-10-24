//! BitVM2 Bitcoin instance: NUMS P2TR + claimer Kickoff + setup-once Challenge.
//!
//! No watch key. Key-path is unspendable (NUMS). Kickoff dest-locks to the
//! user and starts the dest-lock CSV clock. Challenge is presigned at setup
//! (Assert connector or return-to-instance) then setup secrets are deleted.

use crate::btc_tx::{
    self, bip340_sign, bip340_verify, bip341_scriptpath_sighash, nums_internal_from_tag,
    p2tr_spk, tapleaf_hash, taproot_control_block, taproot_tweak, TxIn, TxOut, SIGHASH_DEFAULT,
};
use crate::vault::{self, DUST_SATS};
use fd_pl_bitvm2::spend::{
    build_assert_connector_taptree, build_instance_taptree, parse_operator_xonly, parse_setup_xonly,
    script_commits_xonly, AssertConnectorTree, InstanceTapTree, ASSERT_CSV_BLOCKS,
    INSTANCE_NUMS_TAG, KIND_KICKOFF, KIND_OPERATOR_RECOVER, KIND_SETUP, PAYOUT_CSV_BLOCKS,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use fd_pl_bitvm2::winternitz::LamportPk;

pub const MIN_SETUP: usize = 2;

#[derive(Debug, Clone)]
pub struct InstanceKeys {
    pub operator_xonly: [u8; 32],
    pub setup_xonly: Vec<[u8; 32]>,
    pub csv_blocks: u32,
    pub assert_csv_blocks: u32,
    pub lamport: Option<LamportPk>,
}

impl InstanceKeys {
    pub fn new(
        operator_xonly: [u8; 32],
        setup_xonly: Vec<[u8; 32]>,
        csv_blocks: u32,
        assert_csv_blocks: u32,
    ) -> Result<Self, String> {
        if setup_xonly.len() < MIN_SETUP {
            return Err("need ≥2 setup x-only keys".into());
        }
        if csv_blocks == 0 || assert_csv_blocks == 0 {
            return Err("CSV must be > 0".into());
        }
        Ok(Self {
            operator_xonly,
            setup_xonly,
            csv_blocks,
            assert_csv_blocks,
            lamport: None,
        })
    }

    pub fn from_hex(
        operator_hex: &str,
        setup_hex: &[String],
        csv_blocks: u32,
        assert_csv_blocks: u32,
    ) -> Result<Self, String> {
        Self::new(
            parse_operator_xonly(operator_hex)?,
            parse_setup_xonly(setup_hex)?,
            csv_blocks,
            assert_csv_blocks,
        )
    }

    /// `BITVM2_OPERATOR_XONLY` + comma-separated `BITVM2_SETUP_XONLY`.
    pub fn from_env() -> Option<Self> {
        let op = std::env::var("BITVM2_OPERATOR_XONLY").ok()?;
        let setup = std::env::var("BITVM2_SETUP_XONLY").ok()?;
        if op.trim().is_empty() || setup.trim().is_empty() {
            return None;
        }
        let keys: Vec<String> = setup
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let csv = std::env::var("BITVM2_PAYOUT_CSV")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(PAYOUT_CSV_BLOCKS);
        let acsv = std::env::var("BITVM2_ASSERT_CSV")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(ASSERT_CSV_BLOCKS);
        let mut k = Self::from_hex(&op, &keys, csv, acsv).ok()?;
        if let Ok(p) = std::env::var("BITVM2_LAMPORT_PK_JSON") {
            if let Ok(raw) = std::fs::read_to_string(p) {
                k.lamport = serde_json::from_str(&raw).ok();
            }
        }
        Some(k)
    }

    pub fn with_lamport(mut self, pk: LamportPk) -> Self {
        self.lamport = Some(pk);
        self
    }
}

pub fn instance_tree(keys: &InstanceKeys) -> Result<InstanceTapTree, String> {
    build_instance_taptree(&keys.operator_xonly, keys.csv_blocks, &keys.setup_xonly)
}

pub fn assert_tree(keys: &InstanceKeys) -> Result<AssertConnectorTree, String> {
    build_assert_connector_taptree(
        &keys.setup_xonly,
        keys.assert_csv_blocks,
        &keys.operator_xonly,
        keys.lamport.as_ref(),
    )
}

pub fn instance_output_key(keys: &InstanceKeys) -> Result<([u8; 32], u8), String> {
    let tree = instance_tree(keys)?;
    let root = tree.merkle_root()?;
    let (_x, p) = nums_internal_from_tag(INSTANCE_NUMS_TAG);
    taproot_tweak(&p, Some(&root))
}

pub fn instance_spk(keys: &InstanceKeys) -> Result<Vec<u8>, String> {
    let (x, _) = instance_output_key(keys)?;
    p2tr_spk(&x)
}

pub fn instance_address(keys: &InstanceKeys) -> Result<String, String> {
    let (x, _) = instance_output_key(keys)?;
    vault::p2tr_testnet_address(&x)
}

pub fn assert_output_key(keys: &InstanceKeys) -> Result<([u8; 32], u8), String> {
    let tree = assert_tree(keys)?;
    let root = tree.merkle_root()?;
    let (_x, p) = nums_internal_from_tag(INSTANCE_NUMS_TAG);
    taproot_tweak(&p, Some(&root))
}

pub fn assert_spk(keys: &InstanceKeys) -> Result<Vec<u8>, String> {
    let (x, _) = assert_output_key(keys)?;
    p2tr_spk(&x)
}

fn instance_control(keys: &InstanceKeys, kind: &str) -> Result<Vec<u8>, String> {
    let tree = instance_tree(keys)?;
    let (internal, _) = nums_internal_from_tag(INSTANCE_NUMS_TAG);
    let (_, parity) = instance_output_key(keys)?;
    let nodes = tree.control_nodes(kind)?;
    Ok(taproot_control_block(parity, &internal, &nodes))
}

/// Claimer Kickoff: dest-lock to user + change to instance. No setup keys. No watch.
pub fn sign_kickoff(
    keys: &InstanceKeys,
    claimer_secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    dest: &str,
    amount: u64,
    abort_commit: &[u8; 32],
    dest_csv: u32,
    fee: u64,
) -> Result<String, String> {
    let got = btc_tx::xonly_from_secret(claimer_secret)?;
    if got != keys.operator_xonly {
        return Err("claimer secret does not match instance claimer x-only".into());
    }
    let dest_h160 = crate::bitvm::dest_h160_from_address(dest)?;
    let (_ws, lock) = dest_lock_spk(abort_commit, &dest_h160, dest_csv)?;
    let outputs = user_kickoff_outputs(keys, sats, fee, amount, &lock)?;
    let isp = instance_spk(keys)?;
    user_kickoff_policy_ok(dest, amount, abort_commit, dest_csv, &outputs, &isp, sats)?;
    let tree = instance_tree(keys)?;
    let script = tree.script(KIND_KICKOFF)?;
    let leaf_h = tapleaf_hash(&script);
    let inputs = [TxIn {
        txid_display: prev_txid.to_string(),
        vout,
        sats,
        sequence: 0xffff_fffe,
        script_pubkey: isp,
    }];
    let pre = bip341_scriptpath_sighash(2, 0, &inputs, &outputs, 0, SIGHASH_DEFAULT, &leaf_h)?;
    let sig = bip340_sign(claimer_secret, &pre)?;
    bip340_verify(&keys.operator_xonly, &pre, &sig)?;
    let ctrl = instance_control(keys, KIND_KICKOFF)?;
    let raw = btc_tx::assemble_segwit_tx(2, 0, &inputs, &outputs, &[vec![sig.to_vec(), script, ctrl]])?;
    Ok(hex::encode(raw))
}

fn challenge_outputs(
    keys: &InstanceKeys,
    instance_sats: u64,
    fee: u64,
    statement_digest: Option<&[u8; 32]>,
) -> Result<Vec<TxOut>, String> {
    if instance_sats <= fee {
        return Err("challenge fee ≥ value".into());
    }
    let pay = instance_sats - fee;
    if pay < DUST_SATS {
        return Err("challenge dust".into());
    }
    let asp = assert_spk(keys)?;
    let mut outs = vec![TxOut {
        sats: pay,
        script_pubkey: asp,
    }];
    if let Some(d) = statement_digest {
        outs.push(TxOut {
            sats: 0,
            script_pubkey: btc_tx::bitvm2_claim_opreturn(d),
        });
    }
    Ok(outs)
}

fn challenge_input(keys: &InstanceKeys, prev_txid: &str, vout: u32, sats: u64) -> Result<TxIn, String> {
    Ok(TxIn {
        txid_display: prev_txid.to_string(),
        vout,
        sats,
        sequence: 0xffff_fffe,
        script_pubkey: instance_spk(keys)?,
    })
}

/// One setup signer's BIP340 sig on the Challenge template (n-of-n).
pub fn sign_challenge_partial(
    keys: &InstanceKeys,
    setup_secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    fee: u64,
    statement_digest: Option<&[u8; 32]>,
) -> Result<[u8; 64], String> {
    let pk = btc_tx::xonly_from_secret(setup_secret)?;
    if !keys.setup_xonly.iter().any(|k| k == &pk) {
        return Err("setup secret is not a setup signer".into());
    }
    let inputs = [challenge_input(keys, prev_txid, vout, sats)?];
    let outputs = challenge_outputs(keys, sats, fee, statement_digest)?;
    let tree = instance_tree(keys)?;
    let script = tree.script(KIND_SETUP)?;
    let leaf_h = tapleaf_hash(&script);
    let pre = bip341_scriptpath_sighash(
        2,
        0,
        &inputs,
        &outputs,
        0,
        SIGHASH_DEFAULT,
        &leaf_h,
    )?;
    let sig = bip340_sign(setup_secret, &pre)?;
    bip340_verify(&pk, &pre, &sig)?;
    Ok(sig)
}

/// Combine n setup signatures (order = `keys.setup_xonly`) into a broadcastable Challenge.
pub fn assemble_challenge(
    keys: &InstanceKeys,
    sigs: &[[u8; 64]],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    fee: u64,
    statement_digest: Option<&[u8; 32]>,
) -> Result<String, String> {
    if sigs.len() != keys.setup_xonly.len() {
        return Err("need one signature per setup key, in key order".into());
    }
    let inputs = [challenge_input(keys, prev_txid, vout, sats)?];
    let outputs = challenge_outputs(keys, sats, fee, statement_digest)?;
    let tree = instance_tree(keys)?;
    let script = tree.script(KIND_SETUP)?;
    let leaf_h = tapleaf_hash(&script);
    let pre = bip341_scriptpath_sighash(
        2,
        0,
        &inputs,
        &outputs,
        0,
        SIGHASH_DEFAULT,
        &leaf_h,
    )?;
    for (pk, sig) in keys.setup_xonly.iter().zip(sigs.iter()) {
        bip340_verify(pk, &pre, sig)?;
    }
    let ctrl = instance_control(keys, KIND_SETUP)?;
    let mut wit: Vec<Vec<u8>> = sigs.iter().map(|s| s.to_vec()).collect();
    wit.push(script);
    wit.push(ctrl);
    let raw = btc_tx::assemble_segwit_tx(2, 0, &inputs, &outputs, &[wit])?;
    Ok(hex::encode(raw))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresignedSpend {
    pub kind: String,
    pub txid: String,
    pub vout: u32,
    pub sats: u64,
    pub fee: u64,
    pub raw_hex: String,
    pub output_spk_hex: String,
    pub instance_spk_hex: String,
}

fn abort_to_instance_outputs(
    keys: &InstanceKeys,
    instance_sats: u64,
    fee: u64,
) -> Result<Vec<TxOut>, String> {
    if instance_sats <= fee {
        return Err("abort fee ≥ value".into());
    }
    let pay = instance_sats - fee;
    if pay < DUST_SATS {
        return Err("abort dust".into());
    }
    Ok(vec![TxOut {
        sats: pay,
        script_pubkey: instance_spk(keys)?,
    }])
}

/// Presign one setup-n-of-n spend (Challenge→Assert or abort→instance). Then delete secrets.
pub fn presign_setup_spend(
    keys: &InstanceKeys,
    setup_secrets: &[[u8; 32]],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    fee: u64,
    kind: &str,
) -> Result<PresignedSpend, String> {
    if setup_secrets.len() != keys.setup_xonly.len() {
        return Err("need every setup secret, in key order".into());
    }
    for (sk, pk) in setup_secrets.iter().zip(keys.setup_xonly.iter()) {
        if btc_tx::xonly_from_secret(sk)? != *pk {
            return Err("setup secret order does not match x-only keys".into());
        }
    }
    let outputs = match kind {
        "challenge_assert" => challenge_outputs(keys, sats, fee, None)?,
        "abort_instance" => abort_to_instance_outputs(keys, sats, fee)?,
        _ => return Err("presign kind must be challenge_assert or abort_instance".into()),
    };
    let mut sigs = Vec::new();
    for sk in setup_secrets {
        sigs.push(sign_setup_outputs_partial(
            keys, sk, prev_txid, vout, sats, &outputs,
        )?);
    }
    let raw = assemble_setup_outputs(keys, &sigs, prev_txid, vout, sats, &outputs)?;
    Ok(PresignedSpend {
        kind: kind.to_string(),
        txid: prev_txid.to_string(),
        vout,
        sats,
        fee,
        raw_hex: raw,
        output_spk_hex: hex::encode(&outputs[0].script_pubkey),
        instance_spk_hex: hex::encode(instance_spk(keys)?),
    })
}

pub fn wipe_secret_file(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let n = std::fs::metadata(path).map_err(|e| e.to_string())?.len() as usize;
    let zeros = vec![0u8; n.max(64)];
    std::fs::write(path, &zeros).map_err(|e| e.to_string())?;
    std::fs::remove_file(path).map_err(|e| e.to_string())
}

/// True if a confirmed spend of the instance is an honest dest-lock Kickoff.
pub fn kickoff_raw_policy_ok(
    raw_hex: &str,
    dest: &str,
    amount: u64,
    abort_commit: &[u8; 32],
    dest_csv: u32,
    instance_spk: &[u8],
    input_sats: u64,
) -> Result<(), String> {
    let raw = crate::btc_spv::hex_decode(raw_hex)?;
    let tx = crate::btc_spv::parse_tx(&raw)?;
    let outs: Vec<TxOut> = tx
        .outputs
        .iter()
        .filter(|(sats, _)| *sats > 0)
        .map(|(sats, spk)| TxOut {
            sats: *sats,
            script_pubkey: spk.clone(),
        })
        .collect();
    user_kickoff_policy_ok(dest, amount, abort_commit, dest_csv, &outs, instance_spk, input_sats)
}

fn setup_leaf_sighash(
    keys: &InstanceKeys,
    prev_txid: &str,
    vout: u32,
    sats: u64,
    outputs: &[TxOut],
) -> Result<([u8; 32], Vec<u8>, TxIn), String> {
    let inputs = [challenge_input(keys, prev_txid, vout, sats)?];
    let tree = instance_tree(keys)?;
    let script = tree.script(KIND_SETUP)?;
    let leaf_h = tapleaf_hash(&script);
    let pre = bip341_scriptpath_sighash(
        2,
        0,
        &inputs,
        outputs,
        0,
        SIGHASH_DEFAULT,
        &leaf_h,
    )?;
    Ok((pre, script, inputs.into_iter().next().unwrap()))
}

pub fn dest_lock_spk(
    abort_commit: &[u8; 32],
    dest_h160: &[u8; 20],
    csv: u32,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let ws = crate::bitvm::claim_witness_script(abort_commit, dest_h160, csv)?;
    let spk = crate::bitvm::p2wsh_spk(&ws);
    Ok((ws, spk))
}

/// Instance → dest-lock (user) + change to instance. Setup n-of-n; no watch; no dave front.
pub fn user_kickoff_outputs(
    keys: &InstanceKeys,
    instance_sats: u64,
    fee: u64,
    amount: u64,
    dest_lock_spk: &[u8],
) -> Result<Vec<TxOut>, String> {
    if amount < DUST_SATS {
        return Err("kickoff amount dust".into());
    }
    if fee == 0 || instance_sats < fee + amount {
        return Err("instance cannot cover dest-lock + fee".into());
    }
    let change = instance_sats - fee - amount;
    let mut outs = vec![TxOut {
        sats: amount,
        script_pubkey: dest_lock_spk.to_vec(),
    }];
    if change >= DUST_SATS {
        outs.push(TxOut {
            sats: change,
            script_pubkey: instance_spk(keys)?,
        });
    } else if change != 0 {
        return Err("kickoff change dust".into());
    }
    Ok(outs)
}

/// Highest fee a dest-lock Kickoff may leave (sats). Covers the default 1000-sat
/// fee plus change below dust folded into the fee (#49).
pub const MAX_KICKOFF_FEE_SATS: u64 = 5_000;

/// Honest dest-lock Kickoff shape (#49): exactly 1 or 2 value outputs, vout0 the
/// dest-lock for `dest` at exactly `amount`, optional vout1 change to the instance,
/// and `input_sats - sum(outputs)` (the fee) at most [`MAX_KICKOFF_FEE_SATS`].
pub fn user_kickoff_policy_ok(
    dest: &str,
    amount: u64,
    abort_commit: &[u8; 32],
    csv: u32,
    outputs: &[TxOut],
    instance_spk: &[u8],
    input_sats: u64,
) -> Result<(), String> {
    let dest_h160 = crate::bitvm::dest_h160_from_address(dest)?;
    let (_ws, lock) = dest_lock_spk(abort_commit, &dest_h160, csv)?;
    if outputs.is_empty() {
        return Err("kickoff has no outputs".into());
    }
    if outputs.len() > 2 {
        return Err("kickoff has extra outputs (max dest-lock + change)".into());
    }
    let out_sum = outputs
        .iter()
        .try_fold(0u64, |a, o| a.checked_add(o.sats))
        .ok_or_else(|| "kickoff output sum overflow".to_string())?;
    if out_sum > input_sats {
        return Err("kickoff outputs exceed input".into());
    }
    if input_sats - out_sum > MAX_KICKOFF_FEE_SATS {
        return Err("kickoff fee/leftover above bound (change must return to instance)".into());
    }
    if outputs[0].script_pubkey != lock {
        return Err("vout0 is not dest-lock for this dest".into());
    }
    if outputs[0].sats != amount {
        return Err("vout0 amount ≠ burn".into());
    }
    if outputs.len() > 1 && outputs[1].script_pubkey.as_slice() != instance_spk {
        return Err("change must return to instance".into());
    }
    Ok(())
}

pub fn sign_setup_outputs_partial(
    keys: &InstanceKeys,
    setup_secret: &[u8; 32],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    outputs: &[TxOut],
) -> Result<[u8; 64], String> {
    let pk = btc_tx::xonly_from_secret(setup_secret)?;
    if !keys.setup_xonly.iter().any(|k| k == &pk) {
        return Err("setup secret is not a setup signer".into());
    }
    let (pre, _script, _inp) = setup_leaf_sighash(keys, prev_txid, vout, sats, outputs)?;
    let sig = bip340_sign(setup_secret, &pre)?;
    bip340_verify(&pk, &pre, &sig)?;
    Ok(sig)
}

pub fn assemble_setup_outputs(
    keys: &InstanceKeys,
    sigs: &[[u8; 64]],
    prev_txid: &str,
    vout: u32,
    sats: u64,
    outputs: &[TxOut],
) -> Result<String, String> {
    if sigs.len() != keys.setup_xonly.len() {
        return Err("need one signature per setup key, in key order".into());
    }
    let (pre, script, inp) = setup_leaf_sighash(keys, prev_txid, vout, sats, outputs)?;
    for (pk, sig) in keys.setup_xonly.iter().zip(sigs.iter()) {
        bip340_verify(pk, &pre, sig)?;
    }
    let ctrl = instance_control(keys, KIND_SETUP)?;
    let mut wit: Vec<Vec<u8>> = sigs.iter().map(|s| s.to_vec()).collect();
    wit.push(script);
    wit.push(ctrl);
    let raw = btc_tx::assemble_segwit_tx(2, 0, &[inp], outputs, &[wit])?;
    Ok(hex::encode(raw))
}

pub fn default_csv() -> (u32, u32) {
    (PAYOUT_CSV_BLOCKS, ASSERT_CSV_BLOCKS)
}

pub fn nums_tag() -> &'static [u8] {
    INSTANCE_NUMS_TAG
}

/// Sanity: kickoff leaf must commit claimer and must not commit the watch x-only.
pub fn payout_has_no_watch(keys: &InstanceKeys, watch_xonly: &[u8; 32]) -> Result<(), String> {
    let tree = instance_tree(keys)?;
    let kickoff = tree.script(KIND_KICKOFF)?;
    if !script_commits_xonly(&kickoff, &keys.operator_xonly) {
        return Err("kickoff missing claimer".into());
    }
    if script_commits_xonly(&kickoff, watch_xonly) {
        return Err("kickoff commits watch key".into());
    }
    if kickoff.contains(&0xb2) {
        return Err("kickoff must not wait CSV — dest-lock starts the clock".into());
    }
    let setup = tree.script(KIND_SETUP)?;
    if script_commits_xonly(&setup, watch_xonly) {
        return Err("setup n-of-n commits watch key".into());
    }
    let _ = KIND_OPERATOR_RECOVER;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitvm::watch_xonly;
    use crate::btc_spv::{extract_witnesses, parse_tx};
    use crate::btc_tx::{compressed_pubkey_from_secret, xonly_from_secret};
    use crate::vault::hash160;
    use sha2::{Digest, Sha256};

    fn secret(tag: &str) -> [u8; 32] {
        Sha256::digest(tag.as_bytes()).into()
    }

    fn keys() -> (InstanceKeys, [u8; 32], Vec<[u8; 32]>) {
        let op_sec = secret("d1-operator");
        let s0 = secret("d1-setup-0");
        let s1 = secret("d1-setup-1");
        let op = xonly_from_secret(&op_sec).unwrap();
        let setup = vec![
            xonly_from_secret(&s0).unwrap(),
            xonly_from_secret(&s1).unwrap(),
        ];
        let k = InstanceKeys::new(op, setup.clone(), 6, 6).unwrap();
        (k, op_sec, vec![s0, s1])
    }

    #[test]
    fn instance_address_is_p2tr_nums_not_kickoff_pool() {
        let (k, _, _) = keys();
        let addr = instance_address(&k).unwrap();
        assert!(addr.starts_with("tb1p"));
        let kickoff = crate::bitvm::nums_pool_address().unwrap();
        assert_ne!(addr, kickoff);
        let spk = instance_spk(&k).unwrap();
        assert!(btc_tx::is_p2tr(&spk));
    }

    #[test]
    fn payout_leaf_excludes_watch_key() {
        let (k, _, _) = keys();
        payout_has_no_watch(&k, &watch_xonly()).unwrap();
    }

    #[test]
    fn kickoff_dest_lock_no_setup_keys_and_no_watch() {
        let (k, op_sec, _) = keys();
        let dest_sec = secret("d1-dest");
        let pk = compressed_pubkey_from_secret(&dest_sec).unwrap();
        let dest = crate::vault::p2wpkh_testnet_address(&pk).unwrap();
        let abort = crate::bitvm::sha256_32(b"abort-test");
        let csv = crate::bitvm::CLAIM_CSV_BLOCKS;
        let raw = sign_kickoff(
            &k,
            &op_sec,
            &"aa".repeat(32),
            0,
            80_000,
            &dest,
            20_000,
            &abort,
            csv,
            1_000,
        )
        .unwrap();
        let bytes = hex::decode(&raw).unwrap();
        let parsed = parse_tx(&bytes).unwrap();
        let dest_h160 = crate::bitvm::dest_h160_from_address(&dest).unwrap();
        let (_ws, lock) = dest_lock_spk(&abort, &dest_h160, csv).unwrap();
        assert_eq!(parsed.outputs[0].1, lock);
        assert_eq!(parsed.outputs[0].0, 20_000);
        assert_eq!(parsed.outputs[1].1, instance_spk(&k).unwrap());
        let dave = crate::btc_tx::p2wpkh_spk(&hash160(&pk)).unwrap();
        assert_ne!(parsed.outputs[0].1, dave);
        kickoff_raw_policy_ok(
            &raw,
            &dest,
            20_000,
            &abort,
            csv,
            &instance_spk(&k).unwrap(),
            80_000,
        )
        .unwrap();
        let wits = extract_witnesses(&bytes).unwrap();
        assert!(crate::bitvm::is_script_path_witness(&wits[0]));
        let tree = instance_tree(&k).unwrap();
        assert_eq!(hex::encode(&wits[0][1]), tree.leaf(KIND_KICKOFF).unwrap().script_hex);
        assert!(!tree.script(KIND_KICKOFF).unwrap().contains(&0xb2));
    }

    #[test]
    fn challenge_pays_assert_connector_not_operator() {
        let (k, op_sec, setup_secs) = keys();
        let digest = [0x42u8; 32];
        let mut sigs = Vec::new();
        for sec in &setup_secs {
            sigs.push(
                sign_challenge_partial(&k, sec, &"bb".repeat(32), 1, 90_000, 1_000, Some(&digest))
                    .unwrap(),
            );
        }
        let raw = assemble_challenge(
            &k,
            &sigs,
            &"bb".repeat(32),
            1,
            90_000,
            1_000,
            Some(&digest),
        )
        .unwrap();
        let bytes = hex::decode(&raw).unwrap();
        let parsed = parse_tx(&bytes).unwrap();
        assert_eq!(parsed.outputs[0].1, assert_spk(&k).unwrap());
        assert_eq!(parsed.outputs[0].0, 89_000);
        assert_eq!(
            parsed.outputs[1].1,
            crate::btc_tx::bitvm2_claim_opreturn(&digest)
        );
        let op_spk = {
            let pk = compressed_pubkey_from_secret(&op_sec).unwrap();
            crate::btc_tx::p2wpkh_spk(&hash160(&pk)).unwrap()
        };
        assert_ne!(parsed.outputs[0].1, op_spk);
        let wits = extract_witnesses(&bytes).unwrap();
        assert_eq!(wits[0].len(), 4); // sig0, sig1, script, ctrl
        assert!(crate::bitvm::is_script_path_witness(&wits[0]));
    }

    #[test]
    fn challenge_rejects_wrong_setup_key() {
        let (k, _, _) = keys();
        let bad = secret("not-setup");
        let err = sign_challenge_partial(&k, &bad, &"cc".repeat(32), 0, 50_000, 500, None)
            .unwrap_err();
        assert!(err.contains("setup secret"));
    }

    #[test]
    fn kickoff_rejects_wrong_claimer() {
        let (k, _, _) = keys();
        let dest_sec = secret("d1-dest");
        let pk = compressed_pubkey_from_secret(&dest_sec).unwrap();
        let dest = crate::vault::p2wpkh_testnet_address(&pk).unwrap();
        let abort = crate::bitvm::sha256_32(b"abort-test");
        let err = sign_kickoff(
            &k,
            &secret("wrong-op"),
            &"dd".repeat(32),
            0,
            80_000,
            &dest,
            20_000,
            &abort,
            6,
            1_000,
        )
        .unwrap_err();
        assert!(err.contains("claimer secret"));
    }

    #[test]
    fn presign_challenge_pays_assert_abort_pays_instance() {
        let (k, _, setup_secs) = keys();
        let secs: Vec<[u8; 32]> = setup_secs;
        let ch = presign_setup_spend(
            &k,
            &secs,
            &"bb".repeat(32),
            0,
            90_000,
            1_000,
            "challenge_assert",
        )
        .unwrap();
        assert_eq!(ch.output_spk_hex, hex::encode(assert_spk(&k).unwrap()));
        let parsed = parse_tx(&hex::decode(&ch.raw_hex).unwrap()).unwrap();
        assert_eq!(parsed.outputs[0].1, assert_spk(&k).unwrap());
        let ab = presign_setup_spend(
            &k,
            &secs,
            &"bb".repeat(32),
            0,
            90_000,
            1_000,
            "abort_instance",
        )
        .unwrap();
        let parsed = parse_tx(&hex::decode(&ab.raw_hex).unwrap()).unwrap();
        assert_eq!(parsed.outputs[0].1, instance_spk(&k).unwrap());
        assert_eq!(parsed.outputs.len(), 1);
    }

    #[test]
    fn user_kickoff_pays_dest_lock_not_dave_and_no_watch() {
        let (k, op_sec, setup_secs) = keys();
        let dest_sec = secret("d1-user");
        let pk = compressed_pubkey_from_secret(&dest_sec).unwrap();
        let dest_spk = crate::btc_tx::p2wpkh_spk(&hash160(&pk)).unwrap();
        let dest = crate::vault::p2wpkh_testnet_address(&pk).unwrap();
        let dest_h160 = crate::bitvm::dest_h160_from_address(&dest).unwrap();
        let abort = crate::bitvm::sha256_32(b"abort-test");
        let csv = crate::bitvm::CLAIM_CSV_BLOCKS;
        let (_ws, lock) = dest_lock_spk(&abort, &dest_h160, csv).unwrap();
        let outs = user_kickoff_outputs(&k, 90_000, 1_000, 20_000, &lock).unwrap();
        user_kickoff_policy_ok(&dest, 20_000, &abort, csv, &outs, &instance_spk(&k).unwrap(), 90_000).unwrap();
        assert_eq!(outs[0].sats, 20_000);
        assert_eq!(outs[0].script_pubkey, lock);
        assert_eq!(outs[1].script_pubkey, instance_spk(&k).unwrap());
        let dave = {
            let pk = compressed_pubkey_from_secret(&op_sec).unwrap();
            crate::btc_tx::p2wpkh_spk(&hash160(&pk)).unwrap()
        };
        assert_ne!(outs[0].script_pubkey, dave);
        assert_ne!(outs[0].script_pubkey, dest_spk);
        let raw = sign_kickoff(
            &k,
            &op_sec,
            &"ee".repeat(32),
            0,
            90_000,
            &dest,
            20_000,
            &abort,
            csv,
            1_000,
        )
        .unwrap();
        let parsed = parse_tx(&hex::decode(&raw).unwrap()).unwrap();
        assert_eq!(parsed.outputs[0].1, lock);
        assert_eq!(parsed.outputs[0].0, 20_000);
        payout_has_no_watch(&k, &watch_xonly()).unwrap();
        user_kickoff_policy_ok(&dest, 19_000, &abort, csv, &outs, &instance_spk(&k).unwrap(), 90_000)
            .unwrap_err();
    }

    #[test]
    fn kickoff_policy_rejects_extra_outputs_short_change_and_excess_fee() {
        let (k, _op, _setup) = keys();
        let pk = compressed_pubkey_from_secret(&secret("d49-user")).unwrap();
        let dest = crate::vault::p2wpkh_testnet_address(&pk).unwrap();
        let dest_h160 = crate::bitvm::dest_h160_from_address(&dest).unwrap();
        let abort = crate::bitvm::sha256_32(b"abort-49");
        let csv = crate::bitvm::CLAIM_CSV_BLOCKS;
        let (_ws, lock) = dest_lock_spk(&abort, &dest_h160, csv).unwrap();
        let isp = instance_spk(&k).unwrap();
        // (1) honest 2-output Kickoff passes
        let honest = user_kickoff_outputs(&k, 90_000, 1_000, 20_000, &lock).unwrap();
        assert_eq!(honest.len(), 2);
        user_kickoff_policy_ok(&dest, 20_000, &abort, csv, &honest, &isp, 90_000).unwrap();
        // (2) extra 3rd output to the claimer fails
        let mut extra = honest.clone();
        extra[1].sats -= 30_000;
        extra.push(TxOut { sats: 30_000, script_pubkey: crate::btc_tx::p2wpkh_spk(&hash160(&pk)).unwrap() });
        assert!(user_kickoff_policy_ok(&dest, 20_000, &abort, csv, &extra, &isp, 90_000).is_err());
        // (3) change below input - amount - fee fails
        let mut short = honest.clone();
        short[1].sats -= 30_000;
        assert!(user_kickoff_policy_ok(&dest, 20_000, &abort, csv, &short, &isp, 90_000).is_err());
        // (4) 1-output Kickoff leaving the rest as fee fails
        let lone = vec![honest[0].clone()];
        assert!(user_kickoff_policy_ok(&dest, 20_000, &abort, csv, &lone, &isp, 90_000).is_err());
        // 1-output Kickoff whose leftover is within the bound passes
        user_kickoff_policy_ok(&dest, 20_000, &abort, csv, &lone, &isp, 21_000).unwrap();
        // outputs above input fail
        assert!(user_kickoff_policy_ok(&dest, 20_000, &abort, csv, &honest, &isp, 50_000).is_err());
    }
}
