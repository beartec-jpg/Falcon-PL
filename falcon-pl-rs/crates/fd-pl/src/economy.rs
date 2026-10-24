//! Falcon PL v2.2 economics — epoch emission, sector splits, caps, work weights.
//!
//! See `docs/EXPANSION_PLAN_v2.2.0.md`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Hard cap total supply (FPL base units, 1:1 display for proto).
pub const SUPPLY_CAP: u64 = 200_000_000_000;
/// Protocol treasury share of supply at genesis (98%).
pub const TREASURY_GENESIS: u64 = 196_000_000_000;
/// Circulating genesis (2%).
pub const CIRCULATING_GENESIS: u64 = 4_000_000_000;

/// 7-day epoch in ms.
pub const EPOCH_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// Epochs 1..=BOOTSTRAP_EPOCHS have zero claimable emission.
pub const BOOTSTRAP_EPOCHS: u64 = 7;
/// First claimable epoch index (1-based).
pub const FIRST_CLAIM_EPOCH: u64 = 8;
/// First claimable emission bps of remaining treasury (30 bps = 0.30%).
pub const EMISSION_BPS_FIRST: u64 = 30;
/// Minimum bond to enter lottery / claim (FPL units).
pub const MIN_BOND: u64 = 1_000;

/// Cost to clear jail after `jail_count` offences: `min_bond * 2^jail_count`.
///
/// - Never jailed (`jail_count = 0`): equals `min_bond` (initial bond).
/// - After the 1st jail (`jail_count = 1`): `2 * min_bond` (e.g. 2_000).
/// - After the 2nd jail (`jail_count = 2`): `4 * min_bond` (e.g. 4_000).
/// - And so on (8k, 16k, …). Saturates at a 63-bit shift.
pub fn unjail_cost(min_bond: u64, jail_count: u32) -> u64 {
    let shift = jail_count.min(63);
    min_bond.saturating_mul(1u64 << shift)
}

/// Jail count used for Unjail pricing.
///
/// Seats jailed before `jail_count` existed deserialize as `jailed=true` with
/// `jail_count=0`. Treat that as one offence so the first Unjail costs
/// `2 * min_bond`, not `min_bond` (which would under-charge a real jail).
pub fn effective_jail_count(jailed: bool, jail_count: u32) -> u32 {
    if jailed && jail_count == 0 {
        1
    } else {
        jail_count
    }
}
/// Fee burn bps (5000 = 50%).
pub const FEE_BURN_BPS: u64 = 5_000;
/// Mainnet-style unbond cooldown (14 days). Private fleet may override via config.
pub const UNBOND_COOLDOWN_MS: u64 = 14 * 24 * 60 * 60 * 1000;
/// Private-fleet friendly cooldown (1 day) — set `ChainConfig.unbond_cooldown_ms`.
pub const UNBOND_COOLDOWN_PRIVATE_MS: u64 = 24 * 60 * 60 * 1000;

/// Top-level emission shares (must sum 100).
pub const SHARE_VALIDATORS_PCT: u64 = 55;
pub const SHARE_WATCHERS_PCT: u64 = 5;
pub const SHARE_AMM_PCT: u64 = 20;
pub const SHARE_LEND_PCT: u64 = 20;

/// Cap as ten-thousandths of epochEmit (50 = 0.50%, 5 = 0.05%).
pub const CAP_LP_PER_ACCOUNT_E4: u64 = 50; // 0.50%
pub const CAP_WATCHER_PER_ACCOUNT_E4: u64 = 5; // 0.05%

