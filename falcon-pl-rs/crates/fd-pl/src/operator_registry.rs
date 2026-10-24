//! BitVM2 bridge operator registry + emissions reward (DESIGN, NOT ARMED).
//!
//! Model (Scott, 2026-10-18): outside operators front BTC withdrawals with
//! their own BTC and are reimbursed from the locked deposit through the
//! presigned peg-out graph (Take1/Take2). Falcon holds no BTC and no keys.
//! Operator NETWORK (Scott, 2026-10-18): operators are paid in FPL for
//! maintaining the bridge: LIVENESS (signed heartbeat slots), WATCHING (every
//! kit watches all claims; a successful disprove pays a bounty) and PAYOUTS
//! (proven completed withdrawals). Bonded (optional) in FPL, slashed when
//! disproved. No keys or votes over funds: BTC moves only via presigned graphs.
//!
//! Everything here is pure and deterministic. The registry lives in
//! `BridgeState::operators` (omitted from serde while empty, so the commitment
//! root and snapshots are byte-identical until the first registration) and is
//! driven by the `Operator*` tx bodies in `lib.rs`, all refused before the
//! combined bridge height (`params_v2::TESTNET_2300_BRIDGE_FROM_HEIGHT`,
//! `u64::MAX` today). Decided (Scott 2026-10-18): 5% off the top, bond 5k FPL,
//! 100% slash. Split/bounty numbers are preset B (see docs/bridge/OPERATOR_REWARDS.md).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::NETWORK_ID_PUBLIC_TESTNET_2300;

/// Activation height on public testnet 2300 = the one combined bridge height.
pub const TESTNET_2300_OPERATOR_REWARDS_FROM_HEIGHT: u64 =
    crate::params_v2::TESTNET_2300_BRIDGE_FROM_HEIGHT;

/// Activation height for a network (= [`crate::params_v2::bridge_from_height`]).
pub fn operator_rewards_from_height(network_id: u32) -> u64 {
    if network_id == NETWORK_ID_PUBLIC_TESTNET_2300 {
        return TESTNET_2300_OPERATOR_REWARDS_FROM_HEIGHT;
    }
    crate::params_v2::bridge_from_height(network_id)
}

pub fn operator_rewards_active(network_id: u32, height: u64) -> bool {
    height >= operator_rewards_from_height(network_id)
}

/// All tunables. Presets below are menu options, not decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorParams {
    /// Minimum FPL bond (base units) to register. 0 = no bond (permissionless).
    pub min_bond: u64,
    /// Share of epoch emission routed to the operator bucket (bps of epochEmit).
    pub emission_share_bps: u64,
    /// Bucket split (bps of bucket): liveness + watching; payouts get the rest.
    pub liveness_share_bps: u64,
    pub watching_share_bps: u64,
    /// Heartbeat slots per epoch (e.g. 168 = hourly) and the minimum an operator
    /// needs for the liveness + watching parts.
    pub slots_per_epoch: u64,
    pub min_liveness_slots: u64,
    /// Watch units credited per successful disprove (on top of 1 unit per live slot).
    pub disprove_watch_units: u64,
    /// Per-operator cap on the payout part (bps of the bucket); anti-whale.
    pub cap_per_operator_bps: u64,
    /// Max payout fee an operator may deduct from a withdrawal (bps of amount).
    pub max_payout_fee_bps: u64,
    /// Lifetime proven payouts needed for the payout part (0 = none).
    pub min_completed_payouts: u64,
    /// Bond slashed per successful disprove (bps of current bond).
    pub slash_bps: u64,
    /// Part of the slashed FPL paid to the disprover (bps of slash); rest burned.
    pub disprove_bounty_bps: u64,
    /// Epochs of ineligibility after a disprove.
    pub disprove_jail_epochs: u64,
    /// Disproves after which the operator is permanently removed.
    pub max_disproves: u32,
    /// Unbond cooldown in epochs (bond stays slashable meanwhile).
    pub unbond_cooldown_epochs: u64,
}

