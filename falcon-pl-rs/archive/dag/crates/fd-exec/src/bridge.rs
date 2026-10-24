//! Protocol-controlled SPV / light-client bridge substrate.
//!
//! **No federated mint authority.** Rail assets (BTC/ETH/BNB/XRP) increase only when
//! a deposit SPV proof verifies against the on-chain light-client tip. Withdrawals burn
//! on Falcon and emit a claim note for the *external* lock contract / script to release.
//!
//! External chains hold real coins. Relayers/agents only submit proofs — they cannot mint
//! without rules accepting the proof.

use fd_types::{
    bridge_account, sha256_hex, AccountId, AssetId, BridgeProof, GENESIS_RAILS, HashHex, RailFamily,
    RAIL_BNB, RAIL_BTC, RAIL_ETH, RAIL_XRP,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Default confirmations before a deposit header is considered final enough to mint.
pub const DEFAULT_MIN_CONFIRMATIONS: u64 = 6;
/// Keep this many recent headers per rail in state (snapshot-friendly).
pub const HEADER_HISTORY: usize = 256;

/// Genesis lock commitments (placeholders for production vault scripts / contracts).
pub fn genesis_lock_id(rail: &str) -> String {
    sha256_hex(format!("falcon-spv-lock-v1:{rail}"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RailConfig {
    pub asset: AssetId,
    pub family: RailFamily,
    pub min_confirmations: u64,
    pub bridge_account: AccountId,
    pub lock_id: String,
    pub total_minted: u64,
    pub total_burned: u64,
    pub tip_height: u64,
    pub tip_hash: String,
    pub tip_work: u128,
    /// Protocol-committed merkle root over open withdrawal claim leaves (auto on RailWithdraw).
    /// Anyone can recompute from `rail_withdrawals` — not a committee signature.
    #[serde(default)]
    pub claim_root: String,
    /// Number of open notes at last commit.
    #[serde(default)]
    pub claim_note_count: u64,
    /// `applied_user_txs` when claim_root was last committed.
    #[serde(default)]
    pub claim_height: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RailHeader {
    pub height: u64,
    pub hash: String,
    pub parent_hash: String,
    /// BTC: block merkle root. EVM: bind root = sha256(tx_root‖deposit_tree_root).
    pub merkle_root: String,
    /// EVM transactionsRoot (hex 32); empty for non-EVM.
    #[serde(default)]
    pub tx_root: String,
    pub work: u128,
}

/// Empty EVM deposit tree root (no Deposit events in block).
pub fn empty_evm_deposit_root() -> String {
    hex::encode(Sha256::digest(b"falcon-evm-deposit-empty-v1"))
}

/// EVM header bind: sha256( tx_root[32] || deposit_root[32] ).
pub fn evm_bind_root(tx_root_hex: &str, deposit_root_hex: &str) -> String {
    let t = note_id_bytes32(tx_root_hex);
    let d = note_id_bytes32(deposit_root_hex);
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&t);
    buf.extend_from_slice(&d);
    hex::encode(Sha256::digest(&buf))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WithdrawalNote {
    pub note_id: HashHex,
    pub rail: AssetId,
    pub amount: u64,
    pub from: AccountId,
    pub external_to: String,
    pub falcon_height: u64,
}

fn dsha256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

/// Protocol-generic pairing (dev / EVM substrate until full MPT): sha256_hex("{left}|{right}").
pub fn merkle_root_protocol(leaf: &str, path: &[String], index: u32) -> String {
    let mut hash = leaf.to_string();
    let mut idx = index;
    for sib in path {
        if idx % 2 == 0 {
            hash = sha256_hex(format!("{hash}|{sib}"));
        } else {
            hash = sha256_hex(format!("{sib}|{hash}"));
        }
        idx /= 2;
    }
    hash
}

/// Bitcoin-style merkle: leaf and siblings are 32-byte hashes as lowercase hex;
/// parent = dsha256(left || right). Matches Electrum / Bitcoin Core branch orientation.
pub fn merkle_root_bitcoin(leaf_hex: &str, path: &[String], index: u32) -> Option<String> {
    let mut hash = hex::decode(leaf_hex.trim()).ok()?;
    if hash.len() != 32 {
        return None;
    }
    let mut idx = index;
    for sib_hex in path {
        let sib = hex::decode(sib_hex.trim()).ok()?;
        if sib.len() != 32 {
            return None;
        }
        let mut concat = Vec::with_capacity(64);
        if idx % 2 == 0 {
            concat.extend_from_slice(&hash);
            concat.extend_from_slice(&sib);
        } else {
            concat.extend_from_slice(&sib);
            concat.extend_from_slice(&hash);
        }
        hash = dsha256(&concat).to_vec();
        idx /= 2;
    }
    Some(hex::encode(hash))
}

/// Binary sha256 merkle (32-byte hex leaf + siblings): parent = sha256(left||right).
/// Used for EVM deposit-event trees and claim trees.
pub fn merkle_root_binary(leaf_hex: &str, path: &[String], index: u32) -> Option<String> {
    let mut hash = hex::decode(leaf_hex.trim()).ok()?;
    if hash.len() != 32 {
        return None;
    }
    let mut idx = index;
    for sib_hex in path {
        let sib = hex::decode(sib_hex.trim()).ok()?;
        if sib.len() != 32 {
            return None;
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
    Some(hex::encode(hash))
}

/// EVM FalconRailLock Deposit event leaf (matches agent indexer):
/// sha256( tx_hash[32] || log_index_be8 || amount_be8 || lock_id[32] )
pub fn evm_deposit_leaf(tx_hash: &str, log_index: u64, amount: u64, lock_id: &str) -> String {
    let mut buf = Vec::with_capacity(32 + 8 + 8 + 32);
    buf.extend_from_slice(&note_id_bytes32(tx_hash));
    buf.extend_from_slice(&log_index.to_be_bytes());
    buf.extend_from_slice(&amount.to_be_bytes());
    buf.extend_from_slice(&note_id_bytes32(lock_id));
    hex::encode(Sha256::digest(&buf))
}

/// Empty XRP deposit tree (no protocol payments in ledger).
pub fn empty_xrp_deposit_root() -> String {
    hex::encode(Sha256::digest(b"falcon-xrp-deposit-empty-v1"))
}

/// XRPL protocol Payment leaf (matches agent indexer):
/// sha256( tx_hash[32] || amount_be8 || lock_id[32] || sha256(destination) )
pub fn xrp_deposit_leaf(tx_hash: &str, amount: u64, lock_id: &str, destination: &str) -> String {
    let mut buf = Vec::with_capacity(32 + 8 + 32 + 32);
    buf.extend_from_slice(&note_id_bytes32(tx_hash));
    buf.extend_from_slice(&amount.to_be_bytes());
    buf.extend_from_slice(&note_id_bytes32(lock_id));
    buf.extend_from_slice(&Sha256::digest(destination.as_bytes()));
    hex::encode(Sha256::digest(&buf))
}

/// XRP header bind: sha256( ledger_tx_root[32] || deposit_root[32] ) — same shape as EVM bind.
pub fn xrp_bind_root(ledger_tx_root_hex: &str, deposit_root_hex: &str) -> String {
    evm_bind_root(ledger_tx_root_hex, deposit_root_hex)
}

/// Family-aware merkle root for deposit verification.
pub fn merkle_root_from_proof(
    family: RailFamily,
    leaf: &str,
    path: &[String],
    index: u32,
) -> Option<String> {
    match family {
        RailFamily::Bitcoin => merkle_root_bitcoin(leaf, path, index),
        // EVM + XRP protocol payment trees use binary sha256.
        RailFamily::Evm | RailFamily::XrpLedger => merkle_root_binary(leaf, path, index),
    }
}

pub fn is_protocol_rail(asset: &str) -> bool {
    GENESIS_RAILS.contains(&asset) || matches!(asset, RAIL_BTC | RAIL_ETH | RAIL_BNB | RAIL_XRP)
}

pub fn default_rail_configs() -> BTreeMap<AssetId, RailConfig> {
    let mut m = BTreeMap::new();
    for &rail in GENESIS_RAILS {
        let family = RailFamily::for_rail(rail).expect("genesis rail family");
        m.insert(
            rail.to_string(),
            RailConfig {
                asset: rail.to_string(),
                family,
                min_confirmations: DEFAULT_MIN_CONFIRMATIONS,
                bridge_account: bridge_account(rail),
                lock_id: genesis_lock_id(rail),
                total_minted: 0,
                total_burned: 0,
                tip_height: 0,
                tip_hash: String::new(),
                tip_work: 0,
                claim_root: empty_claim_root(),
                claim_note_count: 0,
                claim_height: 0,
            },
        );
    }
    m
}

/// Bitcoin display txid (RPC/Esplora) → internal merkle leaf hex (byte-reversed).
pub fn btc_txid_to_internal_leaf(display_hex: &str) -> Option<String> {
    let t = display_hex.trim().trim_start_matches("0x");
    let b = hex::decode(t).ok()?;
    if b.len() != 32 {
        return None;
    }
    let rev: Vec<u8> = b.into_iter().rev().collect();
    Some(hex::encode(rev))
}

/// Resolve the merkle leaf for a deposit proof (family-specific).
pub fn deposit_proof_leaf(rail: &RailConfig, proof: &BridgeProof, amount: u64) -> Option<String> {
    match rail.family {
        RailFamily::Bitcoin => {
            // external_txid is display-order txid (Blockstream/Esplora); merkle uses internal order.
            btc_txid_to_internal_leaf(&proof.external_txid)
                .or_else(|| Some(proof.external_txid.clone()))
        }
        RailFamily::Evm => {
            // external_txid = "{txhash}:{log_index}" or raw 32-byte leaf hex
            if let Some((tx, li)) = proof.external_txid.split_once(':') {
                let log_index: u64 = li.parse().ok()?;
                Some(evm_deposit_leaf(tx, log_index, amount, &proof.lock_id))
            } else if proof.external_txid.len() == 64
                && hex::decode(&proof.external_txid).map(|b| b.len() == 32).unwrap_or(false)
            {
                Some(proof.external_txid.clone())
            } else {
                Some(evm_deposit_leaf(
                    &proof.external_txid,
                    0,
                    amount,
                    &proof.lock_id,
                ))
            }
        }
        RailFamily::XrpLedger => {
            // external_txid = "{txhash}" or "{txhash}:{dest}"; destination in external_to or suffix
            let (tx, dest) = if let Some((t, d)) = proof.external_txid.split_once(':') {
                // if second part looks like hex index only, dest from external_to
                if d.chars().all(|c| c.is_ascii_digit()) {
                    (t, proof.external_to.as_str())
                } else {
                    (t, d)
                }
            } else {
                (proof.external_txid.as_str(), proof.external_to.as_str())
            };
            let dest = if dest.is_empty() { "xrp:lock" } else { dest };
            Some(xrp_deposit_leaf(tx, amount, &proof.lock_id, dest))
        }
    }
}

/// Verify deposit proof against rail tip + header store.
pub fn verify_deposit(
    rail: &RailConfig,
    headers: &BTreeMap<u64, RailHeader>,
    proof: &BridgeProof,
    amount: u64,
    spent: &BTreeSet<String>,
) -> bool {
    if amount == 0 || proof.external_txid.is_empty() || proof.block_hash.is_empty() {
        return false;
    }
    if proof.lock_id != rail.lock_id {
        return false;
    }
    if spent.contains(&proof.external_txid) {
        return false;
    }
    if rail.tip_height == 0 || proof.height > rail.tip_height {
        return false;
    }
    if rail.tip_height.saturating_sub(proof.height) < rail.min_confirmations {
        return false;
    }
    let Some(hdr) = headers.get(&proof.height) else {
        return false;
    };
    if hdr.hash != proof.block_hash {
        return false;
    }
    let Some(leaf) = deposit_proof_leaf(rail, proof, amount) else {
        return false;
    };
    let Some(tree_root) =
        merkle_root_from_proof(rail.family, &leaf, &proof.merkle_path, proof.merkle_index)
    else {
        return false;
    };

    match rail.family {
        // EVM / XRP: path proves into deposit tree; header.merkle_root binds tx_root‖deposit_root.
        RailFamily::Evm | RailFamily::XrpLedger => {
            let deposit_root = if !proof.merkle_root.is_empty() {
                proof.merkle_root.clone()
            } else {
                tree_root.clone()
            };
            if tree_root != deposit_root {
                return false;
            }
            if hdr.tx_root.is_empty() {
                return false;
            }
            let bind = evm_bind_root(&hdr.tx_root, &deposit_root);
            bind == hdr.merkle_root
        }
        _ => {
            tree_root == hdr.merkle_root
                || (tree_root == proof.merkle_root && proof.merkle_root == hdr.merkle_root)
        }
    }
}

/// Synthetic inclusion for protocol-pairing families (ETH/BNB/XRP substrate + legacy tests).
pub fn synthetic_inclusion(leaf: &str, siblings: &[&str], index: u32) -> (Vec<String>, String) {
    let path: Vec<String> = siblings.iter().map(|s| (*s).to_string()).collect();
    let root = merkle_root_protocol(leaf, &path, index);
    (path, root)
}

/// Synthetic Bitcoin-style inclusion: leaf/siblings are 32-byte hex hashes.
pub fn synthetic_inclusion_bitcoin(
    leaf_hex: &str,
    siblings_hex: &[&str],
    index: u32,
) -> Option<(Vec<String>, String)> {
    let path: Vec<String> = siblings_hex.iter().map(|s| (*s).to_string()).collect();
    let root = merkle_root_bitcoin(leaf_hex, &path, index)?;
    Some((path, root))
}

/// Random-looking but deterministic 32-byte hex from a label (for local BTC synthetic proofs).
pub fn synth_hash32(label: &str) -> String {
    let h = Sha256::digest(label.as_bytes());
    hex::encode(h)
}

/// Empty claim tree root (no open withdrawals).
pub fn empty_claim_root() -> String {
    hex::encode(Sha256::digest(b"falcon-claim-root-empty-v1"))
}

/// 32-byte note id as fixed bytes (hex note_id or hash of string).
pub fn note_id_bytes32(note_id: &str) -> [u8; 32] {
    let t = note_id.trim_start_matches("0x");
    if let Ok(b) = hex::decode(t) {
        if b.len() == 32 {
            let mut out = [0u8; 32];
            out.copy_from_slice(&b);
            return out;
        }
    }
    let h = Sha256::digest(note_id.as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&h);
    out
}

/// Claim leaf matching FalconRailLock.sol:
/// sha256(noteId[32] || amount_be8 || height_be8 || sha256(external_to) || sha256(rail))
pub fn withdrawal_claim_leaf(note: &WithdrawalNote) -> String {
    let mut buf = Vec::with_capacity(32 + 8 + 8 + 32 + 32);
    buf.extend_from_slice(&note_id_bytes32(&note.note_id));
    buf.extend_from_slice(&note.amount.to_be_bytes());
    buf.extend_from_slice(&note.falcon_height.to_be_bytes());
    buf.extend_from_slice(&Sha256::digest(note.external_to.as_bytes()));
    buf.extend_from_slice(&Sha256::digest(note.rail.as_bytes()));
    hex::encode(Sha256::digest(&buf))
}

/// Parent node: sha256(left[32] || right[32]) — matches Solidity sha256(abi.encodePacked(a,b)).
fn claim_parent_hex(left_hex: &str, right_hex: &str) -> String {
    let l = hex::decode(left_hex).unwrap_or_default();
    let r = hex::decode(right_hex).unwrap_or_default();
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&l);
    buf.extend_from_slice(&r);
    hex::encode(Sha256::digest(&buf))
}

/// Sorted open notes (by note_id) optionally filtered by rail.
pub fn sorted_withdrawals<'a>(
    notes: &'a BTreeMap<HashHex, WithdrawalNote>,
    rail: Option<&str>,
) -> Vec<&'a WithdrawalNote> {
    let mut v: Vec<&WithdrawalNote> = notes
        .values()
        .filter(|n| rail.map(|r| n.rail == r).unwrap_or(true))
        .collect();
    v.sort_by(|a, b| a.note_id.cmp(&b.note_id));
    v
}

/// Merkle root over claim leaves (binary sha256) for Falcon commit + EVM verify.
pub fn withdrawal_claim_root(notes: &[&WithdrawalNote]) -> String {
    if notes.is_empty() {
        return empty_claim_root();
    }
    let leaves: Vec<String> = notes.iter().map(|n| withdrawal_claim_leaf(n)).collect();
    merkle_root_of_claim_leaves(&leaves)
}

/// Merkle proof (siblings + index) for `note_id` under claim tree.
pub fn withdrawal_claim_proof(
    notes: &[&WithdrawalNote],
    note_id: &str,
) -> Option<(Vec<String>, u32, String)> {
    let leaves: Vec<String> = notes.iter().map(|n| withdrawal_claim_leaf(n)).collect();
    let idx = notes.iter().position(|n| n.note_id == note_id)? as u32;
    let path = merkle_proof_claim(&leaves, idx as usize)?;
    let root = merkle_root_of_claim_leaves(&leaves);
    Some((path, idx, root))
}

/// Verify a claim proof against a root (agent / smoke / EVM preflight).
pub fn verify_claim_proof(leaf_hex: &str, path: &[String], index: u32, root_hex: &str) -> bool {
    let mut hash = leaf_hex.to_string();
    let mut idx = index;
    for sib in path {
        if idx % 2 == 0 {
            hash = claim_parent_hex(&hash, sib);
        } else {
            hash = claim_parent_hex(sib, &hash);
        }
        idx /= 2;
    }
    hash == root_hex
}

fn merkle_root_of_claim_leaves(leaves: &[String]) -> String {
    if leaves.is_empty() {
        return empty_claim_root();
    }
    let mut layer = leaves.to_vec();
    while layer.len() > 1 {
        if layer.len() % 2 == 1 {
            if let Some(last) = layer.last().cloned() {
                layer.push(last);
            }
        }
        let mut next = Vec::with_capacity(layer.len() / 2);
        for i in (0..layer.len()).step_by(2) {
            next.push(claim_parent_hex(&layer[i], &layer[i + 1]));
        }
        layer = next;
    }
    layer[0].clone()
}

fn merkle_proof_claim(leaves: &[String], mut index: usize) -> Option<Vec<String>> {
    if leaves.is_empty() || index >= leaves.len() {
        return None;
    }
    let mut layer = leaves.to_vec();
    let mut path = Vec::new();
    while layer.len() > 1 {
        if layer.len() % 2 == 1 {
            if let Some(last) = layer.last().cloned() {
                layer.push(last);
            }
        }
        let sib = if index % 2 == 0 {
            layer.get(index + 1).cloned()
        } else {
            layer.get(index - 1).cloned()
        }?;
        path.push(sib);
        let mut next = Vec::with_capacity(layer.len() / 2);
        for i in (0..layer.len()).step_by(2) {
            next.push(claim_parent_hex(&layer[i], &layer[i + 1]));
        }
        layer = next;
        index /= 2;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_merkle_roundtrip() {
        let leaf = "txid_abc";
        let (path, root) = synthetic_inclusion(leaf, &["sib0", "sib1"], 0);
        assert_eq!(merkle_root_protocol(leaf, &path, 0), root);
    }

    #[test]
    fn bitcoin_merkle_roundtrip() {
        let leaf = synth_hash32("tx1");
        let s0 = synth_hash32("s0");
        let s1 = synth_hash32("s1");
        let (path, root) = synthetic_inclusion_bitcoin(&leaf, &[&s0, &s1], 0).unwrap();
        assert_eq!(merkle_root_bitcoin(&leaf, &path, 0).unwrap(), root);
        assert_eq!(root.len(), 64);
    }

    #[test]
    fn btc_display_txid_endian_and_verify() {
        let internal = synth_hash32("btc_internal_tx");
        let display = btc_txid_to_internal_leaf(&internal).unwrap(); // reverse once
        let back = btc_txid_to_internal_leaf(&display).unwrap();
        assert_eq!(back, internal);
        let s0 = synth_hash32("s0");
        let s1 = synth_hash32("s1");
        let (path, root) = synthetic_inclusion_bitcoin(&internal, &[&s0, &s1], 0).unwrap();
        let rail = RailConfig {
            asset: "BTC".into(),
            family: RailFamily::Bitcoin,
            min_confirmations: 0,
            bridge_account: "bridge:BTC".into(),
            lock_id: genesis_lock_id("BTC"),
            total_minted: 0,
            total_burned: 0,
            tip_height: 1,
            tip_hash: "bh1".into(),
            tip_work: 1,
            claim_root: empty_claim_root(),
            claim_note_count: 0,
            claim_height: 0,
        };
        let mut headers = BTreeMap::new();
        headers.insert(
            1,
            RailHeader {
                height: 1,
                hash: "bh1".into(),
                parent_hash: String::new(),
                merkle_root: root.clone(),
                tx_root: String::new(),
                work: 1,
            },
        );
        let proof = BridgeProof {
            external_txid: display, // display-order
            block_hash: "bh1".into(),
            height: 1,
            merkle_path: path,
            merkle_index: 0,
            lock_id: genesis_lock_id("BTC"),
            merkle_root: root,
            ..Default::default()
        };
        assert!(verify_deposit(
            &rail,
            &headers,
            &proof,
            1000,
            &BTreeSet::new()
        ));
    }

    #[test]
    fn bnb_rail_is_evm_family() {
        let cfg = default_rail_configs();
        let bnb = cfg.get("BNB").expect("BNB genesis rail");
        assert_eq!(bnb.family, RailFamily::Evm);
        assert_eq!(bnb.lock_id, genesis_lock_id("BNB"));
        assert_ne!(bnb.lock_id, genesis_lock_id("ETH"));
    }

    #[test]
    fn xrp_rail_is_xrp_ledger_family() {
        let cfg = default_rail_configs();
        let xrp = cfg.get("XRP").expect("XRP genesis rail");
        assert_eq!(xrp.family, RailFamily::XrpLedger);
        assert_eq!(xrp.lock_id, genesis_lock_id("XRP"));
        assert_ne!(xrp.lock_id, genesis_lock_id("BTC"));
    }

    #[test]
    fn xrp_deposit_tree_roundtrip() {
        let lock = genesis_lock_id("XRP");
        let dest = "rN7n7otQDd6FczFgLdlqtyMVrn3e5u5xK";
        let leaf = xrp_deposit_leaf(&"cc".repeat(32), 5000, &lock, dest);
        let dep_root = leaf.clone(); // single-leaf tree
        let tx_root = synth_hash32("xrp_ledger_txroot");
        let bind = xrp_bind_root(&tx_root, &dep_root);
        let rail = RailConfig {
            asset: "XRP".into(),
            family: RailFamily::XrpLedger,
            min_confirmations: 0,
            bridge_account: "bridge:XRP".into(),
            lock_id: lock.clone(),
            total_minted: 0,
            total_burned: 0,
            tip_height: 1,
            tip_hash: "L1".into(),
            tip_work: 1,
            claim_root: empty_claim_root(),
            claim_note_count: 0,
            claim_height: 0,
        };
        let mut headers = BTreeMap::new();
        headers.insert(
            1,
            RailHeader {
                height: 1,
                hash: "L1".into(),
                parent_hash: String::new(),
                merkle_root: bind,
                tx_root: tx_root.clone(),
                work: 1,
            },
        );
        let proof = BridgeProof {
            external_txid: format!("{}:{}", "cc".repeat(32), dest),
            block_hash: "L1".into(),
            height: 1,
            merkle_path: vec![],
            merkle_index: 0,
            lock_id: lock,
            merkle_root: dep_root,
            tx_root,
            external_to: dest.into(),
            ..Default::default()
        };
        assert!(verify_deposit(
            &rail,
            &headers,
            &proof,
            5000,
            &BTreeSet::new()
        ));
    }

    #[test]
    fn evm_deposit_tree_roundtrip() {
        let lock = genesis_lock_id("ETH");
        let leaf0 = evm_deposit_leaf("aa".repeat(32).as_str(), 0, 1000, &lock);
        let leaf1 = evm_deposit_leaf("bb".repeat(32).as_str(), 1, 2000, &lock);
        let mut concat = hex::decode(&leaf0).unwrap();
        concat.extend_from_slice(&hex::decode(&leaf1).unwrap());
        let dep_root = hex::encode(Sha256::digest(&concat));
        assert_eq!(
            merkle_root_binary(&leaf0, &[leaf1.clone()], 0).unwrap(),
            dep_root
        );
        let tx_root = synth_hash32("txroot");
        let bind = evm_bind_root(&tx_root, &dep_root);
        let rail = RailConfig {
            asset: "ETH".into(),
            family: RailFamily::Evm,
            min_confirmations: 0,
            bridge_account: "bridge:ETH".into(),
            lock_id: lock.clone(),
            total_minted: 0,
            total_burned: 0,
            tip_height: 1,
            tip_hash: "h1".into(),
            tip_work: 1,
            claim_root: empty_claim_root(),
            claim_note_count: 0,
            claim_height: 0,
        };
        let mut headers = BTreeMap::new();
        headers.insert(
            1,
            RailHeader {
                height: 1,
                hash: "h1".into(),
                parent_hash: String::new(),
                merkle_root: bind,
                tx_root: tx_root.clone(),
                work: 1,
            },
        );
        let proof = BridgeProof {
            external_txid: format!("{}:0", "aa".repeat(32)),
            block_hash: "h1".into(),
            height: 1,
            merkle_path: vec![leaf1],
            merkle_index: 0,
            lock_id: lock,
            merkle_root: dep_root,
            tx_root,
            ..Default::default()
        };
        assert!(verify_deposit(
            &rail,
            &headers,
            &proof,
            1000,
            &BTreeSet::new()
        ));
    }

    #[test]
    fn withdrawal_claim_tree() {
        let mut notes = BTreeMap::new();
        for i in 0..3u64 {
            let id = hex::encode(Sha256::digest(format!("note{i}").as_bytes()));
            notes.insert(
                id.clone(),
                WithdrawalNote {
                    note_id: id,
                    rail: "ETH".into(),
                    amount: 100 + i,
                    from: "acct1".into(),
                    external_to: format!("0x{:040x}", i),
                    falcon_height: i,
                },
            );
        }
        let sorted = sorted_withdrawals(&notes, Some("ETH"));
        assert_eq!(sorted.len(), 3);
        let root = withdrawal_claim_root(&sorted);
        let target = sorted[1].note_id.clone();
        let (path, idx, root2) = withdrawal_claim_proof(&sorted, &target).unwrap();
        assert_eq!(root, root2);
        let leaf = withdrawal_claim_leaf(sorted[idx as usize]);
        assert!(verify_claim_proof(&leaf, &path, idx, &root));
    }

    /// Golden vectors shared with FalconRailLock.sol / claim_codec_smoke / Python agents.
    /// leaf = sha256(noteId[32] || amount_be8 || height_be8 || sha256(external_to) || sha256(rail))
    #[test]
    fn claim_leaf_golden_matches_solidity_spec() {
        let n1 = WithdrawalNote {
            note_id: "11".repeat(32),
            rail: "BTC".into(),
            amount: 7000,
            from: "acct2".into(),
            external_to: "bc1q_restart_out".into(),
            falcon_height: 9,
        };
        let n2 = WithdrawalNote {
            note_id: "22".repeat(32),
            rail: "ETH".into(),
            amount: 1500,
            from: "acct3".into(),
            external_to: "0xeth_smoke_out".into(),
            falcon_height: 12,
        };
        let n3 = WithdrawalNote {
            note_id: "33".repeat(32),
            rail: "BNB".into(),
            amount: 1000,
            from: "acct4".into(),
            external_to: "0xbnb_smoke_out".into(),
            falcon_height: 15,
        };
        let leaf1 = withdrawal_claim_leaf(&n1);
        let leaf2 = withdrawal_claim_leaf(&n2);
        let leaf3 = withdrawal_claim_leaf(&n3);
        assert_eq!(
            leaf1,
            "9ae206ffe6f284b3c64883949436e6e21cb6da3ebac76fb09c7b588a8cff4e85"
        );
        assert_eq!(
            leaf2,
            "939f90ad03104b5c2a1381b4c814352f367f99bd4ab4868596b2101770058132"
        );
        assert_eq!(
            leaf3,
            "1a76906c79499f0a65c6b219910493a418e5adaa35041f2c135ae8fd7578d594"
        );
        assert_eq!(withdrawal_claim_root(&[&n1]), leaf1);
        assert_eq!(
            withdrawal_claim_root(&[&n1, &n2]),
            "66b64c675f1a89617e23dfc575832d324a5cd50fdef8ce62c690fd3726ad3823"
        );
        assert_eq!(
            withdrawal_claim_root(&[&n1, &n2, &n3]),
            "d83931a298ae420ff828525c844f810690464a2a1dae74dd6ddaa11d92be4a8a"
        );
        assert_eq!(
            empty_claim_root(),
            "55eabc17e925fa0d01fbfcb220fecafce4dab37a12b5a165642a760c0167845c"
        );
        let (path, idx, root) = withdrawal_claim_proof(&[&n1, &n2, &n3], &n2.note_id).unwrap();
        assert_eq!(idx, 1);
        assert!(verify_claim_proof(&leaf2, &path, idx, &root));
        assert_eq!(
            root,
            "d83931a298ae420ff828525c844f810690464a2a1dae74dd6ddaa11d92be4a8a"
        );
    }
}
