//! Legacy BTC exit helpers: dest-locked claim (CSV) + NUMS Kickoff with watch co-sign.
//!
//! This is **not** BitVM2. Kickoff requires dest BIP340 + operator watch BIP340.
//! Next path: `docs/BITVM2_ENGINE.md` (operator-fronting + optimistic SNARK challenge).
//!
//! Claim script shape: OP_IF hashlock / OP_ELSE CSV+CHECKSIG. Abort preimage stays
//! unpublished until FPL accepts RailChallenge. ELSE pays only the committed dest.

use crate::btc_spv::{hex_decode, hex_encode};
use crate::btc_tx::{decode_address, is_p2pkh, is_p2wpkh, p2wpkh_hash160, write_compact};
use crate::crypto::sha256_hex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Same CSV as the old qXRP BitVM vault (`kBTC_VAULT_CSV_BLOCKS`).
pub const CLAIM_CSV_BLOCKS: u32 = 6;
pub const FPL_CHALLENGE_LEDGERS: u64 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Pending,
    Challenged,
    Final,
    Paid,
}

impl Default for ClaimStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BitvmClaim {
    pub note_id: String,
    pub from: String,
    pub dest: String,
    pub dest_h160: String,
    pub amount: u64,
    pub abort_commit: String,
    pub csv_blocks: u32,
    pub open_height: u64,
    pub status: ClaimStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge_by: Option<String>,
    /// Set only after an accepted challenge. Empty on pending/final.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub published_abort: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub claim_spk_hex: String,
}

pub fn sha256_32(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn abort_commit(preimage: &[u8]) -> [u8; 32] {
    sha256_32(preimage)
}

/// Random-looking abort secret that is NOT a public function of dest/amount.
/// Caller should pass 32 urandom bytes in production.
pub fn abort_preimage_from_seed(seed: &[u8], note_id: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"fpl-bitvm-abort-v1|");
    h.update(seed);
    h.update(note_id.as_bytes());
    h.finalize().into()
}

pub fn push_csv(csv: u32) -> Result<Vec<u8>, String> {
    if csv == 0 {
        return Ok(vec![0x00]);
    }
    if csv <= 16 {
        return Ok(vec![0x50 + csv as u8]);
    }
    let b = csv.to_le_bytes();
    let n = b.iter().rposition(|x| *x != 0).map(|i| i + 1).unwrap_or(1);
    let mut s = vec![n as u8];
    s.extend_from_slice(&b[..n]);
    Ok(s)
}

/// Witness script: IF sha256(abort)==commit TRUE ELSE csv DROP DUP HASH160 dest EQUALVERIFY CHECKSIG ENDIF
pub fn claim_witness_script(abort_commit: &[u8; 32], dest_h160: &[u8; 20], csv: u32) -> Result<Vec<u8>, String> {
    let mut s = Vec::with_capacity(80);
    s.push(0x63); // IF
    s.push(0xa8); // SHA256
    s.push(0x20);
    s.extend_from_slice(abort_commit);
    s.push(0x88); // EQUALVERIFY
    s.push(0x51); // TRUE
    s.push(0x67); // ELSE
    s.extend_from_slice(&push_csv(csv)?);
    s.push(0xb2); // CSV
    s.push(0x75); // DROP
    s.push(0x76); // DUP
    s.push(0xa9); // HASH160
    s.push(0x14);
    s.extend_from_slice(dest_h160);
    s.push(0x88); // EQUALVERIFY
    s.push(0xac); // CHECKSIG
    s.push(0x68); // ENDIF
    Ok(s)
}

pub fn p2wsh_spk(witness_script: &[u8]) -> Vec<u8> {
    let h = sha256_32(witness_script);
    let mut s = Vec::with_capacity(34);
    s.push(0x00);
    s.push(0x20);
    s.extend_from_slice(&h);
    s
}

pub fn p2wsh_spk_hex(witness_script: &[u8]) -> String {
    hex_encode(&p2wsh_spk(witness_script))
}

pub fn dest_h160_from_address(addr: &str) -> Result<[u8; 20], String> {
    let (_k, spk) = decode_address(addr)?;
    if is_p2wpkh(&spk) {
        return p2wpkh_hash160(&spk);
    }
    if is_p2pkh(&spk) {
        let mut h = [0u8; 20];
        h.copy_from_slice(&spk[3..23]);
        return Ok(h);
    }
    Err("dest must be P2PKH or P2WPKH".into())
}

