//! Fee escalation + node intake metrics (v2.2 private fleet).
//!
//! Congestion drives a **suggested / enforced min fee** (not dynamic burn — that is
//! Class E / post-v2.3). Aligns with PoC lesson: under full mempool we need backpressure
//! **and** a visible fee market signal (Falcon Ledger used 503 + fee escalate).
//!
//! Lock: AMD-20260811-1 · see `docs/AMENDMENTS.md` and EXPANSION_PLAN §4.5b.

use serde::{Deserialize, Serialize};

/// Bumped when Class B economic/consensus parameters change in code.
pub const PARAMS_VERSION: u32 = 2;

/// Default base minimum fee (FPL units) when mempool is idle.
pub const DEFAULT_MIN_FEE: u64 = 1;

/// Rolling window for fee sample stats (accepted sealed fees).
pub const FEE_SAMPLE_CAP: usize = 256;

/// Mempool utilization → fee multiplier (locked schedule).
///
/// | util bps | tier       | mult |
/// |----------|------------|------|
/// | 0–2499   | idle       | 1×   |
/// | 2500–4999| light      | 2×   |
/// | 5000–7499| busy       | 4×   |
/// | 7500–8999| hot        | 8×   |
/// | ≥9000    | critical   | 16×  |
pub fn fee_multiplier(util_bps: u64) -> u64 {
    match util_bps {
        0..=2499 => 1,
        2500..=4999 => 2,
        5000..=7499 => 4,
        7500..=8999 => 8,
        _ => 16,
    }
}

pub fn congestion_tier(util_bps: u64) -> &'static str {
    match util_bps {
        0..=2499 => "idle",
        2500..=4999 => "light",
        5000..=7499 => "busy",
        7500..=8999 => "hot",
        _ => "critical",
    }
}

/// Mempool fill in basis points (0–10_000). Unlimited mempool → 0 util for fee math.
pub fn mempool_util_bps(len: usize, max: usize) -> u64 {
    if max == 0 {
        return 0;
    }
    let bps = (len as u128).saturating_mul(10_000) / max as u128;
    bps.min(10_000) as u64
}