/// Hourly slots in a 7-day epoch.
pub const WATCHER_SLOTS_PER_EPOCH: u64 = 7 * 24;

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EpochWork {
    /// Packer: txs included in accepted non-empty ledgers this epoch.
    pub pack_txs: BTreeMap<String, u64>,
    /// Checker: qualifying OK votes this epoch (ledger_hash counted once per voter).
    pub check_votes: BTreeMap<String, u64>,
    /// Watcher: work units this epoch.
    pub watcher_work: BTreeMap<String, u64>,
    /// Watcher: bitset of hourly slots as u64 words? simplified: count of active slots.
    pub watcher_slots: BTreeMap<String, u64>,
    /// Fees accumulated into reward path this epoch (after burn already applied externally).
    pub fee_reward_pool: u64,
    /// Ledger hashes already counted for a voter's check credit.
    #[serde(skip)]
    pub check_seen: BTreeMap<String, std::collections::BTreeSet<String>>,
    /// v2.9.62 (epochs >= E_v2): persisted hourly slot bitset per watcher
    /// (168 bits in 3 words). Replaces the `#[serde(skip)]` dedup set, which
    /// was lost on restart and let seats count a slot twice (audit bug 1).
    /// Empty (and not serialized) before v2, so snapshots are byte-identical.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub watcher_slot_bits: BTreeMap<String, [u64; 3]>,
    /// v2.9.62 B3: proposer credits (`64 + min(fee-paying txs, 64)` per sealed block).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub proposer_credit: BTreeMap<String, u64>,
    /// v2.9.62 B4: +1 per committed block whose `parent_cert` the seat signed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vote_credit: BTreeMap<String, u64>,
    /// v2.9.62 B7 hold rule: LP units held at epoch start (`"amm|pool|acct"`,
    /// `"lend|market|acct"`). Settle values `min(start, now)`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lp_start: BTreeMap<String, u64>,
    /// True once `lp_start` was captured at this epoch's start (else the hold
    /// rule falls back to current units, e.g. the first epoch after a roll).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lp_start_taken: bool,
    /// Heights in this epoch counted for the uptime floor (0 before the
    /// presence height, so snapshots stay byte-identical).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub presence_heights: u64,
    /// Heights this epoch the seat sealed or was named in the parent certificate.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub present: BTreeMap<String, u64>,
}

impl EpochWork {
    /// v2: mark `slot` (0..168) for `account` in the persisted bitset; count it once.
    pub fn record_watcher_slot_v2(&mut self, account: &str, slot: u64) {
        if slot >= WATCHER_SLOTS_PER_EPOCH {
            return;
        }
        let words = self
            .watcher_slot_bits
            .entry(account.to_string())
            .or_insert([0u64; 3]);
        let (i, bit) = ((slot / 64) as usize, 1u64 << (slot % 64));
        if words[i] & bit == 0 {
            words[i] |= bit;
            *self.watcher_slots.entry(account.to_string()).or_insert(0) += 1;
        }
    }

    /// True if `account` filled `slot` this epoch, from the persisted v2 bitset
    /// or the legacy (pre-v2, non-persisted) dedup set.
    pub fn watcher_has_slot(&self, account: &str, slot: u64) -> bool {
        if slot < WATCHER_SLOTS_PER_EPOCH {
            if let Some(w) = self.watcher_slot_bits.get(account) {
                if w[(slot / 64) as usize] & (1u64 << (slot % 64)) != 0 {
                    return true;
                }
            }
        }
        self.check_seen
            .get(&format!("wslot:{account}"))
            .map(|s| s.contains(&slot.to_string()))
            .unwrap_or(false)
    }

    /// Number of distinct watchers present in `slot` this epoch.
    pub fn watchers_in_slot(&self, slot: u64) -> u64 {
        let mut n = 0u64;
        for (acct, w) in &self.watcher_slot_bits {
            let in_bits = slot < WATCHER_SLOTS_PER_EPOCH
                && w[(slot / 64) as usize] & (1u64 << (slot % 64)) != 0;
            if in_bits || self.legacy_has_slot(acct, slot) {
                n += 1;
            }
        }
        let s = slot.to_string();
        for (k, set) in &self.check_seen {
            if let Some(acct) = k.strip_prefix("wslot:") {
                if !self.watcher_slot_bits.contains_key(acct) && set.contains(&s) {
                    n += 1;
                }
            }
        }
        n
    }