pub fn parse_claim_script(ws: &[u8]) -> Result<([u8; 32], [u8; 20], u32), String> {
    // IF SHA256 0x20 <32> EQUALVERIFY TRUE ELSE <csv> CSV DROP DUP HASH160 0x14 <20> EQUALVERIFY CHECKSIG ENDIF
    if ws.len() < 60 {
        return Err(format!("claim script too short {}", ws.len()));
    }
    let mut i = 0usize;
    let take = |i: &mut usize, b: u8| -> Result<(), String> {
        if *i >= ws.len() || ws[*i] != b {
            return Err("claim script parse".into());
        }
        *i += 1;
        Ok(())
    };
    take(&mut i, 0x63)?;
    take(&mut i, 0xa8)?;
    take(&mut i, 0x20)?;
    let mut commit = [0u8; 32];
    commit.copy_from_slice(&ws[i..i + 32]);
    i += 32;
    take(&mut i, 0x88)?;
    take(&mut i, 0x51)?;
    take(&mut i, 0x67)?;
    let csv = if ws[i] >= 0x51 && ws[i] <= 0x60 {
        let v = (ws[i] - 0x50) as u32;
        i += 1;
        v
    } else if ws[i] == 0x01 {
        i += 1;
        let v = ws[i] as u32;
        i += 1;
        v
    } else {
        return Err("csv encode".into());
    };
    take(&mut i, 0xb2)?;
    take(&mut i, 0x75)?;
    take(&mut i, 0x76)?;
    take(&mut i, 0xa9)?;
    take(&mut i, 0x14)?;
    let mut dest = [0u8; 20];
    dest.copy_from_slice(&ws[i..i + 20]);
    i += 20;
    take(&mut i, 0x88)?;
    take(&mut i, 0xac)?;
    take(&mut i, 0x68)?;
    if i != ws.len() {
        return Err("claim script trailing".into());
    }
    Ok((commit, dest, csv))
}

/// Abort witness stack (plus script): [preimage, 0x01 (IF)]
pub fn abort_witness(preimage: &[u8], witness_script: &[u8]) -> Vec<Vec<u8>> {
    vec![preimage.to_vec(), vec![0x01], witness_script.to_vec()]
}

pub fn abort_path_valid(preimage: &[u8], commit: &[u8; 32]) -> bool {
    abort_commit(preimage) == *commit
}

pub fn abort_dir(keys_dir: &Path) -> PathBuf {
    keys_dir.join("bitvm-abort")
}

pub fn abort_path(keys_dir: &Path, note_id: &str) -> PathBuf {
    abort_dir(keys_dir).join(note_id)
}

pub fn abort_seed_path(keys_dir: &Path) -> PathBuf {
    keys_dir.join("bitvm-abort-seed")
}

pub fn write_abort_seed(keys_dir: &Path, seed: &[u8; 32]) -> Result<(), String> {
    std::fs::create_dir_all(keys_dir).map_err(|e| e.to_string())?;
    let p = abort_seed_path(keys_dir);
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, seed).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(tmp, p).map_err(|e| e.to_string())
}

pub fn write_abort_secret(keys_dir: &Path, note_id: &str, secret: &[u8; 32]) -> Result<(), String> {
    std::fs::create_dir_all(abort_dir(keys_dir)).map_err(|e| e.to_string())?;
    let p = abort_path(keys_dir, note_id);
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, hex_encode(secret)).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(tmp, p).map_err(|e| e.to_string())
}

pub fn read_abort_secret(keys_dir: &Path, note_id: &str) -> Result<[u8; 32], String> {
    let raw = std::fs::read_to_string(abort_path(keys_dir, note_id)).map_err(|e| e.to_string())?;
    let b = hex_decode(raw.trim())?;
    if b.len() != 32 {
        return Err("abort secret must be 32 bytes".into());
    }
    let mut o = [0u8; 32];
    o.copy_from_slice(&b);
    Ok(o)
}

pub fn claim_id(from: &str, dest: &str, amount: u64, height: u64) -> String {
    sha256_hex(format!("bitvm-claim:{from}:{dest}:{amount}:{height}").as_bytes())
}

/// Compact-size encode a witness script for tests / raw tx helpers.
pub fn encode_push(data: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    write_compact(&mut o, data.len() as u64);
    o.extend_from_slice(data);
    o
}

/// NUMS seed — no corresponding secp private key is used or stored.
pub const NUMS_TAG: &[u8] = b"FalconPL-BitVM-Kickoff-NUMS-2300";

pub fn nums_digest() -> [u8; 32] {
    sha256_32(NUMS_TAG)
}

/// Internal NUMS x-only (BIP341 H + rG, r = sha256(NUMS_TAG)). Key-path is dead.
pub fn nums_internal_xonly() -> [u8; 32] {
    crate::btc_tx::nums_internal_from_tag(NUMS_TAG).0
}

/// Retired v1 Kickoff leaf (`SHA256 DROP CHECKSIG`). Dest pubkey was in the
/// witness, so any key could move the pool. Kept only to spend that UTXO
/// into v2.
pub fn kickoff_tapscript_v1() -> Vec<u8> {
    vec![0xa8, 0x75, 0xac]
}

