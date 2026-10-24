//! Crypto backends for batch attestations.
//!
//! * `hmac-sha256-dev` — fast unit tests / differential vs Python HMAC path
//! * `falcon-512` — NIST PQ Falcon-512 via PQClean (`pqcrypto-falcon`)

use fd_types::{batch_sign_message, NodeId, Transaction};
use hmac::{Hmac, Mac};
use pqcrypto_falcon::falcon512;
use pqcrypto_traits::sign::{DetachedSignature as _, PublicKey as _, SecretKey as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

pub const FALCON512_PK_LEN: usize = 897;
pub const FALCON512_SK_LEN: usize = 1281;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("unknown validator {0}")]
    UnknownValidator(String),
    #[error("sign failed: {0}")]
    SignFailed(String),
    #[error("no secret key for {0}")]
    NoSecret(String),
    #[error("io: {0}")]
    Io(String),
    #[error("bad key material: {0}")]
    BadKey(String),
}

pub trait CryptoBackend: Send + Sync {
    fn scheme(&self) -> &str;
    fn sign(&self, validator_id: &str, message: &str) -> Result<String, CryptoError>;
    fn verify(&self, validator_id: &str, message: &str, signature: &str) -> bool;
    fn public_ids(&self) -> Vec<NodeId>;
    fn export_public(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
    fn sample_sig_bytes(&self) -> Option<usize> {
        None
    }

    fn sign_batch(&self, validator_id: &str, batch_hash: &str) -> Result<String, CryptoError> {
        self.sign(validator_id, &batch_sign_message(batch_hash))
    }

    fn verify_batch(&self, validator_id: &str, batch_hash: &str, sig: &str) -> bool {
        self.verify(validator_id, &batch_sign_message(batch_hash), sig)
    }
}

pub type SharedCrypto = Arc<dyn CryptoBackend>;

// ---------------------------------------------------------------------------
// Phase A: verified-signature cache (lock-free for hot path)
// ---------------------------------------------------------------------------

/// Cache key: (validator_id, batch_hash) → signature hex that already verified.
/// Prevents re-verifying the same Falcon-512 sig on every gossip hop / support_weight.
#[derive(Default)]
pub struct VerifiedSigCache {
    /// (validator_id || 0xff || batch_hash) → sig hex
    map: dashmap::DashMap<String, String>,
    pub hits: std::sync::atomic::AtomicU64,
    pub misses: std::sync::atomic::AtomicU64,
}

impl VerifiedSigCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(validator_id: &str, batch_hash: &str) -> String {
        format!("{validator_id}\0{batch_hash}")
    }

    pub fn get(&self, validator_id: &str, batch_hash: &str, signature: &str) -> bool {
        if let Some(v) = self.map.get(&Self::key(validator_id, batch_hash)) {
            if v.as_str() == signature {
                self.hits
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return true;
            }
        }
        false
    }

    pub fn insert(&self, validator_id: &str, batch_hash: &str, signature: &str) {
        self.map
            .insert(Self::key(validator_id, batch_hash), signature.to_string());
    }

    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(std::sync::atomic::Ordering::Relaxed),
            self.misses.load(std::sync::atomic::Ordering::Relaxed),
        )
    }
}

/// Wrapper that caches successful batch signature verifications.
pub struct CachingCrypto {
    inner: SharedCrypto,
    pub cache: Arc<VerifiedSigCache>,
}

impl CachingCrypto {
    pub fn wrap(inner: SharedCrypto) -> SharedCrypto {
        Arc::new(Self {
            inner,
            cache: Arc::new(VerifiedSigCache::new()),
        })
    }

    pub fn wrap_with_cache(inner: SharedCrypto, cache: Arc<VerifiedSigCache>) -> SharedCrypto {
        Arc::new(Self { inner, cache })
    }
}

