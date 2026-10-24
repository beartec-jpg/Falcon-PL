//! Dealerless FROST (secp256k1 Taproot) for the 2300 BTC vault.
//!
//! Combined **signature** exists. Combined **secret** does not. Each seat
//! keeps only `keys/vault-frost-share-<id>` (mode 600). DKG round secrets are
//! wiped after part3.
//!
//! Shamir `combine_secret` is sweep-only for the existing P2WPKH. New pool
//! spends are P2TR key-path signed with this module.

use crate::btc_tx::{
    assemble_segwit_tx, bip341_keypath_sighash, p2tr_spk, TxIn, TxOut, SIGHASH_DEFAULT,
};
use crate::vault::{p2tr_testnet_address, WithdrawPlan, DUST_SATS};
use frost_secp256k1_tr as frost;
use frost_secp256k1_tr::keys::{KeyPackage, PublicKeyPackage, Tweak};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SCHEME: &str = "frost-secp256k1-tr";

fn ferr(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn seat_identifier(seats: &[String], seat: &str) -> Result<frost::Identifier, String> {
    let idx = seats
        .iter()
        .position(|s| s == seat)
        .ok_or_else(|| format!("seat {seat} not in list"))?;
    let n = u16::try_from(idx + 1).map_err(|_| "too many seats".to_string())?;
    frost::Identifier::try_from(n).map_err(ferr)
}

fn write_600(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(ferr)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(ferr)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(tmp, path).map_err(ferr)
}

fn write_644(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(ferr)?;
    }
    std::fs::write(path, bytes).map_err(ferr)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644));
    }
    Ok(())
}

fn json_to<T: Serialize>(v: &T) -> Result<String, String> {
    serde_json::to_string(v).map_err(ferr)
}

fn json_from<T: for<'de> Deserialize<'de>>(s: &str) -> Result<T, String> {
    serde_json::from_str(s).map_err(ferr)
}

pub fn r1_secret_path(keys_dir: &Path, seat: &str) -> PathBuf {
    keys_dir.join(format!("frost-dkg-r1-secret-{seat}"))
}
pub fn r1_pkg_path(keys_dir: &Path, seat: &str) -> PathBuf {
    keys_dir.join(format!("frost-dkg-r1-{seat}"))
}
pub fn r2_secret_path(keys_dir: &Path, seat: &str) -> PathBuf {
    keys_dir.join(format!("frost-dkg-r2-secret-{seat}"))
}
pub fn r2_pkg_path(keys_dir: &Path, from: &str, to: &str) -> PathBuf {
    keys_dir.join(format!("frost-dkg-r2-{from}-to-{to}"))
}
pub fn share_path(keys_dir: &Path, seat: &str) -> PathBuf {
    keys_dir.join(format!("vault-frost-share-{seat}"))
}
pub fn pubkey_path(keys_dir: &Path) -> PathBuf {
    keys_dir.join("vault-frost-pub.json")
}

pub fn dkg_part1(
    seats: &[String],
    seat: &str,
    t: u16,
    keys_dir: &Path,
) -> Result<PathBuf, String> {
    let n = u16::try_from(seats.len()).map_err(|_| "n".to_string())?;
    if t < 2 || t > n {
        return Err("bad t".into());
    }
    let id = seat_identifier(seats, seat)?;
    let mut rng = OsRng;
    let (secret, pkg) =
        frost::keys::dkg::part1(id, n, t, &mut rng).map_err(ferr)?;
    write_600(&r1_secret_path(keys_dir, seat), json_to(&secret)?.as_bytes())?;
    let pub_path = r1_pkg_path(keys_dir, seat);
    write_644(&pub_path, json_to(&pkg)?.as_bytes())?;
    Ok(pub_path)
}

pub fn dkg_part2(seats: &[String], seat: &str, keys_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let secret_s = std::fs::read_to_string(r1_secret_path(keys_dir, seat)).map_err(ferr)?;
    let secret: frost::keys::dkg::round1::SecretPackage = json_from(&secret_s)?;
    let mut round1 = BTreeMap::new();
    for other in seats {
        if other == seat {
            continue;
        }
        let s = std::fs::read_to_string(r1_pkg_path(keys_dir, other)).map_err(|e| {
            format!("missing r1 package for {other}: {e}")
        })?;
        let pkg: frost::keys::dkg::round1::Package = json_from(&s)?;
        round1.insert(seat_identifier(seats, other)?, pkg);
    }
    let (r2_secret, r2_pkgs) = frost::keys::dkg::part2(secret, &round1).map_err(ferr)?;
    write_600(&r2_secret_path(keys_dir, seat), json_to(&r2_secret)?.as_bytes())?;
    let _ = std::fs::remove_file(r1_secret_path(keys_dir, seat));
    let mut out = Vec::new();
    for other in seats {
        if other == seat {
            continue;
        }
        let oid = seat_identifier(seats, other)?;
        let pkg = r2_pkgs
            .get(&oid)
            .ok_or_else(|| format!("no r2 package for {other}"))?;
        let p = r2_pkg_path(keys_dir, seat, other);
        write_644(&p, json_to(pkg)?.as_bytes())?;
        out.push(p);
    }
    Ok(out)
}