    fn legacy_has_slot(&self, account: &str, slot: u64) -> bool {
        self.check_seen
            .get(&format!("wslot:{account}"))
            .map(|s| s.contains(&slot.to_string()))
            .unwrap_or(false)
    }

    pub fn record_pack(&mut self, packer: &str, n_txs: u64) {
        *self.pack_txs.entry(packer.to_string()).or_insert(0) += n_txs;
    }

    pub fn record_check(&mut self, voter: &str, ledger_hash: &str) {
        let seen = self
            .check_seen
            .entry(voter.to_string())
            .or_default();
        if seen.insert(ledger_hash.to_string()) {
            *self.check_votes.entry(voter.to_string()).or_insert(0) += 1;
        }
    }

    pub fn record_watcher_work(&mut self, account: &str, units: u64) {
        *self.watcher_work.entry(account.to_string()).or_insert(0) += units;
    }

    pub fn record_watcher_slot(&mut self, account: &str, slot: u64) {
        // store max slot index seen + use a set via counting unique slots in a set
        let key = account.to_string();
        let s = self
            .check_seen
            .entry(format!("wslot:{key}"))
            .or_default();
        if s.insert(slot.to_string()) {
            *self.watcher_slots.entry(key).or_insert(0) += 1;
        }
    }
}

/// Compute epoch emission from remaining treasury.
pub fn epoch_emit(treasury: u64, emission_bps: u64) -> u64 {
    treasury.saturating_mul(emission_bps) / 10_000
}

/// Split top-level buckets from epochEmit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopBuckets {
    pub validators: u64,
    pub watchers: u64,
    pub amm: u64,
    pub lend: u64,
}

pub fn top_buckets(epoch_emit: u64) -> TopBuckets {
    TopBuckets {
        validators: epoch_emit * SHARE_VALIDATORS_PCT / 100,
        watchers: epoch_emit * SHARE_WATCHERS_PCT / 100,
        amm: epoch_emit * SHARE_AMM_PCT / 100,
        lend: epoch_emit * SHARE_LEND_PCT / 100,
    }
}

pub fn cap_lp(epoch_emit: u64) -> u64 {
    epoch_emit.saturating_mul(CAP_LP_PER_ACCOUNT_E4) / 10_000
}

pub fn cap_watcher(epoch_emit: u64) -> u64 {
    epoch_emit.saturating_mul(CAP_WATCHER_PER_ACCOUNT_E4) / 10_000
}

/// Pro-rata with per-account cap; returns (paid_per_id, remainder_unpaid).
pub fn pro_rata_capped(
    pot: u64,
    weights: &BTreeMap<String, u64>,
    per_cap: u64,
) -> (BTreeMap<String, u64>, u64) {
    let total_w: u64 = weights.values().sum();
    if pot == 0 || total_w == 0 {
        return (BTreeMap::new(), pot);
    }
    let mut paid = BTreeMap::new();
    let mut used = 0u64;
    for (id, w) in weights {
        if *w == 0 {
            continue;
        }
        let raw = pot.saturating_mul(*w) / total_w;
        let give = raw.min(per_cap);
        if give > 0 {
            paid.insert(id.clone(), give);
            used = used.saturating_add(give);
        }
    }
    (paid, pot.saturating_sub(used))
}

/// Watcher weight = work * presence; presence = slots/slots_total.
pub fn watcher_weights(
    work: &BTreeMap<String, u64>,
    slots: &BTreeMap<String, u64>,
    slots_total: u64,
) -> BTreeMap<String, u64> {
    let slots_total = slots_total.max(1);
    let mut out = BTreeMap::new();
    for (id, w) in work {
        if *w == 0 {
            continue;
        }
        let s = *slots.get(id).unwrap_or(&0);
        // presence in ppm (parts per million) then weight = work * presence_ppm / 1e6
        // use: weight = work * slots / slots_total  (integer)
        let weight = w.saturating_mul(s.min(slots_total)) / slots_total;
        if weight > 0 {
            out.insert(id.clone(), weight);
        }
    }
    out
}

