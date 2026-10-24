//! EVM deposit-event SPV (ETH native + ERC-20 USDC).
//!
//! Headers are hash-linked watcher submissions. A deposit leaf binds
//! `txid|log_index|token|dest20|amount|lock_id`. Parent = sha256(left||right).
//! This is not a full Ethereum light client (no sync committee / MPT receipts).
//! Mint is allowed only when the leaf verifies against a submitted header.

use crate::btc_spv::dest_id20;
use crate::crypto::sha256_hex;
use sha2::{Digest, Sha256};

pub const RAIL_ETH: &str = "ETH";
pub const RAIL_USDC: &str = "USDC";
pub const RAIL_BNB: &str = "BNB";

/// Native ETH token sentinel (20 zero bytes hex).
pub const ETH_NATIVE_TOKEN: &str = "0000000000000000000000000000000000000000";
/// Sepolia USDC (Circle test). Overridden by rail lock_id extra if set.
pub const SEPOLIA_USDC: &str = "1c7d4b196cb0c7b01d743fbc6116a902379c7238";

/// Live v1 QC. After FalconQcBridgeV2 deploy, set `FPL_SEPOLIA_QC_BRIDGE` (or
/// `FPL_SEPOLIA_QC_V2_BRIDGE`) so Deposit logs from V2 also mint.
pub const DEFAULT_SEPOLIA_QC_BRIDGE: &str = "0xf8F1471643792eb1cD5d0C31061629777E55bc48";
/// Live Sepolia FalconDestLock (claimer Kickoff + dest take). Keep in the mint
/// union until dest-lock deposits stop (PR 9).
pub const DEFAULT_SEPOLIA_DESTLOCK: &str = "0xdBF6855b00B78c047A729A21E13bfE5f4C991C05";

/// Sepolia QC bridge address for receipt Deposit log matching.
/// Override with `FPL_SEPOLIA_QC_BRIDGE` when testing a new deployment.
pub fn sepolia_qc_bridge_addr() -> Result<[u8; 20], String> {
    if let Ok(v) = std::env::var("FPL_SEPOLIA_QC_BRIDGE") {
        let t = v.trim();
        if !t.is_empty() {
            return parse_eth_addr(&t);
        }
    }
    parse_eth_addr(DEFAULT_SEPOLIA_QC_BRIDGE)
}

/// Optional V2 QC (FalconQcBridgeV2). Empty until ceremony + deploy.
pub fn sepolia_qc_v2_bridge_addr() -> Result<Option<[u8; 20]>, String> {
    for key in ["FPL_SEPOLIA_QC_V2_BRIDGE", "FALCON_QC_V2_BRIDGE"] {
        if let Ok(v) = std::env::var(key) {
            let t = v.trim();
            if !t.is_empty() {
                return Ok(Some(parse_eth_addr(t)?));
            }
        }
    }
    Ok(None)
}

/// Dest-lock contract for ETH/USDC peg-in. Override with SEPOLIA_BRIDGE / FALCON_DESTLOCK.
pub fn sepolia_destlock_bridge_addr() -> Result<[u8; 20], String> {
    for key in ["SEPOLIA_BRIDGE", "FALCON_DESTLOCK"] {
        if let Ok(v) = std::env::var(key) {
            let t = v.trim();
            if !t.is_empty() {
                return parse_eth_addr(t);
            }
        }
    }
    parse_eth_addr(DEFAULT_SEPOLIA_DESTLOCK)
}

/// FalconQcBridgeV3 on Sepolia (deployed 2026-10-09, tx 0xd083…b21f; Checks
/// 0xAe89…dD7a, AnchorSet 0x9742…3409, anchor 353130). Same on every seat;
/// `FPL_SEPOLIA_QC_V3_BRIDGE` is only an override. Never added to
/// [`sepolia_mint_bridge_addrs`]: V1 `mint_evm_lc` must not treat V3 logs as
/// V1 deposits.
pub const DEFAULT_SEPOLIA_QC_V3_BRIDGE: Option<&str> =
    Some("0xEd4eE497F21a9255a59e87535FF2A6097592Cfd4");

/// Env var that overrides [`DEFAULT_SEPOLIA_QC_V3_BRIDGE`] on a seat.
pub const SEPOLIA_QC_V3_BRIDGE_ENV: &str = "FPL_SEPOLIA_QC_V3_BRIDGE";

