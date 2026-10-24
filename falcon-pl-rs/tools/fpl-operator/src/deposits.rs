//! Deposit-graph lookup: which presigned deposit graph serves a Falcon withdrawal.
//!
//! The deposit ceremony leaves `graphs/<dir>/final.json` (peg-in + peg-out
//! graphs). We index each: peg-in id, peg-in confirm txid:0 and amount, and the
//! operator keys that have a peg-out graph on it. A claim is served by an
//! unspent deposit of the same amount on which WE have a peg-out graph and that
//! no other note already took (by-claim map + gossip ClaimIntent).
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DepositGraph {
    pub dir: PathBuf,
    pub peg_in_id: String,
    pub confirm_outpoint: String,
    pub amount: u64,
    pub operator_pubkeys: Vec<String>,
}

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let f = *b.get(*i)?;
    *i += 1;
    let n = match f {
        0xfd => 2,
        0xfe => 4,
        0xff => 8,
        _ => return Some(f as u64),
    };
    let mut v = 0u64;
    for k in 0..n {
        v |= (*b.get(*i + k)? as u64) << (8 * k);
    }
    *i += n;
    Some(v)
}

/// (txid hex, output-0 value) of a raw (segwit or legacy) transaction.
pub fn txid_and_out0(raw: &[u8]) -> Option<(String, u64)> {
    let segwit = raw.len() > 6 && raw[4] == 0 && raw[5] == 1;
    let mut i = if segwit { 6 } else { 4 };
    let ins_start = i;
    let nin = varint(raw, &mut i)?;
    for _ in 0..nin {
        i += 36;
        let l = varint(raw, &mut i)? as usize;
        i += l + 4;
    }
    let nout = varint(raw, &mut i)?;
    let mut out0 = None;
    for k in 0..nout {
        let v = u64::from_le_bytes(raw.get(i..i + 8)?.try_into().ok()?);
        if k == 0 {
            out0 = Some(v);
        }
        i += 8;
        let l = varint(raw, &mut i)? as usize;
        i += l;
    }
    let outs_end = i;
    let mut strip = Vec::with_capacity(raw.len());
    strip.extend_from_slice(&raw[..4]);
    strip.extend_from_slice(raw.get(ins_start..outs_end)?);
    strip.extend_from_slice(raw.get(raw.len() - 4..)?);
    let mut h: [u8; 32] = Sha256::digest(Sha256::digest(&strip)).into();
    h.reverse();
    Some((hex::encode(h), out0?))
}

fn index_one(dir: &Path) -> Option<DepositGraph> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("final.json")).ok()?).ok()?;
    let pi = v["peg_in_graphs"].get(0)?;
    let raw = hex::decode(pi["peg_in_confirm_transaction"]["tx"].as_str()?).ok()?;
    let (txid, amount) = txid_and_out0(&raw)?;
    let mut ops = vec![];
    for g in v["peg_out_graphs"].as_array().into_iter().flatten().filter(|g| g["peg_in_confirm_txid"].as_str().map_or(true, |t| t.eq_ignore_ascii_case(&txid))) {
        if let Some(k) = g["operator_public_key"].as_str() {
            ops.push(k.to_ascii_lowercase());
        }
    }
    Some(DepositGraph { dir: dir.to_path_buf(), peg_in_id: pi["id"].as_str()?.to_string(), confirm_outpoint: format!("{txid}:0"), amount, operator_pubkeys: ops })
}

/// Index all deposit graphs (cached in graphs/index.json by dir name).
pub fn index(graphs: &Path) -> Vec<DepositGraph> {
    let cache_p = graphs.join("index.json");
    let mut cache: Vec<DepositGraph> = std::fs::read_to_string(&cache_p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let mut changed = false;
    if let Ok(rd) = std::fs::read_dir(graphs) {
        for e in rd.flatten() {
            let d = e.path();
            if d.join("final.json").is_file() && !cache.iter().any(|c| c.dir == d) {
                if let Some(g) = index_one(&d) {
                    cache.push(g);
                    changed = true;
                }
            }
        }
    }
    if changed {
        let _ = std::fs::write(&cache_p, serde_json::to_vec_pretty(&cache).unwrap());
    }
    cache
}

pub fn bind_claim(graphs: &Path, note: &str, dir: &Path) {
    let d = graphs.join("by-claim");
    let _ = std::fs::create_dir_all(&d);
    let _ = std::fs::write(d.join(note), dir.to_string_lossy().as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn txid_of_segwit_confirm_tx() {
        // peg-in confirm from testnet4 scenario take1 (public tx)
        let raw = hex::decode(include_str!("../testdata/confirm_tx.hex").trim()).unwrap();
        let (txid, v) = txid_and_out0(&raw).unwrap();
        assert_eq!(v, 10_000);
        assert_eq!(txid, include_str!("../testdata/confirm_txid.txt").trim());
    }
}
