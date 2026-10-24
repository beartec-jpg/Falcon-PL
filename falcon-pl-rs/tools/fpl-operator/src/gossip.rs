//! Signed operator records + peer gossip.
//!
//! Every operator signs records (BIP340 Schnorr, its registry x-only key) about
//! what it sees: WithdrawalSeen (ledger binding), ClaimIntent, PayoutTx, Kickoff,
//! Heartbeat, Disagreement, Challenge. Records are accepted only when the signer
//! is in the operator registry snapshot (peers.json now; the on-chain registry
//! once active), persisted append-only (records.jsonl) and replayed on start.
//! Records carry facts only: no keys, no votes over funds.

use k256::schnorr::{signature::Signer, signature::Verifier, Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DOMAIN: &[u8] = b"FPL-OPGOSSIP-v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Record {
    pub v: u32,
    pub kind: String,
    pub note_id: String,
    /// hex sha256 binding (see `binding`); empty for Heartbeat.
    pub binding: String,
    #[serde(default)]
    pub data: BTreeMap<String, String>,
    pub signer: String,
    pub ts: u64,
    #[serde(default)]
    pub sig: String,
}

/// Withdrawal binding from the Falcon ledger: what every operator must agree
/// on before anyone fronts BTC / kicks off.
pub fn binding(note_id: &str, dest: &str, amount: u64, open_height: u64) -> String {
    let mut h = Sha256::new();
    h.update(b"FPL-WITHDRAWAL-BINDING-v1");
    for part in [note_id, dest, &amount.to_string(), &open_height.to_string()] {
        h.update((part.len() as u32).to_le_bytes());
        h.update(part.as_bytes());
    }
    hex::encode(h.finalize())
}

fn digest(r: &Record) -> [u8; 32] {
    let mut c = r.clone();
    c.sig.clear();
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update(serde_json::to_vec(&c).unwrap()); // BTreeMap + fixed field order = canonical
    h.finalize().into()
}

pub fn record_id(r: &Record) -> String {
    hex::encode(Sha256::digest(r.sig.as_bytes()))
}

pub struct Signer_ {
    key: SigningKey,
    pub xonly: String,
}

impl Signer_ {
    /// From the operator's local 32-byte hex secret (never logged).
    pub fn from_secret_hex(s: &str) -> Result<Self, String> {
        let b = hex::decode(s.trim()).map_err(|_| "secret not hex".to_string())?;
        let key = SigningKey::from_bytes(&b).map_err(|_| "bad secret".to_string())?;
        let xonly = hex::encode(key.verifying_key().to_bytes());
        Ok(Signer_ { key, xonly })
    }
    pub fn sign(&self, mut r: Record) -> Record {
        r.signer = self.xonly.clone();
        r.sig.clear();
        let sig: Signature = self.key.sign(&digest(&r));
        r.sig = hex::encode(sig.to_bytes());
        r
    }
}

pub fn verify(r: &Record) -> bool {
    let (Ok(pk), Ok(sig)) = (hex::decode(&r.signer), hex::decode(&r.sig)) else { return false };
    let (Ok(vk), Ok(sig)) = (VerifyingKey::from_bytes(&pk), Signature::try_from(sig.as_slice())) else { return false };
    vk.verify(&digest(r), &sig).is_ok()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub name: String,
    pub xonly: String,
    #[serde(default)]
    pub url: String, // http://host:19481 (put behind TLS/WireGuard in production)
    #[serde(default)]
    pub account: String,
}

