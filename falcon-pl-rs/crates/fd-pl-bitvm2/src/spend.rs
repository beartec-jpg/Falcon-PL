//! BitVM2 instance spend graph — no watch key, no unsound Disprove leaves.
//!
//! NUMS key-path is unspendable. Instance script paths:
//! - **Kickoff:** claimer BIP340 CHECKSIG (no CSV). Policy: dest-lock + change-to-instance.
//! - **Challenge:** setup n-of-n CHECKSIG (presigned once; output is the Assert connector)
//!
//! Assert connector:
//! - **Operator recover:** CSV + operator CHECKSIG (claim stands; Challenge cannot steal)
//! - **Setup n-of-n:** emergency
//! - **Lamport equivocation:** one leaf per bit; spendable only if operator Asserts twice
//!
//! C.1a SHA256 inequality leaves are **not** on the instance (they are anyone-spend).

use crate::instance::TestnetInstance;
use crate::protocol::InstanceConfig;
use crate::verifier_split::{merkle_path, merkle_root, tapleaf_hash};
use crate::winternitz::{self, equivocation_script, LamportPk, LAMPORT_BITS};
use serde::{Deserialize, Serialize};

/// NUMS seed for the BitVM2 instance (distinct from the legacy Kickoff pool).
pub const INSTANCE_NUMS_TAG: &[u8] = b"FalconPL-BitVM2-Instance-2300";

/// Testnet default ≈ 24h at 10 min blocks.
pub const PAYOUT_CSV_BLOCKS: u32 = 144;
/// Assert-timeout on the connector ≈ 12h.
pub const ASSERT_CSV_BLOCKS: u32 = 72;

pub const KIND_PAYOUT: &str = "payout_optimistic";
pub const KIND_KICKOFF: &str = "kickoff";
pub const KIND_SETUP: &str = "setup_nofn";
pub const KIND_OPERATOR_RECOVER: &str = "operator_recover";
pub const KIND_EQUIVOCATION: &str = "lamport_equivocation";

/// CScriptNum push for CSV / BIP68 (sign-bit safe; 144 is `0x90 0x00`).
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

fn push_xonly(xonly: &[u8; 32]) -> Vec<u8> {
    let mut s = vec![0x20];
    s.extend_from_slice(xonly);
    s
}

/// `<csv> OP_CSV OP_DROP <operator_xonly> OP_CHECKSIG` — Assert recover only, not instance take.
pub fn payout_optimistic_script(csv_blocks: u32, operator_xonly: &[u8; 32]) -> Vec<u8> {
    let mut s = push_csv(csv_blocks);
    s.push(0xb2); // OP_CHECKSEQUENCEVERIFY
    s.push(0x75); // OP_DROP
    s.extend_from_slice(&push_xonly(operator_xonly));
    s.push(0xac); // OP_CHECKSIG (BIP342 Schnorr)
    s
}

/// Kickoff: `<claimer_xonly> OP_CHECKSIG`. Immediate. Dest-lock is policy + Challenge.
pub fn kickoff_script(claimer_xonly: &[u8; 32]) -> Vec<u8> {
    let mut s = push_xonly(claimer_xonly);
    s.push(0xac);
    s
}

/// n-of-n tapscript: `<pk0> CHECKSIGVERIFY … <pkN-1> CHECKSIG`. Witness: sig0..sigN-1.
pub fn setup_nofn_script(setup_xonly: &[[u8; 32]]) -> Result<Vec<u8>, String> {
    if setup_xonly.len() < 2 {
        return Err("setup n-of-n needs ≥2 x-only keys".into());
    }
    let mut s = Vec::new();
    for (i, pk) in setup_xonly.iter().enumerate() {
        s.extend_from_slice(&push_xonly(pk));
        if i + 1 == setup_xonly.len() {
            s.push(0xac); // CHECKSIG
        } else {
            s.push(0xad); // CHECKSIGVERIFY
        }
    }
    Ok(s)
}

