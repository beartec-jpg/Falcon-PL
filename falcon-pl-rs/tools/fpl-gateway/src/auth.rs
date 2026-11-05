//! API keys. Only `sha256(key)` is stored (keys.json); the plaintext key is
//! shown once by `fpl-gatewayd new-key` and never logged.

use crate::config::Tier;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const KEY_PREFIX: &str = "fplk_";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRecord {
    /// Public, non-secret id used in logs/metrics/usage (e.g. "k_3f9a1c").
    pub id: String,
    /// hex(sha256(api_key)).
    pub sha256: String,
    pub tier: Tier,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub disabled: bool,
    /// Set by the billing stub once Stripe is live.
    #[serde(default)]
    pub stripe_customer: Option<String>,
}

#[derive(Default)]
pub struct KeyStore {
    by_hash: HashMap<String, KeyRecord>,
}

pub fn load_records(path: &std::path::Path) -> anyhow::Result<Vec<KeyRecord>> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

pub fn hash_key(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// Generate a fresh key: `fplk_` + 32 random bytes hex. Returns (plaintext, record).
pub fn generate(id: &str, tier: Tier, label: &str) -> (String, KeyRecord) {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    let key = format!("{KEY_PREFIX}{}", hex::encode(b));
    let rec = KeyRecord { id: id.into(), sha256: hash_key(&key), tier, label: label.into(), disabled: false, stripe_customer: None };
    (key, rec)
}

/// Constant-time compare of two equal-length hex strings.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl KeyStore {
    pub fn from_records(recs: Vec<KeyRecord>) -> anyhow::Result<Self> {
        let mut by_hash = HashMap::new();
        for r in recs {
            anyhow::ensure!(r.sha256.len() == 64 && hex::decode(&r.sha256).is_ok(), "key {} has a bad sha256", r.id);
            anyhow::ensure!(by_hash.insert(r.sha256.to_lowercase(), r).is_none(), "duplicate key hash");
        }
        Ok(Self { by_hash })
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        Self::from_records(load_records(path)?)
    }

    /// Merge local/ops keys (keys.json) with portal-sourced keys. Local always
    /// wins: a portal record whose `id` or `sha256` matches a local record is
    /// dropped. Portal records are validated individually; bad ones are skipped
    /// (logged) so one bad row can't take down the whole portal set.
    pub fn merged(local: Vec<KeyRecord>, portal: &[KeyRecord]) -> anyhow::Result<Self> {
        let mut s = Self::from_records(local)?;
        let local_ids: std::collections::HashSet<String> = s.by_hash.values().map(|r| r.id.clone()).collect();
        let mut seen_ids = std::collections::HashSet::new();
        for r in portal {
            let h = r.sha256.to_lowercase();
            if h.len() != 64 || hex::decode(&h).is_err() {
                tracing::warn!(key = %r.id, "portal key has a bad sha256; skipped");
                continue;
            }
            if local_ids.contains(&r.id) || s.by_hash.contains_key(&h) || !seen_ids.insert(r.id.clone()) {
                tracing::debug!(key = %r.id, "portal key shadowed by local/duplicate; skipped");
                continue;
            }
            s.by_hash.insert(h.clone(), KeyRecord { sha256: h, ..r.clone() });
        }
        Ok(s)
    }

    pub fn len(&self) -> usize {
        self.by_hash.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }

    /// Resolve a presented key. `None` for unknown, malformed or disabled keys.
    pub fn authenticate(&self, presented: &str) -> Option<&KeyRecord> {
        if !presented.starts_with(KEY_PREFIX) || presented.len() > 128 {
            return None;
        }
        let h = hash_key(presented);
        let rec = self.by_hash.get(&h)?;
        (ct_eq(rec.sha256.as_bytes(), h.as_bytes()) && !rec.disabled).then_some(rec)
    }
}

/// Pull the key from `Authorization: Bearer <k>` or `X-API-Key: <k>`.
pub fn extract(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(k) = v.strip_prefix("Bearer ") {
            return Some(k.trim().to_string());
        }
    }
    headers.get("x-api-key").and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string())
}