/// Committed watcher x-only (even Y). Secret is operator-only
/// (`keys/bitvm-watch.secret`). Not a lottery seat.
#[cfg(not(test))]
pub const WATCH_XONLY: [u8; 32] = [
    0x17, 0x5b, 0x0d, 0xd7, 0xaa, 0x41, 0xe7, 0x24, 0x83, 0x0a, 0xa7, 0x02, 0xa8, 0x5f, 0x8d,
    0xe9, 0x42, 0xae, 0x51, 0xcd, 0x96, 0xd6, 0xe4, 0xcf, 0xf2, 0x4c, 0xd4, 0xe9, 0xb0, 0xd8,
    0xb6, 0x04,
];

pub fn watch_secret_for_tests() -> [u8; 32] {
    sha256_32(b"fpl-bitvm-watch-test-key")
}

pub fn watch_xonly() -> [u8; 32] {
    #[cfg(test)]
    {
        crate::btc_tx::xonly_from_secret(&watch_secret_for_tests()).expect("test watch")
    }
    #[cfg(not(test))]
    {
        WATCH_XONLY
    }
}

/// Kickoff tapscript v2: SHA256 DROP <watch_xonly> CHECKSIGVERIFY CHECKSIG.
/// Witness: <dest_sig> <dest_xonly> <watch_sig> <withdraw_leaf_preimage>
///
/// Bitcoin still cannot see FPL `claim_root` (no CTV/CAT). The committed
/// watcher key is what stops dest (or anyone) from signing an arbitrary
/// output. Watcher signs only after `kickoff_ok`. NUMS key-path stays dead.
pub fn kickoff_tapscript() -> Vec<u8> {
    let mut s = Vec::with_capacity(37);
    s.push(0xa8); // SHA256
    s.push(0x75); // DROP
    s.push(0x20);
    s.extend_from_slice(&watch_xonly());
    s.push(0xad); // CHECKSIGVERIFY
    s.push(0xac); // CHECKSIG
    s
}

/// Disprove tapscript: anyone who shows SHA256(preimage) != claimed digest.
/// Witness: <claimed_digest> <preimage>
pub fn disprove_tapscript() -> Vec<u8> {
    // SWAP OVER SHA256 EQUAL NOT VERIFY DROP TRUE
    vec![0x7c, 0x78, 0xa8, 0x87, 0x91, 0x69, 0x75, 0x51]
}

pub fn nums_script_root() -> [u8; 32] {
    let a = crate::btc_tx::tapleaf_hash(&kickoff_tapscript());
    let b = crate::btc_tx::tapleaf_hash(&disprove_tapscript());
    crate::btc_tx::tapbranch(&a, &b)
}

pub fn nums_script_root_v1() -> [u8; 32] {
    let a = crate::btc_tx::tapleaf_hash(&kickoff_tapscript_v1());
    let b = crate::btc_tx::tapleaf_hash(&disprove_tapscript());
    crate::btc_tx::tapbranch(&a, &b)
}

pub fn nums_output_key() -> Result<([u8; 32], u8), String> {
    let (_x, p) = crate::btc_tx::nums_internal_from_tag(NUMS_TAG);
    let root = nums_script_root();
    crate::btc_tx::taproot_tweak(&p, Some(&root))
}

pub fn nums_pool_spk() -> Result<Vec<u8>, String> {
    let (x, _) = nums_output_key()?;
    crate::btc_tx::p2tr_spk(&x)
}

pub fn nums_pool_address() -> Result<String, String> {
    let (x, _) = nums_output_key()?;
    crate::vault::p2tr_testnet_address(&x)
}

pub fn is_nums_spk(spk: &[u8]) -> bool {
    match nums_pool_spk() {
        Ok(want) => spk == want.as_slice(),
        Err(_) => false,
    }
}

/// Public vault identity for a NUMS pool. n=t=0: no FROST shares exist.
pub fn nums_vault_pub() -> crate::vault::VaultPub {
    let spk = nums_pool_spk().unwrap_or_default();
    crate::vault::VaultPub {
        epoch: 0,
        n: 0,
        t: 0,
        pubkey_hex: hex::encode(nums_internal_xonly()),
        spk_hex: hex::encode(spk),
    }
}

fn kickoff_control_block() -> Result<Vec<u8>, String> {
    let (internal, _) = crate::btc_tx::nums_internal_from_tag(NUMS_TAG);
    let (_, parity) = nums_output_key()?;
    let sibling = crate::btc_tx::tapleaf_hash(&disprove_tapscript());
    Ok(crate::btc_tx::taproot_control_block(parity, &internal, &[sibling]))
}

fn kickoff_control_block_v1() -> Result<Vec<u8>, String> {
    let (internal, _) = crate::btc_tx::nums_internal_from_tag(NUMS_TAG);
    let (out_x, parity) = {
        let (_x, p) = crate::btc_tx::nums_internal_from_tag(NUMS_TAG);
        crate::btc_tx::taproot_tweak(&p, Some(&nums_script_root_v1()))?
    };
    let _ = out_x;
    let sibling = crate::btc_tx::tapleaf_hash(&disprove_tapscript());
    Ok(crate::btc_tx::taproot_control_block(parity, &internal, &[sibling]))
}