/// Full epoch settlement for validators + watchers (LP pots empty unless weights passed).
#[derive(Debug, Clone)]
pub struct Settlement {
    pub epoch_emit: u64,
    pub validator_pot: u64,
    pub pack_pays: BTreeMap<String, u64>,
    pub check_pays: BTreeMap<String, u64>,
    pub watcher_pays: BTreeMap<String, u64>,
    pub amm_pays: BTreeMap<String, u64>,
    pub lend_pays: BTreeMap<String, u64>,
    /// v2.9.62 B5 floor pays (empty for v1).
    pub floor_pays: BTreeMap<String, u64>,
    /// Operator bucket carved off the top of epoch_emit (0 before operator
    /// activation). Not paid here; whatever the operator registry does not pay
    /// is refunded to treasury by the caller with the rest of the undistributed emit.
    pub operator_bucket: u64,
}

pub fn settle_epoch(
    treasury: u64,
    emission_bps: u64,
    claimable: bool,
    work: &EpochWork,
    amm_lp_weights: &BTreeMap<String, u64>,
    lend_lp_weights: &BTreeMap<String, u64>,
    fee_reward_to_validators: u64,
) -> Settlement {
    settle_epoch_ops(treasury, emission_bps, claimable, work, amm_lp_weights, lend_lp_weights, fee_reward_to_validators, |_| 0)
}

/// Legacy v1 settle with the operator bucket carved off the top (same rule as
/// [`settle_epoch_v2_ops`]); per-account LP / watcher caps use the post-carve
/// emit. `op_bucket` returns 0 before the bridge height -> byte-identical.
#[allow(clippy::too_many_arguments)]
pub fn settle_epoch_ops(
    treasury: u64,
    emission_bps: u64,
    claimable: bool,
    work: &EpochWork,
    amm_lp_weights: &BTreeMap<String, u64>,
    lend_lp_weights: &BTreeMap<String, u64>,
    fee_reward_to_validators: u64,
    op_bucket: impl Fn(u64) -> u64,
) -> Settlement {
    let epoch_emit = if claimable {
        epoch_emit(treasury, emission_bps)
    } else {
        0
    };
    let operator_bucket = op_bucket(epoch_emit).min(epoch_emit);
    let post = epoch_emit - operator_bucket;
    let top = top_buckets(post);
    let lp_cap = cap_lp(post).max(1);
    let w_cap = cap_watcher(post).max(1);

    let (amm_pays, amm_rem) = pro_rata_capped(top.amm, amm_lp_weights, lp_cap);
    let (lend_pays, lend_rem) = pro_rata_capped(top.lend, lend_lp_weights, lp_cap);

    let w_weights = watcher_weights(
        &work.watcher_work,
        &work.watcher_slots,
        WATCHER_SLOTS_PER_EPOCH,
    );
    let (watcher_pays, watch_rem) = pro_rata_capped(top.watchers, &w_weights, w_cap);

    let validator_pot = top
        .validators
        .saturating_add(amm_rem)
        .saturating_add(lend_rem)
        .saturating_add(watch_rem)
        .saturating_add(fee_reward_to_validators)
        .saturating_add(work.fee_reward_pool);

    let pack_half = validator_pot / 2;
    let check_half = validator_pot.saturating_sub(pack_half);

    let (pack_pays, _) = pro_rata_capped(pack_half, &work.pack_txs, u64::MAX);
    let (check_pays, _) = pro_rata_capped(check_half, &work.check_votes, u64::MAX);

    Settlement {
        epoch_emit,
        validator_pot,
        pack_pays,
        check_pays,
        watcher_pays,
        amm_pays,
        lend_pays,
        floor_pays: BTreeMap::new(),
        operator_bucket,
    }
}