impl OperatorParams {
    /// PROPOSAL A "permissionless": no bond; payouts dominate; liveness small.
    pub const PROPOSAL_A: OperatorParams = OperatorParams {
        min_bond: 0,
        emission_share_bps: 300,
        liveness_share_bps: 1_000,
        watching_share_bps: 2_000,
        slots_per_epoch: 168,
        min_liveness_slots: 151, // 90%
        disprove_watch_units: 168,
        cap_per_operator_bps: 3_000,
        max_payout_fee_bps: 30,
        min_completed_payouts: 1,
        slash_bps: 0,
        disprove_bounty_bps: 0,
        disprove_jail_epochs: 52,
        max_disproves: 1,
        unbond_cooldown_epochs: 0,
    };
    /// PROPOSAL B "bonded network" (default; Scott 2026-10-18): 5% emission share,
    /// bond 5k FPL, slash 100%; equal weight on maintenance (live + watching) and payouts.
    pub const PROPOSAL_B: OperatorParams = OperatorParams {
        min_bond: 5_000,
        emission_share_bps: 500,
        liveness_share_bps: 2_500,
        watching_share_bps: 2_500,
        slots_per_epoch: 168,
        min_liveness_slots: 151,
        disprove_watch_units: 336,
        cap_per_operator_bps: 2_500,
        max_payout_fee_bps: 30,
        min_completed_payouts: 1,
        slash_bps: 10_000,
        disprove_bounty_bps: 5_000,
        disprove_jail_epochs: 26,
        max_disproves: 2,
        unbond_cooldown_epochs: 2,
    };
    /// PROPOSAL C "bootstrap": larger bucket, mostly maintenance pay while volume is thin.
    pub const PROPOSAL_C: OperatorParams = OperatorParams {
        min_bond: 5_000,
        emission_share_bps: 1_000,
        liveness_share_bps: 4_000,
        watching_share_bps: 3_000,
        slots_per_epoch: 168,
        min_liveness_slots: 134, // 80%
        disprove_watch_units: 504,
        cap_per_operator_bps: 2_000,
        max_payout_fee_bps: 50,
        min_completed_payouts: 0,
        slash_bps: 10_000,
        disprove_bounty_bps: 3_000,
        disprove_jail_epochs: 52,
        max_disproves: 1,
        unbond_cooldown_epochs: 2,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperatorStatus {
    Active,
    Unbonding { release_epoch: u64 },
    Jailed { until_epoch: u64 },
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorRecord {
    /// Falcon account that registered (receives FPL rewards).
    pub account: String,
    /// Operator's BTC x-only key used in the peg-out graphs (hex, 32 bytes).
    pub btc_xonly_hex: String,
    pub bond: u64,
    pub registered_height: u64,
    pub status: OperatorStatus,
    pub completed_payouts: u64,
    pub completed_sats: u64,
    pub last_payout_epoch: Option<u64>,
    pub disproves: u32,
    pub slashed_total: u64,
    /// Sats of proven payouts credited in the current (unsettled) epoch.
    pub epoch_sats: u64,
    /// Heartbeat slots seen this epoch (bitset-free: sorted unique slot ids).
    #[serde(default)]
    pub epoch_slots: std::collections::BTreeSet<u64>,
    /// Watch units this epoch (live slots + disprove credits).
    #[serde(default)]
    pub epoch_watch_units: u64,
    #[serde(default)]
    pub disproves_won: u64,
    /// Bitcoin scriptPubKey (hex) the operator's kit puts in the reward output
    /// of a Disprove / DisproveChain it broadcasts. Routes the FPL bounty to
    /// whoever actually disproved on Bitcoin (copying the evidence earns nothing).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reward_spk_hex: String,
    /// Kit gossip / feed URL (informational; kits authenticate gossip by btc key).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub gossip_url: String,
    /// Advertised free BTC fronting capacity (wallet - in-flight - reserve, sats),
    /// as last reported by the operator's kit, and the ledger height of the report.
    #[serde(default)]
    pub capacity_sats: u64,
    #[serde(default)]
    pub capacity_height: u64,
}

/// One operator's presigned peg-out graph for one deposit, bound on the
/// ledger by the operator's own account. Graph txids are fixed at presign
/// (segwit txids exclude witnesses), so the Disprove / DisproveChain that
/// spends them identifies the cheating operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphBind {
    pub account: String,
    pub operator_btc_xonly_hex: String,
    /// Peg-in CONFIRM txid (the n-of-n deposit output the graph spends; display hex, lowercase).
    pub deposit_txid: String,
    /// KickOff2 txid: output 1 = connector B (spent by DisproveChain).
    pub kick_off_2_txid: String,
    /// AssertFinal txid: output 1 = connector 5, output 2 = connector C (spent by Disprove).
    pub assert_final_txid: String,
    pub bound_height: u64,
    /// Falcon account the peg-in mints to (empty = legacy bind, never mints).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub depositor_account: String,
    /// BRIDGE: n-of-n co-signer attestation of another operator's graph for
    /// this deposit (same txids). Counts for the mint rule; never slashable
    /// by graph evidence (the graph's operator is the one bound without it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cosign: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorRegistry {
    pub operators: BTreeMap<String, OperatorRecord>,
    /// claim ids already credited / disproved (idempotence).
    pub credited: BTreeMap<String, String>,
    /// `"<deposit_txid>:<operator xonly>"` -> graph binding.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub graphs: BTreeMap<String, GraphBind>,
    /// FPL burned by slashing (slash minus bounty), lifetime.
    #[serde(default)]
    pub burned_total: u64,
}

/// Which BitVM2 fraud proof a Bitcoin tx is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisproveKind {
    /// Spends AssertFinal:1 (connector 5) + AssertFinal:2 (connector C).
    Disprove,
    /// Spends KickOff2:1 (connector B) via the disprove-chain leaf.
    DisproveChain,
}

/// Upstream BitVM (7d1ca36) `UNSPENDABLE_PUBLIC_KEY` (uncompressed SEC1).
pub const BITVM_UNSPENDABLE_PUBKEY_HEX: &str = "0405f818748aecbc8c67a4e61a03cee506888f49480cf343363b04908ed51e25b9615f244c38311983fb0f5b99e3fd52f255c5cc47a03ee2d85e78eaf6fa76bb9d";

/// BitVM burn output scriptPubKey: P2WSH(`<UNSPENDABLE_PUBLIC_KEY> OP_CHECKSIG`).
/// Output 0 of every Disprove / DisproveChain (half the connector value burned).
pub fn bitvm_burn_spk() -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let pk = hex::decode(BITVM_UNSPENDABLE_PUBKEY_HEX).expect("const");
    let mut script = Vec::with_capacity(67);
    script.push(pk.len() as u8); // OP_PUSHBYTES_65
    script.extend_from_slice(&pk);
    script.push(0xac); // OP_CHECKSIG
    let h = Sha256::digest(&script);
    let mut spk = vec![0x00, 0x20];
    spk.extend_from_slice(&h);
    spk
}