pub fn nums_pool_spk_v1() -> Result<Vec<u8>, String> {
    let (_x, p) = crate::btc_tx::nums_internal_from_tag(NUMS_TAG);
    let (x, _) = crate::btc_tx::taproot_tweak(&p, Some(&nums_script_root_v1()))?;
    crate::btc_tx::p2tr_spk(&x)
}

pub fn nums_pool_address_v1() -> Result<String, String> {
    let (_x, p) = crate::btc_tx::nums_internal_from_tag(NUMS_TAG);
    let (x, _) = crate::btc_tx::taproot_tweak(&p, Some(&nums_script_root_v1()))?;
    crate::vault::p2tr_testnet_address(&x)
}

fn disprove_control_block() -> Result<Vec<u8>, String> {
    let (internal, _) = crate::btc_tx::nums_internal_from_tag(NUMS_TAG);
    let (_, parity) = nums_output_key()?;
    let sibling = crate::btc_tx::tapleaf_hash(&kickoff_tapscript());
    Ok(crate::btc_tx::taproot_control_block(parity, &internal, &[sibling]))
}

pub fn withdraw_leaf_preimage(note_id: &str, dest_h160: &[u8; 20], amount: u64) -> Vec<u8> {
    let mut pre = Vec::from(&b"fpl-bitvm-wd-v1|"[..]);
    pre.extend_from_slice(note_id.as_bytes());
    pre.extend_from_slice(dest_h160);
    pre.extend_from_slice(&amount.to_le_bytes());
    pre
}

/// Withdraw leaf the Kickoff program checks against FPL `claim_root`.
pub fn withdraw_leaf(note_id: &str, dest_h160: &[u8; 20], amount: u64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"fpl-bitvm-wd-v1|");
    h.update(note_id.as_bytes());
    h.update(dest_h160);
    h.update(amount.to_le_bytes());
    h.finalize().into()
}

/// Kickoff is valid iff the FPL leaf is in `claim_root` and vout0 is the dest-lock.
pub fn kickoff_valid(
    note_id: &str,
    dest_h160: &[u8; 20],
    amount: u64,
    claim_root: &[u8; 32],
    merkle_path: &[String],
    merkle_index: u32,
    vout0_spk: &[u8],
    abort_commit: &[u8; 32],
    csv: u32,
) -> Result<(), String> {
    let leaf = withdraw_leaf(note_id, dest_h160, amount);
    let got = crate::evm_spv::merkle_root_binary(&hex::encode(leaf), merkle_path, merkle_index)?;
    if !got.eq_ignore_ascii_case(&hex::encode(claim_root)) {
        return Err("Kickoff: FPL withdraw leaf not in claim_root".into());
    }
    let ws = claim_witness_script(abort_commit, dest_h160, csv)?;
    let want = p2wsh_spk(&ws);
    if vout0_spk != want.as_slice() {
        return Err("Kickoff: vout0 is not dest-locked claim script".into());
    }
    Ok(())
}

/// One SHA256-step of the Kickoff program (BitVM-style: challenge a single step).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaStep {
    pub preimage: Vec<u8>,
    pub digest: [u8; 32],
}

pub fn kickoff_trace(
    note_id: &str,
    dest_h160: &[u8; 20],
    amount: u64,
) -> Vec<ShaStep> {
    let mut pre = Vec::from(&b"fpl-bitvm-wd-v1|"[..]);
    pre.extend_from_slice(note_id.as_bytes());
    pre.extend_from_slice(dest_h160);
    pre.extend_from_slice(&amount.to_le_bytes());
    let digest = sha256_32(&pre);
    vec![ShaStep { preimage: pre, digest }]
}

pub fn challenge_sha_step(step: &ShaStep) -> Result<(), String> {
    if sha256_32(&step.preimage) != step.digest {
        return Err("BitVM challenge wins: SHA256 step mismatch".into());
    }
    Ok(())
}

/// HASH160(compressed dest pk) must match the dest-lock hash.
pub fn dest_secret_matches(dest: &str, dest_secret: &[u8; 32]) -> Result<(), String> {
    let want = dest_h160_from_address(dest)?;
    let pk = crate::btc_tx::compressed_pubkey_from_secret(dest_secret)?;
    let got = crate::vault::hash160(&pk);
    if got != want {
        return Err("dest secret does not match dest address".into());
    }
    Ok(())
}