impl CryptoBackend for CachingCrypto {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }

    fn sign(&self, validator_id: &str, message: &str) -> Result<String, CryptoError> {
        self.inner.sign(validator_id, message)
    }

    fn verify(&self, validator_id: &str, message: &str, signature: &str) -> bool {
        // Generic messages not cached by batch key; still verify.
        self.inner.verify(validator_id, message, signature)
    }

    fn public_ids(&self) -> Vec<NodeId> {
        self.inner.public_ids()
    }

    fn export_public(&self) -> BTreeMap<String, String> {
        self.inner.export_public()
    }

    fn sample_sig_bytes(&self) -> Option<usize> {
        self.inner.sample_sig_bytes()
    }

    fn sign_batch(&self, validator_id: &str, batch_hash: &str) -> Result<String, CryptoError> {
        let sig = self.inner.sign_batch(validator_id, batch_hash)?;
        // Local sign is valid by construction — populate cache for free later.
        self.cache.insert(validator_id, batch_hash, &sig);
        Ok(sig)
    }

    fn verify_batch(&self, validator_id: &str, batch_hash: &str, sig: &str) -> bool {
        if self.cache.get(validator_id, batch_hash, sig) {
            return true;
        }
        self.cache
            .misses
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let ok = self.inner.verify_batch(validator_id, batch_hash, sig);
        if ok {
            self.cache.insert(validator_id, batch_hash, sig);
        }
        ok
    }
}

fn wrap_cached(c: SharedCrypto) -> SharedCrypto {
    CachingCrypto::wrap(c)
}

pub fn make_crypto(scheme: &str, unl: &[String], keys_dir: Option<&Path>) -> Result<SharedCrypto, CryptoError> {
    let raw = match scheme {
        "hmac-sha256-dev" | "hmac" | "dev" => Ok(Arc::new(HmacKeyRing::for_unl(unl.iter().cloned())) as SharedCrypto),
        "falcon-512" | "falcon" | "falcon512" => {
            if let Some(dir) = keys_dir {
                Ok(Arc::new(Falcon512KeyRing::load_for_validator(
                    dir,
                    // local id: first with secret on disk, or first unl if only pubs
                    unl.first().map(|s| s.as_str()).unwrap_or("val1"),
                )?) as SharedCrypto)
            } else {
                Ok(Arc::new(Falcon512KeyRing::generate_unl(unl.iter().cloned())?) as SharedCrypto)
            }
        }
        other => Err(CryptoError::BadKey(format!("unknown crypto scheme: {other}"))),
    }?;
    Ok(wrap_cached(raw))
}

/// Load Falcon keys for a specific local validator (has its secret).
pub fn make_crypto_for_validator(
    scheme: &str,
    unl: &[String],
    local_id: &str,
    keys_dir: Option<&Path>,
) -> Result<SharedCrypto, CryptoError> {
    let raw = match scheme {
        "hmac-sha256-dev" | "hmac" | "dev" => Ok(Arc::new(HmacKeyRing::for_unl(unl.iter().cloned())) as SharedCrypto),
        "falcon-512" | "falcon" | "falcon512" => {
            if let Some(dir) = keys_dir {
                Ok(Arc::new(Falcon512KeyRing::load_for_validator(dir, local_id)?) as SharedCrypto)
            } else {
                // Ephemeral full UNL (all secrets) — tests / single-process only
                Ok(Arc::new(Falcon512KeyRing::generate_unl(unl.iter().cloned())?) as SharedCrypto)
            }
        }
        other => Err(CryptoError::BadKey(format!("unknown crypto scheme: {other}"))),
    }?;
    Ok(wrap_cached(raw))
}

// ---------------------------------------------------------------------------
// HMAC (dev)
// ---------------------------------------------------------------------------

pub struct HmacKeyRing {
    seeds: BTreeMap<NodeId, [u8; 32]>,
}

impl HmacKeyRing {
    pub fn for_unl(ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut seeds = BTreeMap::new();
        for id in ids {
            let id = id.into();
            let mut h = Sha256::new();
            h.update(format!("falcon-dag-v1:{id}").as_bytes());
            let dig = h.finalize();
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&dig);
            seeds.insert(id, seed);
        }
        Self { seeds }
    }
}

impl CryptoBackend for HmacKeyRing {
    fn scheme(&self) -> &str {
        "hmac-sha256-dev"
    }