/// Classify a parsed Bitcoin tx as a fraud proof against `bind`. Pure; the
/// caller has already proven the tx is included on the BTC rail.
pub fn classify_disprove(tx: &crate::btc_spv::ParsedTx, bind: &GraphBind) -> Result<DisproveKind, String> {
    if tx.outputs.len() < 2 {
        return Err("disprove evidence: needs burn + reward outputs".into());
    }
    if tx.outputs[0].1 != bitvm_burn_spk() {
        return Err("disprove evidence: output 0 is not the BitVM burn script".into());
    }
    let af = bind.assert_final_txid.as_str();
    let k2 = bind.kick_off_2_txid.as_str();
    let ins: Vec<(String, u32)> = tx.inputs.iter().map(|(t, v)| (t.to_ascii_lowercase(), *v)).collect();
    if ins.len() == 2 {
        let mut v = ins.clone();
        v.sort();
        let mut want = vec![(af.to_string(), 1u32), (af.to_string(), 2u32)];
        want.sort();
        if v == want {
            return Ok(DisproveKind::Disprove);
        }
    }
    if ins.len() == 1 && ins[0] == (k2.to_string(), 1) {
        return Ok(DisproveKind::DisproveChain);
    }
    Err("disprove evidence: inputs do not spend the bound graph's AssertFinal:1+2 or KickOff2:1".into())
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    BondTooLow { need: u64, got: u64 },
    AlreadyRegistered,
    DuplicateBtcKey,
    BadBtcKey,
    Unknown,
    NotActive,
    AlreadyCredited,
    Removed,
    BadInput(&'static str),
    AlreadyBound,
    Jailed,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::BondTooLow { need, got } => write!(f, "operator: bond {got} below min {need} FPL"),
            RegistryError::AlreadyRegistered => write!(f, "operator: account already registered"),
            RegistryError::DuplicateBtcKey => write!(f, "operator: btc key already registered"),
            RegistryError::BadBtcKey => write!(f, "operator: btc key must be 32-byte x-only hex"),
            RegistryError::Unknown => write!(f, "operator: not registered"),
            RegistryError::NotActive => write!(f, "operator: not active"),
            RegistryError::AlreadyCredited => write!(f, "operator: already credited / slashed for this claim"),
            RegistryError::Removed => write!(f, "operator: removed"),
            RegistryError::BadInput(m) => write!(f, "operator: {m}"),
            RegistryError::AlreadyBound => write!(f, "operator: graph already bound for this deposit"),
            RegistryError::Jailed => write!(f, "operator: jailed"),
        }
    }
}

/// Proof the registry accepts as a completed payout: the bridge's Bitvm2Claim
/// reached `PaidOptimistic` (operator reimbursed after the challenge window,
/// i.e. it actually fronted the user's BTC) with the operator named in it.
#[derive(Debug, Clone)]
pub struct PayoutProof<'a> {
    pub claim_id: &'a str,
    pub operator_btc_xonly_hex: &'a str,
    pub amount_sats: u64,
}

impl OperatorRegistry {
    pub fn is_empty(&self) -> bool {
        self.operators.is_empty() && self.credited.is_empty() && self.graphs.is_empty() && self.burned_total == 0
    }