/// V3 QC bridge for `Node::eth_v3_bridge`. Empty env → [`DEFAULT_SEPOLIA_QC_V3_BRIDGE`].
/// Consensus functions take the address as an argument and do not read this.
/// A set-but-malformed value (or non-UTF-8) is an `Err`, never `None`.
pub fn sepolia_qc_v3_bridge_addr() -> Result<Option<[u8; 20]>, String> {
    parse_qc_v3_bridge_env(std::env::var_os(SEPOLIA_QC_V3_BRIDGE_ENV).as_deref())
}

/// [`sepolia_qc_v3_bridge_addr`] for a given env value. Unset or blank →
/// the default. Anything else must be a 20-byte 0x address.
pub fn parse_qc_v3_bridge_env(raw: Option<&std::ffi::OsStr>) -> Result<Option<[u8; 20]>, String> {
    if let Some(raw) = raw {
        let v = raw
            .to_str()
            .ok_or_else(|| format!("{SEPOLIA_QC_V3_BRIDGE_ENV} is not UTF-8"))?;
        let t = v.trim();
        if !t.is_empty() {
            return parse_eth_addr(t)
                .map(Some)
                .map_err(|e| format!("{SEPOLIA_QC_V3_BRIDGE_ENV}={t:?} is malformed: {e}"));
        }
    }
    match DEFAULT_SEPOLIA_QC_V3_BRIDGE {
        Some(s) => Ok(Some(parse_eth_addr(s)?)),
        None => Ok(None),
    }
}

/// Addresses whose Deposit logs count for ETH/USDC LC mint (QC v1, dest-lock, V2).
/// V3 is intentionally absent — see [`DEFAULT_SEPOLIA_QC_V3_BRIDGE`].
pub fn sepolia_mint_bridge_addrs() -> Vec<[u8; 20]> {
    let mut out = Vec::new();
    if let Ok(a) = sepolia_qc_bridge_addr() {
        out.push(a);
    }
    if let Ok(a) = sepolia_destlock_bridge_addr() {
        if !out.iter().any(|x| x == &a) {
            out.push(a);
        }
    }
    if let Ok(Some(a)) = sepolia_qc_v2_bridge_addr() {
        if !out.iter().any(|x| x == &a) {
            out.push(a);
        }
    }
    out
}

pub fn is_evm_rail(asset: &str) -> bool {
    matches!(asset, RAIL_ETH | RAIL_USDC | RAIL_BNB)
}

pub fn default_token(asset: &str) -> &'static str {
    match asset {
        RAIL_USDC => SEPOLIA_USDC,
        _ => ETH_NATIVE_TOKEN,
    }
}

fn norm_hex(s: &str) -> String {
    s.trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .to_ascii_lowercase()
}

/// 32-byte sha256 of the canonical deposit string.
pub fn deposit_leaf(
    txid: &str,
    log_index: u32,
    token: &str,
    dest_account: &str,
    amount: u64,
    lock_id: &str,
) -> [u8; 32] {
    let dest = hex::encode(dest_id20(dest_account));
    let s = format!(
        "evm-dep-v1|{}|{}|{}|{}|{}|{}",
        norm_hex(txid),
        log_index,
        norm_hex(token),
        dest,
        amount,
        lock_id
    );
    Sha256::digest(s.as_bytes()).into()
}

pub fn deposit_leaf_hex(
    txid: &str,
    log_index: u32,
    token: &str,
    dest_account: &str,
    amount: u64,
    lock_id: &str,
) -> String {
    hex::encode(deposit_leaf(txid, log_index, token, dest_account, amount, lock_id))
}

/// Binary sha256 merkle: parent = sha256(left||right). Leaf/siblings are 32-byte hex.
pub fn merkle_root_binary(leaf_hex: &str, path: &[String], index: u32) -> Result<String, String> {
    let mut hash = hex::decode(norm_hex(leaf_hex)).map_err(|e| e.to_string())?;
    if hash.len() != 32 {
        return Err("evm leaf must be 32 bytes".into());
    }
    let mut idx = index;
    for sib_hex in path {
        let sib = hex::decode(norm_hex(sib_hex)).map_err(|e| e.to_string())?;
        if sib.len() != 32 {
            return Err("evm sibling must be 32 bytes".into());
        }
        let mut concat = Vec::with_capacity(64);
        if idx % 2 == 0 {
            concat.extend_from_slice(&hash);
            concat.extend_from_slice(&sib);
        } else {
            concat.extend_from_slice(&sib);
            concat.extend_from_slice(&hash);
        }
        hash = Sha256::digest(&concat).to_vec();
        idx /= 2;
    }
    Ok(hex::encode(hash))
}

