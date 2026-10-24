//! Named accounts: the name *is* the account.
//!
//! Reserve (short hold) → pay activation fee → live.
//! Unpaid reservation expires and the name returns to the pool.
//! Short / obvious names cost more than long dotted names.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const NAME_MIN_LEN: usize = 3;
pub const NAME_MAX_LEN: usize = 32;
/// 30 minutes to fund and activate after reserve.
pub const NAME_RESERVE_MS: u64 = 30 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NameRecord {
    pub name: String,
    pub public_key: String,
    pub reserved_until_ms: u64,
    pub activated: bool,
    pub fee_required: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NameRegistry {
    pub by_name: BTreeMap<String, NameRecord>,
    /// At most one live (not-yet-activated) reservation per public key.
    #[serde(default)]
    pub reserved_by_key: BTreeMap<String, String>,
}

impl NameRegistry {
    pub fn validate_name(raw: &str) -> Result<String, String> {
        let s = raw.trim().to_ascii_lowercase();
        if s.len() < NAME_MIN_LEN || s.len() > NAME_MAX_LEN {
            return Err(format!(
                "name: length must be {NAME_MIN_LEN}–{NAME_MAX_LEN}"
            ));
        }
        if s.starts_with('.') || s.ends_with('.') || s.contains("..") {
            return Err("name: no leading, trailing, or double dots".into());
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.')
        {
            return Err("name: use a–z, 0–9, and optional dots".into());
        }
        if is_reserved_name(&s) {
            return Err(format!("name: '{s}' is reserved"));
        }
        Ok(s)
    }

    /// Alphanumeric length (dots ignored) drives the price.
    pub fn activation_fee(name: &str) -> u64 {
        let n = name.bytes().filter(|b| *b != b'.').count();
        match n {
            0..=3 => 50_000,
            4 => 20_000,
            5 => 5_000,
            6 => 2_000,
            7 | 8 => 500,
            9..=12 => 200,
            _ => 100,
        }
    }

    pub fn expire_due(&mut self, now_ms: u64) {
        let expired: Vec<String> = self
            .by_name
            .iter()
            .filter(|(_, r)| !r.activated && r.reserved_until_ms <= now_ms)
            .map(|(n, _)| n.clone())
            .collect();
        for n in expired {
            if let Some(rec) = self.by_name.remove(&n) {
                if self.reserved_by_key.get(&rec.public_key) == Some(&n) {
                    self.reserved_by_key.remove(&rec.public_key);
                }
            }
        }
    }

    pub fn status(&mut self, name: &str, now_ms: u64) -> NameStatus {
        self.expire_due(now_ms);
        match self.by_name.get(name) {
            Some(r) if r.activated => NameStatus::Activated {
                public_key: r.public_key.clone(),
                fee_required: r.fee_required,
            },
            Some(r) => NameStatus::Reserved {
                public_key: r.public_key.clone(),
                until_ms: r.reserved_until_ms,
                fee_required: r.fee_required,
            },
            None => NameStatus::Free {
                fee_required: Self::activation_fee(name),
            },
        }
    }

    pub fn reserve(
        &mut self,
        name: &str,
        public_key: &str,
        now_ms: u64,
    ) -> Result<NameRecord, String> {
        let name = Self::validate_name(name)?;
        if public_key.trim().is_empty() {
            return Err("name: public key required".into());
        }
        self.expire_due(now_ms);
        match self.by_name.get(&name) {
            Some(r) if r.activated => return Err("name: already activated".into()),
            Some(r) if r.public_key != public_key => {
                return Err("name: reserved by another key".into())
            }
            Some(r) if r.public_key == public_key => {
                return Ok(r.clone());
            }
            _ => {}
        }
        if let Some(held) = self.reserved_by_key.get(public_key) {
            if held != &name {
                return Err(format!("name: this key already reserved '{held}'"));
            }
        }
        let rec = NameRecord {
            fee_required: Self::activation_fee(&name),
            name: name.clone(),
            public_key: public_key.to_string(),
            reserved_until_ms: now_ms.saturating_add(NAME_RESERVE_MS),
            activated: false,
        };
        self.by_name.insert(name.clone(), rec.clone());
        self.reserved_by_key
            .insert(public_key.to_string(), name);
        Ok(rec)
    }

    pub fn activate(
        &mut self,
        name: &str,
        public_key: &str,
        now_ms: u64,
    ) -> Result<u64, String> {
        let name = Self::validate_name(name)?;
        self.expire_due(now_ms);
        match self.by_name.get(&name) {
            Some(r) if r.activated => {
                if r.public_key != public_key {
                    return Err("name: already activated".into());
                }
                return Ok(0);
            }
            Some(r) if r.public_key != public_key => {
                return Err("name: reserved by another key".into());
            }
            Some(_) => {}
            None => {
                // Allow activate without a prior reserve if the name is free.
            }
        }
        let fee = Self::activation_fee(&name);
        let rec = NameRecord {
            name: name.clone(),
            public_key: public_key.to_string(),
            reserved_until_ms: 0,
            activated: true,
            fee_required: fee,
        };
        self.by_name.insert(name, rec);
        self.reserved_by_key.remove(public_key);
        Ok(fee)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameStatus {
    Free { fee_required: u64 },
    Reserved {
        public_key: String,
        until_ms: u64,
        fee_required: u64,
    },
    Activated {
        public_key: String,
        fee_required: u64,
    },
}

pub fn is_reserved_name(name: &str) -> bool {
    matches!(
        name,
        "faucet"
            | "treasury"
            | "admin"
            | "falcon"
            | "fpl"
            | "pool"
            | "lend"
            | "bridge"
            | "community"
            | "builder"
            | "watcher"
            | "watcher-browser"
            | "genesis"
            | "root"
            | "operator"
            | "alice"
            | "bob"
            | "carol"
            | "dave"
            | "v1"
            | "v2"
            | "v3"
            | "v4"
            | "v5"
            | "v6"
            | "v7"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_curve_short_costs_more() {
        assert_eq!(NameRegistry::activation_fee("delta"), 5_000);
        assert_eq!(NameRegistry::activation_fee("north.wind.station"), 100);
        assert_eq!(NameRegistry::activation_fee("ab"), 50_000); // 2 alnum — validate rejects, fee still defined
        assert_eq!(NameRegistry::activation_fee("abcd"), 20_000);
    }

    #[test]
    fn reserve_then_timeout_returns_to_pool() {
        let mut r = NameRegistry::default();
        r.reserve("delta", "pk1", 1_000).unwrap();
        assert!(matches!(r.status("delta", 2_000), NameStatus::Reserved { .. }));
        assert!(r.reserve("delta", "pk2", 2_000).is_err());
        // after 30m
        assert!(matches!(
            r.status("delta", 1_000 + NAME_RESERVE_MS + 1),
            NameStatus::Free { .. }
        ));
        r.reserve("delta", "pk2", 1_000 + NAME_RESERVE_MS + 1)
            .unwrap();
    }

    #[test]
    fn one_live_reservation_per_key() {
        let mut r = NameRegistry::default();
        r.reserve("delta.one", "pk1", 1).unwrap();
        assert!(r.reserve("delta.two", "pk1", 2).is_err());
    }

    #[test]
    fn activate_consumes_fee_and_locks_name() {
        let mut r = NameRegistry::default();
        r.reserve("delta", "pk1", 1).unwrap();
        let fee = r.activate("delta", "pk1", 2).unwrap();
        assert_eq!(fee, 5_000);
        assert!(r.activate("delta", "pk2", 3).is_err());
        assert!(r.reserve("delta", "pk2", 3).is_err());
    }

    #[test]
    fn reserved_words_blocked() {
        assert!(NameRegistry::validate_name("faucet").is_err());
        assert!(NameRegistry::validate_name("v1").is_err());
        assert!(NameRegistry::validate_name("Delta").unwrap() == "delta");
        assert!(NameRegistry::validate_name("ab").is_err());
        assert!(NameRegistry::validate_name(".delta").is_err());
    }
}