    pub fn register(
        &mut self,
        p: &OperatorParams,
        account: &str,
        btc_xonly_hex: &str,
        bond: u64,
        height: u64,
    ) -> Result<(), RegistryError> {
        self.register_ex(p, account, btc_xonly_hex, "", "", 0, bond, height)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn register_ex(
        &mut self,
        p: &OperatorParams,
        account: &str,
        btc_xonly_hex: &str,
        reward_spk_hex: &str,
        gossip_url: &str,
        capacity_sats: u64,
        bond: u64,
        height: u64,
    ) -> Result<(), RegistryError> {
        let reward = reward_spk_hex.trim().to_ascii_lowercase();
        if reward.len() > 160 || hex::decode(&reward).is_err() {
            return Err(RegistryError::BadInput("reward_spk_hex must be hex (<= 80 bytes)"));
        }
        if gossip_url.len() > 200 {
            return Err(RegistryError::BadInput("gossip_url too long"));
        }
        let key = btc_xonly_hex.trim().to_ascii_lowercase();
        if key.len() != 64 || hex::decode(&key).is_err() {
            return Err(RegistryError::BadBtcKey);
        }
        if bond < p.min_bond {
            return Err(RegistryError::BondTooLow { need: p.min_bond, got: bond });
        }
        if let Some(r) = self.operators.get(account) {
            if r.status == OperatorStatus::Removed {
                return Err(RegistryError::Removed);
            }
            return Err(RegistryError::AlreadyRegistered);
        }
        if self.operators.values().any(|r| r.btc_xonly_hex == key) {
            return Err(RegistryError::DuplicateBtcKey);
        }
        self.operators.insert(
            account.to_string(),
            OperatorRecord {
                account: account.to_string(),
                btc_xonly_hex: key,
                bond,
                registered_height: height,
                status: OperatorStatus::Active,
                completed_payouts: 0,
                completed_sats: 0,
                last_payout_epoch: None,
                disproves: 0,
                slashed_total: 0,
                epoch_sats: 0,
                epoch_slots: Default::default(),
                epoch_watch_units: 0,
                disproves_won: 0,
                reward_spk_hex: reward,
                gossip_url: gossip_url.trim().to_string(),
                capacity_sats,
                capacity_height: height,
            },
        );
        Ok(())
    }

    /// Top up the bond (any non-removed operator), e.g. after a slash. A jailed
    /// operator whose jail has ended and whose bond is back at `min_bond` is
    /// active again.
    pub fn rebond(&mut self, p: &OperatorParams, epoch: u64, account: &str, amount: u64) -> Result<(), RegistryError> {
        let r = self.operators.get_mut(account).ok_or(RegistryError::Unknown)?;
        match r.status {
            OperatorStatus::Removed => return Err(RegistryError::Removed),
            OperatorStatus::Unbonding { .. } => return Err(RegistryError::NotActive),
            _ => {}
        }
        if amount == 0 {
            return Err(RegistryError::BadInput("rebond amount is zero"));
        }
        r.bond = r.bond.saturating_add(amount);
        if let OperatorStatus::Jailed { until_epoch } = r.status {
            if epoch >= until_epoch && r.bond >= p.min_bond {
                r.status = OperatorStatus::Active;
            }
        }
        Ok(())
    }

    /// Capacity advert (free BTC the kit can front right now).
    pub fn set_capacity(&mut self, account: &str, sats: u64, height: u64) -> Result<(), RegistryError> {
        let r = self.operators.get_mut(account).ok_or(RegistryError::Unknown)?;
        if r.status == OperatorStatus::Removed {
            return Err(RegistryError::Removed);
        }
        r.capacity_sats = sats;
        r.capacity_height = height;
        Ok(())
    }

    /// Bind this operator's graph for a deposit (immutable once bound).
    pub fn bind_graph(
        &mut self,
        account: &str,
        deposit_txid: &str,
        kick_off_2_txid: &str,
        assert_final_txid: &str,
        height: u64,
    ) -> Result<(), RegistryError> {
        let r = self.operators.get(account).ok_or(RegistryError::Unknown)?;
        if matches!(r.status, OperatorStatus::Removed | OperatorStatus::Unbonding { .. }) {
            return Err(RegistryError::NotActive);
        }
        let (d, k2, af) = (
            deposit_txid.trim().to_ascii_lowercase(),
            kick_off_2_txid.trim().to_ascii_lowercase(),
            assert_final_txid.trim().to_ascii_lowercase(),
        );
        if !is_hex64(&d) || !is_hex64(&k2) || !is_hex64(&af) {
            return Err(RegistryError::BadInput("graph bind txids must be 32-byte hex"));
        }
        if k2 == af || d == k2 || d == af {
            return Err(RegistryError::BadInput("graph bind txids must differ"));
        }
        let key = format!("{d}:{}", r.btc_xonly_hex);
        if self.graphs.contains_key(&key) {
            return Err(RegistryError::AlreadyBound);
        }
        // A graph tx can belong to one (non-cosign) binding only.
        if self.graphs.values().any(|g| g.kick_off_2_txid == k2 || g.assert_final_txid == af) {
            return Err(RegistryError::BadInput("graph txid already bound elsewhere"));
        }
        self.insert_bind(key, account, r.btc_xonly_hex.clone(), d, k2, af, height, false);
        Ok(())
    }

    /// Co-signer attestation: `account` (an n-of-n seat of the deposit's
    /// ceremony) vouches for an operator graph ALREADY bound for `deposit_txid`
    /// with exactly these txids.
    pub fn cosign_graph(
        &mut self,
        account: &str,
        deposit_txid: &str,
        kick_off_2_txid: &str,
        assert_final_txid: &str,
        height: u64,
    ) -> Result<(), RegistryError> {
        let r = self.operators.get(account).ok_or(RegistryError::Unknown)?;
        if matches!(r.status, OperatorStatus::Removed | OperatorStatus::Unbonding { .. }) {
            return Err(RegistryError::NotActive);
        }
        let (d, k2, af) = (
            deposit_txid.trim().to_ascii_lowercase(),
            kick_off_2_txid.trim().to_ascii_lowercase(),
            assert_final_txid.trim().to_ascii_lowercase(),
        );
        let key = format!("{d}:{}", r.btc_xonly_hex);
        if self.graphs.contains_key(&key) {
            return Err(RegistryError::AlreadyBound);
        }
        if !self
            .graphs
            .values()
            .any(|g| !g.cosign && g.deposit_txid == d && g.kick_off_2_txid == k2 && g.assert_final_txid == af)
        {
            return Err(RegistryError::BadInput("cosign: no operator graph bound with these txids for this deposit"));
        }
        self.insert_bind(key, account, r.btc_xonly_hex.clone(), d, k2, af, height, true);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_bind(&mut self, key: String, account: &str, x: String, d: String, k2: String, af: String, height: u64, cosign: bool) {
        self.graphs.insert(
            key,
            GraphBind {
                account: account.to_string(),
                operator_btc_xonly_hex: x,
                deposit_txid: d,
                kick_off_2_txid: k2,
                assert_final_txid: af,
                bound_height: height,
                depositor_account: String::new(),
                cosign,
            },
        );
    }

    /// All bindings for one peg-in confirm txid.
    pub fn graph_bindings_for(&self, deposit_txid: &str) -> Vec<&GraphBind> {
        let d = deposit_txid.trim().to_ascii_lowercase();
        self.graphs.values().filter(|g| g.deposit_txid == d).collect()
    }

    /// Record the depositor Falcon account on a binding (set once, at bind).
    pub fn set_graph_depositor(&mut self, deposit_txid: &str, operator_btc_xonly_hex: &str, acct: &str) {
        let key = format!(
            "{}:{}",
            deposit_txid.trim().to_ascii_lowercase(),
            operator_btc_xonly_hex.trim().to_ascii_lowercase()
        );
        if let Some(g) = self.graphs.get_mut(&key) {
            g.depositor_account = acct.trim().to_string();
        }
    }

    pub fn graph_binding(&self, deposit_txid: &str, operator_btc_xonly_hex: &str) -> Option<&GraphBind> {
        self.graphs.get(&format!(
            "{}:{}",
            deposit_txid.trim().to_ascii_lowercase(),
            operator_btc_xonly_hex.trim().to_ascii_lowercase()
        ))
    }

    /// Registered operator (other than `except`) whose declared reward script is `spk`.
    pub fn account_by_reward_spk(&self, spk: &[u8], except: &str) -> Option<String> {
        if spk.is_empty() {
            return None;
        }
        let h = hex::encode(spk);
        self.operators
            .values()
            .find(|r| r.account != except && !r.reward_spk_hex.is_empty() && r.reward_spk_hex == h)
            .map(|r| r.account.clone())
    }

    pub fn by_account(&self, account: &str) -> Option<&OperatorRecord> {
        self.operators.get(account)
    }

    /// Credit a proven payout by operator account (claim reached PaidOptimistic).
    pub fn credit_payout_account(&mut self, epoch: u64, claim_id: &str, account: &str, amount_sats: u64) -> Result<(), RegistryError> {
        let key = self.operators.get(account).ok_or(RegistryError::Unknown)?.btc_xonly_hex.clone();
        self.credit_payout(epoch, &PayoutProof { claim_id, operator_btc_xonly_hex: &key, amount_sats })
    }

    fn by_key_mut(&mut self, xonly: &str) -> Option<&mut OperatorRecord> {
        let k = xonly.trim().to_ascii_lowercase();
        self.operators.values_mut().find(|r| r.btc_xonly_hex == k)
    }

    /// Operators whose graphs a new deposit must presign (multi-operator graphs):
    /// every non-removed, non-unbonding, non-jailed operator, sorted by key.
    pub fn graph_set(&self, p: &OperatorParams, epoch: u64) -> Vec<&OperatorRecord> {
        let mut v: Vec<_> = self
            .operators
            .values()
            .filter(|r| Self::status_ok(epoch, r) && r.bond >= p.min_bond.max(1))
            .collect();
        v.sort_by(|a, b| a.btc_xonly_hex.cmp(&b.btc_xonly_hex));
        v
    }

    pub fn credit_payout(&mut self, epoch: u64, proof: &PayoutProof) -> Result<(), RegistryError> {
        if self.credited.contains_key(proof.claim_id) {
            return Err(RegistryError::AlreadyCredited);
        }
        let r = self.by_key_mut(proof.operator_btc_xonly_hex).ok_or(RegistryError::Unknown)?;
        if r.status == OperatorStatus::Removed {
            return Err(RegistryError::Removed);
        }
        r.completed_payouts += 1;
        r.completed_sats = r.completed_sats.saturating_add(proof.amount_sats);
        r.epoch_sats = r.epoch_sats.saturating_add(proof.amount_sats);
        r.last_payout_epoch = Some(epoch);
        let acct = r.account.clone();
        self.credited.insert(proof.claim_id.to_string(), acct);
        Ok(())
    }

    /// Signed heartbeat for `slot` (on-chain OperatorHeartbeat tx, like watcher
    /// slots). Counts once per slot; each live slot is also 1 watch unit, since
    /// a live kit is watching all claims.
    pub fn heartbeat(&mut self, p: &OperatorParams, account: &str, slot_in_epoch: u64) -> Result<(), RegistryError> {
        let r = self.operators.get_mut(account).ok_or(RegistryError::Unknown)?;
        if r.status == OperatorStatus::Removed {
            return Err(RegistryError::Removed);
        }
        if slot_in_epoch < p.slots_per_epoch && r.epoch_slots.insert(slot_in_epoch) {
            r.epoch_watch_units += 1;
        }
        Ok(())
    }

    /// A watcher disproved this operator's assertion (Disprove tx proven on the
    /// BTC rail / claim status `Disproved`). Slash, jail or remove the operator;
    /// credit the disprover (if a registered operator) with watch units.
    /// Returns (slashed, bounty_to_disprover, burned).
    pub fn disprove(
        &mut self,
        p: &OperatorParams,
        epoch: u64,
        claim_id: &str,
        operator_btc_xonly_hex: &str,
        disprover_account: Option<&str>,
    ) -> Result<(u64, u64, u64), RegistryError> {
        let tag = format!("disprove:{claim_id}");
        if self.credited.contains_key(&tag) {
            return Err(RegistryError::AlreadyCredited);
        }
        let r = self.by_key_mut(operator_btc_xonly_hex).ok_or(RegistryError::Unknown)?;
        let cut = (r.bond as u128 * p.slash_bps.min(10_000) as u128 / 10_000) as u64;
        r.bond -= cut;
        r.slashed_total = r.slashed_total.saturating_add(cut);
        r.disproves += 1;
        r.epoch_sats = 0; // forfeits this epoch's volume credit
        r.epoch_watch_units = 0;
        r.status = if r.disproves >= p.max_disproves.max(1) {
            OperatorStatus::Removed
        } else {
            OperatorStatus::Jailed { until_epoch: epoch + p.disprove_jail_epochs }
        };
        let bad = r.account.clone();
        // Bounty only to a registered operator other than the cheater; otherwise
        // the whole cut is burned (copying someone else's evidence earns nothing).
        let mut bounty = 0u64;
        if let Some(w) = disprover_account.filter(|w| *w != bad) {
            if let Some(wr) = self.operators.get_mut(w) {
                wr.epoch_watch_units = wr.epoch_watch_units.saturating_add(p.disprove_watch_units);
                wr.disproves_won += 1;
                bounty = (cut as u128 * p.disprove_bounty_bps.min(10_000) as u128 / 10_000) as u64;
            }
        }
        self.credited.insert(tag, bad);
        self.burned_total = self.burned_total.saturating_add(cut - bounty);
        Ok((cut, bounty, cut - bounty))
    }

    pub fn begin_unbond(&mut self, p: &OperatorParams, epoch: u64, account: &str) -> Result<(), RegistryError> {
        let r = self.operators.get_mut(account).ok_or(RegistryError::Unknown)?;
        if !Self::status_ok(epoch, r) {
            return Err(RegistryError::NotActive);
        }
        r.status = OperatorStatus::Unbonding { release_epoch: epoch + p.unbond_cooldown_epochs };
        Ok(())
    }

    /// Voluntary exits whose cooldown has passed: the record is dropped (the
    /// account and key may register again) and the remaining bond is returned
    /// to the caller for crediting. Disproved operators are never dropped here.
    pub fn release_unbonded(&mut self, epoch: u64) -> Vec<(String, u64)> {
        let done: Vec<String> = self
            .operators
            .values()
            .filter(|r| matches!(r.status, OperatorStatus::Unbonding { release_epoch } if epoch >= release_epoch))
            .map(|r| r.account.clone())
            .collect();
        let mut out = Vec::new();
        for a in done {
            if let Some(r) = self.operators.remove(&a) {
                out.push((a, r.bond));
            }
        }
        out
    }

    fn status_ok(epoch: u64, r: &OperatorRecord) -> bool {
        match r.status {
            OperatorStatus::Active => true,
            OperatorStatus::Jailed { until_epoch } => epoch >= until_epoch,
            _ => false,
        }
    }

    /// Liveness + watching parts: registered, bonded, not jailed, live enough.
    pub fn eligible_maintenance(&self, p: &OperatorParams, epoch: u64, r: &OperatorRecord) -> bool {
        Self::status_ok(epoch, r) && r.bond >= p.min_bond && r.epoch_slots.len() as u64 >= p.min_liveness_slots
    }

    /// Payout part: as above (liveness not required) + proven payouts.
    pub fn eligible_payout(&self, p: &OperatorParams, epoch: u64, r: &OperatorRecord) -> bool {
        Self::status_ok(epoch, r) && r.bond >= p.min_bond && r.completed_payouts >= p.min_completed_payouts.max(1) && r.epoch_sats > 0
    }

    /// Split `bucket`: liveness (equal among live operators), watching (pro-rata
    /// watch units), payouts (pro-rata proven sats, capped). Returns (payouts by
    /// account, unpaid remainder -> treasury). Resets per-epoch counters.
    pub fn settle_epoch(&mut self, p: &OperatorParams, epoch: u64, bucket: u64) -> (BTreeMap<String, u64>, u64) {
        let bps = |x: u64, b: u64| (x as u128 * b.min(10_000) as u128 / 10_000) as u64;
        let live_part = bps(bucket, p.liveness_share_bps);
        let watch_part = bps(bucket, p.watching_share_bps).min(bucket - live_part);
        let pay_part = bucket - live_part - watch_part;
        let maint: Vec<String> = self.operators.values().filter(|r| self.eligible_maintenance(p, epoch, r)).map(|r| r.account.clone()).collect();
        let payers: Vec<String> = self.operators.values().filter(|r| self.eligible_payout(p, epoch, r)).map(|r| r.account.clone()).collect();
        let mut out: BTreeMap<String, u64> = BTreeMap::new();
        if !maint.is_empty() {
            let each = live_part / maint.len() as u64;
            let units: u128 = maint.iter().map(|a| self.operators[a].epoch_watch_units as u128).sum();
            for a in &maint {
                let mut amt = each;
                if units > 0 {
                    amt += (watch_part as u128 * self.operators[a].epoch_watch_units as u128 / units) as u64;
                }
                *out.entry(a.clone()).or_default() += amt;
            }
        }
        if !payers.is_empty() {
            let cap = bps(bucket, p.cap_per_operator_bps);
            let total: u128 = payers.iter().map(|a| self.operators[a].epoch_sats as u128).sum();
            for a in &payers {
                let v = (pay_part as u128 * self.operators[a].epoch_sats as u128 / total) as u64;
                *out.entry(a.clone()).or_default() += v.min(cap);
            }
        }
        out.retain(|_, v| *v > 0);
        let paid: u64 = out.values().sum();
        for r in self.operators.values_mut() {
            r.epoch_sats = 0;
            r.epoch_slots.clear();
            r.epoch_watch_units = 0;
        }
        (out, bucket.saturating_sub(paid))
    }
}

/// Decided default (Scott, 2026-10-18): preset B shape with confirmed bond/slash.
/// emission_share_bps=500 (5% off the top), min_bond=5_000 FPL, slash_bps=10_000 (100%).
/// Liveness/watching/payout split and disprove_bounty_bps remain B (bounty still open).
impl Default for OperatorParams {
    fn default() -> Self {
        OperatorParams::PROPOSAL_B
    }
}

/// Operator bucket carved OFF THE TOP of epochEmit (before the
/// validators/watchers/AMM/lend split); 0 before activation.
pub fn operator_bucket(network_id: u32, height: u64, epoch_emit: u64, p: &OperatorParams) -> u64 {
    if !operator_rewards_active(network_id, height) {
        return 0;
    }
    (epoch_emit as u128 * p.emission_share_bps.min(10_000) as u128 / 10_000) as u64
}

/// Payout fee check: an operator may front `amount - fee` with fee <= cap.
pub fn payout_fee_ok(p: &OperatorParams, amount: u64, fronted: u64) -> bool {
    fronted <= amount && (amount - fronted) as u128 * 10_000 <= amount as u128 * p.max_payout_fee_bps as u128
}

#[cfg(test)]
mod tests {
    use super::*;
    const K1: &str = "11111111111111111111111111111111111111111111111111111111111111aa";
    const K2: &str = "22222222222222222222222222222222222222222222222222222222222222bb";

    #[test]
    fn unarmed_everywhere() {
        assert_eq!(TESTNET_2300_OPERATOR_REWARDS_FROM_HEIGHT, 692_000);
        assert_eq!(operator_bucket(NETWORK_ID_PUBLIC_TESTNET_2300, 691_999, 1_000_000, &OperatorParams::PROPOSAL_B), 0);
        assert!(operator_bucket(NETWORK_ID_PUBLIC_TESTNET_2300, 692_000, 1_000_000, &OperatorParams::PROPOSAL_B) > 0);
    }

    #[test]
    fn register_bond_and_dupes() {
        let p = OperatorParams::PROPOSAL_B;
        let mut g = OperatorRegistry::default();
        assert_eq!(g.register(&p, "a", K1, 1, 0), Err(RegistryError::BondTooLow { need: p.min_bond, got: 1 }));
        g.register(&p, "a", K1, p.min_bond, 0).unwrap();
        assert_eq!(g.register(&p, "a", K2, p.min_bond, 0), Err(RegistryError::AlreadyRegistered));
        assert_eq!(g.register(&p, "b", K1, p.min_bond, 0), Err(RegistryError::DuplicateBtcKey));
        assert_eq!(g.register(&p, "b", "zz", p.min_bond, 0), Err(RegistryError::BadBtcKey));
        assert_eq!(g.graph_set(&p, 0).len(), 1);
    }

    fn live(g: &mut OperatorRegistry, p: &OperatorParams, a: &str, slots: u64) {
        for s in 0..slots {
            g.heartbeat(p, a, s).unwrap();
        }
    }

    #[test]
    fn liveness_watching_payout_split() {
        let p = OperatorParams::PROPOSAL_B;
        let mut g = OperatorRegistry::default();
        g.register(&p, "a", K1, p.min_bond, 0).unwrap();
        g.register(&p, "b", K2, p.min_bond, 0).unwrap();
        // nobody live, nobody paid -> all back to treasury
        let (out, rest) = g.settle_epoch(&p, 1, 1_000_000);
        assert!(out.is_empty());
        assert_eq!(rest, 1_000_000);
        // both live (a full, b at threshold); duplicate heartbeat slot counts once
        live(&mut g, &p, "a", 168);
        live(&mut g, &p, "b", 151);
        g.heartbeat(&p, "b", 0).unwrap();
        g.credit_payout(2, &PayoutProof { claim_id: "c1", operator_btc_xonly_hex: K1, amount_sats: 30_000 }).unwrap();
        g.credit_payout(2, &PayoutProof { claim_id: "c2", operator_btc_xonly_hex: K2, amount_sats: 10_000 }).unwrap();
        assert_eq!(
            g.credit_payout(2, &PayoutProof { claim_id: "c1", operator_btc_xonly_hex: K1, amount_sats: 1 }),
            Err(RegistryError::AlreadyCredited)
        );
        let (out, rest) = g.settle_epoch(&p, 2, 1_000_000);
        // liveness 250k -> 125k each; watching 250k by units 168:151; payouts 500k 3:1 capped at 250k
        let wa = 250_000u128 * 168 / 319;
        let wb = 250_000u128 * 151 / 319;
        assert_eq!(out["a"], 125_000 + wa as u64 + 250_000);
        assert_eq!(out["b"], 125_000 + wb as u64 + 125_000);
        assert_eq!(rest, 1_000_000 - out["a"] - out["b"]);
        // below the liveness threshold and no payout -> nothing
        live(&mut g, &p, "a", 100);
        let (out, _) = g.settle_epoch(&p, 3, 1_000_000);
        assert!(out.is_empty());
    }

    #[test]
    fn watcher_earns_without_payouts() {
        let p = OperatorParams::PROPOSAL_B;
        let mut g = OperatorRegistry::default();
        g.register(&p, "w", K1, p.min_bond, 0).unwrap();
        live(&mut g, &p, "w", 168);
        let (out, _) = g.settle_epoch(&p, 1, 1_000_000);
        assert_eq!(out["w"], 250_000 + 250_000); // liveness + all watch units; payout part unpaid
    }

    #[test]
    fn disprove_slashes_jails_removes_and_pays_disprover() {
        let p = OperatorParams::PROPOSAL_B;
        let mut g = OperatorRegistry::default();
        g.register(&p, "a", K1, 20_000, 0).unwrap();
        g.register(&p, "w", K2, p.min_bond, 0).unwrap();
        g.credit_payout(1, &PayoutProof { claim_id: "c1", operator_btc_xonly_hex: K1, amount_sats: 5 }).unwrap();
        assert_eq!(g.disprove(&p, 1, "c9", K1, Some("w")).unwrap(), (20_000, 10_000, 10_000));
        assert_eq!(g.operators["w"].epoch_watch_units, p.disprove_watch_units);
        assert_eq!(g.operators["w"].disproves_won, 1);
        assert_eq!(g.operators["a"].status, OperatorStatus::Jailed { until_epoch: 1 + p.disprove_jail_epochs });
        assert_eq!(g.graph_set(&p, 2).len(), 1); // only w
        assert!(!g.eligible_payout(&p, 2, &g.operators["a"]));
        assert_eq!(g.disprove(&p, 1, "c9", K1, None), Err(RegistryError::AlreadyCredited));
        // self-disprove earns nothing
        g.disprove(&p, 3, "c10", K1, Some("a")).unwrap();
        assert_eq!(g.operators["a"].status, OperatorStatus::Removed);
        assert_eq!(g.operators["a"].disproves_won, 0);
        assert_eq!(g.register(&p, "a", K1, 20_000, 9), Err(RegistryError::Removed));
    }

    #[test]
    fn fee_cap() {
        let p = OperatorParams::PROPOSAL_A; // 30 bps
        assert!(payout_fee_ok(&p, 100_000, 99_700));
        assert!(!payout_fee_ok(&p, 100_000, 99_699));
        assert!(!payout_fee_ok(&p, 100_000, 100_001));
    }

    #[test]
    fn unbond_then_removed_after_cooldown() {
        let p = OperatorParams::PROPOSAL_B;
        let mut g = OperatorRegistry::default();
        g.register(&p, "a", K1, p.min_bond, 0).unwrap();
        g.begin_unbond(&p, 5, "a").unwrap();
        assert!(g.graph_set(&p, 5).is_empty());
        assert!(g.release_unbonded(6).is_empty());
        assert!(matches!(g.operators["a"].status, OperatorStatus::Unbonding { .. }));
        assert_eq!(g.release_unbonded(7), vec![("a".to_string(), p.min_bond)]);
        assert!(g.operators.is_empty());
        // may come back with the same account + key
        g.register(&p, "a", K1, p.min_bond, 9).unwrap();
    }

    fn bind(g: &mut OperatorRegistry, a: &str) -> GraphBind {
        let d = "d".repeat(64);
        let k2 = "2".repeat(64);
        let af = "f".repeat(64);
        g.bind_graph(a, &d, &k2, &af, 10).unwrap();
        g.graph_binding(&d, &g.operators[a].btc_xonly_hex.clone()).unwrap().clone()
    }

    fn ptx(inputs: Vec<(String, u32)>, out0: Vec<u8>, out1: Vec<u8>) -> crate::btc_spv::ParsedTx {
        crate::btc_spv::ParsedTx { txid_display: "e".repeat(64), inputs, outputs: vec![(1_000, out0), (1_000, out1)] }
    }

    #[test]
    fn burn_spk_is_p2wsh_of_unspendable_checksig() {
        let s = bitvm_burn_spk();
        assert_eq!(s.len(), 34);
        assert_eq!(&s[..2], &[0x00, 0x20]);
    }

    #[test]
    fn disprove_evidence_shapes() {
        let p = OperatorParams::PROPOSAL_B;
        let mut g = OperatorRegistry::default();
        g.register(&p, "a", K1, p.min_bond, 0).unwrap();
        let b = bind(&mut g, "a");
        let af = b.assert_final_txid.clone();
        let k2 = b.kick_off_2_txid.clone();
        let burn = bitvm_burn_spk();
        let rw = vec![0x51, 0x20, 7, 7];
        // Disprove (either input order)
        assert_eq!(classify_disprove(&ptx(vec![(af.clone(), 1), (af.clone(), 2)], burn.clone(), rw.clone()), &b), Ok(DisproveKind::Disprove));
        assert_eq!(classify_disprove(&ptx(vec![(af.clone(), 2), (af.clone(), 1)], burn.clone(), rw.clone()), &b), Ok(DisproveKind::Disprove));
        // DisproveChain
        assert_eq!(classify_disprove(&ptx(vec![(k2.clone(), 1)], burn.clone(), rw.clone()), &b), Ok(DisproveKind::DisproveChain));
        // AssertInitial-like spend of KickOff2:1 (no burn output) is not evidence
        assert!(classify_disprove(&ptx(vec![(k2.clone(), 1)], rw.clone(), rw.clone()), &b).is_err());
        // Take2-like (4 inputs) is not evidence
        let mut t2 = ptx(vec![("1".repeat(64), 0), (af.clone(), 0), (af.clone(), 1), (af.clone(), 2)], burn.clone(), rw.clone());
        assert!(classify_disprove(&t2, &b).is_err());
        t2.inputs.truncate(1);
        assert!(classify_disprove(&t2, &b).is_err());
        // other graph's txs
        assert!(classify_disprove(&ptx(vec![("9".repeat(64), 1), ("9".repeat(64), 2)], burn, rw), &b).is_err());
    }

    #[test]
    fn graph_bind_rules() {
        let p = OperatorParams::PROPOSAL_B;
        let mut g = OperatorRegistry::default();
        g.register(&p, "a", K1, p.min_bond, 0).unwrap();
        let d = "d".repeat(64);
        assert!(g.bind_graph("zz", &d, &"1".repeat(64), &"2".repeat(64), 1).is_err());
        assert!(g.bind_graph("a", &d, &"1".repeat(64), "xx", 1).is_err());
        g.bind_graph("a", &d, &"1".repeat(64), &"2".repeat(64), 1).unwrap();
        assert_eq!(g.bind_graph("a", &d, &"3".repeat(64), &"4".repeat(64), 1), Err(RegistryError::AlreadyBound));
        g.register(&p, "b", K2, p.min_bond, 0).unwrap();
        assert!(g.bind_graph("b", &d, &"1".repeat(64), &"5".repeat(64), 1).is_err(), "txid reuse");
        g.bind_graph("b", &d, &"6".repeat(64), &"5".repeat(64), 1).unwrap();
        assert!(!g.is_empty());
    }

    #[test]
    fn slash_100pct_then_rebond_after_jail() {
        let p = OperatorParams::PROPOSAL_B;
        let mut g = OperatorRegistry::default();
        g.register_ex(&p, "a", K1, "", "", 0, p.min_bond, 0).unwrap();
        g.register_ex(&p, "w", K2, "5120aa", "http://w", 7, p.min_bond, 0).unwrap();
        assert_eq!(g.account_by_reward_spk(&[0x51, 0x20, 0xaa], "a"), Some("w".into()));
        assert_eq!(g.account_by_reward_spk(&[0x51, 0x20, 0xaa], "w"), None);
        let (cut, bounty, burned) = g.disprove(&p, 3, "ev1", K1, Some("w")).unwrap();
        assert_eq!((cut, bounty, burned), (5_000, 2_500, 2_500));
        assert_eq!(g.operators["a"].bond, 0, "100% slash");
        assert_eq!(g.burned_total, 2_500);
        assert!(g.graph_set(&p, 100).iter().all(|r| r.account != "a"), "no bond -> not in graph set even after jail");
        assert_eq!(g.begin_unbond(&p, 4, "a"), Err(RegistryError::NotActive));
        g.rebond(&p, 3 + p.disprove_jail_epochs, "a", p.min_bond).unwrap();
        assert_eq!(g.operators["a"].status, OperatorStatus::Active);
        assert_eq!(g.graph_set(&p, 3 + p.disprove_jail_epochs).len(), 2);
    }
}