/// v2.9.62 B1: per-fee split (burn 50%, proposer 30%, pot = remainder).
pub fn fee_split_v2(fee: u64) -> (u64, u64, u64) {
    let burn = fee * crate::params_v2::FEE_BURN_PCT / 100;
    let proposer = fee * crate::params_v2::FEE_PROPOSER_PCT / 100;
    (burn, proposer, fee - burn - proposer)
}

/// v2.9.62 B3 credit for one sealed block.
pub fn proposer_block_credit(fee_paying_txs: u64) -> u64 {
    crate::params_v2::BLOCK_BASE_CREDIT + fee_paying_txs.min(crate::params_v2::TX_BONUS_CAP)
}

/// Pro-rata in u128 (weights may be large FPL values); per-account cap.
pub fn pro_rata_capped_u128(
    pot: u64,
    weights: &BTreeMap<String, u64>,
    per_cap: u64,
) -> (BTreeMap<String, u64>, u64) {
    let total_w: u128 = weights.values().map(|w| *w as u128).sum();
    if pot == 0 || total_w == 0 {
        return (BTreeMap::new(), pot);
    }
    let mut paid = BTreeMap::new();
    let mut used = 0u64;
    for (id, w) in weights {
        if *w == 0 {
            continue;
        }
        let raw = ((pot as u128) * (*w as u128) / total_w) as u64;
        let give = raw.min(per_cap);
        if give > 0 {
            paid.insert(id.clone(), give);
            used = used.saturating_add(give);
        }
    }
    (paid, pot.saturating_sub(used))
}

/// v2.9.62 B2 settlement: validator pot = validator bucket + unpaid LP/watcher
/// remainders + fee pot (the 20% part) + slashed; split 50/40/10 over
/// proposer credits / vote credits / floor weights (100, or 105 for archive).
pub fn settle_epoch_v2(
    treasury: u64,
    emission_bps: u64,
    claimable: bool,
    work: &EpochWork,
    amm_lp_weights: &BTreeMap<String, u64>,
    lend_lp_weights: &BTreeMap<String, u64>,
    floor_weights: &BTreeMap<String, u64>,
) -> Settlement {
    settle_epoch_v2_ops(treasury, emission_bps, claimable, work, amm_lp_weights, lend_lp_weights, floor_weights, |_| 0)
}

/// `settle_epoch_v2` with an operator bucket carved off the top of epoch_emit
/// before the 55/5/20/20 split. `op_bucket(epoch_emit)` must return 0 before
/// operator activation, which makes this byte-identical to the legacy path.
#[allow(clippy::too_many_arguments)]
pub fn settle_epoch_v2_ops(
    treasury: u64,
    emission_bps: u64,
    claimable: bool,
    work: &EpochWork,
    amm_lp_weights: &BTreeMap<String, u64>,
    lend_lp_weights: &BTreeMap<String, u64>,
    floor_weights: &BTreeMap<String, u64>,
    op_bucket: impl Fn(u64) -> u64,
) -> Settlement {
    let epoch_emit = if claimable { epoch_emit(treasury, emission_bps) } else { 0 };
    let operator_bucket = op_bucket(epoch_emit).min(epoch_emit);
    // Per-account LP / watcher caps scale with the post-carve emit, so the
    // operator carve cannot be clawed back by capped accounts.
    let post = epoch_emit - operator_bucket;
    let top = top_buckets(post);
    let lp_cap = cap_lp(post).max(1);
    let w_cap = cap_watcher(post).max(1);
    let (amm_pays, amm_rem) = pro_rata_capped_u128(top.amm, amm_lp_weights, lp_cap);
    let (lend_pays, lend_rem) = pro_rata_capped_u128(top.lend, lend_lp_weights, lp_cap);
    let w_weights = watcher_weights(&work.watcher_work, &work.watcher_slots, WATCHER_SLOTS_PER_EPOCH);
    let (watcher_pays, watch_rem) = pro_rata_capped_u128(top.watchers, &w_weights, w_cap);
    let validator_pot = top
        .validators
        .saturating_add(amm_rem)
        .saturating_add(lend_rem)
        .saturating_add(watch_rem)
        .saturating_add(work.fee_reward_pool);
    let prop_pool = validator_pot * crate::params_v2::POT_PROPOSER_PCT / 100;
    let vote_pool = validator_pot * crate::params_v2::POT_VOTER_PCT / 100;
    let floor_pool = validator_pot - prop_pool - vote_pool;
    let (pack_pays, _) = pro_rata_capped_u128(prop_pool, &work.proposer_credit, u64::MAX);
    let (check_pays, _) = pro_rata_capped_u128(vote_pool, &work.vote_credit, u64::MAX);
    let (floor_pays, _) = pro_rata_capped_u128(floor_pool, floor_weights, u64::MAX);
    Settlement {
        epoch_emit,
        validator_pot,
        pack_pays,
        check_pays,
        watcher_pays,
        amm_pays,
        lend_pays,
        floor_pays,
        operator_bucket,
    }
}

