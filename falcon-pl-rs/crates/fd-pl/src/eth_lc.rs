//! Ethereum PoS light client (Altair sync committee) for Sepolia.
//!
//! Bootstrap is a weak-subjectivity checkpoint (genesis validators root +
//! committee at a slot). Updates are BLS-verified. Watchers relay; they do
//! not mint.
//!
//! **Gloas (v2.9.60, height-gated by the caller).** Sepolia activated Gloas
//! (fork version `0x90000076`) at epoch 353024 = slot 11296768, the first slot
//! of sync period 1379 (2026-10-06 13:53:36 UTC). Gloas light-client data
//! (consensus-specs `specs/gloas/light-client/sync-protocol.md`):
//! - `LightClientHeader` drops `execution` (the payload header) and carries
//!   only `execution_block_hash` with an `execution_branch` into
//!   `BeaconBlockBody` (EIP-7732): for Gloas slots the leaf is
//!   `signed_execution_payload_bid.message.parent_block_hash`, gindex 2856
//!   (depth 11); pre-Gloas headers *upgraded* to the Gloas format prove
//!   `execution_payload.block_hash`, gindex 812 (Deneb+, depth 9), with the
//!   branch zero-padded ("normalized") to depth 11.
//! - BeaconState gindices move (EIP-7688): finalized root 169 -> 735,
//!   current sync committee 86 -> 2945, next 87 -> 2946, chosen by the slot of
//!   the header whose `state_root` is the proof root.
//! - Signature domain uses the fork at `max(signature_slot, 1) - 1` (spec).
//!
//! A Gloas header proves only the execution block **hash**: block number and
//! receipts root are learned when the RLP-verified execution header with that
//! hash is stored (see `BridgeState::eth_lc_resolve_exec_head`). Until then the
//! LC state reports `execution_number = 0`, `receipts_root = ""`.

use crate::eth_ssz::{
    beacon_header_root, chunk_u64, hash_tree_root_byte_list, hash_tree_root_byte_vector,
    merkleize, parse32, parse_n, verify_merkle_gindex, chunks_byte_vector,
};
use serde::{Deserialize, Serialize};


pub const SEPOLIA_GENESIS_VALIDATORS_ROOT: &str =
    "d8ea171f3c94aea21ebc42a1ed61052acf3f9209c00e4efbaaddac09ed9b8078";
pub const DOMAIN_SYNC_COMMITTEE: [u8; 4] = [0x07, 0x00, 0x00, 0x00];
pub const SYNC_COMMITTEE_SIZE: usize = 512;
pub const SLOTS_PER_EPOCH: u64 = 32;
pub const EPOCHS_PER_SYNC_PERIOD: u64 = 256;
/// Electra/Fulu BeaconState field. Branch depth 6 on Sepolia bootstrap.
pub const CURRENT_SYNC_COMMITTEE_GINDEX: u64 = 86;
pub const NEXT_SYNC_COMMITTEE_GINDEX: u64 = 87;
/// BeaconBlockBody execution_payload, 12–13 fields → depth 4.
pub const EXECUTION_PAYLOAD_GINDEX: u64 = 25;
pub const FINALIZED_ROOT_GINDEX: u64 = 169;

const DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BeaconHeader {
    pub slot: u64,
    pub proposer_index: u64,
    pub parent_root: String,
    pub state_root: String,
    pub body_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecPayload {
    pub parent_hash: String,
    pub fee_recipient: String,
    pub state_root: String,
    pub receipts_root: String,
    pub logs_bloom: String,
    pub prev_randao: String,
    pub block_number: u64,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub timestamp: u64,
    pub extra_data: String,
    pub base_fee_per_gas: String,
    pub block_hash: String,
    pub transactions_root: String,
    pub withdrawals_root: String,
    pub blob_gas_used: u64,
    pub excess_blob_gas: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EthLcState {
    pub genesis_validators_root: String,
    pub finalized_slot: u64,
    pub finalized_beacon_root: String,
    pub execution_block_hash: String,
    pub execution_number: u64,
    pub receipts_root: String,
    pub period: u64,
    pub current_agg_pk: String,
    pub current_pks: Vec<String>,
    pub next_agg_pk: String,
    pub next_pks: Vec<String>,
}

pub fn slot_period(slot: u64) -> u64 {
    slot / (SLOTS_PER_EPOCH * EPOCHS_PER_SYNC_PERIOD)
}

pub fn beacon_root(h: &BeaconHeader) -> Result<[u8; 32], String> {
    Ok(beacon_header_root(
        h.slot,
        h.proposer_index,
        &parse32(&h.parent_root)?,
        &parse32(&h.state_root)?,
        &parse32(&h.body_root)?,
    ))
}

fn bls_pubkey_root(pk: &[u8]) -> Result<[u8; 32], String> {
    if pk.len() != 48 {
        return Err("bls pk 48".into());
    }
    Ok(merkleize(&chunks_byte_vector(pk)))
}

pub fn sync_committee_root(pks: &[String], agg: &str) -> Result<[u8; 32], String> {
    if pks.len() != SYNC_COMMITTEE_SIZE {
        return Err("sync committee 512".into());
    }
    let mut pk_roots = Vec::with_capacity(SYNC_COMMITTEE_SIZE);
    for p in pks {
        pk_roots.push(bls_pubkey_root(&parse_n(p, 48)?)?);
    }
    let pks_root = merkleize(&pk_roots);
    let agg_root = bls_pubkey_root(&parse_n(agg, 48)?)?;
    Ok(merkleize(&[pks_root, agg_root]))
}

fn uint256_chunk(dec_or_hex: &str) -> Result<[u8; 32], String> {
    let s = dec_or_hex.trim();
    if s.starts_with("0x") || s.starts_with("0X") {
        let mut b = parse_n(s, 32)?;
        b.reverse(); // JSON hex is BE; SSZ uint256 is LE
        let mut c = [0u8; 32];
        c.copy_from_slice(&b);
        return Ok(c);
    }
    let n = s.parse::<u128>().map_err(|e| e.to_string())?;
    let mut c = [0u8; 32];
    c[..16].copy_from_slice(&n.to_le_bytes());
    Ok(c)
}

pub fn execution_payload_header_root(e: &ExecPayload) -> Result<[u8; 32], String> {
    Ok(merkleize(&execution_payload_header_fields(e)?))
}

/// The 17 Deneb+ `ExecutionPayloadHeader` field roots, in SSZ order.
pub fn execution_payload_header_fields(e: &ExecPayload) -> Result<[[u8; 32]; 17], String> {
    let fee = parse_n(&e.fee_recipient, 20)?;
    let mut fee_chunk = [0u8; 32];
    fee_chunk[..20].copy_from_slice(&fee);
    let bloom = parse_n(&e.logs_bloom, 256)?;
    let extra = hex::decode(e.extra_data.trim().trim_start_matches("0x")).map_err(|e| e.to_string())?;
    let fields = [
        parse32(&e.parent_hash)?,
        fee_chunk,
        parse32(&e.state_root)?,
        parse32(&e.receipts_root)?,
        hash_tree_root_byte_vector(&bloom),
        parse32(&e.prev_randao)?,
        chunk_u64(e.block_number),
        chunk_u64(e.gas_limit),
        chunk_u64(e.gas_used),
        chunk_u64(e.timestamp),
        hash_tree_root_byte_list(&extra, 32),
        uint256_chunk(&e.base_fee_per_gas)?,
        parse32(&e.block_hash)?,
        parse32(&e.transactions_root)?,
        parse32(&e.withdrawals_root)?,
        chunk_u64(e.blob_gas_used),
        chunk_u64(e.excess_blob_gas),
    ];
    Ok(fields)
}

fn parse_branch(v: &[String]) -> Result<Vec<[u8; 32]>, String> {
    v.iter().map(|s| parse32(s)).collect()
}

fn json_u64(v: &serde_json::Value) -> Result<u64, String> {
    if let Some(n) = v.as_u64() {
        return Ok(n);
    }
    v.as_str()
        .ok_or_else(|| "u64".to_string())?
        .parse::<u64>()
        .map_err(|e| e.to_string())
}

fn json_str(v: &serde_json::Value) -> Result<String, String> {
    Ok(v.as_str().ok_or("str")?.to_string())
}

pub fn parse_beacon_header(v: &serde_json::Value) -> Result<BeaconHeader, String> {
    let m = if v.get("slot").is_some() {
        v
    } else {
        v.get("beacon").ok_or("beacon")?
    };
    Ok(BeaconHeader {
        slot: json_u64(m.get("slot").ok_or("slot")?)?,
        proposer_index: json_u64(m.get("proposer_index").ok_or("proposer")?)?,
        parent_root: json_str(m.get("parent_root").ok_or("parent")?)?,
        state_root: json_str(m.get("state_root").ok_or("state")?)?,
        body_root: json_str(m.get("body_root").ok_or("body")?)?,
    })
}

pub fn parse_exec_payload(v: &serde_json::Value) -> Result<ExecPayload, String> {
    Ok(ExecPayload {
        parent_hash: json_str(v.get("parent_hash").ok_or("ph")?)?,
        fee_recipient: json_str(v.get("fee_recipient").ok_or("fee")?)?,
        state_root: json_str(v.get("state_root").ok_or("sr")?)?,
        receipts_root: json_str(v.get("receipts_root").ok_or("rr")?)?,
        logs_bloom: json_str(v.get("logs_bloom").ok_or("bloom")?)?,
        prev_randao: json_str(v.get("prev_randao").ok_or("randao")?)?,
        block_number: json_u64(v.get("block_number").ok_or("bn")?)?,
        gas_limit: json_u64(v.get("gas_limit").ok_or("gl")?)?,
        gas_used: json_u64(v.get("gas_used").ok_or("gu")?)?,
        timestamp: json_u64(v.get("timestamp").ok_or("ts")?)?,
        extra_data: json_str(v.get("extra_data").ok_or("extra")?)?,
        base_fee_per_gas: json_str(v.get("base_fee_per_gas").ok_or("fee2")?)?,
        block_hash: json_str(v.get("block_hash").ok_or("bh")?)?,
        transactions_root: json_str(v.get("transactions_root").ok_or("tr")?)?,
        withdrawals_root: json_str(v.get("withdrawals_root").ok_or("wr")?)?,
        blob_gas_used: json_u64(v.get("blob_gas_used").ok_or("blob")?)?,
        excess_blob_gas: json_u64(v.get("excess_blob_gas").ok_or("exb")?)?,
    })
}

fn verify_execution_in_body(exec: &ExecPayload, branch: &[String], body_root: &str) -> Result<(), String> {
    let leaf = execution_payload_header_root(exec)?;
    let br = parse_branch(branch)?;
    let root = parse32(body_root)?;
    if !verify_merkle_gindex(&leaf, &br, EXECUTION_PAYLOAD_GINDEX, &root) {
        return Err("execution payload not in beacon body_root".into());
    }
    Ok(())
}

fn verify_committee_in_state(
    pks: &[String],
    agg: &str,
    branch: &[String],
    state_root: &str,
    gindex: u64,
) -> Result<(), String> {
    let leaf = sync_committee_root(pks, agg)?;
    let br = parse_branch(branch)?;
    let root = parse32(state_root)?;
    if !verify_merkle_gindex(&leaf, &br, gindex, &root) {
        return Err("sync committee not in state_root".into());
    }
    Ok(())
}

pub fn apply_bootstrap(json: &str) -> Result<EthLcState, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let data = v.get("data").unwrap_or(&v);
    let header = data.get("header").ok_or("header")?;
    let beacon = parse_beacon_header(header)?;
    let exec = parse_exec_payload(header.get("execution").ok_or("execution")?)?;
    let exec_branch: Vec<String> = header
        .get("execution_branch")
        .and_then(|x| x.as_array())
        .ok_or("execution_branch")?
        .iter()
        .map(|x| json_str(x))
        .collect::<Result<_, _>>()?;
    verify_execution_in_body(&exec, &exec_branch, &beacon.body_root)?;
    let sc = data.get("current_sync_committee").ok_or("committee")?;
    let pks: Vec<String> = sc
        .get("pubkeys")
        .and_then(|x| x.as_array())
        .ok_or("pubkeys")?
        .iter()
        .map(json_str)
        .collect::<Result<_, _>>()?;
    let agg = json_str(sc.get("aggregate_pubkey").ok_or("agg")?)?;
    let sc_branch: Vec<String> = data
        .get("current_sync_committee_branch")
        .and_then(|x| x.as_array())
        .ok_or("sc branch")?
        .iter()
        .map(json_str)
        .collect::<Result<_, _>>()?;
    verify_committee_in_state(&pks, &agg, &sc_branch, &beacon.state_root, CURRENT_SYNC_COMMITTEE_GINDEX)?;
    let root = beacon_root(&beacon)?;
    Ok(EthLcState {
        genesis_validators_root: SEPOLIA_GENESIS_VALIDATORS_ROOT.to_string(),
        finalized_slot: beacon.slot,
        finalized_beacon_root: hex::encode(root),
        execution_block_hash: exec.block_hash.trim_start_matches("0x").to_ascii_lowercase(),
        execution_number: exec.block_number,
        receipts_root: exec.receipts_root.trim_start_matches("0x").to_ascii_lowercase(),
        period: slot_period(beacon.slot),
        current_agg_pk: agg.trim_start_matches("0x").to_ascii_lowercase(),
        current_pks: pks
            .into_iter()
            .map(|s| s.trim_start_matches("0x").to_ascii_lowercase())
            .collect(),
        next_agg_pk: String::new(),
        next_pks: Vec::new(),
    })
}

fn compute_domain(fork_version: [u8; 4], genesis_root: &[u8; 32]) -> [u8; 32] {
    // domain = DOMAIN_TYPE || fork_data_root[0..28]
    // ForkData { current_version, genesis_validators_root }
    let mut ver = [0u8; 32];
    ver[..4].copy_from_slice(&fork_version);
    let fd_root = merkleize(&[ver, *genesis_root]);
    let mut domain = [0u8; 32];
    domain[..4].copy_from_slice(&DOMAIN_SYNC_COMMITTEE);
    domain[4..].copy_from_slice(&fd_root[..28]);
    domain
}

fn signing_root(object_root: &[u8; 32], domain: &[u8; 32]) -> [u8; 32] {
    merkleize(&[*object_root, *domain])
}

fn sepolia_fork_version(slot: u64) -> [u8; 4] {
    let epoch = slot / SLOTS_PER_EPOCH;
    // From /eth/v1/config/fork_schedule
    if epoch >= 272_640 {
        [0x90, 0x00, 0x00, 0x75] // fulu
    } else if epoch >= 222_464 {
        [0x90, 0x00, 0x00, 0x74]
    } else if epoch >= 132_608 {
        [0x90, 0x00, 0x00, 0x73]
    } else if epoch >= 56_832 {
        [0x90, 0x00, 0x00, 0x72]
    } else if epoch >= 100 {
        [0x90, 0x00, 0x00, 0x71]
    } else if epoch >= 50 {
        [0x90, 0x00, 0x00, 0x70]
    } else {
        [0x90, 0x00, 0x00, 0x69]
    }
}

fn bit_set(bits: &[u8], i: usize) -> bool {
    let byte = i / 8;
    let bit = i % 8;
    byte < bits.len() && (bits[byte] & (1 << bit)) != 0
}

fn verify_sync_aggregate(pks: &[String], bits_hex: &str, sig_hex: &str, msg: &[u8; 32]) -> Result<(), String> {
    if pks.len() != SYNC_COMMITTEE_SIZE {
        return Err("pk count".into());
    }
    let bits = hex::decode(bits_hex.trim().trim_start_matches("0x")).map_err(|e| e.to_string())?;
    let sig_b = parse_n(sig_hex, 96)?;
    let mut used = Vec::new();
    for (i, pk_hex) in pks.iter().enumerate() {
        if bit_set(&bits, i) {
            used.push(parse_n(pk_hex, 48)?);
        }
    }
    if used.len() * 3 < SYNC_COMMITTEE_SIZE * 2 {
        return Err(format!("sync bits {} < 2/3", used.len()));
    }
    let sig = blst::min_pk::Signature::from_bytes(&sig_b).map_err(|e| format!("sig {e:?}"))?;
    let pks_parsed: Result<Vec<_>, _> = used
        .iter()
        .map(|b| blst::min_pk::PublicKey::from_bytes(b).map_err(|e| format!("pk {e:?}")))
        .collect();
    let pks_parsed = pks_parsed?;
    let refs: Vec<&blst::min_pk::PublicKey> = pks_parsed.iter().collect();
    let err = sig.fast_aggregate_verify(true, msg, DST, &refs);
    if err != blst::BLST_ERROR::BLST_SUCCESS {
        return Err(format!("bls aggregate {err:?}"));
    }
    Ok(())
}

pub fn apply_update(state: &EthLcState, json: &str) -> Result<EthLcState, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let data = v.get("data").unwrap_or(&v);
    let attested = parse_beacon_header(data.get("attested_header").ok_or("attested")?)?;
    let finalized_h = data.get("finalized_header").ok_or("finalized")?;
    let finalized = parse_beacon_header(finalized_h)?;
    let exec = parse_exec_payload(finalized_h.get("execution").ok_or("fin exec")?)?;
    let exec_branch: Vec<String> = finalized_h
        .get("execution_branch")
        .and_then(|x| x.as_array())
        .ok_or("fin exec branch")?
        .iter()
        .map(json_str)
        .collect::<Result<_, _>>()?;
    verify_execution_in_body(&exec, &exec_branch, &finalized.body_root)?;
    let fin_branch: Vec<String> = data
        .get("finality_branch")
        .and_then(|x| x.as_array())
        .ok_or("finality_branch")?
        .iter()
        .map(json_str)
        .collect::<Result<_, _>>()?;
    let fin_root = beacon_root(&finalized)?;
    if !verify_merkle_gindex(
        &fin_root,
        &parse_branch(&fin_branch)?,
        FINALIZED_ROOT_GINDEX,
        &parse32(&attested.state_root)?,
    ) {
        return Err("finalized header not in attested state".into());
    }
    let sig_slot = json_u64(data.get("signature_slot").ok_or("sig slot")?)?;
    let agg = data.get("sync_aggregate").ok_or("sync_aggregate")?;
    let bits = json_str(agg.get("sync_committee_bits").ok_or("bits")?)?;
    let sig = json_str(agg.get("sync_committee_signature").ok_or("csig")?)?;
    let attested_root = beacon_root(&attested)?;
    let genesis = parse32(&state.genesis_validators_root)?;
    let domain = compute_domain(sepolia_fork_version(sig_slot), &genesis);
    let msg = signing_root(&attested_root, &domain);
    let period = slot_period(sig_slot);
    let pks = if period == state.period {
        &state.current_pks
    } else if period == state.period + 1 && !state.next_pks.is_empty() {
        &state.next_pks
    } else {
        return Err(format!("update period {period} not current {} or next", state.period));
    };
    verify_sync_aggregate(pks, &bits, &sig, &msg)?;
    let advancing = finalized.slot > state.finalized_slot;
    let has_next = data.get("next_sync_committee").is_some();
    if !advancing && (!has_next || state.next_pks.len() == SYNC_COMMITTEE_SIZE) {
        return Err("update does not advance finalized slot".into());
    }
    let mut out = state.clone();
    if advancing {
        out.finalized_slot = finalized.slot;
        out.finalized_beacon_root = hex::encode(fin_root);
        out.execution_block_hash = exec.block_hash.trim_start_matches("0x").to_ascii_lowercase();
        out.execution_number = exec.block_number;
        out.receipts_root = exec.receipts_root.trim_start_matches("0x").to_ascii_lowercase();
        if slot_period(finalized.slot) > state.period && !state.next_pks.is_empty() {
            out.current_pks = state.next_pks.clone();
            out.current_agg_pk = state.next_agg_pk.clone();
            out.period = slot_period(finalized.slot);
            out.next_pks = Vec::new();
            out.next_agg_pk = String::new();
        }
    }
    if let Some(nsc) = data.get("next_sync_committee") {
        let npks: Vec<String> = nsc
            .get("pubkeys")
            .and_then(|x| x.as_array())
            .ok_or("next pks")?
            .iter()
            .map(json_str)
            .collect::<Result<_, _>>()?;
        let nagg = json_str(nsc.get("aggregate_pubkey").ok_or("next agg")?)?;
        let br = data
            .get("next_sync_committee_branch")
            .and_then(|x| x.as_array())
            .ok_or("next_sync_committee_branch")?;
        let nbr: Vec<String> = br.iter().map(json_str).collect::<Result<_, _>>()?;
        verify_committee_in_state(
            &npks,
            &nagg,
            &nbr,
            &attested.state_root,
            NEXT_SYNC_COMMITTEE_GINDEX,
        )?;
        out.next_pks = npks
            .into_iter()
            .map(|s| s.trim_start_matches("0x").to_ascii_lowercase())
            .collect();
        out.next_agg_pk = nagg.trim_start_matches("0x").to_ascii_lowercase();
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Gloas light-client rules (v2.9.60). Only reached via the `_ex` entry points
// with `gloas = true`; the legacy functions above are unchanged.
// ---------------------------------------------------------------------------

/// Sepolia Gloas fork epoch (`/eth/v1/config/fork_schedule`).
pub const SEPOLIA_GLOAS_FORK_EPOCH: u64 = 353_024;
pub const SEPOLIA_GLOAS_FORK_VERSION: [u8; 4] = [0x90, 0x00, 0x00, 0x76];
/// Sepolia Deneb fork epoch (first epoch whose LC headers can be upgraded to
/// the `execution_block_hash` form).
pub const SEPOLIA_DENEB_FORK_EPOCH: u64 = 132_608;
/// `get_generalized_index(BeaconState, "finalized_checkpoint", "root")`, Gloas.
pub const FINALIZED_ROOT_GINDEX_GLOAS: u64 = 735;
/// `get_generalized_index(BeaconState, "current_sync_committee")`, Gloas.
pub const CURRENT_SYNC_COMMITTEE_GINDEX_GLOAS: u64 = 2945;
/// `get_generalized_index(BeaconState, "next_sync_committee")`, Gloas.
pub const NEXT_SYNC_COMMITTEE_GINDEX_GLOAS: u64 = 2946;
/// `get_generalized_index(deneb.BeaconBlockBody, "execution_payload", "block_hash")`.
pub const EXECUTION_BLOCK_HASH_GINDEX_DENEB: u64 = 812;
/// `get_generalized_index(BeaconBlockBody, "signed_execution_payload_bid",
/// "message", "parent_block_hash")`, Gloas.
pub const EXECUTION_BLOCK_HASH_GINDEX_GLOAS: u64 = 2856;

/// Fork version for `epoch` including Gloas.
pub fn sepolia_fork_version_gloas(epoch: u64) -> [u8; 4] {
    if epoch >= SEPOLIA_GLOAS_FORK_EPOCH {
        SEPOLIA_GLOAS_FORK_VERSION
    } else {
        sepolia_fork_version(epoch.saturating_mul(SLOTS_PER_EPOCH))
    }
}

fn floorlog2(x: u64) -> usize {
    63 - x.leading_zeros() as usize
}

/// `is_valid_normalized_merkle_branch`: leading `len - depth` entries must be
/// zero, the rest is a normal branch for `gindex`.
pub fn verify_normalized_branch(
    leaf: &[u8; 32],
    branch: &[[u8; 32]],
    gindex: u64,
    root: &[u8; 32],
) -> bool {
    if gindex < 2 {
        return false;
    }
    let depth = floorlog2(gindex);
    if branch.len() < depth {
        return false;
    }
    let extra = branch.len() - depth;
    if branch[..extra].iter().any(|b| *b != [0u8; 32]) {
        return false;
    }
    verify_merkle_gindex(leaf, &branch[extra..], gindex, root)
}

fn json_str_list(v: Option<&serde_json::Value>, what: &str) -> Result<Vec<String>, String> {
    v.and_then(|x| x.as_array())
        .ok_or_else(|| what.to_string())?
        .iter()
        .map(json_str)
        .collect()
}

fn norm_hex(s: &str) -> String {
    s.trim().trim_start_matches("0x").to_ascii_lowercase()
}

/// Execution data a verified LC header commits to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LcExecution {
    /// Lowercase hex, no `0x`.
    pub block_hash: String,
    /// Known only for headers that still carry the full payload header.
    pub number: Option<u64>,
    pub receipts_root: Option<String>,
}

/// `is_valid_light_client_header` for any post-Deneb header, in either the
/// original (`execution` payload header) or the Gloas (`execution_block_hash`)
/// format.
pub fn verify_lc_header_gloas(h: &serde_json::Value) -> Result<(BeaconHeader, LcExecution), String> {
    let beacon = parse_beacon_header(h)?;
    let epoch = beacon.slot / SLOTS_PER_EPOCH;
    let branch_s = json_str_list(h.get("execution_branch"), "execution_branch")?;
    if let Some(ex) = h.get("execution") {
        if epoch >= SEPOLIA_GLOAS_FORK_EPOCH {
            return Err("Gloas LC header must not carry an execution payload header".into());
        }
        let exec = parse_exec_payload(ex)?;
        verify_execution_in_body(&exec, &branch_s, &beacon.body_root)?;
        return Ok((
            beacon,
            LcExecution {
                block_hash: norm_hex(&exec.block_hash),
                number: Some(exec.block_number),
                receipts_root: Some(norm_hex(&exec.receipts_root)),
            },
        ));
    }
    let bh = json_str(h.get("execution_block_hash").ok_or("execution_block_hash")?)?;
    let gindex = if epoch >= SEPOLIA_GLOAS_FORK_EPOCH {
        EXECUTION_BLOCK_HASH_GINDEX_GLOAS
    } else if epoch >= SEPOLIA_DENEB_FORK_EPOCH {
        EXECUTION_BLOCK_HASH_GINDEX_DENEB
    } else {
        return Err("pre-Deneb LC header not supported".into());
    };
    let leaf = parse32(&bh)?;
    if leaf == [0u8; 32] {
        return Err("execution payload not in beacon body_root (zero block hash)".into());
    }
    let br = parse_branch(&branch_s)?;
    if !verify_normalized_branch(&leaf, &br, gindex, &parse32(&beacon.body_root)?) {
        return Err("execution payload not in beacon body_root".into());
    }
    Ok((
        beacon,
        LcExecution {
            block_hash: norm_hex(&bh),
            number: None,
            receipts_root: None,
        },
    ))
}

fn verify_committee_in_state_normalized(
    pks: &[String],
    agg: &str,
    branch: &[String],
    state_root: &str,
    gindex: u64,
) -> Result<(), String> {
    let leaf = sync_committee_root(pks, agg)?;
    let br = parse_branch(branch)?;
    let root = parse32(state_root)?;
    if !verify_normalized_branch(&leaf, &br, gindex, &root) {
        return Err("sync committee not in state_root".into());
    }
    Ok(())
}

fn current_sync_committee_gindex_at_slot(slot: u64) -> u64 {
    if slot / SLOTS_PER_EPOCH >= SEPOLIA_GLOAS_FORK_EPOCH {
        CURRENT_SYNC_COMMITTEE_GINDEX_GLOAS
    } else {
        CURRENT_SYNC_COMMITTEE_GINDEX
    }
}

fn next_sync_committee_gindex_at_slot(slot: u64) -> u64 {
    if slot / SLOTS_PER_EPOCH >= SEPOLIA_GLOAS_FORK_EPOCH {
        NEXT_SYNC_COMMITTEE_GINDEX_GLOAS
    } else {
        NEXT_SYNC_COMMITTEE_GINDEX
    }
}

fn finalized_root_gindex_at_slot(slot: u64) -> u64 {
    if slot / SLOTS_PER_EPOCH >= SEPOLIA_GLOAS_FORK_EPOCH {
        FINALIZED_ROOT_GINDEX_GLOAS
    } else {
        FINALIZED_ROOT_GINDEX
    }
}

/// Bootstrap. `gloas = false` is exactly [`apply_bootstrap`].
pub fn apply_bootstrap_ex(json: &str, gloas: bool) -> Result<EthLcState, String> {
    if !gloas {
        return apply_bootstrap(json);
    }
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let data = v.get("data").unwrap_or(&v);
    let header = data.get("header").ok_or("header")?;
    let (beacon, exec) = verify_lc_header_gloas(header)?;
    let sc = data.get("current_sync_committee").ok_or("committee")?;
    let pks = json_str_list(sc.get("pubkeys"), "pubkeys")?;
    let agg = json_str(sc.get("aggregate_pubkey").ok_or("agg")?)?;
    let sc_branch = json_str_list(data.get("current_sync_committee_branch"), "sc branch")?;
    verify_committee_in_state_normalized(
        &pks,
        &agg,
        &sc_branch,
        &beacon.state_root,
        current_sync_committee_gindex_at_slot(beacon.slot),
    )?;
    let root = beacon_root(&beacon)?;
    Ok(EthLcState {
        genesis_validators_root: SEPOLIA_GENESIS_VALIDATORS_ROOT.to_string(),
        finalized_slot: beacon.slot,
        finalized_beacon_root: hex::encode(root),
        execution_block_hash: exec.block_hash,
        execution_number: exec.number.unwrap_or(0),
        receipts_root: exec.receipts_root.unwrap_or_default(),
        period: slot_period(beacon.slot),
        current_agg_pk: norm_hex(&agg),
        current_pks: pks.iter().map(|s| norm_hex(s)).collect(),
        next_agg_pk: String::new(),
        next_pks: Vec::new(),
    })
}

/// Update (sync-committee / finality). `gloas = false` is exactly
/// [`apply_update`]. With `gloas = true` both the original and the Gloas
/// header formats verify, with gindices and fork version chosen per slot.
pub fn apply_update_ex(state: &EthLcState, json: &str, gloas: bool) -> Result<EthLcState, String> {
    if !gloas {
        return apply_update(state, json);
    }
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let data = v.get("data").unwrap_or(&v);
    let attested = parse_beacon_header(data.get("attested_header").ok_or("attested")?)?;
    let (finalized, exec) = verify_lc_header_gloas(data.get("finalized_header").ok_or("finalized")?)?;
    let sig_slot = json_u64(data.get("signature_slot").ok_or("sig slot")?)?;
    if !(sig_slot > attested.slot && attested.slot >= finalized.slot) {
        return Err(format!(
            "update slots out of order: signature {sig_slot} attested {} finalized {}",
            attested.slot, finalized.slot
        ));
    }
    let fin_branch = json_str_list(data.get("finality_branch"), "finality_branch")?;
    let fin_root = beacon_root(&finalized)?;
    if !verify_normalized_branch(
        &fin_root,
        &parse_branch(&fin_branch)?,
        finalized_root_gindex_at_slot(attested.slot),
        &parse32(&attested.state_root)?,
    ) {
        return Err("finalized header not in attested state".into());
    }
    let agg = data.get("sync_aggregate").ok_or("sync_aggregate")?;
    let bits = json_str(agg.get("sync_committee_bits").ok_or("bits")?)?;
    let sig = json_str(agg.get("sync_committee_signature").ok_or("csig")?)?;
    let attested_root = beacon_root(&attested)?;
    let genesis = parse32(&state.genesis_validators_root)?;
    let fork_version_slot = sig_slot.max(1) - 1;
    let domain = compute_domain(
        sepolia_fork_version_gloas(fork_version_slot / SLOTS_PER_EPOCH),
        &genesis,
    );
    let msg = signing_root(&attested_root, &domain);
    let period = slot_period(sig_slot);
    let pks = if period == state.period {
        &state.current_pks
    } else if period == state.period + 1 && !state.next_pks.is_empty() {
        &state.next_pks
    } else {
        return Err(format!("update period {period} not current {} or next", state.period));
    };
    verify_sync_aggregate(pks, &bits, &sig, &msg)?;
    let advancing = finalized.slot > state.finalized_slot;
    let has_next = data.get("next_sync_committee").is_some();
    if !advancing && (!has_next || state.next_pks.len() == SYNC_COMMITTEE_SIZE) {
        return Err("update does not advance finalized slot".into());
    }
    let mut out = state.clone();
    if advancing {
        out.finalized_slot = finalized.slot;
        out.finalized_beacon_root = hex::encode(fin_root);
        out.execution_block_hash = exec.block_hash.clone();
        out.execution_number = exec.number.unwrap_or(0);
        out.receipts_root = exec.receipts_root.clone().unwrap_or_default();
        if slot_period(finalized.slot) > state.period && !state.next_pks.is_empty() {
            out.current_pks = state.next_pks.clone();
            out.current_agg_pk = state.next_agg_pk.clone();
            out.period = slot_period(finalized.slot);
            out.next_pks = Vec::new();
            out.next_agg_pk = String::new();
        }
    }
    // `next_sync_committee` in the attested state is for period
    // attested_period + 1: store it only when that is the period after the
    // (possibly rotated) current one (spec: attested period == finalized
    // period). The legacy path stored it unconditionally.
    let store_next = has_next && slot_period(attested.slot) == out.period;
    if !advancing && !store_next {
        return Err("update does not advance finalized slot".into());
    }
    if let (true, Some(nsc)) = (store_next, data.get("next_sync_committee")) {
        let npks = json_str_list(nsc.get("pubkeys"), "next pks")?;
        let nagg = json_str(nsc.get("aggregate_pubkey").ok_or("next agg")?)?;
        let nbr = json_str_list(
            data.get("next_sync_committee_branch"),
            "next_sync_committee_branch",
        )?;
        verify_committee_in_state_normalized(
            &npks,
            &nagg,
            &nbr,
            &attested.state_root,
            next_sync_committee_gindex_at_slot(attested.slot),
        )?;
        out.next_pks = npks.iter().map(|s| norm_hex(s)).collect();
        out.next_agg_pk = norm_hex(&nagg);
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::eth_ssz::parse32;

    #[test]
    fn beacon_root_matches_sepolia_api() {
        let h = BeaconHeader {
            slot: 11_000_288,
            proposer_index: 1180,
            parent_root: "0xb5f8e81b959d63c8b7ef2c71fffe0e15834294898f3c150e7cddfc23ca8dbfb1".into(),
            state_root: "0xbd47b5a0977051570c4538984611f37623af08be014866f32caa56cd79aab6a2".into(),
            body_root: "0x7f8cc6fac8fe3569f8a48338414397cc885c0e8dab25d6bf2c4c98d155f80fd9".into(),
        };
        let root = beacon_root(&h).unwrap();
        assert_eq!(
            hex::encode(root),
            hex::encode(parse32("0x15b7004cc5a9d13f74485e05f517c805c451e6fe9204ec94dc91d7f8f093553f").unwrap())
        );
    }

    #[test]
    fn sepolia_bootstrap_verifies_execution_and_committee() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data/sepolia-lc-bootstrap.json");
        if !p.exists() {
            return;
        }
        let s = std::fs::read_to_string(p).unwrap();
        let st = apply_bootstrap(&s).expect("bootstrap");
        assert_eq!(st.genesis_validators_root, SEPOLIA_GENESIS_VALIDATORS_ROOT);
        assert_eq!(st.current_pks.len(), 512);
        assert!(st.execution_number > 0);
        assert_eq!(st.finalized_slot, 11_000_288);
    }

    #[test]
    fn sepolia_update_bls_advances_finalized() {
        let bp = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data/sepolia-lc-bootstrap.json");
        let up = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data/sepolia-lc-update.json");
        if !bp.exists() || !up.exists() {
            return;
        }
        let st = apply_bootstrap(&std::fs::read_to_string(bp).unwrap()).unwrap();
        let raw = std::fs::read_to_string(up).unwrap();
        // API returns a list of {version,data}
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let one = if v.is_array() {
            serde_json::to_string(&v[0]).unwrap()
        } else {
            raw
        };
        let st2 = apply_update(&st, &one).expect("period-1342 update fills next committee");
        assert!(st2.finalized_slot >= st.finalized_slot);
        assert_eq!(st2.current_pks.len(), 512);
        if st2.finalized_slot == st.finalized_slot {
            assert_eq!(st2.next_pks.len(), 512);
        }
    }

    // ---- Gloas (v2.9.60) — real Sepolia data from lodestar-sepolia.chainsafe.io ----

    pub(crate) fn gloas_fixture(name: &str) -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data/gloas")
            .join(name);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    const H_1379: &str = "9a8acc058dffdb10c5a143b937608aff8eb8d61ae94771a95b319529dbe7543d";
    const H_1380: &str = "c3bc0fef2deac61b84bfe5d3c92bc771c2fc4d6843ee70fdf0c4339594c75af5";

    /// Fulu bootstrap (period 1377) → Fulu updates 1377, 1378 with the legacy
    /// rules — the live 2300 LC sits here (period 1378, next committee known).
    pub(crate) fn state_period_1378() -> EthLcState {
        let st = apply_bootstrap(&gloas_fixture("bootstrap-fulu-11280384.json")).unwrap();
        let st = apply_update(&st, &gloas_fixture("update-fulu-period-1377.json")).unwrap();
        let st = apply_update(&st, &gloas_fixture("update-fulu-period-1378.json")).unwrap();
        assert_eq!(st.period, 1378);
        assert_eq!(st.next_pks.len(), SYNC_COMMITTEE_SIZE);
        st
    }

    #[test]
    fn gloas_fork_version_and_gindex_constants() {
        assert_eq!(sepolia_fork_version_gloas(353_023), [0x90, 0, 0, 0x75]);
        assert_eq!(sepolia_fork_version_gloas(353_024), [0x90, 0, 0, 0x76]);
        assert_eq!(sepolia_fork_version_gloas(272_640), [0x90, 0, 0, 0x75]);
        // Gloas starts exactly at sync period 1379.
        assert_eq!(slot_period(SEPOLIA_GLOAS_FORK_EPOCH * SLOTS_PER_EPOCH), 1379);
        assert_eq!(slot_period(SEPOLIA_GLOAS_FORK_EPOCH * SLOTS_PER_EPOCH - 1), 1378);
        assert_eq!(floorlog2(EXECUTION_BLOCK_HASH_GINDEX_GLOAS), 11);
        assert_eq!(floorlog2(EXECUTION_BLOCK_HASH_GINDEX_DENEB), 9);
        assert_eq!(floorlog2(FINALIZED_ROOT_GINDEX_GLOAS), 9);
        assert_eq!(floorlog2(NEXT_SYNC_COMMITTEE_GINDEX_GLOAS), 11);
        // 812 = execution_payload (25) · 32 + block_hash (12).
        assert_eq!(EXECUTION_BLOCK_HASH_GINDEX_DENEB, EXECUTION_PAYLOAD_GINDEX * 32 + 12);
    }

    #[test]
    fn gloas_rules_match_legacy_on_fulu_data() {
        let st = apply_bootstrap(&gloas_fixture("bootstrap-fulu-11280384.json")).unwrap();
        let st_g = apply_bootstrap_ex(&gloas_fixture("bootstrap-fulu-11280384.json"), true).unwrap();
        assert_eq!(st, st_g, "Fulu bootstrap: same state under both rule sets");
        let mut a = st.clone();
        let mut b = st_g;
        for f in ["update-fulu-period-1377.json", "update-fulu-period-1378.json"] {
            a = apply_update(&a, &gloas_fixture(f)).unwrap();
            b = apply_update_ex(&b, &gloas_fixture(f), true).unwrap();
            assert_eq!(a, b, "{f}: same state under both rule sets");
        }
        assert!(a.execution_number > 0);
    }

    #[test]
    fn legacy_rules_reject_gloas_update_this_is_the_freeze() {
        let st = state_period_1378();
        let e = apply_update(&st, &gloas_fixture("update-gloas-period-1379.json")).unwrap_err();
        assert_eq!(e, "fin exec");
        assert!(apply_bootstrap(&gloas_fixture("bootstrap-gloas-11305567.json")).is_err());
    }

    #[test]
    fn gloas_updates_advance_through_fork_and_match_gloas_bootstrap() {
        let st = state_period_1378();
        // Live LC: finalized 11296672 (last Fulu checkpoint), period 1378.
        let mut live = st.clone();
        live.finalized_slot = 11_296_672;
        let s1 = apply_update_ex(&live, &gloas_fixture("update-gloas-period-1379.json"), true)
            .expect("period-1379 Gloas update (signed by next committee) applies");
        assert_eq!(s1.finalized_slot, 11_297_184);
        assert_eq!(s1.period, 1379);
        assert_eq!(s1.current_pks, st.next_pks, "rotated to the 1379 committee");
        assert_eq!(s1.next_pks.len(), SYNC_COMMITTEE_SIZE, "1380 committee stored");
        assert_eq!(s1.execution_block_hash, H_1379);
        assert_eq!(s1.execution_number, 0, "hash-only head until exec header stored");
        assert_eq!(s1.receipts_root, "");
        let s2 = apply_update_ex(&s1, &gloas_fixture("update-gloas-period-1380.json"), true).unwrap();
        assert_eq!(s2.period, 1380);
        assert_eq!(s2.finalized_slot, 11_304_960);
        assert_eq!(s2.execution_block_hash, H_1380);
        let s3 = apply_update_ex(&s2, &gloas_fixture("finality-update-gloas-11305567.json"), true)
            .expect("in-period finality update");
        assert_eq!(s3.finalized_slot, 11_305_567);
        assert_eq!(s3.period, 1380);
        // Replaying the same finality update does not advance.
        let e = apply_update_ex(&s3, &gloas_fixture("finality-update-gloas-11305567.json"), true)
            .unwrap_err();
        assert!(e.contains("does not advance"), "{e}");
        // Cross-check against an independent Gloas bootstrap at the same checkpoint.
        let bs = apply_bootstrap_ex(&gloas_fixture("bootstrap-gloas-11305567.json"), true)
            .expect("Gloas bootstrap verifies (gindex 2945 + 2856)");
        assert_eq!(bs.finalized_slot, s3.finalized_slot);
        assert_eq!(bs.finalized_beacon_root, s3.finalized_beacon_root);
        assert_eq!(bs.execution_block_hash, s3.execution_block_hash);
        assert_eq!(bs.current_pks, s3.current_pks, "sync-committee chain matches bootstrap");
        assert_eq!(bs.current_agg_pk, s3.current_agg_pk);
        assert_eq!(bs.period, 1380);
    }

    fn mutate(json: &str, f: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut v: serde_json::Value = serde_json::from_str(json).unwrap();
        f(v.get_mut("data").unwrap());
        v.to_string()
    }

    #[test]
    fn gloas_update_tampering_rejected() {
        let st = state_period_1378();
        let raw = gloas_fixture("update-gloas-period-1379.json");
        // Different execution block hash: branch no longer proves it.
        let bad = mutate(&raw, |d| {
            d["finalized_header"]["execution_block_hash"] =
                serde_json::Value::String(format!("0x{}", "11".repeat(32)));
        });
        let e = apply_update_ex(&st, &bad, true).unwrap_err();
        assert!(e.contains("execution payload not in"), "{e}");
        // Finality branch tampered.
        let bad = mutate(&raw, |d| {
            d["finality_branch"][3] = serde_json::Value::String(format!("0x{}", "22".repeat(32)));
        });
        let e = apply_update_ex(&st, &bad, true).unwrap_err();
        assert!(e.contains("finalized header not in attested state"), "{e}");
        // Next committee branch tampered.
        let bad = mutate(&raw, |d| {
            d["next_sync_committee_branch"][0] =
                serde_json::Value::String(format!("0x{}", "33".repeat(32)));
        });
        let e = apply_update_ex(&st, &bad, true).unwrap_err();
        assert!(e.contains("sync committee not in state_root"), "{e}");
        // Attested header changed: signature no longer verifies.
        let bad = mutate(&raw, |d| {
            d["attested_header"]["beacon"]["proposer_index"] = serde_json::Value::String("1".into());
        });
        let e = apply_update_ex(&st, &bad, true).unwrap_err();
        assert!(e.contains("bls aggregate") || e.contains("finalized header not in"), "{e}");
        // Old (Fulu) gindex with the Gloas-length branch: rejected.
        let bad = mutate(&raw, |d| {
            let br = d["finality_branch"].as_array().unwrap()[2..].to_vec();
            d["finality_branch"] = serde_json::Value::Array(br);
        });
        assert!(apply_update_ex(&st, &bad, true).is_err());
        // Two committees ahead (signed by an unknown committee): period check.
        let s1 = apply_update_ex(&st, &raw, true).unwrap();
        let mut s_old = s1.clone();
        s_old.period = 1377;
        let e = apply_update_ex(&s_old, &gloas_fixture("update-gloas-period-1380.json"), true)
            .unwrap_err();
        assert!(e.contains("update period"), "{e}");
    }

    /// `upgrade_lc_header_to_gloas` for a Fulu header: `execution_block_hash`
    /// + proof of block_hash inside the payload header + execution_branch,
    /// zero-padded to depth 11 → verifies at gindex 812.
    #[test]
    fn upgraded_pre_gloas_header_verifies_with_normalized_branch() {
        let raw = gloas_fixture("update-fulu-period-1378.json");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let fh = &v["data"]["finalized_header"];
        let exec = parse_exec_payload(&fh["execution"]).unwrap();
        let fields = execution_payload_header_fields(&exec).unwrap();
        // Proof for leaf 12 (block_hash) in a 32-leaf tree.
        let mut layer: Vec<[u8; 32]> = fields.to_vec();
        layer.resize(32, [0u8; 32]);
        let mut idx = 12usize;
        let mut proof: Vec<String> = Vec::new();
        while layer.len() > 1 {
            proof.push(format!("0x{}", hex::encode(layer[idx ^ 1])));
            layer = layer
                .chunks(2)
                .map(|c| crate::eth_ssz::sha256_pair(&c[0], &c[1]))
                .collect();
            idx /= 2;
        }
        let mut branch = vec![format!("0x{}", "00".repeat(32)); 2];
        branch.extend(proof);
        for b in fh["execution_branch"].as_array().unwrap() {
            branch.push(b.as_str().unwrap().to_string());
        }
        assert_eq!(branch.len(), 11);
        let mut up = serde_json::json!({
            "beacon": fh["beacon"].clone(),
            "execution_block_hash": exec.block_hash.clone(),
            "execution_branch": branch.clone(),
        });
        let (b, x) = verify_lc_header_gloas(&up).expect("upgraded Fulu header verifies");
        assert_eq!(b.slot, 11_290_112);
        assert_eq!(x.block_hash, exec.block_hash.trim_start_matches("0x").to_ascii_lowercase());
        assert_eq!(x.number, None);
        // Original format still verifies under the Gloas rules, with number.
        let (_, x2) = verify_lc_header_gloas(fh).unwrap();
        assert_eq!(x2.number, Some(exec.block_number));
        // Non-zero normalization padding is rejected.
        branch[0] = format!("0x{}", "01".repeat(32));
        up["execution_branch"] = serde_json::json!(branch);
        assert!(verify_lc_header_gloas(&up).is_err());
        // A Gloas-slot header that still carries `execution` is rejected.
        let mut g = fh.clone();
        g["beacon"]["slot"] = serde_json::json!("11296768");
        let e = verify_lc_header_gloas(&g).unwrap_err();
        assert!(e.contains("must not carry"), "{e}");
    }
}