    fn sign(&self, validator_id: &str, message: &str) -> Result<String, CryptoError> {
        let seed = self
            .seeds
            .get(validator_id)
            .ok_or_else(|| CryptoError::UnknownValidator(validator_id.into()))?;
        let mut mac = HmacSha256::new_from_slice(seed)
            .map_err(|_| CryptoError::SignFailed("hmac key".into()))?;
        mac.update(message.as_bytes());
        Ok(hex::encode(mac.finalize().into_bytes()))
    }

    fn verify(&self, validator_id: &str, message: &str, signature: &str) -> bool {
        match self.sign(validator_id, message) {
            Ok(expected) => expected == signature,
            Err(_) => false,
        }
    }

    fn public_ids(&self) -> Vec<NodeId> {
        self.seeds.keys().cloned().collect()
    }

    fn sample_sig_bytes(&self) -> Option<usize> {
        Some(32)
    }
}

// ---------------------------------------------------------------------------
// Falcon-512
// ---------------------------------------------------------------------------

pub struct Falcon512KeyRing {
    public_keys: BTreeMap<NodeId, falcon512::PublicKey>,
    secret_keys: BTreeMap<NodeId, falcon512::SecretKey>,
}

#[derive(Serialize, Deserialize)]
struct KeyFile {
    validator_id: String,
    scheme: String,
    public_key: String,
    secret_key: String,
}

impl Falcon512KeyRing {
    pub fn generate_unl(ids: impl IntoIterator<Item = impl Into<String>>) -> Result<Self, CryptoError> {
        let mut public_keys = BTreeMap::new();
        let mut secret_keys = BTreeMap::new();
        for id in ids {
            let id = id.into();
            let (pk, sk) = falcon512::keypair();
            if pk.as_bytes().len() != FALCON512_PK_LEN || sk.as_bytes().len() != FALCON512_SK_LEN {
                return Err(CryptoError::BadKey(format!(
                    "unexpected sizes pk={} sk={}",
                    pk.as_bytes().len(),
                    sk.as_bytes().len()
                )));
            }
            public_keys.insert(id.clone(), pk);
            secret_keys.insert(id, sk);
        }
        Ok(Self {
            public_keys,
            secret_keys,
        })
    }

    pub fn from_public_map(
        public_hex: BTreeMap<String, String>,
        local_id: Option<&str>,
        local_sk_hex: Option<&str>,
    ) -> Result<Self, CryptoError> {
        let mut public_keys = BTreeMap::new();
        for (id, hex_pk) in public_hex {
            let bytes = hex::decode(&hex_pk).map_err(|e| CryptoError::BadKey(e.to_string()))?;
            let pk = falcon512::PublicKey::from_bytes(&bytes)
                .map_err(|e| CryptoError::BadKey(format!("pk {id}: {e:?}")))?;
            public_keys.insert(id, pk);
        }
        let mut secret_keys = BTreeMap::new();
        if let (Some(id), Some(sk_hex)) = (local_id, local_sk_hex) {
            let bytes = hex::decode(sk_hex).map_err(|e| CryptoError::BadKey(e.to_string()))?;
            let sk = falcon512::SecretKey::from_bytes(&bytes)
                .map_err(|e| CryptoError::BadKey(format!("sk: {e:?}")))?;
            secret_keys.insert(id.to_string(), sk);
        }
        Ok(Self {
            public_keys,
            secret_keys,
        })
    }