pub fn dkg_part3(seats: &[String], seat: &str, keys_dir: &Path) -> Result<FrostPub, String> {
    let r2s = std::fs::read_to_string(r2_secret_path(keys_dir, seat)).map_err(ferr)?;
    let r2_secret: frost::keys::dkg::round2::SecretPackage = json_from(&r2s)?;
    let mut round1 = BTreeMap::new();
    let mut round2 = BTreeMap::new();
    for other in seats {
        if other == seat {
            continue;
        }
        let s1 = std::fs::read_to_string(r1_pkg_path(keys_dir, other)).map_err(ferr)?;
        round1.insert(
            seat_identifier(seats, other)?,
            json_from::<frost::keys::dkg::round1::Package>(&s1)?,
        );
        let s2 = std::fs::read_to_string(r2_pkg_path(keys_dir, other, seat)).map_err(|e| {
            format!("missing r2 package {other}→{seat}: {e}")
        })?;
        round2.insert(
            seat_identifier(seats, other)?,
            json_from::<frost::keys::dkg::round2::Package>(&s2)?,
        );
    }
    let (key_pkg, pub_pkg) =
        frost::keys::dkg::part3(&r2_secret, &round1, &round2).map_err(ferr)?;
    write_600(&share_path(keys_dir, seat), json_to(&key_pkg)?.as_bytes())?;
    let _ = std::fs::remove_file(r2_secret_path(keys_dir, seat));
    let frost_pub = frost_pub_from_package(&pub_pkg, seats.len() as u32, crate::vault::threshold(seats.len() as u32))?;
    write_644(
        &pubkey_path(keys_dir),
        (serde_json::to_string_pretty(&frost_pub).map_err(ferr)? + "\n").as_bytes(),
    )?;
    Ok(frost_pub)
}

pub fn read_key_package(keys_dir: &Path, seat: &str) -> Result<KeyPackage, String> {
    let s = std::fs::read_to_string(share_path(keys_dir, seat)).map_err(ferr)?;
    json_from(&s)
}

pub fn read_pubkey_package(keys_dir: &Path) -> Result<PublicKeyPackage, String> {
    let s = std::fs::read_to_string(pubkey_path(keys_dir)).map_err(ferr)?;
    let fp: FrostPub = json_from(&s)?;
    json_from(&fp.pkg_json)
}

/// Watch-script identity for RailDeposit. Public only.
pub fn as_vault_pub(fp: &FrostPub) -> crate::vault::VaultPub {
    crate::vault::VaultPub {
        epoch: fp.epoch,
        n: fp.n,
        t: fp.t,
        pubkey_hex: fp.tweaked_xonly_hex.clone(),
        spk_hex: fp.spk_hex.clone(),
    }
}

pub fn load_vault_pub(keys_dir: &Path) -> Result<crate::vault::VaultPub, String> {
    let s = std::fs::read_to_string(pubkey_path(keys_dir)).map_err(ferr)?;
    let fp: FrostPub = json_from(&s)?;
    Ok(as_vault_pub(&fp))
}

pub fn load_frost_pub(keys_dir: &Path) -> Result<FrostPub, String> {
    let s = std::fs::read_to_string(pubkey_path(keys_dir)).map_err(ferr)?;
    json_from(&s)
}

/// Public vault identity. Never includes a signing share.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrostPub {
    pub epoch: u64,
    pub n: u32,
    pub t: u32,
    pub scheme: String,
    pub internal_pubkey_hex: String,
    pub tweaked_xonly_hex: String,
    pub spk_hex: String,
    pub address: String,
    /// JSON of frost PublicKeyPackage (verifying keys only).
    pub pkg_json: String,
}

pub fn verifying_key_bytes(vk: &frost::VerifyingKey) -> Result<Vec<u8>, String> {
    vk.serialize().map_err(ferr)
}

pub fn xonly32(vk: &frost::VerifyingKey) -> Result<[u8; 32], String> {
    let b = verifying_key_bytes(vk)?;
    let slice = if b.len() == 32 {
        &b[..]
    } else if b.len() == 33 && (b[0] == 0x02 || b[0] == 0x03) {
        &b[1..]
    } else {
        return Err(format!("unexpected frost verifying key len {}", b.len()));
    };
    let mut o = [0u8; 32];
    o.copy_from_slice(slice);
    Ok(o)
}