/// Pairwise merkle over leaves (pad last). Returns root hex + path for `index`.
pub fn build_tree(leaves: &[[u8; 32]], index: usize) -> Result<(String, Vec<String>), String> {
    if leaves.is_empty() {
        return Err("empty evm tree".into());
    }
    if index >= leaves.len() {
        return Err("leaf index".into());
    }
    let mut layer: Vec<[u8; 32]> = leaves.to_vec();
    let mut idx = index;
    let mut path = Vec::new();
    while layer.len() > 1 {
        if layer.len() % 2 == 1 {
            layer.push(*layer.last().unwrap());
        }
        let sib = if idx % 2 == 0 {
            layer[idx + 1]
        } else {
            layer[idx - 1]
        };
        path.push(hex::encode(sib));
        let mut next = Vec::with_capacity(layer.len() / 2);
        for pair in layer.chunks(2) {
            let mut c = [0u8; 64];
            c[..32].copy_from_slice(&pair[0]);
            c[32..].copy_from_slice(&pair[1]);
            next.push(Sha256::digest(c).into());
        }
        idx /= 2;
        layer = next;
    }
    Ok((hex::encode(layer[0]), path))
}

/// Single-leaf tree (common for one deposit per block).
pub fn single_leaf_root(leaf: &[u8; 32]) -> String {
    hex::encode(leaf)
}

pub fn header_hash(height: u64, parent: &str, merkle_root: &str) -> String {
    sha256_hex(format!("evm-hdr-v1|{height}|{}|{}", norm_hex(parent), norm_hex(merkle_root)).as_bytes())
}

/// Parse 0x Ethereum address to 20 bytes.
pub fn parse_eth_addr(s: &str) -> Result<[u8; 20], String> {
    let h = norm_hex(s);
    if h.len() != 40 {
        return Err("ETH dest must be 20-byte 0x address".into());
    }
    let b = hex::decode(&h).map_err(|e| e.to_string())?;
    let mut a = [0u8; 20];
    a.copy_from_slice(&b);
    Ok(a)
}

/// Must match FalconBridge.withdrawLeaf: sha256("evm-wd-v1|" || note32 || dest20 || uint64_be || flag).
pub fn withdraw_leaf(note32: &[u8; 32], dest20: &[u8; 20], amount: u64, usdc: bool) -> [u8; 32] {
    let mut buf = Vec::with_capacity(10 + 32 + 20 + 8 + 1);
    buf.extend_from_slice(b"evm-wd-v1|");
    buf.extend_from_slice(note32);
    buf.extend_from_slice(dest20);
    buf.extend_from_slice(&amount.to_be_bytes());
    buf.push(if usdc { 1 } else { 0 });
    Sha256::digest(&buf).into()
}

pub fn withdraw_leaf_hex(note_id_hex: &str, dest: &str, amount: u64, usdc: bool) -> Result<String, String> {
    let nb = hex::decode(norm_hex(note_id_hex)).map_err(|e| e.to_string())?;
    if nb.len() != 32 {
        return Err("note_id must be 32 bytes".into());
    }
    let mut note = [0u8; 32];
    note.copy_from_slice(&nb);
    let dest20 = parse_eth_addr(dest)?;
    Ok(hex::encode(withdraw_leaf(&note, &dest20, amount, usdc)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_leaf_tree_verifies() {
        let a = deposit_leaf("aa", 0, ETH_NATIVE_TOKEN, "sally", 1_000, "lock");
        let b = deposit_leaf("bb", 1, SEPOLIA_USDC, "dave", 2_000, "lock");
        let (root, path) = build_tree(&[a, b], 0).unwrap();
        let got = merkle_root_binary(&hex::encode(a), &path, 0).unwrap();
        assert_eq!(got, root);
        let (root1, path1) = build_tree(&[a, b], 1).unwrap();
        assert_eq!(root1, root);
        assert_eq!(
            merkle_root_binary(&hex::encode(b), &path1, 1).unwrap(),
            root
        );
    }

    #[test]
    fn sepolia_qc_bridge_default() {
        let addr = sepolia_qc_bridge_addr().unwrap();
        assert_eq!(
            hex::encode(addr),
            "f8f1471643792eb1cd5d0c31061629777e55bc48"
        );
    }

    #[test]
    fn withdraw_leaf_is_32_and_binds_dest() {
        let note = [7u8; 32];
        let dest = parse_eth_addr("0x000000000000000000000000000000000000a11e").unwrap();
        let a = withdraw_leaf(&note, &dest, 500, false);
        let b = withdraw_leaf(&note, &dest, 500, true);
        let c = withdraw_leaf(&note, &[0u8; 20], 500, false);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 32);
    }
}