pub fn load_registry(p: &Path) -> Vec<Peer> {
    std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub struct Store {
    path: PathBuf,
    pub records: Vec<Record>,
    ids: BTreeSet<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Accept {
    New,
    Duplicate,
    BadSig,
    NotRegistered,
    Stale,
}

impl Store {
    pub fn open(path: &Path) -> Store {
        let mut s = Store { path: path.to_path_buf(), records: vec![], ids: BTreeSet::new() };
        if let Ok(txt) = std::fs::read_to_string(path) {
            for line in txt.lines() {
                if let Ok(r) = serde_json::from_str::<Record>(line) {
                    if verify(&r) && s.ids.insert(record_id(&r)) {
                        s.records.push(r);
                    }
                }
            }
        }
        s
    }
    /// Verify + persist. `registry` = allowed signer keys.
    pub fn accept(&mut self, r: Record, registry: &BTreeSet<String>, now: u64) -> Accept {
        if !registry.contains(&r.signer) {
            return Accept::NotRegistered;
        }
        if r.ts > now + 600 {
            return Accept::Stale;
        }
        if !verify(&r) {
            return Accept::BadSig;
        }
        if !self.ids.insert(record_id(&r)) {
            return Accept::Duplicate;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{}", serde_json::to_string(&r).unwrap());
        }
        self.records.push(r);
        Accept::New
    }
    pub fn since(&self, ts: u64) -> Vec<&Record> {
        self.records.iter().filter(|r| r.ts >= ts).collect()
    }
    pub fn for_note<'a>(&'a self, note: &'a str) -> impl Iterator<Item = &'a Record> + 'a {
        self.records.iter().filter(move |r| r.note_id == note)
    }
}

/// Pre-claim cross-check result.
#[derive(Debug, PartialEq, Eq)]
pub enum CrossCheck {
    Agreed { peers: usize },
    Waiting { have: usize, need: usize },
    Disagree { peer: String, theirs: String },
}

/// Peers' WithdrawalSeen records for `note` must match `ours`; `need` agreeing
/// peers (other than `me`). Any mismatch = disagreement (halt).
pub fn cross_check(store: &Store, note: &str, ours: &str, me: &str, need: usize) -> CrossCheck {
    let mut agree = BTreeSet::new();
    for r in store.for_note(note).filter(|r| r.kind == "WithdrawalSeen" && r.signer != me) {
        if r.binding != ours {
            return CrossCheck::Disagree { peer: r.signer.clone(), theirs: r.binding.clone() };
        }
        agree.insert(r.signer.clone());
    }
    if agree.len() >= need {
        CrossCheck::Agreed { peers: agree.len() }
    } else {
        CrossCheck::Waiting { have: agree.len(), need }
    }
}

/// Deterministic winner among ClaimIntents for a note: earliest ts, then lowest key.
pub fn intent_winner(store: &Store, note: &str) -> Option<(String, String)> {
    store
        .for_note(note)
        .filter(|r| r.kind == "ClaimIntent")
        .min_by(|a, b| (a.ts, &a.signer).cmp(&(b.ts, &b.signer)))
        .map(|r| (r.signer.clone(), r.data.get("deposit").cloned().unwrap_or_default()))
}

/// Latest advertised free fronting capacity (sats) of `signer` from its signed
/// Heartbeat / Balance records with `ts <= at`. None = never advertised.
pub fn capacity_at(store: &Store, signer: &str, at: u64) -> Option<u64> {
    store
        .records
        .iter()
        .filter(|r| r.signer == signer && (r.kind == "Heartbeat" || r.kind == "Balance") && r.ts <= at)
        .filter_map(|r| r.data.get("available_sats").and_then(|v| v.parse::<u64>().ok()).map(|c| (r.ts, c)))
        .max_by_key(|(ts, _)| *ts)
        .map(|(_, c)| c)
}

/// Winner among *eligible* ClaimIntents: the signer's capacity advertised at or
/// before its intent must cover `amount`. Earliest ts, then lowest key.
pub fn intent_winner_eligible(store: &Store, note: &str, amount: u64) -> Option<(String, String)> {
    store
        .for_note(note)
        .filter(|r| r.kind == "ClaimIntent")
        .filter(|r| capacity_at(store, &r.signer, r.ts).map_or(false, |c| c >= amount))
        .min_by(|a, b| (a.ts, &a.signer).cmp(&(b.ts, &b.signer)))
        .map(|r| (r.signer.clone(), r.data.get("deposit").cloned().unwrap_or_default()))
}

/// Quorum: ceil(2/3) of the other registered operators (0 when alone).
pub fn quorum(n_registered: usize) -> usize {
    let others = n_registered.saturating_sub(1);
    (others * 2 + 2) / 3
}

#[cfg(test)]
mod tests {
    use super::*;
    fn s(i: u8) -> Signer_ {
        Signer_::from_secret_hex(&hex::encode([i; 32])).unwrap()
    }
    fn rec(kind: &str, note: &str, b: &str, ts: u64) -> Record {
        Record { v: 1, kind: kind.into(), note_id: note.into(), binding: b.into(), data: BTreeMap::new(), signer: String::new(), ts, sig: String::new() }
    }
    #[test]
    fn sign_verify_tamper() {
        let a = s(1);
        let r = a.sign(rec("WithdrawalSeen", "n1", "bb", 5));
        assert!(verify(&r));
        let mut t = r.clone();
        t.binding = "cc".into();
        assert!(!verify(&t));
    }
    #[test]
    fn store_auth_persist_replay_crosscheck() {
        let dir = std::env::temp_dir().join(format!("fplg-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let (a, b, c, x) = (s(1), s(2), s(3), s(9));
        let reg: BTreeSet<String> = [&a, &b, &c].iter().map(|k| k.xonly.clone()).collect();
        let bind = binding("n1", "tb1qdest", 5000, 10);
        let mut st = Store::open(&dir);
        assert_eq!(st.accept(x.sign(rec("WithdrawalSeen", "n1", &bind, 1)), &reg, 100), Accept::NotRegistered);
        let rb = b.sign(rec("WithdrawalSeen", "n1", &bind, 1));
        assert_eq!(st.accept(rb.clone(), &reg, 100), Accept::New);
        assert_eq!(st.accept(rb, &reg, 100), Accept::Duplicate);
        assert_eq!(cross_check(&st, "n1", &bind, &a.xonly, quorum(3)), CrossCheck::Waiting { have: 1, need: 2 });
        st.accept(c.sign(rec("WithdrawalSeen", "n1", &bind, 2)), &reg, 100);
        assert_eq!(cross_check(&st, "n1", &bind, &a.xonly, quorum(3)), CrossCheck::Agreed { peers: 2 });
        // replay from disk
        let st2 = Store::open(&dir);
        assert_eq!(st2.records.len(), 2);
        // a lying peer -> disagreement
        let bad = binding("n1", "tb1qattacker", 5000, 10);
        st.accept(c.sign(rec("WithdrawalSeen", "n1", &bad, 3)), &reg, 100);
        assert!(matches!(cross_check(&st, "n1", &bind, &a.xonly, 2), CrossCheck::Disagree { .. }));
        let _ = std::fs::remove_file(&dir);
    }
    #[test]
    fn capacity_gates_intent_winner() {
        let dir = std::env::temp_dir().join(format!("fplcap-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let (a, b, c) = (s(1), s(2), s(3));
        let reg: BTreeSet<String> = [&a, &b, &c].iter().map(|k| k.xonly.clone()).collect();
        let mut st = Store::open(&dir);
        let cap = |k: &Signer_, kind: &str, sats: u64, ts: u64| {
            let mut r = rec(kind, "", "", ts);
            r.data.insert("available_sats".into(), sats.to_string());
            k.sign(r)
        };
        st.accept(cap(&a, "Heartbeat", 5_000, 1), &reg, 100); // too small
        st.accept(cap(&b, "Heartbeat", 50_000, 1), &reg, 100);
        st.accept(cap(&c, "Heartbeat", 1_000_000, 1), &reg, 100);
        st.accept(cap(&b, "Balance", 20_000, 3), &reg, 100); // b shrinks before its intent
        assert_eq!(capacity_at(&st, &b.xonly, 2), Some(50_000));
        assert_eq!(capacity_at(&st, &b.xonly, 9), Some(20_000));
        let int = |k: &Signer_, ts: u64| {
            let mut r = rec("ClaimIntent", "n1", "bb", ts);
            r.data.insert("deposit".into(), format!("d{ts}"));
            k.sign(r)
        };
        st.accept(int(&a, 4), &reg, 100); // earliest, but capacity 5k < 10k
        st.accept(int(&b, 5), &reg, 100); // 20k covers 10k
        st.accept(int(&c, 6), &reg, 100);
        assert_eq!(intent_winner(&st, "n1").unwrap().0, a.xonly, "legacy rule ignores capacity");
        assert_eq!(intent_winner_eligible(&st, "n1", 10_000).unwrap().0, b.xonly);
        assert_eq!(intent_winner_eligible(&st, "n1", 30_000).unwrap().0, c.xonly);
        assert_eq!(intent_winner_eligible(&st, "n1", 2_000_000), None);
        // b's balance drops after its intent: does not retroactively disqualify it
        st.accept(cap(&b, "Balance", 0, 7), &reg, 100);
        assert_eq!(intent_winner_eligible(&st, "n1", 10_000).unwrap().0, b.xonly);
        let _ = std::fs::remove_file(&dir);
    }
    #[test]
    fn quorum_sizes() {
        assert_eq!(quorum(1), 0);
        assert_eq!(quorum(2), 1);
        assert_eq!(quorum(4), 2);
        assert_eq!(quorum(10), 6);
    }
}