pub fn frost_pub_from_package(
    pkg: &PublicKeyPackage,
    n: u32,
    t: u32,
) -> Result<FrostPub, String> {
    let internal = xonly32(pkg.verifying_key())?;
    let tweaked = pkg.clone().tweak(None::<&[u8]>);
    let xonly = xonly32(tweaked.verifying_key())?;
    let spk = p2tr_spk(&xonly)?;
    let addr = p2tr_testnet_address(&xonly)?;
    Ok(FrostPub {
        epoch: 0,
        n,
        t,
        scheme: SCHEME.to_string(),
        internal_pubkey_hex: hex::encode(internal),
        tweaked_xonly_hex: hex::encode(xonly),
        spk_hex: hex::encode(spk),
        address: addr,
        pkg_json: json_to(pkg)?,
    })
}

/// In-process dealerless DKG. For tests and an air-gapped ceremony machine
/// that is **wiped before** the address is published. Live seats must run
/// `dkg_part1/2/3` so no host ever holds `t` key packages.
pub fn dkg_simulate(
    seats: &[String],
    t: u16,
) -> Result<(BTreeMap<String, KeyPackage>, PublicKeyPackage), String> {
    let n = u16::try_from(seats.len()).map_err(|_| "n".to_string())?;
    let mut rng = OsRng;
    let mut r1_secret = BTreeMap::new();
    let mut r1_recv: BTreeMap<frost::Identifier, BTreeMap<frost::Identifier, frost::keys::dkg::round1::Package>> =
        BTreeMap::new();
    for seat in seats {
        let id = seat_identifier(seats, seat)?;
        let (sec, pkg) = frost::keys::dkg::part1(id, n, t, &mut rng).map_err(ferr)?;
        r1_secret.insert(id, sec);
        for other in seats {
            if other == seat {
                continue;
            }
            let oid = seat_identifier(seats, other)?;
            r1_recv.entry(oid).or_default().insert(id, pkg.clone());
        }
    }
    let mut r2_secret = BTreeMap::new();
    let mut r2_recv: BTreeMap<frost::Identifier, BTreeMap<frost::Identifier, frost::keys::dkg::round2::Package>> =
        BTreeMap::new();
    for seat in seats {
        let id = seat_identifier(seats, seat)?;
        let sec = r1_secret.remove(&id).ok_or("r1 secret")?;
        let (sec2, pkgs) = frost::keys::dkg::part2(sec, &r1_recv[&id]).map_err(ferr)?;
        r2_secret.insert(id, sec2);
        for (rid, pkg) in pkgs {
            r2_recv.entry(rid).or_default().insert(id, pkg);
        }
    }
    let mut keys = BTreeMap::new();
    let mut pub_pkg = None;
    for seat in seats {
        let id = seat_identifier(seats, seat)?;
        let (kp, pp) =
            frost::keys::dkg::part3(&r2_secret[&id], &r1_recv[&id], &r2_recv[&id]).map_err(ferr)?;
        keys.insert(seat.clone(), kp);
        pub_pkg = Some(pp);
    }
    Ok((keys, pub_pkg.ok_or("dkg empty")?))
}

pub fn nonce_path(keys_dir: &Path, seat: &str) -> PathBuf {
    keys_dir.join(format!("frost-sign-nonce-{seat}"))
}
pub fn commit_path(keys_dir: &Path, seat: &str) -> PathBuf {
    keys_dir.join(format!("frost-sign-commit-{seat}"))
}
pub fn sigshare_path(keys_dir: &Path, seat: &str) -> PathBuf {
    keys_dir.join(format!("frost-sign-share-{seat}"))
}

/// Round 1 of a Kickoff signature. Nonces stay on this seat (600).
pub fn sign_round1(keys_dir: &Path, seat: &str) -> Result<PathBuf, String> {
    let kp = read_key_package(keys_dir, seat)?;
    let tk = kp.tweak(None::<&[u8]>);
    let mut rng = OsRng;
    let (nonces, commits) = frost::round1::commit(tk.signing_share(), &mut rng);
    write_600(&nonce_path(keys_dir, seat), json_to(&nonces)?.as_bytes())?;
    let p = commit_path(keys_dir, seat);
    write_644(&p, json_to(&commits)?.as_bytes())?;
    Ok(p)
}