pub fn suggested_min_fee(base_min_fee: u64, util_bps: u64) -> u64 {
    let base = base_min_fee.max(1);
    base.saturating_mul(fee_multiplier(util_bps))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeMetrics {
    /// Newly accepted submits (not dups).
    pub submit_accepted: u64,
    pub submit_duplicate: u64,
    pub submit_reject_mempool_full: u64,
    pub submit_reject_fee_low: u64,
    pub submit_reject_crypto: u64,
    pub submit_reject_network: u64,
    pub submit_reject_other: u64,
    /// Sealed-ledger fee totals (lifetime this process / snapshot).
    pub fees_sealed_total: u64,
    pub fees_burned_total: u64,
    pub fees_reward_pool_total: u64,
    pub ledgers_sealed: u64,
    pub txs_sealed: u64,
    /// Last computed suggested min fee (updated on status / submit).
    pub last_suggested_min_fee: u64,
    pub last_util_bps: u64,
    /// Recent sealed per-tx fees for rough percentile (ring).
    #[serde(default)]
    pub fee_samples: Vec<u64>,
}

impl NodeMetrics {
    pub fn record_submit_ok(&mut self, is_new: bool) {
        if is_new {
            self.submit_accepted = self.submit_accepted.saturating_add(1);
        } else {
            self.submit_duplicate = self.submit_duplicate.saturating_add(1);
        }
    }

    pub fn record_submit_err(&mut self, err: &str) {
        let e = err.to_ascii_lowercase();
        if e.contains("mempool full") {
            self.submit_reject_mempool_full = self.submit_reject_mempool_full.saturating_add(1);
        } else if e.contains("fee too low") || e.contains("fee_low") {
            self.submit_reject_fee_low = self.submit_reject_fee_low.saturating_add(1);
        } else if e.contains("bad tx crypto") || e.contains("bad crypto") {
            self.submit_reject_crypto = self.submit_reject_crypto.saturating_add(1);
        } else if e.contains("wrong network") {
            self.submit_reject_network = self.submit_reject_network.saturating_add(1);
        } else {
            self.submit_reject_other = self.submit_reject_other.saturating_add(1);
        }
    }

    pub fn record_sealed_fees(&mut self, fee_total: u64, burned: u64, reward: u64, n_txs: u64) {
        self.fees_sealed_total = self.fees_sealed_total.saturating_add(fee_total);
        self.fees_burned_total = self.fees_burned_total.saturating_add(burned);
        self.fees_reward_pool_total = self.fees_reward_pool_total.saturating_add(reward);
        self.ledgers_sealed = self.ledgers_sealed.saturating_add(1);
        self.txs_sealed = self.txs_sealed.saturating_add(n_txs);
    }

    pub fn push_fee_sample(&mut self, fee: u64) {
        if self.fee_samples.len() >= FEE_SAMPLE_CAP {
            self.fee_samples.remove(0);
        }
        self.fee_samples.push(fee);
    }

    /// Rough percentiles over recent sealed per-tx fees (0 if empty).
    pub fn fee_percentile(&self, pct: u8) -> u64 {
        if self.fee_samples.is_empty() {
            return 0;
        }
        let mut v = self.fee_samples.clone();
        v.sort_unstable();
        let pct = pct.min(100) as usize;
        let idx = (v.len().saturating_sub(1)) * pct / 100;
        v[idx]
    }

    pub fn reject_rate_bps(&self) -> u64 {
        let ok = self.submit_accepted.saturating_add(self.submit_duplicate);
        let bad = self
            .submit_reject_mempool_full
            .saturating_add(self.submit_reject_fee_low)
            .saturating_add(self.submit_reject_crypto)
            .saturating_add(self.submit_reject_network)
            .saturating_add(self.submit_reject_other);
        let total = ok.saturating_add(bad);
        if total == 0 {
            return 0;
        }
        ((bad as u128).saturating_mul(10_000) / total as u128) as u64
    }

    pub fn to_json(&self, mempool_len: usize, max_mempool: usize, base_min: u64, escalate: bool) -> serde_json::Value {
        let util = mempool_util_bps(mempool_len, max_mempool);
        let suggested = if escalate {
            suggested_min_fee(base_min, util)
        } else {
            base_min.max(1)
        };
        serde_json::json!({
            "params_version": PARAMS_VERSION,
            "submit_accepted": self.submit_accepted,
            "submit_duplicate": self.submit_duplicate,
            "submit_reject_mempool_full": self.submit_reject_mempool_full,
            "submit_reject_fee_low": self.submit_reject_fee_low,
            "submit_reject_crypto": self.submit_reject_crypto,
            "submit_reject_network": self.submit_reject_network,
            "submit_reject_other": self.submit_reject_other,
            "reject_rate_bps": self.reject_rate_bps(),
            "fees_sealed_total": self.fees_sealed_total,
            "fees_burned_total": self.fees_burned_total,
            "fees_reward_pool_total": self.fees_reward_pool_total,
            "ledgers_sealed": self.ledgers_sealed,
            "txs_sealed": self.txs_sealed,
            "fee_p50": self.fee_percentile(50),
            "fee_p90": self.fee_percentile(90),
            "fee_market": {
                "base_min_fee": base_min,
                "escalate": escalate,
                "mempool_util_bps": util,
                "congestion_tier": congestion_tier(util),
                "fee_multiplier": fee_multiplier(util),
                "suggested_min_fee": suggested,
                "schedule": "1x@0-25% 2x@25-50% 4x@50-75% 8x@75-90% 16x@90%+",
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn util_and_tiers() {
        assert_eq!(mempool_util_bps(0, 100), 0);
        assert_eq!(mempool_util_bps(25, 100), 2500);
        assert_eq!(mempool_util_bps(50, 100), 5000);
        assert_eq!(mempool_util_bps(90, 100), 9000);
        assert_eq!(fee_multiplier(0), 1);
        assert_eq!(fee_multiplier(2500), 2);
        assert_eq!(fee_multiplier(5000), 4);
        assert_eq!(fee_multiplier(7500), 8);
        assert_eq!(fee_multiplier(9000), 16);
        assert_eq!(suggested_min_fee(1, 9000), 16);
        assert_eq!(suggested_min_fee(2, 5000), 8);
    }

    #[test]
    fn reject_classification() {
        let mut m = NodeMetrics::default();
        m.record_submit_err("mempool full (1/1)");
        m.record_submit_err("fee too low want 8 got 1");
        m.record_submit_err("bad tx crypto");
        assert_eq!(m.submit_reject_mempool_full, 1);
        assert_eq!(m.submit_reject_fee_low, 1);
        assert_eq!(m.submit_reject_crypto, 1);
    }
}