/// Script-path dest-lock Kickoff. Does not call FROST. Committee cannot
/// key-path this UTXO (NUMS). Dest owner + committed watcher sign the tapleaf.
pub fn nums_dest_lock_kickoff(
    utxo_txid: &str,
    vout: u32,
    sats: u64,
    dest: &str,
    dest_secret: &[u8; 32],
    watch_secret: &[u8; 32],
    amount: u64,
    fee: u64,
    from: &str,
    abort_seed: &[u8; 32],
) -> Result<String, String> {
    if std::env::var("FPL_KICKOFF_FROZEN")
        .ok()
        .is_some_and(|v| v != "0")
    {
        return Err("Kickoff frozen: BTC exits are BitVM2 instance-only (unset FPL_KICKOFF_FROZEN to override)".into());
    }
    dest_secret_matches(dest, dest_secret)?;
    let watch_pk = crate::btc_tx::xonly_from_secret(watch_secret)?;
    if watch_pk != watch_xonly() {
        return Err("watch secret does not match committed watch key".into());
    }
    let vault_spk = nums_pool_spk()?;
    let utxos = [crate::vault::VaultUtxo {
        txid: utxo_txid.to_string(),
        vout,
        sats,
        height: 0,
    }];
    let plan = crate::vault::coin_select(&utxos, amount, dest, fee)?;
    let note_id = crate::bridge::btc_withdraw_note_id(from, amount, dest);
    let abort = abort_preimage_from_seed(abort_seed, &note_id);
    let dest_h160 = dest_h160_from_address(dest)?;
    let commit = abort_commit(&abort);
    let ws = claim_witness_script(&commit, &dest_h160, CLAIM_CSV_BLOCKS)?;
    let claim_spk = p2wsh_spk(&ws);
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
        script_pubkey: claim_spk,
    }];
    if plan.change >= crate::vault::DUST_SATS {
        outputs.push(crate::btc_tx::TxOut {
            sats: plan.change,
            script_pubkey: vault_spk.clone(),
        });
    }
    let leaf_h = crate::btc_tx::tapleaf_hash(&kickoff_tapscript());
    let mut witnesses = Vec::new();
    let preimage = withdraw_leaf_preimage(&note_id, &dest_h160, amount);
    let xonly = crate::btc_tx::xonly_from_secret(dest_secret)?;
    let ctrl = kickoff_control_block()?;
    for i in 0..inputs.len() {
        let pre = crate::btc_tx::bip341_scriptpath_sighash(
            2,
            0,
            &inputs,
            &outputs,
            i,
            crate::btc_tx::SIGHASH_DEFAULT,
            &leaf_h,
        )?;
        let sig = crate::btc_tx::bip340_sign(dest_secret, &pre)?;
        crate::btc_tx::bip340_verify(&xonly, &pre, &sig)?;
        let wsig = crate::btc_tx::bip340_sign(watch_secret, &pre)?;
        crate::btc_tx::bip340_verify(&watch_pk, &pre, &wsig)?;
        witnesses.push(vec![
            sig.to_vec(),
            xonly.to_vec(),
            wsig.to_vec(),
            preimage.clone(),
            kickoff_tapscript(),
            ctrl.clone(),
        ]);
    }
    let raw = crate::btc_tx::assemble_segwit_tx(2, 0, &inputs, &outputs, &witnesses)?;
    Ok(hex::encode(raw))
}

/// One-time v1 → v2 pool move. Uses the retired anyone-CHECKSIG leaf to pay
/// the watcher-bound NUMS. Not a user withdraw.
pub fn nums_migrate_v1_to_v2(
    utxo_txid: &str,
    vout: u32,
    sats: u64,
    dest_secret: &[u8; 32],
    fee: u64,
) -> Result<String, String> {
    if sats <= fee + crate::vault::DUST_SATS {
        return Err("v1 migrate dust".into());
    }
    let old_spk = nums_pool_spk_v1()?;
    let new_spk = nums_pool_spk()?;
    if old_spk == new_spk {
        return Err("v1 and v2 pool scripts match".into());
    }
    let inputs = [crate::btc_tx::TxIn {
        txid_display: utxo_txid.to_string(),
        vout,
        sats,
        sequence: 0xffff_fffe,
        script_pubkey: old_spk,
    }];
    let outputs = [crate::btc_tx::TxOut {
        sats: sats - fee,
        script_pubkey: new_spk,
    }];
    let leaf_h = crate::btc_tx::tapleaf_hash(&kickoff_tapscript_v1());
    let xonly = crate::btc_tx::xonly_from_secret(dest_secret)?;
    let ctrl = kickoff_control_block_v1()?;
    let pre = crate::btc_tx::bip341_scriptpath_sighash(
        2,
        0,
        &inputs,
        &outputs,
        0,
        crate::btc_tx::SIGHASH_DEFAULT,
        &leaf_h,
    )?;
    let sig = crate::btc_tx::bip340_sign(dest_secret, &pre)?;
    crate::btc_tx::bip340_verify(&xonly, &pre, &sig)?;
    let preimage = withdraw_leaf_preimage("migrate-v1-v2", &[0u8; 20], sats - fee);
    let witnesses = [vec![
        sig.to_vec(),
        xonly.to_vec(),
        preimage,
        kickoff_tapscript_v1(),
        ctrl,
    ]];
    let raw = crate::btc_tx::assemble_segwit_tx(2, 0, &inputs, &outputs, &witnesses)?;
    Ok(hex::encode(raw))
}