/// Coordinator: build the signing package from ≥ t public commitments.
pub fn make_signing_package(
    seats: &[String],
    committers: &[String],
    commits_dir: &Path,
    message: &[u8],
) -> Result<frost::SigningPackage, String> {
    let mut map = BTreeMap::new();
    for seat in committers {
        let s = std::fs::read_to_string(commit_path(commits_dir, seat)).map_err(ferr)?;
        let c: frost::round1::SigningCommitments = json_from(&s)?;
        map.insert(seat_identifier(seats, seat)?, c);
    }
    Ok(frost::SigningPackage::new(map, message))
}

/// Round 2. Consumes this seat's nonces. Emits a signature share (not the key).
pub fn sign_round2(
    keys_dir: &Path,
    _seats: &[String],
    seat: &str,
    signing: &frost::SigningPackage,
) -> Result<PathBuf, String> {
    let kp = read_key_package(keys_dir, seat)?;
    let tk = kp.tweak(None::<&[u8]>);
    let ns = std::fs::read_to_string(nonce_path(keys_dir, seat)).map_err(ferr)?;
    let nonces: frost::round1::SigningNonces = json_from(&ns)?;
    let sh = frost::round2::sign(signing, &nonces, &tk).map_err(ferr)?;
    let _ = std::fs::remove_file(nonce_path(keys_dir, seat));
    let p = sigshare_path(keys_dir, seat);
    write_644(&p, json_to(&sh)?.as_bytes())?;
    Ok(p)
}

/// Aggregate signature shares + public package. Coordinator holds no KeyPackage.
pub fn aggregate_shares(
    seats: &[String],
    signers: &[String],
    shares_dir: &Path,
    pub_pkg: &PublicKeyPackage,
    signing: &frost::SigningPackage,
) -> Result<Vec<u8>, String> {
    let tweaked_pub = pub_pkg.clone().tweak(None::<&[u8]>);
    let mut shares = BTreeMap::new();
    for seat in signers {
        let s = std::fs::read_to_string(sigshare_path(shares_dir, seat)).map_err(ferr)?;
        let sh: frost::round2::SignatureShare = json_from(&s)?;
        shares.insert(seat_identifier(seats, seat)?, sh);
    }
    let sig = frost::aggregate(signing, &shares, &tweaked_pub).map_err(ferr)?;
    sig.serialize().map_err(ferr)
}

/// FROST-sign `message` with exactly the provided key packages (must be ≥ t).
/// Tweaked for BIP341 key-path (no script tree).
pub fn frost_sign_tweaked(
    signers: &[(String, KeyPackage)],
    seats: &[String],
    pub_pkg: &PublicKeyPackage,
    message: &[u8],
) -> Result<Vec<u8>, String> {
    if signers.is_empty() {
        return Err("no signers".into());
    }
    let mut rng = OsRng;
    let tweaked_pub = pub_pkg.clone().tweak(None::<&[u8]>);
    let mut nonces = BTreeMap::new();
    let mut commits = BTreeMap::new();
    let mut tweaked_keys = BTreeMap::new();
    for (seat, kp) in signers {
        let id = seat_identifier(seats, seat)?;
        let tk = kp.clone().tweak(None::<&[u8]>);
        let (n, c) = frost::round1::commit(tk.signing_share(), &mut rng);
        nonces.insert(id, n);
        commits.insert(id, c);
        tweaked_keys.insert(id, tk);
    }
    let signing = frost::SigningPackage::new(commits, message);
    let mut shares = BTreeMap::new();
    for (id, n) in &nonces {
        let kp = tweaked_keys.get(id).ok_or("missing tweaked key")?;
        let sh = frost::round2::sign(&signing, n, kp).map_err(ferr)?;
        shares.insert(*id, sh);
    }
    let sig = frost::aggregate(&signing, &shares, &tweaked_pub).map_err(ferr)?;
    let bytes = sig.serialize().map_err(ferr)?;
    Ok(bytes)
}

