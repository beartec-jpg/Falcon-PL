//! Bitcoin transaction build / BIP143 / BIP341 / address decode for the vault rail.
//!
//! Key-path FROST Schnorr lives in `frost_vault`. NUMS script-path Kickoff
//! (BIP340 dest CHECKSIG + BitVM SHA256 disprove) lives in `bitvm`.

use crate::btc_spv::{
    dest_id20, display_hex, display_to_internal, hash256, hex_decode, hex_encode, parse_tx,
    reverse_32, FALC_MAGIC,
};
use k256::elliptic_curve::point::AffineCoordinates;
use k256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use k256::elliptic_curve::{Group, PrimeField};
use k256::{AffinePoint, EncodedPoint, FieldBytes, ProjectivePoint, Scalar};
use sha2::{Digest, Sha256};

pub const SIGHASH_ALL: u32 = 1;
pub const SIGHASH_DEFAULT: u8 = 0;
pub const BECH32M_CONST: u32 = 0x2bc830a3;

#[derive(Debug, Clone)]
pub struct TxIn {
    pub txid_display: String,
    pub vout: u32,
    pub sats: u64,
    pub sequence: u32,
    pub script_pubkey: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TxOut {
    pub sats: u64,
    pub script_pubkey: Vec<u8>,
}

pub fn write_compact(out: &mut Vec<u8>, n: u64) {
    if n < 0xfd {
        out.push(n as u8);
    } else if n <= 0xffff {
        out.push(0xfd);
        out.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        out.push(0xfe);
        out.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        out.push(0xff);
        out.extend_from_slice(&n.to_le_bytes());
    }
}

pub fn p2pkh_spk(h160: &[u8]) -> Result<Vec<u8>, String> {
    if h160.len() != 20 {
        return Err("p2pkh hash160 must be 20 bytes".into());
    }
    let mut s = Vec::with_capacity(25);
    s.extend_from_slice(&[0x76, 0xa9, 0x14]);
    s.extend_from_slice(h160);
    s.extend_from_slice(&[0x88, 0xac]);
    Ok(s)
}

pub fn p2wpkh_spk(h160: &[u8]) -> Result<Vec<u8>, String> {
    if h160.len() != 20 {
        return Err("p2wpkh program must be 20 bytes".into());
    }
    let mut s = Vec::with_capacity(22);
    s.push(0x00);
    s.push(0x14);
    s.extend_from_slice(h160);
    Ok(s)
}

pub fn p2wpkh_script_code(h160: &[u8]) -> Result<Vec<u8>, String> {
    // BIP143: 0x19 76 a9 14 {20} 88 ac
    let mut s = Vec::with_capacity(26);
    s.push(0x19);
    s.extend_from_slice(&p2pkh_spk(h160)?);
    Ok(s)
}

pub fn falc_opreturn(account: &str) -> Vec<u8> {
    let id = dest_id20(account);
    let mut payload = Vec::with_capacity(24);
    payload.extend_from_slice(FALC_MAGIC);
    payload.extend_from_slice(&id);
    op_return_script(&payload)
}

/// `OP_RETURN` with push-only payload (≤80 bytes).
pub fn op_return_script(payload: &[u8]) -> Vec<u8> {
    if payload.is_empty() || payload.len() > 80 {
        panic!("op_return payload must be 1..=80 bytes");
    }
    let mut s = Vec::with_capacity(2 + payload.len());
    s.push(0x6a);
    s.push(payload.len() as u8);
    s.extend_from_slice(payload);
    s
}

/// BitVM2 Claim marker: `FPL2` || 32-byte statement digest.
pub fn bitvm2_claim_opreturn(digest: &[u8; 32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(36);
    payload.extend_from_slice(b"FPL2");
    payload.extend_from_slice(digest);
    op_return_script(&payload)
}

pub fn is_p2wsh(spk: &[u8]) -> bool {
    spk.len() == 34 && spk[0] == 0x00 && spk[1] == 0x20
}

pub fn is_p2wpkh(spk: &[u8]) -> bool {
    spk.len() == 22 && spk[0] == 0x00 && spk[1] == 0x14
}

pub fn is_p2tr(spk: &[u8]) -> bool {
    spk.len() == 34 && spk[0] == 0x51 && spk[1] == 0x20
}

pub fn p2tr_spk(xonly: &[u8]) -> Result<Vec<u8>, String> {
    if xonly.len() != 32 {
        return Err("p2tr program must be 32 bytes".into());
    }
    let mut s = Vec::with_capacity(34);
    s.push(0x51);
    s.push(0x20);
    s.extend_from_slice(xonly);
    Ok(s)
}

pub fn p2tr_xonly(spk: &[u8]) -> Result<[u8; 32], String> {
    if !is_p2tr(spk) {
        return Err("not p2tr".into());
    }
    let mut o = [0u8; 32];
    o.copy_from_slice(&spk[2..34]);
    Ok(o)
}

pub fn sha256_once(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn tagged_hash(tag: &[u8], msg: &[u8]) -> [u8; 32] {
    let tagh = sha256_once(tag);
    let mut h = Sha256::new();
    h.update(tagh);
    h.update(tagh);
    h.update(msg);
    h.finalize().into()
}

pub fn is_p2pkh(spk: &[u8]) -> bool {
    spk.len() == 25 && spk[0] == 0x76 && spk[1] == 0xa9 && spk[2] == 0x14 && spk[23] == 0x88 && spk[24] == 0xac
}

pub fn p2wpkh_hash160(spk: &[u8]) -> Result<[u8; 20], String> {
    if !is_p2wpkh(spk) {
        return Err("not p2wpkh".into());
    }
    let mut h = [0u8; 20];
    h.copy_from_slice(&spk[2..22]);
    Ok(h)
}

// ── base58 / bech32 decode (testnet) ───────────────────────────────────────

const B58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

fn b58_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut acc = vec![0u8; 1];
    for c in s.bytes() {
        let v = B58
            .iter()
            .position(|&x| x == c)
            .ok_or_else(|| format!("bad base58 char {}", c as char))?;
        let mut carry = v as u32;
        for b in acc.iter_mut().rev() {
            let t = (*b as u32) * 58 + carry;
            *b = (t & 0xff) as u8;
            carry = t >> 8;
        }
        while carry > 0 {
            acc.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let pad = s.bytes().take_while(|&c| c == b'1').count();
    let mut out = vec![0u8; pad];
    // strip leading zeros from acc except we already counted '1's
    let start = acc.iter().position(|&b| b != 0).unwrap_or(acc.len());
    out.extend_from_slice(&acc[start..]);
    Ok(out)
}

fn b58check_payload(s: &str) -> Result<Vec<u8>, String> {
    let raw = b58_decode(s)?;
    if raw.len() < 5 {
        return Err("base58 too short".into());
    }
    let (payload, chk) = raw.split_at(raw.len() - 4);
    let want = hash256(payload);
    if chk != &want[..4] {
        return Err("base58 checksum".into());
    }
    Ok(payload.to_vec())
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

fn bech32_decode(addr: &str) -> Result<(String, u8, Vec<u8>), String> {
    let s = addr.to_ascii_lowercase();
    let (hrp, data) = s.split_once('1').ok_or("bech32 missing separator")?;
    if hrp != "tb" && hrp != "bc" && hrp != "bcrt" {
        return Err(format!("unknown bech32 hrp {hrp}"));
    }
    let mut values = Vec::with_capacity(data.len());
    for c in data.bytes() {
        let v = BECH32_CHARSET
            .iter()
            .position(|&x| x == c)
            .ok_or("bech32 charset")?;
        values.push(v as u8);
    }
    if values.len() < 7 {
        return Err("bech32 too short".into());
    }
    let mut chk_in = bech32_hrp_expand(hrp);
    chk_in.extend_from_slice(&values);
    let pm = bech32_polymod(&chk_in);
    let bech32m = pm == BECH32M_CONST;
    if pm != 1 && !bech32m {
        return Err("bech32 checksum".into());
    }
    let witver = values[0];
    if witver == 0 && bech32m {
        return Err("witness v0 must be bech32".into());
    }
    if witver != 0 && !bech32m {
        return Err("witness v1+ must be bech32m".into());
    }
    let prog5 = &values[1..values.len() - 6];
    let prog = convert_bits(prog5, 5, 8, false).ok_or("bech32 convert")?;
    Ok((hrp.to_string(), witver, prog))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrKind {
    P2pkh,
    P2wpkh,
    P2wsh,
    P2tr,
}

/// Decode a Bitcoin address to (kind, scriptPubKey). Testnet + mainnet.
pub fn decode_address(addr: &str) -> Result<(AddrKind, Vec<u8>), String> {
    let a = addr.trim();
    if a.is_empty() {
        return Err("empty address".into());
    }
    if a.to_ascii_lowercase().starts_with("tb1")
        || a.to_ascii_lowercase().starts_with("bc1")
        || a.to_ascii_lowercase().starts_with("bcrt1")
    {
        let (_hrp, ver, prog) = bech32_decode(a)?;
        if ver == 0 && prog.len() == 20 {
            return Ok((AddrKind::P2wpkh, p2wpkh_spk(&prog)?));
        }
        if ver == 0 && prog.len() == 32 {
            let mut s = Vec::with_capacity(34);
            s.push(0x00);
            s.push(0x20);
            s.extend_from_slice(&prog);
            return Ok((AddrKind::P2wsh, s));
        }
        if ver == 1 && prog.len() == 32 {
            return Ok((AddrKind::P2tr, p2tr_spk(&prog)?));
        }
        return Err("unsupported witness program".into());
    }
    let payload = b58check_payload(a)?;
    if payload.len() != 21 {
        return Err("unexpected base58 payload".into());
    }
    let ver = payload[0];
    // mainnet 0x00, testnet 0x6f
    if ver == 0x00 || ver == 0x6f {
        return Ok((AddrKind::P2pkh, p2pkh_spk(&payload[1..])?));
    }
    Err(format!("unsupported address version {ver:#x}"))
}

pub fn serialize_outpoint(txid_display: &str, vout: u32) -> Result<[u8; 36], String> {
    let internal = display_to_internal(txid_display)?;
    let mut o = [0u8; 36];
    o[..32].copy_from_slice(&internal);
    o[32..].copy_from_slice(&vout.to_le_bytes());
    Ok(o)
}

pub fn serialize_output(out: &TxOut) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&out.sats.to_le_bytes());
    write_compact(&mut b, out.script_pubkey.len() as u64);
    b.extend_from_slice(&out.script_pubkey);
    b
}

pub fn hash_prevouts(inputs: &[TxIn]) -> Result<[u8; 32], String> {
    let mut buf = Vec::with_capacity(inputs.len() * 36);
    for i in inputs {
        buf.extend_from_slice(&serialize_outpoint(&i.txid_display, i.vout)?);
    }
    Ok(hash256(&buf))
}

pub fn hash_sequence(inputs: &[TxIn]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(inputs.len() * 4);
    for i in inputs {
        buf.extend_from_slice(&i.sequence.to_le_bytes());
    }
    hash256(&buf)
}

pub fn hash_outputs(outputs: &[TxOut]) -> [u8; 32] {
    let mut buf = Vec::new();
    for o in outputs {
        buf.extend_from_slice(&serialize_output(o));
    }
    hash256(&buf)
}

/// BIP341 key-path sighash. `hash_type` 0 = SIGHASH_DEFAULT (ALL).
pub fn bip341_keypath_sighash(
    version: u32,
    locktime: u32,
    inputs: &[TxIn],
    outputs: &[TxOut],
    index: usize,
    hash_type: u8,
) -> Result<[u8; 32], String> {
    if index >= inputs.len() {
        return Err("input index".into());
    }
    if hash_type != SIGHASH_DEFAULT && hash_type != 1 {
        return Err("only SIGHASH_DEFAULT/ALL".into());
    }
    let mut prev = Vec::with_capacity(inputs.len() * 36);
    for i in inputs {
        prev.extend_from_slice(&serialize_outpoint(&i.txid_display, i.vout)?);
    }
    let mut amounts = Vec::with_capacity(inputs.len() * 8);
    for i in inputs {
        amounts.extend_from_slice(&i.sats.to_le_bytes());
    }
    let mut spks = Vec::new();
    for i in inputs {
        write_compact(&mut spks, i.script_pubkey.len() as u64);
        spks.extend_from_slice(&i.script_pubkey);
    }
    let mut seqs = Vec::with_capacity(inputs.len() * 4);
    for i in inputs {
        seqs.extend_from_slice(&i.sequence.to_le_bytes());
    }
    let mut outs = Vec::new();
    for o in outputs {
        outs.extend_from_slice(&serialize_output(o));
    }
    let mut sigmsg = Vec::with_capacity(175);
    sigmsg.push(hash_type);
    sigmsg.extend_from_slice(&version.to_le_bytes());
    sigmsg.extend_from_slice(&locktime.to_le_bytes());
    sigmsg.extend_from_slice(&sha256_once(&prev));
    sigmsg.extend_from_slice(&sha256_once(&amounts));
    sigmsg.extend_from_slice(&sha256_once(&spks));
    sigmsg.extend_from_slice(&sha256_once(&seqs));
    sigmsg.extend_from_slice(&sha256_once(&outs));
    sigmsg.push(0x00); // spend_type: key-path, no annex
    sigmsg.extend_from_slice(&(index as u32).to_le_bytes());
    let mut pre = Vec::with_capacity(1 + sigmsg.len());
    pre.push(0x00); // epoch
    pre.extend_from_slice(&sigmsg);
    Ok(tagged_hash(b"TapSighash", &pre))
}

/// BIP341 script-path sighash (spend_type = 0x02, no annex).
pub fn bip341_scriptpath_sighash(
    version: u32,
    locktime: u32,
    inputs: &[TxIn],
    outputs: &[TxOut],
    index: usize,
    hash_type: u8,
    tapleaf_hash: &[u8; 32],
) -> Result<[u8; 32], String> {
    if index >= inputs.len() {
        return Err("input index".into());
    }
    if hash_type != SIGHASH_DEFAULT && hash_type != 1 {
        return Err("only SIGHASH_DEFAULT/ALL".into());
    }
    let mut prev = Vec::with_capacity(inputs.len() * 36);
    for i in inputs {
        prev.extend_from_slice(&serialize_outpoint(&i.txid_display, i.vout)?);
    }
    let mut amounts = Vec::with_capacity(inputs.len() * 8);
    for i in inputs {
        amounts.extend_from_slice(&i.sats.to_le_bytes());
    }
    let mut spks = Vec::new();
    for i in inputs {
        write_compact(&mut spks, i.script_pubkey.len() as u64);
        spks.extend_from_slice(&i.script_pubkey);
    }
    let mut seqs = Vec::with_capacity(inputs.len() * 4);
    for i in inputs {
        seqs.extend_from_slice(&i.sequence.to_le_bytes());
    }
    let mut outs = Vec::new();
    for o in outputs {
        outs.extend_from_slice(&serialize_output(o));
    }
    let mut sigmsg = Vec::with_capacity(220);
    sigmsg.push(hash_type);
    sigmsg.extend_from_slice(&version.to_le_bytes());
    sigmsg.extend_from_slice(&locktime.to_le_bytes());
    sigmsg.extend_from_slice(&sha256_once(&prev));
    sigmsg.extend_from_slice(&sha256_once(&amounts));
    sigmsg.extend_from_slice(&sha256_once(&spks));
    sigmsg.extend_from_slice(&sha256_once(&seqs));
    sigmsg.extend_from_slice(&sha256_once(&outs));
    sigmsg.push(0x02); // spend_type: script-path, no annex
    sigmsg.extend_from_slice(&(index as u32).to_le_bytes());
    sigmsg.extend_from_slice(tapleaf_hash);
    sigmsg.push(0x00); // key_version
    sigmsg.extend_from_slice(&0xffff_ffffu32.to_le_bytes()); // codesep_pos
    let mut pre = Vec::with_capacity(1 + sigmsg.len());
    pre.push(0x00); // epoch
    pre.extend_from_slice(&sigmsg);
    Ok(tagged_hash(b"TapSighash", &pre))
}

/// BIP341 TapLeaf hash. Leaf version 0xc0 (tapscript).
pub fn tapleaf_hash(script: &[u8]) -> [u8; 32] {
    let mut msg = Vec::with_capacity(1 + 9 + script.len());
    msg.push(0xc0);
    write_compact(&mut msg, script.len() as u64);
    msg.extend_from_slice(script);
    tagged_hash(b"TapLeaf", &msg)
}

/// BIP341 TapBranch. Sorts the two 32-byte hashes lexicographically.
pub fn tapbranch(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut msg = [0u8; 64];
    if a.as_slice() < b.as_slice() {
        msg[..32].copy_from_slice(a);
        msg[32..].copy_from_slice(b);
    } else {
        msg[..32].copy_from_slice(b);
        msg[32..].copy_from_slice(a);
    }
    tagged_hash(b"TapBranch", &msg)
}

fn scalar_from_32(b: &[u8; 32]) -> Result<Scalar, String> {
    Option::<Scalar>::from(Scalar::from_repr(FieldBytes::from(*b))).ok_or_else(|| "scalar >= n".into())
}

fn affine_xonly(p: &AffinePoint) -> [u8; 32] {
    let x = p.x();
    let mut o = [0u8; 32];
    o.copy_from_slice(x.as_slice());
    o
}

fn affine_y_odd(p: &AffinePoint) -> bool {
    bool::from(p.y_is_odd())
}

/// BIP340 lift_x: even-y point with this x, or None if not on the curve.
pub fn lift_x(xonly: &[u8; 32]) -> Option<AffinePoint> {
    let mut enc = [0u8; 33];
    enc[0] = 0x02;
    enc[1..].copy_from_slice(xonly);
    let ep = EncodedPoint::from_bytes(enc).ok()?;
    Option::<AffinePoint>::from(AffinePoint::from_encoded_point(&ep))
}

/// BIP341 H — lift_x of SHA256(uncompressed G). No discrete log is known.
pub fn bip341_h() -> AffinePoint {
    let mut x = [0u8; 32];
    x.copy_from_slice(
        &hex_decode("50929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0").unwrap(),
    );
    lift_x(&x).expect("BIP341 H is on-curve")
}

/// NUMS internal key: H + int(sha256(tag))*G. r is public so DL(H+rG) is unknown.
pub fn nums_internal_from_tag(tag: &[u8]) -> ([u8; 32], AffinePoint) {
    let mut r_b = sha256_once(tag);
    let r = loop {
        if let Ok(s) = scalar_from_32(&r_b) {
            break s;
        }
        r_b = sha256_once(&r_b);
    };
    let p = ProjectivePoint::from(bip341_h()) + ProjectivePoint::GENERATOR * r;
    let aff: AffinePoint = p.to_affine();
    // BIP341 lift_x is even-Y. Tweaking an odd-Y internal produces an address
    // that cannot be spent with a standard control block.
    let aff = if affine_y_odd(&aff) {
        (-ProjectivePoint::from(aff)).to_affine()
    } else {
        aff
    };
    (affine_xonly(&aff), aff)
}

/// BIP341 tweak: Q = P + tagged_hash("TapTweak", Px || merkle)*G.
/// Returns (output x-only, output y parity 0=even/1=odd).
pub fn taproot_tweak(
    internal: &AffinePoint,
    merkle_root: Option<&[u8; 32]>,
) -> Result<([u8; 32], u8), String> {
    let px = affine_xonly(internal);
    let mut msg = Vec::with_capacity(64);
    msg.extend_from_slice(&px);
    if let Some(r) = merkle_root {
        msg.extend_from_slice(r);
    }
    let t_b = tagged_hash(b"TapTweak", &msg);
    let t = scalar_from_32(&t_b)?;
    let q = ProjectivePoint::from(*internal) + ProjectivePoint::GENERATOR * t;
    if bool::from(q.is_identity()) {
        return Err("taproot tweak is identity".into());
    }
    let aff = q.to_affine();
    let parity = if affine_y_odd(&aff) { 1 } else { 0 };
    Ok((affine_xonly(&aff), parity))
}

/// Control block: (leaf_version|parity) || internal_xonly || merkle nodes.
pub fn taproot_control_block(
    output_parity: u8,
    internal_xonly: &[u8; 32],
    merkle_nodes: &[[u8; 32]],
) -> Vec<u8> {
    let mut c = Vec::with_capacity(33 + 32 * merkle_nodes.len());
    c.push(0xc0 | (output_parity & 1));
    c.extend_from_slice(internal_xonly);
    for n in merkle_nodes {
        c.extend_from_slice(n);
    }
    c
}

pub fn is_taproot_control_block(b: &[u8]) -> bool {
    if b.len() < 33 || (b.len() - 33) % 32 != 0 {
        return false;
    }
    (b[0] & 0xfe) == 0xc0
}

pub fn compressed_pubkey_from_secret(secret: &[u8; 32]) -> Result<Vec<u8>, String> {
    let d = scalar_from_32(secret)?;
    if bool::from(d.is_zero()) {
        return Err("secret is zero".into());
    }
    let p = (ProjectivePoint::GENERATOR * d).to_affine();
    Ok(p.to_encoded_point(true).as_bytes().to_vec())
}

pub fn xonly_from_secret(secret: &[u8; 32]) -> Result<[u8; 32], String> {
    let d = scalar_from_32(secret)?;
    if bool::from(d.is_zero()) {
        return Err("secret is zero".into());
    }
    let p = (ProjectivePoint::GENERATOR * d).to_affine();
    Ok(affine_xonly(&p))
}

/// BIP340 even-y secret: if P.y is odd, use n-d.
fn bip340_d(secret: &[u8; 32]) -> Result<(Scalar, [u8; 32]), String> {
    let d0 = scalar_from_32(secret)?;
    if bool::from(d0.is_zero()) {
        return Err("secret is zero".into());
    }
    let p = (ProjectivePoint::GENERATOR * d0).to_affine();
    let d = if affine_y_odd(&p) { -d0 } else { d0 };
    Ok((d, affine_xonly(&p)))
}

fn xor32(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut o = [0u8; 32];
    for i in 0..32 {
        o[i] = a[i] ^ b[i];
    }
    o
}

/// BIP340 Schnorr over a 32-byte message (BIP341 sighash). aux=0.
pub fn bip340_sign(secret: &[u8; 32], msg32: &[u8; 32]) -> Result<[u8; 64], String> {
    let (d, px) = bip340_d(secret)?;
    let aux = [0u8; 32];
    let mut d_b = [0u8; 32];
    d_b.copy_from_slice(d.to_bytes().as_slice());
    let t = xor32(&d_b, &tagged_hash(b"BIP0340/aux", &aux));
    let mut nonce_in = [0u8; 96];
    nonce_in[..32].copy_from_slice(&t);
    nonce_in[32..64].copy_from_slice(&px);
    nonce_in[64..].copy_from_slice(msg32);
    let mut k_b = tagged_hash(b"BIP0340/nonce", &nonce_in);
    let k0 = loop {
        match scalar_from_32(&k_b) {
            Ok(s) if !bool::from(s.is_zero()) => break s,
            _ => k_b = sha256_once(&k_b),
        }
    };
    let r_pt = (ProjectivePoint::GENERATOR * k0).to_affine();
    let k = if affine_y_odd(&r_pt) { -k0 } else { k0 };
    let rx = affine_xonly(&r_pt);
    let mut ch = [0u8; 96];
    ch[..32].copy_from_slice(&rx);
    ch[32..64].copy_from_slice(&px);
    ch[64..].copy_from_slice(msg32);
    let e = scalar_from_32(&tagged_hash(b"BIP0340/challenge", &ch)).unwrap_or(Scalar::ZERO);
    let s = k + e * d;
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&rx);
    sig[32..].copy_from_slice(s.to_bytes().as_slice());
    Ok(sig)
}

pub fn bip340_verify(xonly: &[u8; 32], msg32: &[u8; 32], sig64: &[u8]) -> Result<(), String> {
    if sig64.len() != 64 {
        return Err("schnorr sig must be 64 bytes".into());
    }
    let p = lift_x(xonly).ok_or("pubkey not on curve")?;
    let mut rx = [0u8; 32];
    rx.copy_from_slice(&sig64[..32]);
    let mut s_b = [0u8; 32];
    s_b.copy_from_slice(&sig64[32..]);
    let s = scalar_from_32(&s_b)?;
    let mut ch = [0u8; 96];
    ch[..32].copy_from_slice(&rx);
    ch[32..64].copy_from_slice(xonly);
    ch[64..].copy_from_slice(msg32);
    let e = scalar_from_32(&tagged_hash(b"BIP0340/challenge", &ch)).unwrap_or(Scalar::ZERO);
    let r_pt = ProjectivePoint::GENERATOR * s - ProjectivePoint::from(p) * e;
    if bool::from(r_pt.is_identity()) {
        return Err("schnorr R is identity".into());
    }
    let r_aff = r_pt.to_affine();
    if affine_y_odd(&r_aff) {
        return Err("schnorr R y is odd".into());
    }
    if affine_xonly(&r_aff) != rx {
        return Err("schnorr R mismatch".into());
    }
    Ok(())
}

/// BIP143 sighash for one P2WPKH input.
pub fn bip143_sighash(
    version: u32,
    locktime: u32,
    inputs: &[TxIn],
    outputs: &[TxOut],
    index: usize,
    sighash_type: u32,
) -> Result<[u8; 32], String> {
    let inp = inputs.get(index).ok_or("input index")?;
    if !is_p2wpkh(&inp.script_pubkey) {
        return Err("bip143 helper is for p2wpkh inputs".into());
    }
    let h160 = p2wpkh_hash160(&inp.script_pubkey)?;
    let script_code = p2wpkh_script_code(&h160)?;
    let mut pre = Vec::with_capacity(200);
    pre.extend_from_slice(&version.to_le_bytes());
    pre.extend_from_slice(&hash_prevouts(inputs)?);
    pre.extend_from_slice(&hash_sequence(inputs));
    pre.extend_from_slice(&serialize_outpoint(&inp.txid_display, inp.vout)?);
    pre.extend_from_slice(&script_code);
    pre.extend_from_slice(&inp.sats.to_le_bytes());
    pre.extend_from_slice(&inp.sequence.to_le_bytes());
    pre.extend_from_slice(&hash_outputs(outputs));
    pre.extend_from_slice(&locktime.to_le_bytes());
    pre.extend_from_slice(&sighash_type.to_le_bytes());
    Ok(hash256(&pre))
}

/// BIP143 sighash for one P2WSH input (`scriptCode` = compact-size || witness script).
pub fn bip143_p2wsh_sighash(
    version: u32,
    locktime: u32,
    inputs: &[TxIn],
    outputs: &[TxOut],
    index: usize,
    sighash_type: u32,
    witness_script: &[u8],
) -> Result<[u8; 32], String> {
    let inp = inputs.get(index).ok_or("input index")?;
    if !is_p2wsh(&inp.script_pubkey) {
        return Err("bip143 p2wsh helper needs p2wsh input".into());
    }
    let mut script_code = Vec::with_capacity(1 + witness_script.len());
    write_compact(&mut script_code, witness_script.len() as u64);
    script_code.extend_from_slice(witness_script);
    let mut pre = Vec::with_capacity(200 + witness_script.len());
    pre.extend_from_slice(&version.to_le_bytes());
    pre.extend_from_slice(&hash_prevouts(inputs)?);
    pre.extend_from_slice(&hash_sequence(inputs));
    pre.extend_from_slice(&serialize_outpoint(&inp.txid_display, inp.vout)?);
    pre.extend_from_slice(&script_code);
    pre.extend_from_slice(&inp.sats.to_le_bytes());
    pre.extend_from_slice(&inp.sequence.to_le_bytes());
    pre.extend_from_slice(&hash_outputs(outputs));
    pre.extend_from_slice(&locktime.to_le_bytes());
    pre.extend_from_slice(&sighash_type.to_le_bytes());
    Ok(hash256(&pre))
}

/// Legacy P2PKH sighash (SIGHASH_ALL).
pub fn legacy_p2pkh_sighash(
    version: u32,
    locktime: u32,
    inputs: &[TxIn],
    outputs: &[TxOut],
    index: usize,
    sighash_type: u32,
) -> Result<[u8; 32], String> {
    let mut raw = Vec::new();
    raw.extend_from_slice(&version.to_le_bytes());
    write_compact(&mut raw, inputs.len() as u64);
    for (i, inp) in inputs.iter().enumerate() {
        raw.extend_from_slice(&serialize_outpoint(&inp.txid_display, inp.vout)?);
        if i == index {
            write_compact(&mut raw, inp.script_pubkey.len() as u64);
            raw.extend_from_slice(&inp.script_pubkey);
        } else {
            write_compact(&mut raw, 0);
        }
        raw.extend_from_slice(&inp.sequence.to_le_bytes());
    }
    write_compact(&mut raw, outputs.len() as u64);
    for o in outputs {
        raw.extend_from_slice(&serialize_output(o));
    }
    raw.extend_from_slice(&locktime.to_le_bytes());
    raw.extend_from_slice(&sighash_type.to_le_bytes());
    Ok(hash256(&raw))
}

/// Assemble a signed legacy P2PKH tx (one scriptSig per input).
pub fn assemble_legacy_tx(
    version: u32,
    locktime: u32,
    inputs: &[TxIn],
    outputs: &[TxOut],
    script_sigs: &[Vec<u8>],
) -> Result<Vec<u8>, String> {
    if script_sigs.len() != inputs.len() {
        return Err("scriptSig count".into());
    }
    let mut raw = Vec::new();
    raw.extend_from_slice(&version.to_le_bytes());
    write_compact(&mut raw, inputs.len() as u64);
    for (inp, sig) in inputs.iter().zip(script_sigs.iter()) {
        raw.extend_from_slice(&serialize_outpoint(&inp.txid_display, inp.vout)?);
        write_compact(&mut raw, sig.len() as u64);
        raw.extend_from_slice(sig);
        raw.extend_from_slice(&inp.sequence.to_le_bytes());
    }
    write_compact(&mut raw, outputs.len() as u64);
    for o in outputs {
        raw.extend_from_slice(&serialize_output(o));
    }
    raw.extend_from_slice(&locktime.to_le_bytes());
    Ok(raw)
}

/// Assemble a signed P2WPKH tx (empty scriptSig, witness stack per input).
pub fn assemble_segwit_tx(
    version: u32,
    locktime: u32,
    inputs: &[TxIn],
    outputs: &[TxOut],
    witnesses: &[Vec<Vec<u8>>],
) -> Result<Vec<u8>, String> {
    if witnesses.len() != inputs.len() {
        return Err("witness count".into());
    }
    let mut raw = Vec::new();
    raw.extend_from_slice(&version.to_le_bytes());
    raw.push(0x00); // marker
    raw.push(0x01); // flag
    write_compact(&mut raw, inputs.len() as u64);
    for inp in inputs {
        raw.extend_from_slice(&serialize_outpoint(&inp.txid_display, inp.vout)?);
        write_compact(&mut raw, 0);
        raw.extend_from_slice(&inp.sequence.to_le_bytes());
    }
    write_compact(&mut raw, outputs.len() as u64);
    for o in outputs {
        raw.extend_from_slice(&serialize_output(o));
    }
    for wit in witnesses {
        write_compact(&mut raw, wit.len() as u64);
        for item in wit {
            write_compact(&mut raw, item.len() as u64);
            raw.extend_from_slice(item);
        }
    }
    raw.extend_from_slice(&locktime.to_le_bytes());
    Ok(raw)
}

pub fn txid_of(raw: &[u8]) -> Result<String, String> {
    Ok(parse_tx(raw)?.txid_display)
}

pub fn hex_of(raw: &[u8]) -> String {
    hex_encode(raw)
}

pub fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    hex_decode(s)
}

/// DER-encode r||s (32+32) as Bitcoin ECDSA signature body (no sighash byte).
pub fn der_encode_rs(rs: &[u8]) -> Result<Vec<u8>, String> {
    if rs.len() != 64 {
        return Err("compact sig must be 64 bytes".into());
    }
    fn enc_int(x: &[u8]) -> Vec<u8> {
        let mut v = x.to_vec();
        while v.len() > 1 && v[0] == 0 && v[1] < 0x80 {
            v.remove(0);
        }
        if v[0] & 0x80 != 0 {
            v.insert(0, 0);
        }
        let mut o = vec![0x02, v.len() as u8];
        o.extend_from_slice(&v);
        o
    }
    let r = enc_int(&rs[..32]);
    let s = enc_int(&rs[32..]);
    let mut out = vec![0x30, (r.len() + s.len()) as u8];
    out.extend_from_slice(&r);
    out.extend_from_slice(&s);
    Ok(out)
}

/// Pushdata (script) for a byte string.
pub fn push_bytes(data: &[u8]) -> Vec<u8> {
    let mut o = Vec::with_capacity(data.len() + 2);
    if data.len() < 0x4c {
        o.push(data.len() as u8);
    } else if data.len() <= 0xff {
        o.push(0x4c);
        o.push(data.len() as u8);
    } else {
        o.push(0x4d);
        o.extend_from_slice(&(data.len() as u16).to_le_bytes());
    }
    o.extend_from_slice(data);
    o
}

/// Check a signed vault P2WPKH tx matches dest/amount/change/inputs.
pub fn vault_spend_matches(
    raw: &[u8],
    dest_spk: &[u8],
    amount: u64,
    change_spk: &[u8],
    change: u64,
    inputs: &[TxIn],
) -> Result<String, String> {
    let tx = parse_tx(raw)?;
    if tx.outputs.is_empty() {
        return Err("vault spend has no outputs".into());
    }
    if tx.outputs[0].0 != amount || tx.outputs[0].1 != dest_spk {
        return Err("vault spend dest/amount mismatch".into());
    }
    if change >= 546 {
        if tx.outputs.len() < 2 {
            return Err("vault spend missing change".into());
        }
        if tx.outputs[1].0 != change || tx.outputs[1].1 != change_spk {
            return Err("vault spend change mismatch".into());
        }
    }
    // Input count must match the plan (we cannot see outpoints from ParsedTx).
    // Re-parse vin count from raw.
    let n_in = count_inputs(raw)?;
    if n_in != inputs.len() {
        return Err("vault spend input count mismatch".into());
    }
    Ok(tx.txid_display)
}

fn count_inputs(raw: &[u8]) -> Result<usize, String> {
    if raw.len() < 6 {
        return Err("tx too short".into());
    }
    let mut i = 4usize;
    if raw.get(4) == Some(&0x00) && raw.get(5) == Some(&0x01) {
        i = 6;
    }
    // compact
    if i >= raw.len() {
        return Err("eof vin count".into());
    }
    let n = raw[i];
    if n < 0xfd {
        Ok(n as usize)
    } else {
        Err("too many inputs for vault spend parser".into())
    }
}

/// Display-order txid helper (for tests).
pub fn display32(internal: &[u8; 32]) -> String {
    display_hex(internal)
}

pub fn reverse32(b: &[u8; 32]) -> [u8; 32] {
    reverse_32(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_testnet_p2pkh_and_p2wpkh() {
        let (k, spk) = decode_address("n1fH7uhaDauQYfuwG6t1aTncvyuBSZtDEY").unwrap();
        assert_eq!(k, AddrKind::P2pkh);
        assert!(is_p2pkh(&spk));
        let (k2, spk2) = decode_address("tb1qesum00x0jm6w2a0dt5vksckhyt45430c0yg5sj").unwrap();
        assert_eq!(k2, AddrKind::P2wpkh);
        assert_eq!(hex_encode(&spk2), "0014cc39b7bccf96f4e575ed5d196862d722eb4ac5f8");
    }

    #[test]
    fn falc_opreturn_is_24_payload() {
        let s = falc_opreturn("sally");
        assert_eq!(s[0], 0x6a);
        assert_eq!(s[1], 24);
        assert_eq!(&s[2..6], b"FALC");
    }

    #[test]
    fn bip340_sign_verify_roundtrip() {
        let mut secret = [0u8; 32];
        secret[31] = 3;
        let msg = [0u8; 32];
        let pk = xonly_from_secret(&secret).unwrap();
        let sig = bip340_sign(&secret, &msg).unwrap();
        bip340_verify(&pk, &msg, &sig).unwrap();
        let mut bad = msg;
        bad[0] ^= 1;
        assert!(bip340_verify(&pk, &bad, &sig).is_err());
    }

    #[test]
    fn bip341_h_is_even_y() {
        let h = bip341_h();
        assert!(!affine_y_odd(&h));
    }
}