    pub fn save_dir(&self, directory: impl AsRef<Path>) -> Result<(), CryptoError> {
        let d = directory.as_ref();
        std::fs::create_dir_all(d).map_err(|e| CryptoError::Io(e.to_string()))?;
        let pub_map: BTreeMap<String, String> = self
            .public_keys
            .iter()
            .map(|(k, v)| (k.clone(), hex::encode(v.as_bytes())))
            .collect();
        let pub_path = d.join("unl_public.json");
        std::fs::write(
            &pub_path,
            serde_json::to_string_pretty(&pub_map).unwrap() + "\n",
        )
        .map_err(|e| CryptoError::Io(e.to_string()))?;
        for (vid, sk) in &self.secret_keys {
            let pk = self
                .public_keys
                .get(vid)
                .ok_or_else(|| CryptoError::BadKey(format!("missing pk for {vid}")))?;
            let payload = KeyFile {
                validator_id: vid.clone(),
                scheme: "falcon-512".into(),
                public_key: hex::encode(pk.as_bytes()),
                secret_key: hex::encode(sk.as_bytes()),
            };
            let path = d.join(format!("{vid}.json"));
            std::fs::write(&path, serde_json::to_string_pretty(&payload).unwrap() + "\n")
                .map_err(|e| CryptoError::Io(e.to_string()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
        Ok(())
    }

    pub fn load_for_validator(
        directory: impl AsRef<Path>,
        validator_id: &str,
    ) -> Result<Self, CryptoError> {
        let d = directory.as_ref();
        let pub_path = d.join("unl_public.json");
        let text = std::fs::read_to_string(&pub_path).map_err(|e| CryptoError::Io(e.to_string()))?;
        let public_hex: BTreeMap<String, String> =
            serde_json::from_str(&text).map_err(|e| CryptoError::BadKey(e.to_string()))?;
        let local = d.join(format!("{validator_id}.json"));
        let local_sk = if local.is_file() {
            let t = std::fs::read_to_string(&local).map_err(|e| CryptoError::Io(e.to_string()))?;
            let kf: KeyFile =
                serde_json::from_str(&t).map_err(|e| CryptoError::BadKey(e.to_string()))?;
            Some(kf.secret_key)
        } else {
            None
        };
        Self::from_public_map(
            public_hex,
            Some(validator_id),
            local_sk.as_deref(),
        )
    }

    pub fn has_secret(&self, validator_id: &str) -> bool {
        self.secret_keys.contains_key(validator_id)
    }
}

impl CryptoBackend for Falcon512KeyRing {
    fn scheme(&self) -> &str {
        "falcon-512"
    }

    fn sign(&self, validator_id: &str, message: &str) -> Result<String, CryptoError> {
        let sk = self
            .secret_keys
            .get(validator_id)
            .ok_or_else(|| CryptoError::NoSecret(validator_id.into()))?;
        let sig = falcon512::detached_sign(message.as_bytes(), sk);
        Ok(hex::encode(sig.as_bytes()))
    }

    fn verify(&self, validator_id: &str, message: &str, signature: &str) -> bool {
        let Some(pk) = self.public_keys.get(validator_id) else {
            return false;
        };
        if signature.is_empty() {
            return false;
        }
        let Ok(bytes) = hex::decode(signature) else {
            return false;
        };
        let Ok(sig) = falcon512::DetachedSignature::from_bytes(&bytes) else {
            return false;
        };
        falcon512::verify_detached_signature(&sig, message.as_bytes(), pk).is_ok()
    }

    fn public_ids(&self) -> Vec<NodeId> {
        self.public_keys.keys().cloned().collect()
    }

    fn export_public(&self) -> BTreeMap<String, String> {
        self.public_keys
            .iter()
            .map(|(k, v)| (k.clone(), hex::encode(v.as_bytes())))
            .collect()
    }

    fn sample_sig_bytes(&self) -> Option<usize> {
        let vid = self.secret_keys.keys().next()?;
        self.sign(vid, "size-probe")
            .ok()
            .map(|s| hex::decode(s).map(|b| b.len()).unwrap_or(0))
    }
}

// ---------------------------------------------------------------------------
// Wallet (user transaction signing)
// ---------------------------------------------------------------------------

/// Wallet backend for user payment txs (distinct from validator DA keys).
pub trait WalletBackend: Send + Sync {
    fn scheme(&self) -> &str;
    /// Sign and attach `signature` + `public_key` on the transaction.
    fn sign_tx(&self, tx: &mut Transaction, network_id: u32) -> Result<(), CryptoError>;
    /// Verify an already-signed transaction.
    fn verify_tx(&self, tx: &Transaction, network_id: u32) -> bool;
}

pub type SharedWallet = Arc<dyn WalletBackend>;

/// HMAC-dev wallets: secret derived from account id (research only).
pub struct HmacWallet;

impl HmacWallet {
    pub fn new() -> Self {
        Self
    }

    fn seed_for(account: &str) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(format!("falcon-dag-wallet-v1:{account}").as_bytes());
        let dig = h.finalize();
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&dig);
        seed
    }

    pub fn public_key_for(account: &str) -> String {
        format!("hmac-dev:{}", account)
    }

    fn sign_message(account: &str, message: &str) -> Result<String, CryptoError> {
        let seed = Self::seed_for(account);
        let mut mac = HmacSha256::new_from_slice(&seed)
            .map_err(|_| CryptoError::SignFailed("hmac wallet".into()))?;
        mac.update(message.as_bytes());
        Ok(hex::encode(mac.finalize().into_bytes()))
    }
}

impl Default for HmacWallet {
    fn default() -> Self {
        Self::new()
    }
}

impl WalletBackend for HmacWallet {
    fn scheme(&self) -> &str {
        "hmac-sha256-dev"
    }