/// Anyone can take the pool back to NUMS if a published SHA256 step is a lie.
pub fn nums_disprove_tx(
    utxo_txid: &str,
    vout: u32,
    sats: u64,
    fee: u64,
    step: &ShaStep,
) -> Result<String, String> {
    if challenge_sha_step(step).is_ok() {
        return Err("disprove needs a lying SHA256 step".into());
    }
    if sats <= fee {
        return Err("disprove fee ≥ value".into());
    }
    let vault_spk = nums_pool_spk()?;
    let inputs = [crate::btc_tx::TxIn {
        txid_display: utxo_txid.to_string(),
        vout,
        sats,
        sequence: 0xffff_ffff,
        script_pubkey: vault_spk.clone(),
    }];
    let outputs = [crate::btc_tx::TxOut {
        sats: sats - fee,
        script_pubkey: vault_spk,
    }];
    let wit = vec![
        step.digest.to_vec(),
        step.preimage.clone(),
        disprove_tapscript(),
        disprove_control_block()?,
    ];
    let raw = crate::btc_tx::assemble_segwit_tx(2, 0, &inputs, &outputs, &[wit])?;
    Ok(hex::encode(raw))
}

pub fn is_script_path_witness(stack: &[Vec<u8>]) -> bool {
    stack
        .last()
        .map(|b| crate::btc_tx::is_taproot_control_block(b))
        .unwrap_or(false)
}

pub fn is_keypath_witness(stack: &[Vec<u8>]) -> bool {
    stack.len() == 1 && (stack[0].len() == 64 || stack[0].len() == 65)
}