/// Assert connector: after Challenge, operator recovers with the same CSV+CHECKSIG.
pub fn operator_recover_script(csv_blocks: u32, operator_xonly: &[u8; 32]) -> Vec<u8> {
    payout_optimistic_script(csv_blocks, operator_xonly)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpendLeaf {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_id: Option<u8>,
    pub script_hex: String,
    pub tapleaf_hash_hex: String,
    pub merkle_path_hex: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceTapTree {
    pub operator_xonly_hex: String,
    pub csv_blocks: u32,
    pub setup_xonly_hex: Vec<String>,
    pub leaves: Vec<SpendLeaf>,
    pub merkle_root_hex: String,
    pub nums_tag: String,
    pub notes: String,
}

impl InstanceTapTree {
    pub fn leaf(&self, kind: &str) -> Option<&SpendLeaf> {
        self.leaves.iter().find(|l| l.kind == kind)
    }

    pub fn merkle_root(&self) -> Result<[u8; 32], String> {
        decode32(&self.merkle_root_hex)
    }

    pub fn control_nodes(&self, kind: &str) -> Result<Vec<[u8; 32]>, String> {
        let leaf = self.leaf(kind).ok_or_else(|| format!("no leaf {kind}"))?;
        leaf.merkle_path_hex.iter().map(|h| decode32(h)).collect()
    }

    pub fn script(&self, kind: &str) -> Result<Vec<u8>, String> {
        let leaf = self.leaf(kind).ok_or_else(|| format!("no leaf {kind}"))?;
        hex::decode(&leaf.script_hex).map_err(|e| e.to_string())
    }

    pub fn tapleaf_hash(&self, kind: &str) -> Result<[u8; 32], String> {
        let leaf = self.leaf(kind).ok_or_else(|| format!("no leaf {kind}"))?;
        decode32(&leaf.tapleaf_hash_hex)
    }
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

fn parse_xonly(h: &str) -> Result<[u8; 32], String> {
    decode32(h)
}

fn finish_leaves(scripts: Vec<(String, Option<u8>, Vec<u8>)>) -> Result<Vec<SpendLeaf>, String> {
    let hashes: Vec<[u8; 32]> = scripts.iter().map(|(_, _, s)| tapleaf_hash(s)).collect();
    let mut leaves = Vec::with_capacity(scripts.len());
    for (i, (kind, chunk, script)) in scripts.into_iter().enumerate() {
        let th = hashes[i];
        let path = merkle_path(&hashes, i)?;
        leaves.push(SpendLeaf {
            kind,
            chunk_id: chunk,
            script_hex: hex::encode(script),
            tapleaf_hash_hex: hex::encode(th),
            merkle_path_hex: path.iter().map(|p| hex::encode(p)).collect(),
        });
    }
    Ok(leaves)
}

/// Instance taptree: kickoff (claimer) + setup n-of-n. No operator-to-self payout.
pub fn build_instance_taptree(
    operator_xonly: &[u8; 32],
    csv_blocks: u32,
    setup_xonly: &[[u8; 32]],
) -> Result<InstanceTapTree, String> {
    if csv_blocks == 0 {
        return Err("dest-lock / assert CSV must be > 0".into());
    }
    let scripts: Vec<(String, Option<u8>, Vec<u8>)> = vec![
        (
            KIND_KICKOFF.into(),
            None,
            kickoff_script(operator_xonly),
        ),
        (
            KIND_SETUP.into(),
            None,
            setup_nofn_script(setup_xonly)?,
        ),
    ];
    let hashes: Vec<[u8; 32]> = scripts.iter().map(|(_, _, s)| tapleaf_hash(s)).collect();
    let root = merkle_root(&hashes);
    let leaves = finish_leaves(scripts)?;
    Ok(InstanceTapTree {
        operator_xonly_hex: hex::encode(operator_xonly),
        csv_blocks,
        setup_xonly_hex: setup_xonly.iter().map(|k| hex::encode(k)).collect(),
        leaves,
        merkle_root_hex: hex::encode(root),
        nums_tag: String::from_utf8_lossy(INSTANCE_NUMS_TAG).into_owned(),
        notes: "NUMS key-path dead. Kickoff=claimer CHECKSIG dest-lock. Challenge=setup n-of-n → Assert (presign once, delete keys). No watch. No operator-to-self.".into(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssertConnectorTree {
    pub csv_blocks: u32,
    pub setup_xonly_hex: Vec<String>,
    pub leaves: Vec<SpendLeaf>,
    pub merkle_root_hex: String,
    pub nums_tag: String,
    pub notes: String,
}

impl AssertConnectorTree {
    pub fn leaf(&self, kind: &str) -> Option<&SpendLeaf> {
        self.leaves.iter().find(|l| l.kind == kind)
    }
    pub fn merkle_root(&self) -> Result<[u8; 32], String> {
        decode32(&self.merkle_root_hex)
    }
    pub fn control_nodes(&self, kind: &str) -> Result<Vec<[u8; 32]>, String> {
        let leaf = self.leaf(kind).ok_or_else(|| format!("no leaf {kind}"))?;
        leaf.merkle_path_hex.iter().map(|h| decode32(h)).collect()
    }
    pub fn script(&self, kind: &str) -> Result<Vec<u8>, String> {
        let leaf = self.leaf(kind).ok_or_else(|| format!("no leaf {kind}"))?;
        hex::decode(&leaf.script_hex).map_err(|e| e.to_string())
    }
    pub fn tapleaf_hash(&self, kind: &str) -> Result<[u8; 32], String> {
        let leaf = self.leaf(kind).ok_or_else(|| format!("no leaf {kind}"))?;
        decode32(&leaf.tapleaf_hash_hex)
    }
}

pub fn build_assert_connector_taptree(
    setup_xonly: &[[u8; 32]],
    csv_blocks: u32,
    operator_xonly: &[u8; 32],
    lamport: Option<&LamportPk>,
) -> Result<AssertConnectorTree, String> {
    if csv_blocks == 0 {
        return Err("assert CSV must be > 0".into());
    }
    let mut scripts = vec![
        (
            KIND_SETUP.into(),
            None,
            setup_nofn_script(setup_xonly)?,
        ),
        (
            KIND_OPERATOR_RECOVER.into(),
            None,
            operator_recover_script(csv_blocks, operator_xonly),
        ),
    ];
    if let Some(pk) = lamport {
        if pk.zero_hex.len() != LAMPORT_BITS {
            return Err("lamport pk bits".into());
        }
        for i in 0..LAMPORT_BITS {
            let (a, b) = pk.bit_pks(i)?;
            scripts.push((
                KIND_EQUIVOCATION.into(),
                Some(i as u8),
                equivocation_script(&a, &b),
            ));
        }
    }
    let hashes: Vec<[u8; 32]> = scripts.iter().map(|(_, _, s)| tapleaf_hash(s)).collect();
    let root = merkle_root(&hashes);
    let leaves = finish_leaves(scripts)?;
    let lamport_note = if lamport.is_some() {
        format!(
            "Lamport equivocation {} bits (vk={}).",
            LAMPORT_BITS,
            hex::encode(winternitz::vk_root(lamport.unwrap()))
        )
    } else {
        "No Lamport keys (tests).".into()
    };
    Ok(AssertConnectorTree {
        csv_blocks,
        setup_xonly_hex: setup_xonly.iter().map(|k| hex::encode(k)).collect(),
        leaves,
        merkle_root_hex: hex::encode(root),
        nums_tag: String::from_utf8_lossy(INSTANCE_NUMS_TAG).into_owned(),
        notes: format!(
            "Challenge vout0. Operator recover=CSV+operator (no OP_TRUE). Setup n-of-n. {lamport_note}"
        ),
    })
}

pub fn parse_setup_xonly(hexes: &[String]) -> Result<Vec<[u8; 32]>, String> {
    hexes.iter().map(|h| parse_xonly(h)).collect()
}

pub fn parse_operator_xonly(h: &str) -> Result<[u8; 32], String> {
    parse_xonly(h)
}

/// Skeleton instance with a concrete taptree (Phase D.1).
pub fn funded_skeleton(
    config: InstanceConfig,
    operator_xonly_hex: &str,
    setup_xonly_hex: &[String],
    csv_blocks: u32,
) -> Result<(TestnetInstance, InstanceTapTree), String> {
    let op = parse_operator_xonly(operator_xonly_hex)?;
    let setup = parse_setup_xonly(setup_xonly_hex)?;
    let tree = build_instance_taptree(&op, csv_blocks, &setup)?;
    let mut inst = TestnetInstance::skeleton(config, setup_xonly_hex.to_vec());
    inst.notes = format!(
        "BitVM2 instance taptree root={} dest-lock csv={csv_blocks} (kickoff+challenge, no watch).",
        tree.merkle_root_hex
    );
    Ok((inst, tree))
}

/// True if `script` commits `xonly` as a 32-byte push (pubkey in the leaf).
pub fn script_commits_xonly(script: &[u8], xonly: &[u8; 32]) -> bool {
    script.windows(32).any(|w| w == xonly.as_slice())
}

pub fn instance_has_only_safe_leaves(tree: &InstanceTapTree) -> bool {
    tree.leaves
        .iter()
        .all(|l| l.kind == KIND_KICKOFF || l.kind == KIND_SETUP || l.kind == KIND_PAYOUT)
}
