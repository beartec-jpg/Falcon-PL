//! Certificate DAG (data availability layer).

use fd_crypto::{HmacKeyRing, SharedCrypto};
use fd_types::{
    genesis_batch_hash, genesis_cert_id, Batch, Certificate, HashHex, NodeId, ProtocolConfig,
    Transaction,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

pub struct CertificateDag {
    pub validator_id: NodeId,
    pub config: ProtocolConfig,
    pub unl: BTreeMap<NodeId, f64>,
    pub crypto: SharedCrypto,
    pub batches: HashMap<HashHex, Batch>,
    pub certificates: HashMap<HashHex, Certificate>,
    pub children: HashMap<HashHex, HashSet<HashHex>>,
    pub tx_index: HashMap<HashHex, Transaction>,
    pub rejected_sigs: u64,
    pub rejected_batches: u64,
    pub equivocation_count: u64,
    /// Breakdown of batch ingest rejects (forensic / continuous body-DA).
    pub reject_parents_too_many: u64,
    pub reject_hash_mismatch: u64,
    pub reject_max_known: u64,
    pub reject_max_uncertified: u64,
    pub reject_max_per_creator: u64,
    pub reject_equivocation: u64,
    pub reject_genesis: u64,
    pub ingest_new_ok: u64,
    pub ingest_merge_ok: u64,
    /// New UNL foreign signatures accepted via add_signature / body merge.
    pub foreign_sig_merged: u64,
    /// Partial sigs dropped because batch body was not held yet.
    pub foreign_sig_missing_batch: u64,
    /// creator -> parent_fp -> first batch hash
    attest_by_parents: HashMap<NodeId, HashMap<String, HashHex>>,
}

impl CertificateDag {
    pub fn new(
        validator_id: NodeId,
        unl_ids: impl IntoIterator<Item = impl Into<String>>,
        config: ProtocolConfig,
    ) -> Self {
        let ids: Vec<String> = unl_ids.into_iter().map(|s| s.into()).collect();
        let crypto: SharedCrypto = Arc::new(HmacKeyRing::for_unl(ids.iter().cloned()));
        Self::with_crypto(validator_id, ids, config, crypto)
    }

    pub fn with_crypto(
        validator_id: NodeId,
        unl_ids: impl IntoIterator<Item = impl Into<String>>,
        config: ProtocolConfig,
        crypto: SharedCrypto,
    ) -> Self {
        let ids: Vec<String> = unl_ids.into_iter().map(|s| s.into()).collect();
        let n = ids.len().max(1) as f64;
        let unl: BTreeMap<_, _> = ids.iter().cloned().map(|id| (id, 1.0 / n)).collect();
        let g = genesis_batch_hash();
        let mut batches = HashMap::new();
        let mut certificates = HashMap::new();
        let mut sigs = BTreeMap::new();
        for id in &ids {
            sigs.insert(id.clone(), "GENESIS".into());
        }
        batches.insert(
            g.clone(),
            Batch {
                batch_hash: g.clone(),
                parents: vec![],
                tx_ids: vec![],
                creator: "genesis".into(),
                timestamp: 0.0,
                signatures: sigs,
                txs: BTreeMap::new(),
            },
        );
        certificates.insert(
            g.clone(),
            Certificate {
                cert_id: genesis_cert_id(),
                batch_hash: g.clone(),
                signers: ids.clone(),
                parents: vec![],
                creator: "genesis".into(),
                timestamp: 0.0,
                round: 0,
            },
        );
        Self {
            validator_id,
            config,
            unl,
            crypto,
            batches,
            certificates,
            children: HashMap::new(),
            tx_index: HashMap::new(),
            rejected_sigs: 0,
            rejected_batches: 0,
            equivocation_count: 0,
            reject_parents_too_many: 0,
            reject_hash_mismatch: 0,
            reject_max_known: 0,
            reject_max_uncertified: 0,
            reject_max_per_creator: 0,
            reject_equivocation: 0,
            reject_genesis: 0,
            ingest_new_ok: 0,
            ingest_merge_ok: 0,
            foreign_sig_merged: 0,
            foreign_sig_missing_batch: 0,
            attest_by_parents: HashMap::new(),
        }
    }

    /// Weight of admitted signatures. Signatures already on the batch were verified
    /// at `add_signature` (and may be cache hits). Do **not** re-verify Falcon on every
    /// `try_certify` / tick — that was the live-node CPU wall.
    pub fn support_weight(&self, batch: &Batch) -> f64 {
        let mut w = 0.0;
        for (vid, sig) in &batch.signatures {
            if !self.unl.contains_key(vid) {
                continue;
            }
            if sig.is_empty() {
                continue;
            }
            if batch.batch_hash == genesis_batch_hash() || sig == "GENESIS" {
                if batch.batch_hash == genesis_batch_hash() {
                    w += self.unl[vid];
                }
                continue;
            }
            // Admitted signature — trust insert-time verify (+ cache).
            w += self.unl[vid];
        }
        w
    }

    pub fn certified_tips(&self) -> Vec<HashHex> {
        let mut tips = Vec::new();
        for bh in self.certificates.keys() {
            let kids = self.children.get(bh);
            let has_cert_child = kids
                .map(|k| k.iter().any(|c| self.certificates.contains_key(c)))
                .unwrap_or(false);
            if !has_cert_child {
                tips.push(bh.clone());
            }
        }
        // Highest-round tips only → shared frontier, less multi-node fork width.
        let max_round = tips
            .iter()
            .filter_map(|t| self.certificates.get(t).map(|c| c.round))
            .max()
            .unwrap_or(0);
        tips.retain(|t| {
            self.certificates
                .get(t)
                .map(|c| c.round == max_round)
                .unwrap_or(false)
        });
        tips.sort();
        if tips.is_empty() {
            vec![genesis_batch_hash()]
        } else {
            tips
        }
    }

    pub fn ingest_tx(&mut self, tx: Transaction) {
        self.tx_index.insert(tx.tx_id.clone(), tx);
    }

    pub fn ingest_batch(&mut self, mut batch: Batch) -> bool {
        if batch.batch_hash == genesis_batch_hash() {
            self.reject_genesis += 1;
            return false;
        }
        if batch.parents.len() > self.config.max_batch_parents {
            self.rejected_batches += 1;
            self.reject_parents_too_many += 1;
            return false;
        }
        let expected = Batch::compute_hash(
            &batch.tx_ids,
            &batch.parents,
            &batch.creator,
            batch.timestamp,
        );
        if expected != batch.batch_hash {
            self.rejected_batches += 1;
            self.reject_hash_mismatch += 1;
            return false;
        }
        let bh = batch.batch_hash.clone();
        let is_new = !self.batches.contains_key(&bh);
        if is_new {
            if self.batches.len() >= self.config.max_known_batches {
                self.rejected_batches += 1;
                self.reject_max_known += 1;
                return false;
            }
            let unc = self
                .batches
                .keys()
                .filter(|b| !self.certificates.contains_key(*b))
                .count();
            if unc >= self.config.max_uncertified_batches {
                self.rejected_batches += 1;
                self.reject_max_uncertified += 1;
                return false;
            }
            let per = self
                .batches
                .values()
                .filter(|b| {
                    b.creator == batch.creator && !self.certificates.contains_key(&b.batch_hash)
                })
                .count();
            if per >= self.config.max_uncertified_per_creator {
                self.rejected_batches += 1;
                self.reject_max_per_creator += 1;
                return false;
            }
            // Reject a second distinct proposal from the same creator for the same parents
            // only while the first is still *uncertified* (classic equivocation / fork).
            // After the prior batch is certified, a new batch may reuse the same parent
            // tips as the frontier advances slowly — rejecting those left open batches
            // stranded on the creator (force-before-move + empty peer views).
            let fp = parents_fp(&batch.parents);
            if let Some(prev) = self
                .attest_by_parents
                .get(&batch.creator)
                .and_then(|m| m.get(&fp))
            {
                if prev != &bh
                    && self.batches.contains_key(prev)
                    && !self.certificates.contains_key(prev)
                {
                    self.equivocation_count += 1;
                    self.rejected_batches += 1;
                    self.reject_equivocation += 1;
                    return false;
                }
            }
            for p in &batch.parents {
                self.children
                    .entry(p.clone())
                    .or_default()
                    .insert(bh.clone());
            }
            for t in batch.txs.values() {
                self.tx_index.insert(t.tx_id.clone(), t.clone());
            }
            self.batches.insert(bh.clone(), batch);
            self.ingest_new_ok += 1;
        } else {
            self.ingest_merge_ok += 1;
            let creator = self.batches.get(&bh).map(|b| b.creator.clone());
            let existing = self.batches.get_mut(&bh).unwrap();
            for (vid, sig) in batch.signatures {
                if !existing.signatures.contains_key(&vid) {
                    if self.crypto.verify_batch(&vid, &existing.batch_hash, &sig) {
                        let is_foreign_unl = self.unl.contains_key(&vid)
                            && creator.as_ref().map(|c| c != &vid).unwrap_or(true);
                        existing.signatures.insert(vid, sig);
                        if is_foreign_unl {
                            self.foreign_sig_merged += 1;
                        }
                    } else {
                        self.rejected_sigs += 1;
                    }
                }
            }
            existing.txs.append(&mut batch.txs);
        }
        // A1: always re-attempt cert after body/sig merge (foreign weight may tip quorum).
        let _ = self.try_certify(&bh);
        is_new
    }

    pub fn add_signature(
        &mut self,
        batch_hash: &str,
        validator_id: &str,
        signature: &str,
    ) -> Option<Certificate> {
        if !self.unl.contains_key(validator_id) {
            return None;
        }
        let batch = self.batches.get(batch_hash)?;
        if batch.signatures.get(validator_id).map(|s| s.as_str()) == Some(signature) {
            return self.try_certify(batch_hash);
        }
        if !self.crypto.verify_batch(validator_id, batch_hash, signature) {
            self.rejected_sigs += 1;
            return None;
        }
        if batch.creator == validator_id {
            let fp = parents_fp(&batch.parents);
            let entry = self
                .attest_by_parents
                .entry(validator_id.to_string())
                .or_default();
            if let Some(prev) = entry.get(&fp) {
                // Only equivocation if prior proposal for these parents is still open.
                if prev != batch_hash
                    && self.batches.contains_key(prev)
                    && !self.certificates.contains_key(prev)
                {
                    self.equivocation_count += 1;
                    return None;
                }
                // Prior certified (or gone): allow this hash and update attest pointer.
                entry.insert(fp, batch_hash.to_string());
            } else {
                entry.insert(fp, batch_hash.to_string());
            }
        }
        let is_foreign_unl = {
            let batch = self.batches.get(batch_hash).unwrap();
            self.unl.contains_key(validator_id) && batch.creator != validator_id
        };
        let batch = self.batches.get_mut(batch_hash).unwrap();
        let was_new = !batch.signatures.contains_key(validator_id);
        batch
            .signatures
            .insert(validator_id.to_string(), signature.to_string());
        if was_new && is_foreign_unl {
            self.foreign_sig_merged += 1;
        }
        // A1: immediate cert attempt after every accepted merge.
        self.try_certify(batch_hash)
    }

    pub fn sign_local(&self, batch_hash: &str) -> String {
        self.crypto
            .sign_batch(&self.validator_id, batch_hash)
            .expect("local key")
    }

    pub fn try_certify(&mut self, batch_hash: &str) -> Option<Certificate> {
        if self.certificates.contains_key(batch_hash) {
            return None; // already certified — no re-verify, no re-work
        }
        let batch = self.batches.get(batch_hash)?;
        if self.support_weight(batch) + 1e-12 < self.config.unl_threshold {
            return None;
        }
        let mut parent_rounds = Vec::new();
        for p in &batch.parents {
            let c = self.certificates.get(p)?;
            parent_rounds.push(c.round);
        }
        let rnd = parent_rounds.iter().max().map(|r| r + 1).unwrap_or(1);
        let cert = Certificate::from_batch(batch, rnd);
        self.certificates
            .insert(batch_hash.to_string(), cert.clone());
        Some(cert)
    }

    pub fn prune_uncertified(&mut self, now: f64) -> u64 {
        let ttl = self.config.uncertified_ttl_s;
        if ttl <= 0.0 {
            return 0;
        }
        // Hysteresis: require TTL * 1.25 age to reduce pack→prune→requeue bounce.
        let ttl_hyst = ttl * 1.25;
        let drop: Vec<HashHex> = self
            .batches
            .iter()
            .filter(|(bh, b)| {
                *bh != &genesis_batch_hash()
                    && !self.certificates.contains_key(*bh)
                    && (now - b.timestamp) > ttl_hyst
                    // Keep near-quorum bodies (waiting on parent certs).
                    && self.support_weight(b) + 1e-12 < self.config.unl_threshold
                    // Steady-TPS: never drop a body that already has a foreign sig —
                    // peers may certify it while we prune → creator-missing cert set
                    // (L100 forensic: val3 lost own r6 bodies that others certified).
                    && b.signatures.len() <= 1
            })
            .map(|(bh, _)| bh.clone())
            .collect();
        let n = drop.len() as u64;
        for bh in drop {
            // Do NOT free attest_by_parents: re-proposing the same parent set after
            // prune creates multiple hashes peers still hold (cert set diverge).
            self.batches.remove(&bh);
            for kids in self.children.values_mut() {
                kids.remove(&bh);
            }
            self.children.remove(&bh);
        }
        n
    }

    /// Drop a still-uncertified batch body (creator recovery / repool).
    /// Frees `attest_by_parents` when it still points at this hash so the
    /// creator may re-propose on the same parents after body-DA failure.
    /// Returns the removed batch, or None if missing / already certified.
    pub fn abandon_uncertified(&mut self, batch_hash: &str) -> Option<Batch> {
        if self.certificates.contains_key(batch_hash) {
            return None;
        }
        let batch = self.batches.remove(batch_hash)?;
        let fp = parents_fp(&batch.parents);
        if let Some(m) = self.attest_by_parents.get_mut(&batch.creator) {
            if m.get(&fp).map(|h| h.as_str() == batch_hash).unwrap_or(false) {
                m.remove(&fp);
            }
            if m.is_empty() {
                self.attest_by_parents.remove(&batch.creator);
            }
        }
        for kids in self.children.values_mut() {
            kids.remove(batch_hash);
        }
        self.children.remove(batch_hash);
        Some(batch)
    }

    pub fn uncertified_count(&self) -> usize {
        self.batches
            .keys()
            .filter(|b| !self.certificates.contains_key(*b))
            .count()
    }

    /// Non-genesis uncertified batches (stall diagnostics / packer recovery).
    pub fn uncertified_count_non_genesis(&self) -> usize {
        self.batches
            .values()
            .filter(|b| {
                b.batch_hash != genesis_batch_hash()
                    && !self.certificates.contains_key(&b.batch_hash)
            })
            .count()
    }

    /// Oldest non-genesis uncertified batch timestamp, if any.
    pub fn oldest_uncertified_timestamp(&self) -> Option<f64> {
        self.batches
            .values()
            .filter(|b| {
                b.batch_hash != genesis_batch_hash()
                    && !self.certificates.contains_key(&b.batch_hash)
            })
            .map(|b| b.timestamp)
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// Uncertified batches created by `creator` (excludes genesis).
    pub fn uncertified_by_creator(&self, creator: &str) -> usize {
        self.batches
            .values()
            .filter(|b| {
                b.creator == creator
                    && b.batch_hash != genesis_batch_hash()
                    && !self.certificates.contains_key(&b.batch_hash)
            })
            .count()
    }

    /// Hash of this creator's outstanding uncertified proposal, if any.
    pub fn outstanding_proposal(&self, creator: &str) -> Option<HashHex> {
        self.batches
            .values()
            .find(|b| {
                b.creator == creator
                    && b.batch_hash != genesis_batch_hash()
                    && !self.certificates.contains_key(&b.batch_hash)
            })
            .map(|b| b.batch_hash.clone())
    }

    /// True if creator already has an uncertified batch with the same parent set.
    pub fn has_uncertified_on_parents(&self, creator: &str, parents: &[HashHex]) -> bool {
        let fp = parents_fp(parents);
        self.batches.values().any(|b| {
            b.creator == creator
                && b.batch_hash != genesis_batch_hash()
                && !self.certificates.contains_key(&b.batch_hash)
                && parents_fp(&b.parents) == fp
        })
    }

    /// Snapshot restore: insert batch without DoS caps (still checks hash).
    pub fn force_ingest_batch(&mut self, batch: Batch) -> bool {
        if batch.batch_hash == genesis_batch_hash() {
            return false;
        }
        let expected = Batch::compute_hash(
            &batch.tx_ids,
            &batch.parents,
            &batch.creator,
            batch.timestamp,
        );
        if expected != batch.batch_hash {
            self.rejected_batches += 1;
            return false;
        }
        let bh = batch.batch_hash.clone();
        for t in batch.txs.values() {
            self.tx_index.insert(t.tx_id.clone(), t.clone());
        }
        for p in &batch.parents {
            self.children
                .entry(p.clone())
                .or_default()
                .insert(bh.clone());
        }
        self.batches.insert(bh, batch);
        true
    }

    /// Install a certificate record (snapshot path when re-certify is incomplete).
    pub fn install_certificate(&mut self, cert: Certificate) {
        self.certificates
            .insert(cert.batch_hash.clone(), cert);
    }

    pub fn rebuild_children(&mut self) {
        self.children.clear();
        for (bh, batch) in &self.batches {
            for p in &batch.parents {
                self.children
                    .entry(p.clone())
                    .or_default()
                    .insert(bh.clone());
            }
        }
    }
}

fn parents_fp(parents: &[HashHex]) -> String {
    let mut p = parents.to_vec();
    p.sort();
    p.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fd_crypto::Falcon512KeyRing;
    use fd_types::Transaction;

    #[test]
    fn certify_with_quorum_hmac() {
        let ids = vec!["val1", "val2", "val3", "val4", "val5"];
        let mut dag = CertificateDag::new(
            "val1".into(),
            ids.iter().copied(),
            ProtocolConfig {
                require_sig_verify: true,
                crypto_scheme: "hmac-sha256-dev".into(),
                ..Default::default()
            },
        );
        let tx = Transaction::create("acct1", 1, "acct2", 1, 10, 0.0);
        let b = Batch::create(vec![tx], vec![genesis_batch_hash()], "val1".into(), 1.0);
        let hash = b.batch_hash.clone();
        dag.ingest_batch(b);
        for id in ["val1", "val2", "val3", "val4"] {
            let sig = dag.crypto.sign_batch(id, &hash).unwrap();
            dag.add_signature(&hash, id, &sig);
        }
        assert!(dag.certificates.contains_key(&hash));
    }

    #[test]
    fn certify_with_quorum_falcon512() {
        let ids: Vec<String> = (1..=5).map(|i| format!("val{i}")).collect();
        let ring = Arc::new(Falcon512KeyRing::generate_unl(ids.iter().cloned()).unwrap());
        let mut dag = CertificateDag::with_crypto(
            "val1".into(),
            ids.clone(),
            ProtocolConfig {
                crypto_scheme: "falcon-512".into(),
                ..Default::default()
            },
            ring,
        );
        let tx = Transaction::create("acct1", 1, "acct2", 1, 10, 0.0);
        let b = Batch::create(vec![tx], vec![genesis_batch_hash()], "val1".into(), 1.0);
        let hash = b.batch_hash.clone();
        dag.ingest_batch(b);
        for id in &ids[..4] {
            let sig = dag.crypto.sign_batch(id, &hash).unwrap();
            assert!(
                dag.crypto.verify_batch(id, &hash, &sig),
                "verify failed for {id}"
            );
            dag.add_signature(&hash, id, &sig);
        }
        assert!(
            dag.certificates.contains_key(&hash),
            "expected cert after 4/5 Falcon sigs, weight={}",
            dag.support_weight(dag.batches.get(&hash).unwrap())
        );
        assert_eq!(dag.crypto.scheme(), "falcon-512");
        let sample = dag.crypto.sample_sig_bytes().unwrap();
        assert!((600..=690).contains(&sample), "sig bytes {sample}");
    }
}