/// Pool P2TR → vout0 dest-lock P2WSH + change-to-pool. No combine_secret.
pub fn sign_kickoff_tx_frost(
    signers: &[(String, KeyPackage)],
    seats: &[String],
    pub_pkg: &PublicKeyPackage,
    plan: &WithdrawPlan,
    vault_spk_hex: &str,
    claim_spk: &[u8],
) -> Result<String, String> {
    if claim_spk.is_empty() {
        return Err("kickoff needs dest-lock spk".into());
    }
    let vault_spk = hex::decode(vault_spk_hex).map_err(ferr)?;
    if !crate::btc_tx::is_p2tr(&vault_spk) {
        return Err("FROST kickoff is P2TR key-path; Shamir P2WPKH is sweep-only".into());
    }
    let mut inputs = Vec::new();
    for u in &plan.inputs {
        inputs.push(TxIn {
            txid_display: u.txid.clone(),
            vout: u.vout,
            sats: u.sats,
            sequence: 0xffff_fffe,
            script_pubkey: vault_spk.clone(),
        });
    }
    let mut outputs = vec![TxOut {
        sats: plan.amount,
        script_pubkey: claim_spk.to_vec(),
    }];
    if plan.change >= DUST_SATS {
        outputs.push(TxOut {
            sats: plan.change,
            script_pubkey: vault_spk.clone(),
        });
    }
    let mut witnesses = Vec::new();
    for i in 0..inputs.len() {
        let pre = bip341_keypath_sighash(2, 0, &inputs, &outputs, i, SIGHASH_DEFAULT)?;
        let sig = frost_sign_tweaked(signers, seats, pub_pkg, &pre)?;
        if sig.len() != 64 {
            return Err(format!("schnorr sig must be 64 bytes, got {}", sig.len()));
        }
        witnesses.push(vec![sig]);
    }
    let raw = assemble_segwit_tx(2, 0, &inputs, &outputs, &witnesses)?;
    Ok(hex::encode(raw))
}

pub fn signed_dest_lock_kickoff_frost(
    signers: &[(String, KeyPackage)],
    seats: &[String],
    pub_pkg: &PublicKeyPackage,
    utxos: &[crate::vault::VaultUtxo],
    dest: &str,
    amount: u64,
    fee: u64,
    vault_spk_hex: &str,
    note_id: &str,
) -> Result<String, String> {
    let plan = crate::vault::coin_select(utxos, amount, dest, fee)?;
    let abort = crate::bitvm::abort_preimage_from_seed(
        &crate::bitvm::sha256_32(b"fpl-test-abort-seed"),
        note_id,
    );
    let dest_h160 = crate::bitvm::dest_h160_from_address(dest)?;
    let commit = crate::bitvm::abort_commit(&abort);
    let ws = crate::bitvm::claim_witness_script(
        &commit,
        &dest_h160,
        crate::bitvm::CLAIM_CSV_BLOCKS,
    )?;
    let claim_spk = crate::bitvm::p2wsh_spk(&ws);
    sign_kickoff_tx_frost(
        signers,
        seats,
        pub_pkg,
        &plan,
        vault_spk_hex,
        &claim_spk,
    )
}

/// Unsigned P2TR key-path pay. Returns BIP341 sighash (input 0) + output layout.
pub fn keypath_pay_sighash(
    utxo_txid: &str,
    vout: u32,
    sats: u64,
    dest_spk: &[u8],
    amount: u64,
    fee: u64,
    vault_spk: &[u8],
) -> Result<([u8; 32], Vec<TxOut>), String> {
    if !crate::btc_tx::is_p2tr(vault_spk) {
        return Err("keypath pay needs P2TR vault".into());
    }
    if amount + fee > sats {
        return Err("amount+fee exceeds utxo".into());
    }
    let change = sats - amount - fee;
    let mut outputs = vec![TxOut {
        sats: amount,
        script_pubkey: dest_spk.to_vec(),
    }];
    if change >= DUST_SATS {
        outputs.push(TxOut {
            sats: change,
            script_pubkey: vault_spk.to_vec(),
        });
    }
    let inputs = [TxIn {
        txid_display: utxo_txid.to_string(),
        vout,
        sats,
        sequence: 0xffff_fffe,
        script_pubkey: vault_spk.to_vec(),
    }];
    let pre = bip341_keypath_sighash(2, 0, &inputs, &outputs, 0, SIGHASH_DEFAULT)?;
    Ok((pre, outputs))
}

pub fn keypath_pay_finish(
    utxo_txid: &str,
    vout: u32,
    sats: u64,
    outputs: &[TxOut],
    vault_spk: &[u8],
    sig64: &[u8],
) -> Result<String, String> {
    if sig64.len() != 64 {
        return Err("schnorr sig must be 64 bytes".into());
    }
    let inputs = [TxIn {
        txid_display: utxo_txid.to_string(),
        vout,
        sats,
        sequence: 0xffff_fffe,
        script_pubkey: vault_spk.to_vec(),
    }];
    let raw = assemble_segwit_tx(2, 0, &inputs, outputs, &[vec![sig64.to_vec()]])?;
    Ok(hex::encode(raw))
}

/// Unsigned dest-lock Kickoff (P2TR key-path). Coordinator holds no share.
pub struct KickoffUnsigned {
    pub sighash: [u8; 32],
    pub outputs: Vec<TxOut>,
    pub claim_spk: Vec<u8>,
    pub witness_script: Vec<u8>,
    pub note_id: String,
    pub amount: u64,
    pub change: u64,
    pub fee: u64,
}