    fn sign_tx(&self, tx: &mut Transaction, network_id: u32) -> Result<(), CryptoError> {
        let msg = tx.sign_message(network_id);
        let sig = Self::sign_message(&tx.account, &msg)?;
        tx.public_key = Some(Self::public_key_for(&tx.account));
        tx.signature = Some(sig);
        Ok(())
    }

    fn verify_tx(&self, tx: &Transaction, network_id: u32) -> bool {
        let Some(sig) = tx.signature.as_deref() else {
            return false;
        };
        let Some(pk) = tx.public_key.as_deref() else {
            return false;
        };
        if pk != Self::public_key_for(&tx.account) {
            return false;
        }
        let msg = tx.sign_message(network_id);
        match Self::sign_message(&tx.account, &msg) {
            Ok(expected) => expected == sig,
            Err(_) => false,
        }
    }
}

/// Falcon-512 wallet ring keyed by account id.
pub struct Falcon512Wallet {
    secrets: BTreeMap<String, falcon512::SecretKey>,
    publics: BTreeMap<String, falcon512::PublicKey>,
}

impl Falcon512Wallet {
    pub fn generate_accounts(accounts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut secrets = BTreeMap::new();
        let mut publics = BTreeMap::new();
        for a in accounts {
            let a = a.into();
            let (pk, sk) = falcon512::keypair();
            secrets.insert(a.clone(), sk);
            publics.insert(a, pk);
        }
        Self { secrets, publics }
    }

    /// Faucet accounts acct1..acctN used by genesis.
    pub fn faucet(n: u32) -> Self {
        Self::generate_accounts((1..=n).map(|i| format!("acct{i}")))
    }