/// NUMS pool: script-path only. A 64-byte FROST key-path is invalid here.
pub fn nums_kickoff_structure_ok(raw: &[u8], vault_spk: &[u8]) -> Result<(), String> {
    if !is_nums_spk(vault_spk) {
        return Err("not the NUMS pool".into());
    }
    let parsed = crate::btc_spv::parse_tx(raw)?;
    let wits = crate::btc_spv::extract_witnesses(raw)?;
    if wits.is_empty() {
        return Err("NUMS Kickoff needs a segwit witness".into());
    }
    for (i, w) in wits.iter().enumerate() {
        if is_keypath_witness(w) {
            return Err(format!("NUMS input {i} is a key-path spend — FROST cannot move this UTXO"));
        }
        if !is_script_path_witness(w) {
            return Err(format!("NUMS input {i} is not a script-path spend"));
        }
        let ctrl = w.last().unwrap();
        let internal = nums_internal_xonly();
        if ctrl.len() < 33 || ctrl[1..33] != internal {
            return Err("control block is not the NUMS internal key".into());
        }
        if w.len() < 6 {
            return Err("Kickoff witness needs dest sig, dest key, watch sig, preimage, script".into());
        }
        let script = &w[w.len() - 2];
        if script.as_slice() != kickoff_tapscript().as_slice() {
            return Err("Kickoff leaf is not dest+watch CHECKSIG".into());
        }
        if w[2].len() != 64 {
            return Err("Kickoff missing watcher BIP340 sig".into());
        }
    }
    let vout0 = parsed.outputs.first().ok_or("kickoff missing vout0")?;
    if !crate::btc_tx::is_p2wsh(&vout0.1) {
        return Err("Kickoff vout0 is not dest-lock P2WSH".into());
    }
    let nums = nums_pool_spk()?;
    for (i, (sats, spk)) in parsed.outputs.iter().enumerate() {
        if i == 0 {
            continue;
        }
        if spk.as_slice() == nums.as_slice() && *sats >= crate::vault::DUST_SATS {
            continue;
        }
        if !spk.is_empty() && spk[0] == 0x6a {
            continue;
        }
        return Err("NUMS Kickoff change must return to the NUMS pool".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_script_roundtrip_and_dest_bind() {
        let abort = sha256_32(b"secret-abort");
        let commit = abort_commit(&abort);
        let dest = dest_h160_from_address("n1fH7uhaDauQYfuwG6t1aTncvyuBSZtDEY").unwrap();
        let ws = claim_witness_script(&commit, &dest, 6).unwrap();
        let (c2, d2, csv) = parse_claim_script(&ws).unwrap();
        assert_eq!(c2, commit);
        assert_eq!(d2, dest);
        assert_eq!(csv, 6);
        assert!(abort_path_valid(&abort, &commit));
        assert!(!abort_path_valid(b"wrong", &commit));
        assert_eq!(p2wsh_spk(&ws).len(), 34);
    }

    #[test]
    fn abort_file_is_not_the_commit() {
        let dir = std::env::temp_dir().join("bitvm-abort-test");
        let _ = std::fs::remove_dir_all(&dir);
        let secret = sha256_32(b"file-secret");
        write_abort_secret(&dir, "note1", &secret).unwrap();
        let got = read_abort_secret(&dir, "note1").unwrap();
        assert_eq!(got, secret);
        let disk = std::fs::read_to_string(abort_path(&dir, "note1")).unwrap();
        assert!(!disk.contains(&hex_encode(&abort_commit(&secret))));
    }

    #[test]
    fn nums_pool_is_p2tr_and_key_path_has_no_secret() {
        let spk = nums_pool_spk().unwrap();
        assert!(crate::btc_tx::is_p2tr(&spk), "{}", hex::encode(&spk));
        let addr = nums_pool_address().unwrap();
        assert!(addr.starts_with("tb1p"), "{addr}");
        let (x, p) = crate::btc_tx::nums_internal_from_tag(NUMS_TAG);
        assert_eq!(x, nums_internal_xonly());
        let _ = p;
        let mut secret = [0u8; 32];
        secret[31] = 1;
        let msg = [9u8; 32];
        let sig = crate::btc_tx::bip340_sign(&secret, &msg).unwrap();
        let (out_x, _) = nums_output_key().unwrap();
        assert!(
            crate::btc_tx::bip340_verify(&out_x, &msg, &sig).is_err(),
            "random key must not verify against NUMS output key"
        );
    }

    #[test]
    fn kickoff_program_accepts_real_leaf_rejects_fake() {
        let dest = dest_h160_from_address("n1fH7uhaDauQYfuwG6t1aTncvyuBSZtDEY").unwrap();
        let abort = sha256_32(b"abort");
        let commit = abort_commit(&abort);
        let leaf = withdraw_leaf("note-1", &dest, 20_000);
        let (root, path) = crate::evm_spv::build_tree(&[leaf], 0).unwrap();
        let mut root_b = [0u8; 32];
        root_b.copy_from_slice(&hex::decode(&root).unwrap());
        let ws = claim_witness_script(&commit, &dest, 6).unwrap();
        let spk = p2wsh_spk(&ws);
        kickoff_valid("note-1", &dest, 20_000, &root_b, &path, 0, &spk, &commit, 6).unwrap();

        let thief = dest_h160_from_address("n1BkijgCanBDsDvFjjwpaM7fX8UgBx8w5A").unwrap();
        let err = kickoff_valid("note-1", &thief, 20_000, &root_b, &path, 0, &spk, &commit, 6)
            .unwrap_err();
        assert!(err.contains("claim_root") || err.contains("dest"), "{err}");

        let bad_spk = vec![0x00, 0x14];
        let err2 = kickoff_valid("note-1", &dest, 20_000, &root_b, &path, 0, &bad_spk, &commit, 6)
            .unwrap_err();
        assert!(err2.contains("dest-locked"), "{err2}");
    }

    #[test]
    fn bitvm_sha_step_challenge_catches_lie() {
        let dest = dest_h160_from_address("n1fH7uhaDauQYfuwG6t1aTncvyuBSZtDEY").unwrap();
        let mut steps = kickoff_trace("note-1", &dest, 20_000);
        challenge_sha_step(&steps[0]).unwrap();
        steps[0].digest[0] ^= 0xff;
        assert!(challenge_sha_step(&steps[0]).unwrap_err().contains("challenge wins"));
    }

    fn dest_secret() -> [u8; 32] {
        crate::btc_tx::sha256_once(b"fpl-nums-dest-test-key")
    }

    fn dest_p2wpkh() -> String {
        let pk = crate::btc_tx::compressed_pubkey_from_secret(&dest_secret()).unwrap();
        crate::vault::p2wpkh_testnet_address(&pk).unwrap()
    }

    #[test]
    fn nums_kickoff_dest_lock_no_frost_and_reject_thief() {
        let dest = dest_p2wpkh();
        let from = "dave";
        let amount = 20_000u64;
        let fee = 1_000u64;
        let sats = 80_000u64;
        let txid = "aa".repeat(32);
        let seed = sha256_32(b"fpl-live-abort-seed");
        let raw = nums_dest_lock_kickoff(
            &txid,
            0,
            sats,
            &dest,
            &dest_secret(),
            &watch_secret_for_tests(),
            amount,
            fee,
            from,
            &seed,
        )
        .unwrap();
        let bytes = hex::decode(&raw).unwrap();
        let vault = nums_pool_spk().unwrap();
        nums_kickoff_structure_ok(&bytes, &vault).unwrap();
        let parsed = crate::btc_spv::parse_tx(&bytes).unwrap();
        let wits = crate::btc_spv::extract_witnesses(&bytes).unwrap();
        assert!(is_script_path_witness(&wits[0]));
        assert!(!is_keypath_witness(&wits[0]));
        let dest_h160 = dest_h160_from_address(&dest).unwrap();
        let note_id = crate::bridge::btc_withdraw_note_id(from, amount, &dest);
        let abort = abort_preimage_from_seed(&seed, &note_id);
        let commit = abort_commit(&abort);
        let ws = claim_witness_script(&commit, &dest_h160, CLAIM_CSV_BLOCKS).unwrap();
        assert_eq!(parsed.outputs[0].1, p2wsh_spk(&ws));
        assert_eq!(parsed.outputs[0].0, amount);
        let leaf = withdraw_leaf(&note_id, &dest_h160, amount);
        let (root, path) = crate::evm_spv::build_tree(&[leaf], 0).unwrap();
        let mut root_b = [0u8; 32];
        root_b.copy_from_slice(&hex::decode(&root).unwrap());
        kickoff_valid(
            &note_id,
            &dest_h160,
            amount,
            &root_b,
            &path,
            0,
            &parsed.outputs[0].1,
            &commit,
            CLAIM_CSV_BLOCKS,
        )
        .unwrap();
        let thief = dest_h160_from_address("n1BkijgCanBDsDvFjjwpaM7fX8UgBx8w5A").unwrap();
        let err = kickoff_valid(
            &note_id,
            &thief,
            amount,
            &root_b,
            &path,
            0,
            &parsed.outputs[0].1,
            &commit,
            CLAIM_CSV_BLOCKS,
        )
        .unwrap_err();
        assert!(err.contains("claim_root") || err.contains("dest"), "{err}");
    }

    #[test]
    fn nums_disprove_builds_when_sha_step_lies() {
        let dest = dest_h160_from_address(&dest_p2wpkh()).unwrap();
        let mut steps = kickoff_trace("note-1", &dest, 20_000);
        challenge_sha_step(&steps[0]).unwrap();
        steps[0].digest[0] ^= 0xff;
        let raw = nums_disprove_tx(&"cc".repeat(32), 0, 80_000, 1_000, &steps[0]).unwrap();
        let bytes = hex::decode(&raw).unwrap();
        let parsed = crate::btc_spv::parse_tx(&bytes).unwrap();
        assert_eq!(parsed.outputs[0].1, nums_pool_spk().unwrap());
        let wits = crate::btc_spv::extract_witnesses(&bytes).unwrap();
        assert!(is_script_path_witness(&wits[0]));
    }

    #[test]
    fn v1_control_block_matches_bitcoin_lift_x() {
        let (ix, aff) = crate::btc_tx::nums_internal_from_tag(NUMS_TAG);
        let even = crate::btc_tx::lift_x(&ix).expect("lift");
        let root = nums_script_root_v1();
        let (q_ours, p_ours) = crate::btc_tx::taproot_tweak(&aff, Some(&root)).unwrap();
        let (q_btc, p_btc) = crate::btc_tx::taproot_tweak(&even, Some(&root)).unwrap();
        eprintln!(
            "ours {} par={p_ours} btc {} par={p_btc} same_q={}",
            hex::encode(q_ours),
            hex::encode(q_btc),
            q_ours == q_btc
        );
        let ctrl = kickoff_control_block_v1().unwrap();
        assert_eq!(&ctrl[1..33], ix.as_slice());
        assert_eq!(ctrl[0] & 0xfe, 0xc0);
        assert_eq!(&ctrl[33..], crate::btc_tx::tapleaf_hash(&disprove_tapscript()));
        assert_eq!(q_ours, q_btc, "odd-Y internal makes BIP341 spend miss the address");
        assert_eq!(ctrl[0] & 1, p_btc);
    }

    #[test]
    fn v2_pool_is_not_v1_and_migrate_pays_v2() {
        let v1 = nums_pool_spk_v1().unwrap();
        let v2 = nums_pool_spk().unwrap();
        assert_ne!(v1, v2);
        assert!(crate::btc_tx::is_p2tr(&v1));
        assert!(crate::btc_tx::is_p2tr(&v2));
        let addr_v1 = nums_pool_address_v1().unwrap();
        assert!(addr_v1.starts_with("tb1p"));
        // Odd-Y historical receive address is unspendable; not this even-Y v1.
        let sk = dest_secret();
        let raw = nums_migrate_v1_to_v2(&"dd".repeat(32), 0, 141_000, &sk, 1_000).unwrap();
        let parsed = crate::btc_spv::parse_tx(&hex::decode(&raw).unwrap()).unwrap();
        assert_eq!(parsed.outputs[0].1, v2);
        assert_eq!(parsed.outputs[0].0, 140_000);
        let wits = crate::btc_spv::extract_witnesses(&hex::decode(&raw).unwrap()).unwrap();
        assert_eq!(wits[0][wits[0].len() - 2], kickoff_tapscript_v1());
        assert!(nums_kickoff_structure_ok(&hex::decode(&raw).unwrap(), &v2).is_err());
    }

    #[test]
    fn v2_kickoff_witness_binds_watcher() {
        let dest = dest_p2wpkh();
        let raw = nums_dest_lock_kickoff(
            &"aa".repeat(32),
            0,
            80_000,
            &dest,
            &dest_secret(),
            &watch_secret_for_tests(),
            20_000,
            1_000,
            "dave",
            &sha256_32(b"fpl-live-abort-seed"),
        )
        .unwrap();
        let bytes = hex::decode(&raw).unwrap();
        let wits = crate::btc_spv::extract_witnesses(&bytes).unwrap();
        assert_eq!(wits[0][wits[0].len() - 2], kickoff_tapscript());
        assert_eq!(wits[0][2].len(), 64);
        nums_kickoff_structure_ok(&bytes, &nums_pool_spk().unwrap()).unwrap();
    }
}