/// 1-based epoch index from genesis time.
pub fn epoch_index(now_ms: u64, genesis_ms: u64, epoch_ms: u64) -> u64 {
    let epoch_ms = epoch_ms.max(1);
    let elapsed = now_ms.saturating_sub(genesis_ms);
    elapsed / epoch_ms + 1
}

/// Mainnet default: emit only from epoch 8 (epochs 1–7 = airdrop/bootstrap).
pub fn epoch_claimable(epoch_1based: u64) -> bool {
    epoch_claimable_from(epoch_1based, FIRST_CLAIM_EPOCH)
}

/// Testnet / private: set `first` to 1 so epoch 1 already emits.
pub fn epoch_claimable_from(epoch_1based: u64, first_claim_epoch: u64) -> bool {
    epoch_1based >= first_claim_epoch.max(1)
}

pub fn fee_burn_and_reward(fee: u64) -> (u64, u64) {
    let burn = fee.saturating_mul(FEE_BURN_BPS) / 10_000;
    (burn, fee.saturating_sub(burn))
}

#[cfg(test)]
mod tests {
    #[test]
    fn watcher_v2_bitset_is_visible_to_slot_queries() {
        let mut w = EpochWork::default();
        w.record_watcher_slot_v2("a", 81);
        w.record_watcher_slot_v2("a", 81);
        w.record_watcher_slot_v2("b", 81);
        w.record_watcher_slot_v2("b", 80);
        w.record_watcher_slot("legacy", 81);
        assert!(w.watcher_has_slot("a", 81));
        assert!(!w.watcher_has_slot("a", 80));
        assert!(w.watcher_has_slot("legacy", 81));
        assert_eq!(w.watcher_slots["a"], 1);
        assert_eq!(w.watchers_in_slot(81), 3);
        assert_eq!(w.watchers_in_slot(80), 1);
        assert_eq!(w.watchers_in_slot(500), 0);
    }

    use super::*;

    #[test]
    fn first_claimable_emit_30bps() {
        let e = epoch_emit(TREASURY_GENESIS, EMISSION_BPS_FIRST);
        assert_eq!(e, 588_000_000);
    }

    #[test]
    fn top_split_sums() {
        let e = 588_000_000u64;
        let t = top_buckets(e);
        assert_eq!(
            t.validators + t.watchers + t.amm + t.lend,
            e
        );
    }

    #[test]
    fn watcher_presence_dust() {
        let mut work = BTreeMap::new();
        work.insert("alice".into(), 100u64);
        let mut slots = BTreeMap::new();
        slots.insert("alice".into(), 1u64); // one hour of 168
        let w = watcher_weights(&work, &slots, WATCHER_SLOTS_PER_EPOCH);
        // 100 * 1 / 168 = 0 integer → 0 weight (dust)
        assert_eq!(*w.get("alice").unwrap_or(&0), 0);
        slots.insert("alice".into(), 168);
        let w = watcher_weights(&work, &slots, WATCHER_SLOTS_PER_EPOCH);
        assert_eq!(w["alice"], 100);
    }