    pub fn save_dir(&self, directory: impl AsRef<Path>) -> Result<(), CryptoError> {
        let d = directory.as_ref();
        std::fs::create_dir_all(d).map_err(|e| CryptoError::Io(e.to_string()))?;
        let pubs: BTreeMap<String, String> = self
            .publics
            .iter()
            .map(|(k, v)| (k.clone(), hex::encode(v.as_bytes())))
            .collect();
        std::fs::write(
            d.join("wallet_public.json"),
            serde_json::to_string_pretty(&pubs).unwrap() + "\n",
        )
        .map_err(|e| CryptoError::Io(e.to_string()))?;
        for (acct, sk) in &self.secrets {
            let pk = self.publics.get(acct).unwrap();
            let payload = serde_json::json!({
                "account": acct,
                "scheme": "falcon-512",
                "public_key": hex::encode(pk.as_bytes()),
                "secret_key": hex::encode(sk.as_bytes()),
            });
            let path = d.join(format!("{acct}.json"));
            std::fs::write(&path, serde_json::to_string_pretty(&payload).unwrap() + "\n")
                .map_err(|e| CryptoError::Io(e.to_string()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
        Ok(())
    }

    pub fn load_dir(directory: impl AsRef<Path>) -> Result<Self, CryptoError> {
        let d = directory.as_ref();
        let pub_text = std::fs::read_to_string(d.join("wallet_public.json"))
            .map_err(|e| CryptoError::Io(e.to_string()))?;
        let pubs_hex: BTreeMap<String, String> =
            serde_json::from_str(&pub_text).map_err(|e| CryptoError::BadKey(e.to_string()))?;
        let mut publics = BTreeMap::new();
        for (acct, hex_pk) in pubs_hex {
            let bytes = hex::decode(&hex_pk).map_err(|e| CryptoError::BadKey(e.to_string()))?;
            let pk = falcon512::PublicKey::from_bytes(&bytes)
                .map_err(|e| CryptoError::BadKey(format!("{e:?}")))?;
            publics.insert(acct, pk);
        }
        let mut secrets = BTreeMap::new();
        for entry in std::fs::read_dir(d).map_err(|e| CryptoError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| CryptoError::Io(e.to_string()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "wallet_public.json" || !name.ends_with(".json") {
                continue;
            }
            let text =
                std::fs::read_to_string(entry.path()).map_err(|e| CryptoError::Io(e.to_string()))?;
            let v: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| CryptoError::BadKey(e.to_string()))?;
            let acct = v["account"]
                .as_str()
                .ok_or_else(|| CryptoError::BadKey("missing account".into()))?
                .to_string();
            if let Some(sk_hex) = v["secret_key"].as_str() {
                let bytes =
                    hex::decode(sk_hex).map_err(|e| CryptoError::BadKey(e.to_string()))?;
                let sk = falcon512::SecretKey::from_bytes(&bytes)
                    .map_err(|e| CryptoError::BadKey(format!("{e:?}")))?;
                secrets.insert(acct, sk);
            }
        }
        Ok(Self { secrets, publics })
    }

    /// Verify-only ring (all UNL pubs, no secrets) — for nodes that don't faucet-sign.
    pub fn verify_only(publics: BTreeMap<String, falcon512::PublicKey>) -> Self {
        Self {
            secrets: BTreeMap::new(),
            publics,
        }
    }
}

impl WalletBackend for Falcon512Wallet {
    fn scheme(&self) -> &str {
        "falcon-512"
    }

    fn sign_tx(&self, tx: &mut Transaction, network_id: u32) -> Result<(), CryptoError> {
        let sk = self
            .secrets
            .get(&tx.account)
            .ok_or_else(|| CryptoError::NoSecret(tx.account.clone()))?;
        let pk = self
            .publics
            .get(&tx.account)
            .ok_or_else(|| CryptoError::UnknownValidator(tx.account.clone()))?;
        let msg = tx.sign_message(network_id);
        let sig = falcon512::detached_sign(msg.as_bytes(), sk);
        tx.public_key = Some(hex::encode(pk.as_bytes()));
        tx.signature = Some(hex::encode(sig.as_bytes()));
        Ok(())
    }

    fn verify_tx(&self, tx: &Transaction, network_id: u32) -> bool {
        let Some(sig_hex) = tx.signature.as_deref() else {
            return false;
        };
        let Some(pk_hex) = tx.public_key.as_deref() else {
            return false;
        };
        // Prefer embedded pk (wallet can rotate); optionally check against known map
        let Ok(pk_bytes) = hex::decode(pk_hex) else {
            return false;
        };
        let Ok(pk) = falcon512::PublicKey::from_bytes(&pk_bytes) else {
            return false;
        };
        if let Some(known) = self.publics.get(&tx.account) {
            if known.as_bytes() != pk.as_bytes() {
                return false;
            }
        }
        let Ok(sig_bytes) = hex::decode(sig_hex) else {
            return false;
        };
        let Ok(sig) = falcon512::DetachedSignature::from_bytes(&sig_bytes) else {
            return false;
        };
        let msg = tx.sign_message(network_id);
        falcon512::verify_detached_signature(&sig, msg.as_bytes(), &pk).is_ok()
    }
}

pub fn make_wallet(scheme: &str, wallet_dir: Option<&Path>) -> Result<SharedWallet, CryptoError> {
    match scheme {
        "hmac-sha256-dev" | "hmac" | "dev" => Ok(Arc::new(HmacWallet::new())),
        "falcon-512" | "falcon" | "falcon512" => {
            if let Some(dir) = wallet_dir {
                Ok(Arc::new(Falcon512Wallet::load_dir(dir)?))
            } else {
                Ok(Arc::new(Falcon512Wallet::faucet(20)))
            }
        }
        other => Err(CryptoError::BadKey(format!("unknown wallet scheme: {other}"))),
    }
}

/// Verify a signed tx using only embedded `public_key` (no wallet store).
pub fn verify_tx_embedded(tx: &Transaction, network_id: u32) -> bool {
    let Some(pk) = tx.public_key.as_deref() else {
        return false;
    };
    if pk.starts_with("hmac-dev:") {
        return HmacWallet::new().verify_tx(tx, network_id);
    }
    // Falcon-512 embedded pk
    let Some(sig_hex) = tx.signature.as_deref() else {
        return false;
    };
    let Ok(pk_bytes) = hex::decode(pk) else {
        return false;
    };
    let Ok(pk) = falcon512::PublicKey::from_bytes(&pk_bytes) else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(sig_hex) else {
        return false;
    };
    let Ok(sig) = falcon512::DetachedSignature::from_bytes(&sig_bytes) else {
        return false;
    };
    let msg = tx.sign_message(network_id);
    falcon512::verify_detached_signature(&sig, msg.as_bytes(), &pk).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_sign_verify_roundtrip() {
        let ring = HmacKeyRing::for_unl(["val1", "val2"]);
        let msg = batch_sign_message("abcd");
        let sig = ring.sign("val1", &msg).unwrap();
        assert!(ring.verify("val1", &msg, &sig));
        assert!(!ring.verify("val2", &msg, &sig));
    }

    #[test]
    fn falcon512_sign_verify_and_sizes() {
        let ring = Falcon512KeyRing::generate_unl(["val1", "val2"]).unwrap();
        assert_eq!(falcon512::public_key_bytes(), FALCON512_PK_LEN);
        assert_eq!(falcon512::secret_key_bytes(), FALCON512_SK_LEN);
        let sig = ring.sign_batch("val1", "deadbeef").unwrap();
        let sig_len = hex::decode(&sig).unwrap().len();
        // compressed Falcon-512 ~650–690 bytes (not padded max)
        assert!(
            (600..=690).contains(&sig_len),
            "unexpected sig len {sig_len}"
        );
        assert!(ring.verify_batch("val1", "deadbeef", &sig));
        assert!(!ring.verify_batch("val2", "deadbeef", &sig));
        assert!(!ring.verify_batch("val1", "other", &sig));
    }

    #[test]
    fn falcon512_save_load() {
        let dir = std::env::temp_dir().join(format!("fd-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ring = Falcon512KeyRing::generate_unl(["val1", "val2"]).unwrap();
        ring.save_dir(&dir).unwrap();
        let loaded = Falcon512KeyRing::load_for_validator(&dir, "val1").unwrap();
        let msg = "hello-pq";
        let sig = loaded.sign("val1", msg).unwrap();
        assert!(loaded.verify("val1", msg, &sig));
        assert!(loaded.verify("val2", msg, &sig) == false);
        // val2 pub present, no secret
        assert!(!loaded.has_secret("val2"));
        assert!(loaded.verify("val1", msg, &sig));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hmac_wallet_tx_roundtrip() {
        let w = HmacWallet::new();
        let mut tx = Transaction::create("acct1", 1, "acct2", 10, 1, 0.0);
        w.sign_tx(&mut tx, 9002).unwrap();
        assert!(tx.is_signed());
        assert!(w.verify_tx(&tx, 9002));
        assert!(!w.verify_tx(&tx, 9003)); // wrong network
        tx.amount = 99;
        // tx_id not recomputed; message uses amount field → fail
        assert!(!w.verify_tx(&tx, 9002));
    }

    #[test]
    fn falcon_wallet_tx_roundtrip() {
        let w = Falcon512Wallet::faucet(3);
        let mut tx = Transaction::create("acct1", 1, "acct2", 10, 1, 0.0);
        w.sign_tx(&mut tx, 9002).unwrap();
        assert!(w.verify_tx(&tx, 9002));
        let sig_len = hex::decode(tx.signature.as_ref().unwrap()).unwrap().len();
        assert!((600..=690).contains(&sig_len));
        // tamper
        tx.destination = "acct3".into();
        assert!(!w.verify_tx(&tx, 9002));
    }
}