pub fn kickoff_unsigned(
    utxo_txid: &str,
    vout: u32,
    sats: u64,
    dest: &str,
    amount: u64,
    fee: u64,
    vault_spk: &[u8],
    from: &str,
    abort_seed: &[u8; 32],
) -> Result<KickoffUnsigned, String> {
    if !crate::btc_tx::is_p2tr(vault_spk) {
        return Err("kickoff needs P2TR vault".into());
    }
    let utxos = [crate::vault::VaultUtxo {
        txid: utxo_txid.to_string(),
        vout,
        sats,
        height: 0,
    }];
    let plan = crate::vault::coin_select(&utxos, amount, dest, fee)?;
    let note_id = crate::bridge::btc_withdraw_note_id(from, amount, dest);
    let abort = crate::bitvm::abort_preimage_from_seed(abort_seed, &note_id);
    let dest_h160 = crate::bitvm::dest_h160_from_address(dest)?;
    let commit = crate::bitvm::abort_commit(&abort);
    let ws = crate::bitvm::claim_witness_script(
        &commit,
        &dest_h160,
        crate::bitvm::CLAIM_CSV_BLOCKS,
    )?;
    let claim_spk = crate::bitvm::p2wsh_spk(&ws);
    let mut inputs = Vec::new();
    for u in &plan.inputs {
        inputs.push(TxIn {
            txid_display: u.txid.clone(),
            vout: u.vout,
            sats: u.sats,
            sequence: 0xffff_fffe,
            script_pubkey: vault_spk.to_vec(),
        });
    }
    let mut outputs = vec![TxOut {
        sats: plan.amount,
        script_pubkey: claim_spk.clone(),
    }];
    if plan.change >= DUST_SATS {
        outputs.push(TxOut {
            sats: plan.change,
            script_pubkey: vault_spk.to_vec(),
        });
    }
    let pre = bip341_keypath_sighash(2, 0, &inputs, &outputs, 0, SIGHASH_DEFAULT)?;
    Ok(KickoffUnsigned {
        sighash: pre,
        outputs,
        claim_spk,
        witness_script: ws,
        note_id,
        amount: plan.amount,
        change: plan.change,
        fee: plan.fee,
    })
}

pub fn kickoff_finish(
    utxo_txid: &str,
    vout: u32,
    sats: u64,
    outputs: &[TxOut],
    vault_spk: &[u8],
    sig64: &[u8],
) -> Result<String, String> {
    keypath_pay_finish(utxo_txid, vout, sats, outputs, vault_spk, sig64)
}