    #[test]
    fn settle_no_lp_remainder_to_val() {
        let work = EpochWork::default();
        let empty = BTreeMap::new();
        let s = settle_epoch(
            TREASURY_GENESIS,
            EMISSION_BPS_FIRST,
            true,
            &work,
            &empty,
            &empty,
            0,
        );
        // all LP+watcher pots unused → validator pot = full emit
        assert_eq!(s.epoch_emit, 588_000_000);
        assert_eq!(s.validator_pot, 588_000_000);
    }

    #[test]
    fn settle_pack_perf_split() {
        let mut work = EpochWork::default();
        work.record_pack("v1", 100);
        work.record_pack("v2", 50);
        work.record_check("v1", "h1");
        work.record_check("v2", "h1");
        work.record_check("v2", "h2");
        let empty = BTreeMap::new();
        let s = settle_epoch(
            TREASURY_GENESIS,
            EMISSION_BPS_FIRST,
            true,
            &work,
            &empty,
            &empty,
            0,
        );
        // pack half 294M: v1 2/3, v2 1/3
        let pack_half = s.validator_pot / 2;
        assert_eq!(s.pack_pays["v1"], pack_half * 100 / 150);
        assert_eq!(s.pack_pays["v2"], pack_half * 50 / 150);
        // check: v1 1 vote, v2 2 votes
        let check_half = s.validator_pot - pack_half;
        assert_eq!(s.check_pays["v1"], check_half * 1 / 3);
        assert_eq!(s.check_pays["v2"], check_half * 2 / 3);
    }

    #[test]
    fn bootstrap_not_claimable() {
        assert!(!epoch_claimable(1));
        assert!(!epoch_claimable(7));
        assert!(epoch_claimable(8));
        assert!(epoch_claimable_from(1, 1), "testnet pays from epoch 1");
        assert!(!epoch_claimable_from(1, 8));
    }

    #[test]
    fn unjail_cost_doubles_per_jail() {
        assert_eq!(unjail_cost(MIN_BOND, 0), MIN_BOND);
        assert_eq!(unjail_cost(MIN_BOND, 1), 2 * MIN_BOND);
        assert_eq!(unjail_cost(MIN_BOND, 2), 4 * MIN_BOND);
        assert_eq!(unjail_cost(MIN_BOND, 3), 8 * MIN_BOND);
    }

    fn op_inputs() -> (EpochWork, BTreeMap<String, u64>, BTreeMap<String, u64>, BTreeMap<String, u64>) {
        let mut w = EpochWork::default();
        for s in ["v1", "v3", "v6"] {
            w.proposer_credit.insert(s.into(), 1_000);
            w.vote_credit.insert(s.into(), 500);
        }
        let amm: BTreeMap<String, u64> = (0..400).map(|i| (format!("a{i}"), 1)).collect();
        let lend: BTreeMap<String, u64> = (0..400).map(|i| (format!("l{i}"), 1)).collect();
        let floor: BTreeMap<String, u64> = ["v1", "v3", "v6"].iter().map(|s| (s.to_string(), 100)).collect();
        (w, amm, lend, floor)
    }

    #[test]
    fn operator_bucket_pre_activation_byte_identical() {
        use crate::operator_registry::{operator_bucket, OperatorParams};
        let (w, amm, lend, floor) = op_inputs();
        let p = OperatorParams::default();
        assert_eq!(p, OperatorParams::PROPOSAL_B);
        assert_eq!(p.emission_share_bps, 500);
        let t = 190_771_061_468u64;
        let a = settle_epoch_v2(t, 30, true, &w, &amm, &lend, &floor);
        let b = settle_epoch_v2_ops(t, 30, true, &w, &amm, &lend, &floor, |e| {
            operator_bucket(crate::NETWORK_ID_PUBLIC_TESTNET_2300, crate::params_v2::TESTNET_2300_BRIDGE_FROM_HEIGHT - 1, e, &p)
        });
        assert_eq!(b.operator_bucket, 0);
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }

