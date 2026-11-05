//! Self-serve keys from the Falcon Cloud portal (Falcon-faucet-wallet).
//!
//! The portal's Postgres (`falcon_cloud_keys`) is the SSOT for Stripe-bought
//! keys. The gateway polls `GET {portal.url}` with `Authorization: Bearer
//! $FPL_GATEWAY_PORTAL_TOKEN` and gets hashes only:
//! `{"keys":[{"key_id","sha256","tier","stripe_customer","disabled"}]}`.
//! The result is merged with keys.json (local wins). On fetch/parse failure
//! the last good portal snapshot is kept (fail-static), so a portal outage
//! never logs out paying users or ops keys.

use crate::auth::{KeyRecord, KeyStore};
use crate::config::{PortalConfig, Tier};
use parking_lot::Mutex;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct PortalKey {
    pub key_id: String,
    pub sha256: String,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub stripe_customer: Option<String>,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct PortalExport {
    pub keys: Vec<PortalKey>,
}

/// Convert portal rows to gateway records. Rows with unknown/missing tier are skipped.
pub fn to_records(exp: PortalExport) -> Vec<KeyRecord> {
    exp.keys
        .into_iter()
        .filter_map(|k| {
            let tier: Tier = match k.tier.as_deref().map(|t| serde_json::from_value(serde_json::Value::String(t.to_lowercase()))) {
                Some(Ok(t)) => t,
                _ => {
                    tracing::warn!(key = %k.key_id, "portal key has unknown tier; skipped");
                    return None;
                }
            };
            Some(KeyRecord { id: k.key_id, sha256: k.sha256, tier, label: "portal".into(), disabled: k.disabled, stripe_customer: k.stripe_customer })
        })
        .collect()
}

pub fn parse(body: &str) -> anyhow::Result<Vec<KeyRecord>> {
    Ok(to_records(serde_json::from_str(body)?))
}

/// Both key sources; rebuilds the live KeyStore whenever either changes.
pub struct Sources {
    pub keys_path: PathBuf,
    portal: Mutex<Vec<KeyRecord>>,
}

impl Sources {
    pub fn new(keys_path: PathBuf) -> Self {
        Self { keys_path, portal: Mutex::new(Vec::new()) }
    }

    /// Re-read keys.json and merge with the last portal snapshot.
    pub fn build(&self) -> anyhow::Result<KeyStore> {
        KeyStore::merged(crate::auth::load_records(&self.keys_path)?, &self.portal.lock())
    }

    /// Replace the portal snapshot and rebuild.
    pub fn set_portal(&self, recs: Vec<KeyRecord>) -> anyhow::Result<KeyStore> {
        let local = crate::auth::load_records(&self.keys_path)?;
        let s = KeyStore::merged(local, &recs)?;
        *self.portal.lock() = recs;
        Ok(s)
    }

    pub fn portal_len(&self) -> usize {
        self.portal.lock().len()
    }
}

pub async fn fetch(client: &reqwest::Client, cfg: &PortalConfig, token: &str) -> anyhow::Result<Vec<KeyRecord>> {
    let r = client.get(&cfg.url).bearer_auth(token).send().await?.error_for_status()?;
    parse(&r.text().await?)
}