/// Count frost share files on disk. Live gate: each host must have exactly one.
pub fn count_share_files(keys_dir: &Path) -> Result<usize, String> {
    if !keys_dir.is_dir() {
        return Ok(0);
    }
    let mut n = 0usize;
    for e in std::fs::read_dir(keys_dir).map_err(ferr)? {
        let e = e.map_err(ferr)?;
        let name = e.file_name();
        let s = name.to_string_lossy();
        if s.starts_with("vault-frost-share-") || s.starts_with("vault-share-") {
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitvm::{kickoff_valid, p2wsh_spk};
    use crate::vault::{coin_select, VaultUtxo};

    fn seats6() -> Vec<String> {
        (1..=6).map(|i| format!("v{i}")).collect()
    }

    fn utxo(txid: &str, sats: u64) -> VaultUtxo {
        VaultUtxo {
            txid: txid.into(),
            vout: 0,
            sats,
            height: 10,
        }
    }

    #[test]
    fn dkg_t_of_n_and_t_minus_one_cannot_sign() {
        let seats = seats6();
        let t = crate::vault::threshold(6) as u16;
        assert_eq!(t, 4);
        let (keys, pub_pkg) = dkg_simulate(&seats, t).unwrap();
        assert_eq!(keys.len(), 6);
        let fp = frost_pub_from_package(&pub_pkg, 6, t as u32).unwrap();
        assert!(fp.address.starts_with("tb1p"), "{}", fp.address);
        assert_eq!(fp.spk_hex.len(), 68); // 34 bytes hex
        assert!(!fp.pkg_json.to_ascii_lowercase().contains("signing_share")
            && !fp.pkg_json.contains("secret"));
        let msg = b"fpl-frost-test-msg";
        let four: Vec<(String, KeyPackage)> = seats[..4]
            .iter()
            .map(|s| (s.clone(), keys.get(s).unwrap().clone()))
            .collect();
        let sig = frost_sign_tweaked(&four, &seats, &pub_pkg, msg).unwrap();
        assert_eq!(sig.len(), 64);
        let three: Vec<(String, KeyPackage)> = seats[..3]
            .iter()
            .map(|s| (s.clone(), keys.get(s).unwrap().clone()))
            .collect();
        let err = frost_sign_tweaked(&three, &seats, &pub_pkg, msg);
        assert!(err.is_err(), "t-1 must not produce a valid group signature");
        // Public JSON must not contain a share scalar.
        let pub_s = serde_json::to_string(&fp).unwrap();
        for (_s, kp) in &four {
            let kj = serde_json::to_string(kp).unwrap();
            // the signing share bytes, if present, must not appear in FrostPub
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&kj) {
                if let Some(share) = v.get("signing_share") {
                    let dump = share.to_string();
                    if dump.len() > 8 {
                        assert!(
                            !pub_s.contains(&dump),
                            "FrostPub leaked a signing share"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn frost_kickoff_dest_lock_and_mutated_dest() {
        let seats = seats6();
        let t = 4u16;
        let (keys, pub_pkg) = dkg_simulate(&seats, t).unwrap();
        let fp = frost_pub_from_package(&pub_pkg, 6, 4).unwrap();
        let signers: Vec<(String, KeyPackage)> = seats[..4]
            .iter()
            .map(|s| (s.clone(), keys.get(s).unwrap().clone()))
            .collect();
        let dest = "n1fH7uhaDauQYfuwG6t1aTncvyuBSZtDEY";
        let vault = vec![utxo(&"aa".repeat(32), 80_000)];
        let hex = signed_dest_lock_kickoff_frost(
            &signers,
            &seats,
            &pub_pkg,
            &vault,
            dest,
            50_000,
            1_000,
            &fp.spk_hex,
            "note-frost-1",
        )
        .unwrap();
        let raw = hex::decode(&hex).unwrap();
        let parsed = crate::btc_spv::parse_tx(&raw).unwrap();
        assert_eq!(parsed.outputs[0].0, 50_000);
        let abort = crate::bitvm::abort_preimage_from_seed(
            &crate::bitvm::sha256_32(b"fpl-test-abort-seed"),
            "note-frost-1",
        );
        let dest_h160 = crate::bitvm::dest_h160_from_address(dest).unwrap();
        let commit = crate::bitvm::abort_commit(&abort);
        let ws = crate::bitvm::claim_witness_script(
            &commit,
            &dest_h160,
            crate::bitvm::CLAIM_CSV_BLOCKS,
        )
        .unwrap();
        let claim_spk = p2wsh_spk(&ws);
        assert_eq!(parsed.outputs[0].1, claim_spk);
        let leaf = crate::bitvm::withdraw_leaf("note-frost-1", &dest_h160, 50_000);
        let (root, path) = crate::evm_spv::build_tree(&[leaf], 0).unwrap();
        let mut root_b = [0u8; 32];
        root_b.copy_from_slice(&hex::decode(&root).unwrap());
        kickoff_valid(
            "note-frost-1",
            &dest_h160,
            50_000,
            &root_b,
            &path,
            0,
            &claim_spk,
            &commit,
            crate::bitvm::CLAIM_CSV_BLOCKS,
        )
        .unwrap();
        let thief = crate::bitvm::dest_h160_from_address("n1BkijgCanBDsDvFjjwpaM7fX8UgBx8w5A").unwrap();
        let err = kickoff_valid(
            "note-frost-1",
            &thief,
            50_000,
            &root_b,
            &path,
            0,
            &claim_spk,
            &commit,
            crate::bitvm::CLAIM_CSV_BLOCKS,
        );
        assert!(err.is_err(), "mutated dest must fail kickoff_valid");
        let _ = coin_select(&vault, 50_000, dest, 1_000).unwrap();
    }

    #[test]
    fn kickoff_unsigned_uses_dest_note_id_not_height() {
        let seats = seats6();
        let (keys, pub_pkg) = dkg_simulate(&seats, 4).unwrap();
        let fp = frost_pub_from_package(&pub_pkg, 6, 4).unwrap();
        let vault_spk = hex::decode(&fp.spk_hex).unwrap();
        let dest = "n1fH7uhaDauQYfuwG6t1aTncvyuBSZtDEY";
        let from = "dave";
        let amount = 50_000u64;
        let seed = crate::bitvm::sha256_32(b"fpl-live-abort-seed");
        let u = kickoff_unsigned(
            &"bb".repeat(32),
            0,
            80_000,
            dest,
            amount,
            1_000,
            &vault_spk,
            from,
            &seed,
        )
        .unwrap();
        let want_note = crate::bridge::btc_withdraw_note_id(from, amount, dest);
        assert_eq!(u.note_id, want_note);
        let dest_h160 = crate::bitvm::dest_h160_from_address(dest).unwrap();
        let abort = crate::bitvm::abort_preimage_from_seed(&seed, &want_note);
        let commit = crate::bitvm::abort_commit(&abort);
        let ws = crate::bitvm::claim_witness_script(
            &commit,
            &dest_h160,
            crate::bitvm::CLAIM_CSV_BLOCKS,
        )
        .unwrap();
        assert_eq!(u.claim_spk, p2wsh_spk(&ws));
        let signers: Vec<(String, KeyPackage)> = seats[..4]
            .iter()
            .map(|s| (s.clone(), keys.get(s).unwrap().clone()))
            .collect();
        let sig = frost_sign_tweaked(&signers, &seats, &pub_pkg, &u.sighash).unwrap();
        let raw = kickoff_finish(&"bb".repeat(32), 0, 80_000, &u.outputs, &vault_spk, &sig).unwrap();
        let parsed = crate::btc_spv::parse_tx(&hex::decode(&raw).unwrap()).unwrap();
        assert_eq!(parsed.outputs[0].1, u.claim_spk);
        assert_eq!(parsed.outputs[0].0, amount);
        let leaf = crate::bitvm::withdraw_leaf(&want_note, &dest_h160, amount);
        let (root, path) = crate::evm_spv::build_tree(&[leaf], 0).unwrap();
        let mut root_b = [0u8; 32];
        root_b.copy_from_slice(&hex::decode(&root).unwrap());
        kickoff_valid(
            &want_note,
            &dest_h160,
            amount,
            &root_b,
            &path,
            0,
            &u.claim_spk,
            &commit,
            crate::bitvm::CLAIM_CSV_BLOCKS,
        )
        .unwrap();
    }

    #[test]
    fn per_seat_files_never_write_all_shares() {
        let tmp = std::env::temp_dir().join(format!(
            "fpl-frost-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let seats = vec!["v1".into(), "v2".into(), "v3".into()];
        dkg_part1(&seats, "v1", 2, &tmp).unwrap();
        assert!(r1_secret_path(&tmp, "v1").exists());
        assert!(r1_pkg_path(&tmp, "v1").exists());
        assert!(!share_path(&tmp, "v2").exists());
        assert_eq!(count_share_files(&tmp).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn coordinator_never_holds_key_packages() {
        let seats = seats6();
        let t = 4u16;
        let (keys, pub_pkg) = dkg_simulate(&seats, t).unwrap();
        let base = std::env::temp_dir().join(format!(
            "fpl-frost-sign-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        // Each seat dir has only that seat's share.
        for s in &seats {
            let d = base.join(s);
            std::fs::create_dir_all(&d).unwrap();
            write_600(&share_path(&d, s), json_to(keys.get(s).unwrap()).unwrap().as_bytes()).unwrap();
        }
        let coord = base.join("coord");
        std::fs::create_dir_all(&coord).unwrap();
        let signers: Vec<String> = seats[..4].to_vec();
        for s in &signers {
            sign_round1(&base.join(s), s).unwrap();
            std::fs::copy(commit_path(&base.join(s), s), commit_path(&coord, s)).unwrap();
        }
        let msg = b"kickoff-sighash-placeholder";
        let pkg = make_signing_package(&seats, &signers, &coord, msg).unwrap();
        for s in &signers {
            sign_round2(&base.join(s), &seats, s, &pkg).unwrap();
            assert!(!nonce_path(&base.join(s), s).exists());
            std::fs::copy(sigshare_path(&base.join(s), s), sigshare_path(&coord, s)).unwrap();
            // coordinator dir must not contain a frost share
            assert!(!share_path(&coord, s).exists());
        }
        let sig = aggregate_shares(&seats, &signers, &coord, &pub_pkg, &pkg).unwrap();
        assert_eq!(sig.len(), 64);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn bip350_p2tr_roundtrip() {
        let x = [0x11u8; 32];
        let addr = p2tr_testnet_address(&x).unwrap();
        assert!(addr.starts_with("tb1p"), "{addr}");
        let (kind, spk) = crate::btc_tx::decode_address(&addr).unwrap();
        assert_eq!(kind, crate::btc_tx::AddrKind::P2tr);
        assert_eq!(spk, p2tr_spk(&x).unwrap());
        // BIP350 published vector
        let v = "tb1pqqqqp399et2xygdj5xreqhjjvcmzhxw4aywxecjdzew6hylgvsesf3hn0c";
        let (k, s) = crate::btc_tx::decode_address(v).unwrap();
        assert_eq!(k, crate::btc_tx::AddrKind::P2tr);
        assert_eq!(s.len(), 34);
        assert_eq!(s[0], 0x51);
    }
}