    #[test]
    fn v1_settle_carves_operator_bucket_and_caps_use_post_carve_emit() {
        let (w, amm, lend, _floor) = op_inputs();
        let t = 190_771_061_468u64;
        let a = settle_epoch(t, 30, true, &w, &amm, &lend, 0);
        let b = settle_epoch_ops(t, 30, true, &w, &amm, &lend, 0, |_| 0);
        assert_eq!(format!("{a:?}"), format!("{b:?}"), "pre-activation byte-identical");
        let c = settle_epoch_ops(t, 30, true, &w, &amm, &lend, 0, |e| e * 500 / 10_000);
        let e = c.epoch_emit;
        assert_eq!(c.operator_bucket, e * 500 / 10_000);
        let post = e - c.operator_bucket;
        let paid: u64 = c.pack_pays.values().chain(c.check_pays.values()).chain(c.watcher_pays.values()).chain(c.amm_pays.values()).chain(c.lend_pays.values()).sum();
        assert!(paid + c.operator_bucket <= e, "never over-emit");
        // one whale LP: capped at cap_lp(post), not cap_lp(e)
        let whale: BTreeMap<String, u64> = [("whale".to_string(), 1u64)].into_iter().collect();
        let d = settle_epoch_ops(t, 30, true, &w, &whale, &whale, 0, |e| e * 500 / 10_000);
        assert_eq!(d.amm_pays["whale"], cap_lp(post).min(top_buckets(post).amm));
        let d2 = settle_epoch_v2_ops(t, 30, true, &w, &whale, &whale, &BTreeMap::new(), |e| e * 500 / 10_000);
        assert_eq!(d2.amm_pays["whale"], cap_lp(post).min(top_buckets(post).amm));
        assert!(cap_lp(post) < cap_lp(e) || cap_lp(e) == 0);
    }

    #[test]
    fn operator_bucket_post_activation_split_5_5225_475_19_19() {
        let emit = 1_000_000_000u64;
        let op = emit * 500 / 10_000;
        let top = top_buckets(emit - op);
        assert_eq!(op, 50_000_000);
        assert_eq!(top.validators, 522_500_000);
        assert_eq!(top.watchers, 47_500_000);
        assert_eq!(top.amm, 190_000_000);
        assert_eq!(top.lend, 190_000_000);
        assert_eq!(op + top.validators + top.watchers + top.amm + top.lend, emit);

        let (w, amm, lend, floor) = op_inputs();
        let t = 190_771_061_468u64;
        let s = settle_epoch_v2_ops(t, 30, true, &w, &amm, &lend, &floor, |e| e * 500 / 10_000);
        let e = s.epoch_emit;
        assert_eq!(s.operator_bucket, e * 500 / 10_000);
        let amm_paid: u64 = s.amm_pays.values().sum();
        let lend_paid: u64 = s.lend_pays.values().sum();
        let close = |got: u64, bps: u64| (got as i128 - (e as u128 * bps as u128 / 10_000) as i128).abs() <= 1_000;
        assert!(close(amm_paid, 1_900), "amm 19%");
        assert!(close(lend_paid, 1_900), "lend 19%");
        assert!(close(s.validator_pot, 5_225 + 475), "validators 52.25% + unpaid watchers 4.75%");
        let paid: u64 = s.pack_pays.values().chain(s.check_pays.values()).chain(s.floor_pays.values()).chain(s.watcher_pays.values()).sum::<u64>() + amm_paid + lend_paid;
        assert!(paid + s.operator_bucket <= e, "conserve: never over-emit");
        assert!(e - paid - s.operator_bucket < 1_000, "dust only; operator bucket refunds to treasury if unpaid");
    }
}
