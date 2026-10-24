//! Light ledger ordering: **Bullshark over the Narwhal certificate DAG**.
//!
//! # Split
//! - DA produces availability certificates (Narwhal).
//! - **Bullshark** derives commits from the DAG alone (zero extra consensus messages):
//!   even-round leaders (anchors) commit when round `r+1` has ≥ f+1 supporting parents.
//! - Optional **Tip-BFT** (propose/vote/commit wire) remains as a fallback accelerator.
//! - Under each commit, expand the cert sub-DAG with a deterministic topo order.

use fd_types::{
    genesis_batch_hash, genesis_cert_id, AnchorCommit, Certificate, HashHex, NodeId, ProtocolConfig,
    TipCommit, TipProposal, TipVote,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Pure helper still used by tests: full cert-set topo (no tip voting).
pub fn order_certificates_only(
    certs: &HashMap<HashHex, Certificate>,
    hard_finality_h: u64,
) -> (Vec<HashHex>, BTreeSet<HashHex>) {
    let mut cfg = ProtocolConfig::default();
    cfg.hard_finality_h = hard_finality_h;
    let mut ord = OrderingEngine::new(cfg);
    // Simulate a single commit at the max tip for offline use.
    let tips = offline_tips(certs);
    for tip in tips {
        ord.expand_under_tip(certs, &tip);
    }
    (ord.ordered.clone(), ord.hard_final.clone())
}

fn offline_tips(certs: &HashMap<HashHex, Certificate>) -> Vec<HashHex> {
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for (bh, c) in certs {
        for p in &c.parents {
            children.entry(p.as_str()).or_default().push(bh.as_str());
        }
    }
    let mut tips: Vec<HashHex> = certs
        .keys()
        .filter(|bh| !children.contains_key(bh.as_str()))
        .cloned()
        .collect();
    tips.sort();
    if tips.is_empty() {
        vec![genesis_batch_hash()]
    } else {
        tips
    }
}

// ---------------------------------------------------------------------------
// Bullshark (DAG-derived commits — primary ordering path)
// ---------------------------------------------------------------------------

/// Classic BFT f for n validators: floor((n-1)/3).
pub fn bft_f(n: usize) -> usize {
    n.saturating_sub(1) / 3
}

/// Support needed to commit an anchor: f+1 distinct next-round creators.
pub fn bullshark_support_needed(n: usize) -> usize {
    bft_f(n).saturating_add(1).max(1)
}

/// Deterministic multipack tie-breaker for ordering decisions that used to
/// depend on mesh receipt order or wall-clock timestamp alone.
///
/// Batches are multi-tx (no single `account_id` on the cert). Option A maps
/// naturally onto **creator** (account-sharded packer chain) + **round**
/// (height within that chain): group by creator, prefer highest ready round,
/// iterate creators in lexicographic order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BullsharkTiebreaker {
    /// `(causal_round, creator, round, batch_hash)` — multipack-safe, no wall-clock.
    #[default]
    CreatorLexHighestRound,
    /// Legacy expand sort: `(causal_round, timestamp, batch_hash)`.
    TimestampThenHash,
}

impl BullsharkTiebreaker {
    /// Parse harness / env values (`ACCOUNT_ID_LEXICOGRAPHIC`, `CREATOR_LEX`, …).
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "" | "CREATOR_LEX" | "CREATOR_LEX_HIGHEST_ROUND" | "ACCOUNT_ID_LEXICOGRAPHIC"
            | "ACCOUNT_ID" | "OPTION_A" => Self::CreatorLexHighestRound,
            "TIMESTAMP" | "TIMESTAMP_THEN_HASH" | "LEGACY" => Self::TimestampThenHash,
            _ => Self::CreatorLexHighestRound,
        }
    }

    pub fn from_env() -> Self {
        std::env::var("BULLSHARK_TIEBREAKER")
            .map(|v| Self::parse(&v))
            .unwrap_or_default()
    }
}

/// Full Bullshark commit engine over certified batches.
///
/// ## Commit rule (every-round / multi-lane sparse DA)
/// Classic Bullshark only anchors **even** rounds. Multi-lane DA is often shallow
/// (rounds 1–2 only), so even-only left order to Tip-BFT and flakes.
///
/// **Every-round rule** (odd + even — “two Bullsharks” as one engine):
/// - For **every** round `r ≥ 1`, pick an anchor cert at that round
/// - A certificate in round `r+1` that parents the anchor is a **vote**
/// - Commit when ≥ **f+1** distinct UNL supporters (creator + signers on r+1)
/// - When committing, also ride the DAG: earlier supported anchors that are
///   ancestors of this commit (any parity)
///
/// Leader schedule: even rounds keep classic wave index; odd rounds use
/// `(r-1) % n`. Sparse DA still falls back to lowest hash in the round.
///
/// ## Multipack (Option A)
/// Concurrent packers produce multiple certified vertices per round. Anchor
/// selection stays leader-based; **expand** uses a deterministic creator-lex
/// ready-set so all honest nodes with the same cert view pick the same order
/// (mesh receipt order never decides).
#[derive(Debug, Clone)]
pub struct BullsharkEngine {
    /// Rounds (odd or even) whose anchors are committed.
    pub committed_rounds: BTreeSet<u64>,
    pub anchors: Vec<AnchorCommit>,
    pub commit_ids: BTreeSet<HashHex>,
    /// Last committed anchor batch hash (genesis until first commit).
    pub last_anchor: HashHex,
    /// Highest round we have considered (metrics; name kept for ABI).
    pub highest_even_seen: u64,
}

impl Default for BullsharkEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl BullsharkEngine {
    pub fn new() -> Self {
        Self {
            committed_rounds: BTreeSet::new(),
            anchors: Vec::new(),
            commit_ids: BTreeSet::new(),
            last_anchor: genesis_batch_hash(),
            highest_even_seen: 0,
        }
    }

    pub fn sorted_unl(unl: &BTreeMap<NodeId, f64>) -> Vec<NodeId> {
        unl.keys().cloned().collect()
    }

    /// Scheduled leader for round `r` (odd or even).
    /// Even rounds: classic wave `(r/2 - 1) % n`. Odd: `(r - 1) % n`.
    pub fn leader(unl_sorted: &[NodeId], round: u64) -> Option<&NodeId> {
        if unl_sorted.is_empty() || round == 0 {
            return None;
        }
        if round % 2 == 0 {
            let wave = (round / 2).saturating_sub(1) as usize;
            Some(&unl_sorted[wave % unl_sorted.len()])
        } else {
            let idx = (round.saturating_sub(1) as usize) % unl_sorted.len();
            Some(&unl_sorted[idx])
        }
    }

    /// Anchor certificate in `round` (odd or even).
    /// Prefer the scheduled leader’s cert; if absent (sparse DA), fall back to the
    /// lowest batch_hash among all certificates in that round so waves still close.
    pub fn find_anchor(
        certs: &HashMap<HashHex, Certificate>,
        unl_sorted: &[NodeId],
        round: u64,
    ) -> Option<(NodeId, HashHex)> {
        let scheduled = Self::leader(unl_sorted, round).cloned();
        let mut leader_certs: Vec<HashHex> = certs
            .iter()
            .filter(|(_, c)| {
                c.round == round && scheduled.as_ref().map(|l| l == &c.creator).unwrap_or(false)
            })
            .map(|(bh, _)| bh.clone())
            .collect();
        if !leader_certs.is_empty() {
            leader_certs.sort();
            let h = leader_certs[0].clone();
            let leader = scheduled.unwrap_or_else(|| "unknown".into());
            return Some((leader, h));
        }
        // Sparse-DA fallback: first cert in the round by hash.
        let mut any: Vec<(HashHex, NodeId)> = certs
            .iter()
            .filter(|(_, c)| c.round == round)
            .map(|(bh, c)| (bh.clone(), c.creator.clone()))
            .collect();
        if any.is_empty() {
            return None;
        }
        any.sort_by(|a, b| a.0.cmp(&b.0));
        Some((any[0].1.clone(), any[0].0.clone()))
    }

    /// Votes for an anchor: UNL members who either **created** or **signed** a
    /// round `round+1` certificate that parents the anchor.
    ///
    /// Classic Bullshark counts one vote per next-round vertex (one per validator).
    /// Our DA often has one vertex per round, but that cert already carries a UNL
    /// availability quorum — those signers count as support so f+1 is reachable.
    pub fn supporters(
        certs: &HashMap<HashHex, Certificate>,
        unl: &BTreeMap<NodeId, f64>,
        round: u64,
        anchor_hash: &str,
    ) -> Vec<NodeId> {
        let next = round.saturating_add(1);
        let mut voters: BTreeSet<NodeId> = BTreeSet::new();
        for c in certs.values() {
            if c.round != next {
                continue;
            }
            if !c.parents.iter().any(|p| p == anchor_hash) {
                continue;
            }
            if unl.contains_key(&c.creator) {
                voters.insert(c.creator.clone());
            }
            for s in &c.signers {
                if unl.contains_key(s) {
                    voters.insert(s.clone());
                }
            }
        }
        voters.into_iter().collect()
    }

    pub fn can_commit_anchor(
        certs: &HashMap<HashHex, Certificate>,
        unl: &BTreeMap<NodeId, f64>,
        unl_sorted: &[NodeId],
        round: u64,
    ) -> Option<(NodeId, HashHex, Vec<NodeId>)> {
        let (leader, anchor) = Self::find_anchor(certs, unl_sorted, round)?;
        let supporters = Self::supporters(certs, unl, round, &anchor);
        let need = bullshark_support_needed(unl_sorted.len());
        if supporters.len() >= need {
            Some((leader, anchor, supporters))
        } else {
            None
        }
    }

    /// Prefer this parent when packing: highest uncommitted supported-looking tip.
    pub fn preferred_parent(
        &self,
        certs: &HashMap<HashHex, Certificate>,
        unl_sorted: &[NodeId],
    ) -> Option<HashHex> {
        let max_round = certs.values().map(|c| c.round).max().unwrap_or(0);
        // Prefer highest round ≤ max that has an uncommitted anchor candidate.
        let mut r = max_round;
        while r >= 1 {
            if !self.committed_rounds.contains(&r) {
                if let Some((_, h)) = Self::find_anchor(certs, unl_sorted, r) {
                    return Some(h);
                }
            }
            r = r.saturating_sub(1);
            if r == 0 {
                break;
            }
        }
        None
    }

    /// Scan DAG for newly committable anchors; returns new commits in ascending round order.
    ///
    /// **Every-round:** try `r = 1..=max_round` (odd and even), not only even waves.
    pub fn try_commit_from_dag(
        &mut self,
        certs: &HashMap<HashHex, Certificate>,
        unl: &BTreeMap<NodeId, f64>,
    ) -> Vec<AnchorCommit> {
        let unl_sorted = Self::sorted_unl(unl);
        if unl_sorted.is_empty() {
            return Vec::new();
        }
        let max_round = certs.values().map(|c| c.round).max().unwrap_or(0);
        self.highest_even_seen = self.highest_even_seen.max(max_round);

        let mut newly: Vec<AnchorCommit> = Vec::new();

        // Direct commits: every round with f+1 support from r+1.
        let mut r = 1u64;
        while r <= max_round {
            if !self.committed_rounds.contains(&r) {
                if let Some((leader, anchor, supporters)) =
                    Self::can_commit_anchor(certs, unl, &unl_sorted, r)
                {
                    if let Some(c) = self.install_anchor(r, leader, anchor, supporters) {
                        newly.push(c);
                    }
                }
            }
            r += 1;
        }

        // Ride the DAG: for each committed anchor (high→low), commit earlier anchors
        // that are ancestors and already have f+1 support (any parity).
        let committed_list: Vec<(u64, HashHex)> = self
            .anchors
            .iter()
            .map(|a| (a.round, a.batch_hash.clone()))
            .collect();
        for (hi_round, hi_hash) in committed_list.iter().rev() {
            let mut earlier = 1u64;
            while earlier < *hi_round {
                if !self.committed_rounds.contains(&earlier) {
                    if let Some((leader, anchor, supporters)) =
                        Self::can_commit_anchor(certs, unl, &unl_sorted, earlier)
                    {
                        if is_ancestor(certs, &anchor, hi_hash) {
                            if let Some(c) =
                                self.install_anchor(earlier, leader, anchor, supporters)
                            {
                                newly.push(c);
                            }
                        }
                    }
                }
                earlier += 1;
            }
        }

        newly.sort_by_key(|c| c.round);
        newly
    }

    fn install_anchor(
        &mut self,
        round: u64,
        leader: NodeId,
        batch_hash: HashHex,
        supporters: Vec<NodeId>,
    ) -> Option<AnchorCommit> {
        if self.committed_rounds.contains(&round) {
            return None;
        }
        let commit_id =
            AnchorCommit::compute_id(round, &batch_hash, &leader, &supporters);
        if self.commit_ids.contains(&commit_id) {
            return None;
        }
        let c = AnchorCommit {
            round,
            leader,
            batch_hash: batch_hash.clone(),
            supporters,
            commit_id: commit_id.clone(),
        };
        self.commit_ids.insert(commit_id);
        self.committed_rounds.insert(round);
        self.last_anchor = batch_hash;
        self.anchors.push(c.clone());
        self.anchors.sort_by_key(|a| a.round);
        Some(c)
    }
}

// ---------------------------------------------------------------------------
// Tip-BFT state machine (optional fallback / accelerator)
// ---------------------------------------------------------------------------

/// Light tip-ordering protocol (HotStuff-style wire; secondary to Bullshark).
#[derive(Debug, Clone)]
pub struct TipBftEngine {
    pub next_round: u64,
    pub last_commit: HashHex,
    pub commits: Vec<TipCommit>,
    /// round -> accepted proposal
    pub proposals: BTreeMap<u64, TipProposal>,
    /// (round, tip_batch_hash) -> voter -> vote
    pub votes: BTreeMap<(u64, HashHex), BTreeMap<NodeId, TipVote>>,
    pub voted_rounds: BTreeSet<u64>,
    pub proposed_rounds: BTreeSet<u64>,
    /// Installed commit_ids (dedup).
    pub commit_ids: BTreeSet<HashHex>,
    /// View number for leader rotation when the primary is offline / not ledger-running.
    pub view: u64,
    /// Ticks without a proposal for `next_round` (triggers view change).
    pub idle_ticks: u64,
    /// Idle ticks before rotating leader view (small for fast recovery).
    pub view_change_after: u64,
}

impl Default for TipBftEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TipBftEngine {
    pub fn new() -> Self {
        Self {
            next_round: 1,
            last_commit: genesis_batch_hash(),
            commits: Vec::new(),
            proposals: BTreeMap::new(),
            votes: BTreeMap::new(),
            voted_rounds: BTreeSet::new(),
            proposed_rounds: BTreeSet::new(),
            commit_ids: BTreeSet::new(),
            view: 0,
            idle_ticks: 0,
            view_change_after: 2,
        }
    }

    /// Deterministic round leader: sorted UNL, index `(round-1+view) % n`.
    pub fn leader(unl_sorted: &[NodeId], round: u64, view: u64) -> Option<&NodeId> {
        if unl_sorted.is_empty() {
            return None;
        }
        let idx = (round.saturating_sub(1) as usize).wrapping_add(view as usize) % unl_sorted.len();
        Some(&unl_sorted[idx])
    }

    /// Drop an open proposal that is not the current canonical tip (stale early propose).
    pub fn invalidate_stale_proposal(&mut self, certs: &HashMap<HashHex, Certificate>) {
        let round = self.next_round;
        let Some(prop) = self.proposals.get(&round).cloned() else {
            return;
        };
        let Some(canonical) = Self::pick_tip(certs, &self.last_commit) else {
            return;
        };
        if prop.tip_batch_hash == canonical {
            return;
        }
        // Not yet committed — free the round so we can re-propose the canonical tip.
        self.proposals.remove(&round);
        self.proposed_rounds.remove(&round);
        self.voted_rounds.remove(&round);
        self.votes.retain(|(r, _), _| *r != round);
        self.idle_ticks = 0;
    }

    /// Bump view when no proposal has arrived (offline primary / DA-only leader).
    pub fn maybe_view_change(&mut self) {
        let round = self.next_round;
        if self.proposals.contains_key(&round) {
            self.idle_ticks = 0;
            return;
        }
        self.idle_ticks = self.idle_ticks.saturating_add(1);
        if self.idle_ticks >= self.view_change_after {
            self.view = self.view.saturating_add(1);
            self.idle_ticks = 0;
            // Allow a new primary to propose this same round.
            self.proposed_rounds.remove(&round);
        }
    }

    pub fn sorted_unl(unl: &BTreeMap<NodeId, f64>) -> Vec<NodeId> {
        unl.keys().cloned().collect()
    }

    /// Pick a certified tip that extends `last_commit` (prefer highest causal round, then hash).
    pub fn pick_tip(
        certs: &HashMap<HashHex, Certificate>,
        last_commit: &str,
    ) -> Option<HashHex> {
        if !certs.contains_key(last_commit) && last_commit != genesis_batch_hash() {
            return None;
        }
        // Candidates: certified nodes that have last_commit as ancestor (or are last_commit's
        // descendants). Prefer tips (no certified child) with max round.
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        for (bh, c) in certs {
            for p in &c.parents {
                children.entry(p.as_str()).or_default().push(bh.as_str());
            }
        }
        let mut best: Option<(u64, HashHex)> = None;
        for (bh, c) in certs {
            if bh == last_commit {
                continue;
            }
            if !is_ancestor(certs, last_commit, bh) {
                continue;
            }
            let has_cert_child = children
                .get(bh.as_str())
                .map(|kids| kids.iter().any(|k| certs.contains_key(*k)))
                .unwrap_or(false);
            if has_cert_child {
                continue;
            }
            let score = (c.round, bh.clone());
            if best.as_ref().map(|b| score > *b).unwrap_or(true) {
                best = Some(score);
            }
        }
        best.map(|(_, h)| h)
    }

    /// Current-view leader proposes the certified tip for `next_round`.
    pub fn try_propose(
        &mut self,
        local_id: &str,
        unl_sorted: &[NodeId],
        certs: &HashMap<HashHex, Certificate>,
        sign: impl FnOnce(&str) -> Option<String>,
    ) -> Option<TipProposal> {
        let round = self.next_round;
        if self.proposals.contains_key(&round) {
            return None;
        }
        if self.proposed_rounds.contains(&round) {
            return None;
        }
        let leader = Self::leader(unl_sorted, round, self.view)?;
        if leader != local_id {
            return None;
        }
        let tip = Self::pick_tip(certs, &self.last_commit)?;
        if tip == self.last_commit {
            return None;
        }
        if !certs.contains_key(&tip) {
            return None;
        }
        let mut prop = TipProposal {
            round,
            tip_batch_hash: tip,
            parent_commit: self.last_commit.clone(),
            proposer: local_id.into(),
            signature: String::new(),
        };
        let msg = prop.sign_message();
        prop.signature = sign(&msg)?;
        self.proposed_rounds.insert(round);
        self.proposals.insert(round, prop.clone());
        self.idle_ticks = 0;
        Some(prop)
    }

    /// Accept a remote (or local) proposal; return a vote if we should vote.
    ///
    /// Accepts the scheduled leader for the **current view**, or any prior view's
    /// leader for this round (catch-up / late primary). First valid proposal wins.
    pub fn on_proposal(
        &mut self,
        prop: TipProposal,
        local_id: &str,
        unl_sorted: &[NodeId],
        unl: &BTreeMap<NodeId, f64>,
        certs: &HashMap<HashHex, Certificate>,
        verify: impl FnOnce(&str, &str, &str) -> bool,
        sign: impl FnOnce(&str) -> Option<String>,
    ) -> Option<TipVote> {
        if prop.round != self.next_round {
            return None;
        }
        if prop.parent_commit != self.last_commit {
            return None;
        }
        if !unl.contains_key(&prop.proposer) {
            return None;
        }
        // Already locked on a different *tip* this round (same tip / proposal_id is ok).
        if let Some(existing) = self.proposals.get(&prop.round) {
            if existing.proposal_id() != prop.proposal_id() {
                return None;
            }
            // Same tip from another leader — keep existing, still may need to vote.
        } else {
            // Proposer must be a valid leader for some view 0..=current+n (rotation window).
            let mut ok_leader = false;
            let n = unl_sorted.len().max(1) as u64;
            for v in 0..=self.view.saturating_add(n) {
                if Self::leader(unl_sorted, prop.round, v).map(|l| l == &prop.proposer).unwrap_or(false)
                {
                    ok_leader = true;
                    break;
                }
            }
            if !ok_leader {
                return None;
            }
        }
        if !certs.contains_key(&prop.tip_batch_hash) {
            return None;
        }
        if prop.tip_batch_hash == prop.parent_commit {
            return None;
        }
        if !is_ancestor(certs, &prop.parent_commit, &prop.tip_batch_hash) {
            return None;
        }
        // Prefer canonical tip: reject proposals for a non-canonical tip when we already
        // know a better certified tip (prevents split-brain after late certs arrive).
        if let Some(canonical) = Self::pick_tip(certs, &self.last_commit) {
            if prop.tip_batch_hash != canonical {
                return None;
            }
        }
        if !verify(&prop.proposer, &prop.sign_message(), &prop.signature) {
            return None;
        }
        self.proposals.insert(prop.round, prop.clone());
        self.idle_ticks = 0;

        if self.voted_rounds.contains(&prop.round) {
            return None;
        }
        // Any ledger-running UNL member may vote (including leader).
        if !unl.contains_key(local_id) {
            return None;
        }
        let mut vote = TipVote {
            round: prop.round,
            tip_batch_hash: prop.tip_batch_hash.clone(),
            proposal_id: prop.proposal_id(),
            voter: local_id.into(),
            signature: String::new(),
        };
        let msg = vote.sign_message();
        vote.signature = sign(&msg)?;
        self.voted_rounds.insert(prop.round);
        self.record_vote(vote.clone());
        Some(vote)
    }

    pub fn record_vote(&mut self, vote: TipVote) {
        self.votes
            .entry((vote.round, vote.tip_batch_hash.clone()))
            .or_default()
            .insert(vote.voter.clone(), vote);
    }

    /// Ingest a vote; if quorum reached, produce TipCommit.
    pub fn on_vote(
        &mut self,
        vote: TipVote,
        unl: &BTreeMap<NodeId, f64>,
        threshold: f64,
        verify: impl FnOnce(&str, &str, &str) -> bool,
    ) -> Option<TipCommit> {
        if vote.round != self.next_round {
            return None;
        }
        if !unl.contains_key(&vote.voter) {
            return None;
        }
        let prop = self.proposals.get(&vote.round)?;
        if prop.tip_batch_hash != vote.tip_batch_hash || prop.proposal_id() != vote.proposal_id {
            return None;
        }
        if !verify(&vote.voter, &vote.sign_message(), &vote.signature) {
            return None;
        }
        self.record_vote(vote);
        self.try_finalize_round(unl, threshold)
    }

    /// Public finalize check (re-run after late votes / catch-up).
    pub fn try_finalize_round(
        &mut self,
        unl: &BTreeMap<NodeId, f64>,
        threshold: f64,
    ) -> Option<TipCommit> {
        let round = self.next_round;
        let prop = self.proposals.get(&round)?.clone();
        let votes = self
            .votes
            .get(&(round, prop.tip_batch_hash.clone()))?;
        let mut weight = 0.0;
        let mut voters: Vec<NodeId> = Vec::new();
        for (vid, _) in votes {
            if let Some(w) = unl.get(vid) {
                weight += *w;
                voters.push(vid.clone());
            }
        }
        if weight + 1e-12 < threshold {
            return None;
        }
        voters.sort();
        let commit = TipCommit {
            round,
            tip_batch_hash: prop.tip_batch_hash.clone(),
            parent_commit: prop.parent_commit.clone(),
            proposal_id: prop.proposal_id(),
            voters: voters.clone(),
            commit_id: TipCommit::compute_id(
                round,
                &prop.tip_batch_hash,
                &prop.parent_commit,
                &prop.proposal_id(),
                &voters,
            ),
        };
        self.install_commit(commit.clone())
    }

    /// Install a commit (local quorum or remote gossip). Advances next_round.
    pub fn install_commit(&mut self, commit: TipCommit) -> Option<TipCommit> {
        if self.commit_ids.contains(&commit.commit_id) {
            return None;
        }
        if commit.round != self.next_round {
            // Allow catch-up if sequential and parent matches.
            if commit.round < self.next_round {
                return None;
            }
            if commit.round > self.next_round {
                return None;
            }
        }
        if commit.parent_commit != self.last_commit {
            return None;
        }
        self.commit_ids.insert(commit.commit_id.clone());
        self.last_commit = commit.tip_batch_hash.clone();
        self.next_round = commit.round.saturating_add(1);
        self.commits.push(commit.clone());
        // Fresh view for the next round.
        self.view = 0;
        self.idle_ticks = 0;
        // Drop vote bookkeeping for past rounds (keep proposals lightly).
        self.votes.retain(|(r, _), _| *r >= self.next_round);
        Some(commit)
    }

    /// Accept remote commit if valid under local UNL weights (voters listed).
    pub fn on_remote_commit(
        &mut self,
        commit: TipCommit,
        unl: &BTreeMap<NodeId, f64>,
        threshold: f64,
    ) -> Option<TipCommit> {
        if self.commit_ids.contains(&commit.commit_id) {
            return None;
        }
        let mut weight = 0.0;
        for v in &commit.voters {
            if let Some(w) = unl.get(v) {
                weight += *w;
            }
        }
        if weight + 1e-12 < threshold {
            return None;
        }
        let expect = TipCommit::compute_id(
            commit.round,
            &commit.tip_batch_hash,
            &commit.parent_commit,
            &commit.proposal_id,
            &commit.voters,
        );
        if expect != commit.commit_id {
            return None;
        }
        self.install_commit(commit)
    }
}

/// True if `ancestor` is a (non-strict) ancestor of `node` in the cert parent DAG.
fn is_ancestor(
    certs: &HashMap<HashHex, Certificate>,
    ancestor: &str,
    node: &str,
) -> bool {
    if ancestor == node {
        return true;
    }
    let mut stack = vec![node.to_string()];
    let mut seen = BTreeSet::new();
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        if cur == ancestor {
            return true;
        }
        if let Some(c) = certs.get(&cur) {
            for p in &c.parents {
                stack.push(p.clone());
            }
        } else if cur == genesis_batch_hash() && ancestor == genesis_batch_hash() {
            return true;
        }
    }
    // genesis is ancestor of all certified nodes that eventually root to it
    if ancestor == genesis_batch_hash() {
        return certs.contains_key(node) || node == genesis_batch_hash();
    }
    false
}

// ---------------------------------------------------------------------------
// Ordering engine: expand under tip commits + hard finality
// ---------------------------------------------------------------------------

pub struct OrderingEngine {
    pub config: ProtocolConfig,
    pub ordered: Vec<HashHex>,
    pub ordered_set: BTreeSet<HashHex>,
    pub ordered_certs: Vec<HashHex>,
    pub hard_final: BTreeSet<HashHex>,
    /// Primary: DAG-derived Bullshark anchors.
    pub bullshark: BullsharkEngine,
    /// Secondary: tip propose/vote wire (optional accelerator).
    pub tip_bft: TipBftEngine,
    /// Multipack expand / ready-set tie-breaker (Option A default).
    pub tiebreaker: BullsharkTiebreaker,
}

impl OrderingEngine {
    pub fn new(config: ProtocolConfig) -> Self {
        let g = genesis_batch_hash();
        let mut ordered_set = BTreeSet::new();
        ordered_set.insert(g.clone());
        let mut hard_final = BTreeSet::new();
        hard_final.insert(g.clone());
        Self {
            config,
            ordered: vec![g],
            ordered_set,
            ordered_certs: vec![genesis_cert_id()],
            hard_final,
            bullshark: BullsharkEngine::new(),
            tip_bft: TipBftEngine::new(),
            tiebreaker: BullsharkTiebreaker::from_env(),
        }
    }

    /// True when every parent of `c` is already ordered (or is genesis / unknown
    /// non-cert parent treated as external — same rule as expand readiness).
    fn parents_all_ordered(
        &self,
        c: &Certificate,
        certs: &HashMap<HashHex, Certificate>,
    ) -> bool {
        c.parents.iter().all(|p| {
            if p == &genesis_batch_hash() || self.ordered_set.contains(p) {
                true
            } else if certs.contains_key(p) {
                false
            } else {
                // Missing parent cert: allow (same as expand_under_tip force path).
                true
            }
        })
    }

    /// Option A: among certified-but-not-yet-ordered batches whose parents are
    /// all ordered, pick **one tip per creator** — the highest `round` (then
    /// lowest `batch_hash`). Creators emit in BTreeMap (lexicographic) order.
    ///
    /// All honest validators with the same cert view compute the same list.
    /// Receipt order / timestamps never participate.
    pub fn select_ready_tips_per_creator(
        &self,
        certs: &HashMap<HashHex, Certificate>,
    ) -> Vec<HashHex> {
        let g = genesis_batch_hash();
        // creator -> (round, batch_hash) best candidate
        let mut best: BTreeMap<NodeId, (u64, HashHex)> = BTreeMap::new();
        for (bh, c) in certs {
            if bh == &g || self.ordered_set.contains(bh) {
                continue;
            }
            if !self.parents_all_ordered(c, certs) {
                continue;
            }
            let creator = c.creator.clone();
            let cand = (c.round, bh.clone());
            match best.get(&creator) {
                None => {
                    best.insert(creator, cand);
                }
                Some((r, h)) => {
                    // Prefer higher round; tie-break lower batch_hash (deterministic).
                    if cand.0 > *r || (cand.0 == *r && cand.1 < *h) {
                        best.insert(creator, cand);
                    }
                }
            }
        }
        best.into_values().map(|(_, h)| h).collect()
    }

    /// Expand every causally-ready certified batch not yet ordered, in
    /// deterministic creator-lex waves (Option A multipack liveness).
    ///
    /// Returns the number of newly ordered certificates. Safe to call after
    /// anchor expands: only parents-already-ordered vertices enter the queue,
    /// so total order stays a linear extension of the cert DAG.
    pub fn expand_ready_multipack(&mut self, certs: &HashMap<HashHex, Certificate>) -> usize {
        let mut total = 0usize;
        // Bound waves: at most one wave per remaining cert.
        let max_waves = certs.len().saturating_add(1);
        for _ in 0..max_waves {
            let tips = self.select_ready_tips_per_creator(certs);
            if tips.is_empty() {
                break;
            }
            let mut progressed = false;
            for tip in tips {
                let n = self.expand_under_tip(certs, &tip);
                if n > 0 {
                    total = total.saturating_add(n);
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        total
    }

    fn causal_round(
        &self,
        batch_hash: &str,
        certs: &HashMap<HashHex, Certificate>,
        memo: &mut BTreeMap<String, u64>,
    ) -> u64 {
        if let Some(r) = memo.get(batch_hash) {
            return *r;
        }
        if batch_hash == genesis_batch_hash() {
            memo.insert(batch_hash.to_string(), 0);
            return 0;
        }
        let Some(cert) = certs.get(batch_hash) else {
            memo.insert(batch_hash.to_string(), 0);
            return 0;
        };
        let r = if cert.parents.is_empty() {
            1
        } else {
            1 + cert
                .parents
                .iter()
                .map(|p| self.causal_round(p, certs, memo))
                .max()
                .unwrap_or(0)
        };
        memo.insert(batch_hash.to_string(), r);
        r
    }

    /// Expand ordered prefix to include all cert ancestors of `tip` not yet ordered.
    /// Deterministic topo: parents before children; tie-break depends on `tiebreaker`:
    /// - **CreatorLexHighestRound (Option A):** `(causal_round, creator, round, hash)`
    /// - **TimestampThenHash (legacy):** `(causal_round, timestamp, hash)`
    pub fn expand_under_tip(
        &mut self,
        certs: &HashMap<HashHex, Certificate>,
        tip: &str,
    ) -> usize {
        if tip == genesis_batch_hash() {
            return 0;
        }
        // Collect ancestors of tip (including tip) that are certified.
        let mut need: BTreeSet<HashHex> = BTreeSet::new();
        let mut stack = vec![tip.to_string()];
        while let Some(cur) = stack.pop() {
            if self.ordered_set.contains(&cur) {
                continue;
            }
            if !need.insert(cur.clone()) {
                continue;
            }
            if let Some(c) = certs.get(&cur) {
                for p in &c.parents {
                    if !self.ordered_set.contains(p) {
                        stack.push(p.clone());
                    }
                }
            }
        }
        if need.is_empty() {
            return 0;
        }

        let before = self.ordered.len();
        let mut remaining = need;
        let mut memo = BTreeMap::new();

        while !remaining.is_empty() {
            let mut ready: Vec<HashHex> = remaining
                .iter()
                .filter(|bh| {
                    let parents = certs
                        .get(*bh)
                        .map(|c| c.parents.clone())
                        .unwrap_or_default();
                    parents.iter().all(|p| {
                        self.ordered_set.contains(p) || !certs.contains_key(p)
                    }) && (parents.is_empty()
                        || parents
                            .iter()
                            .all(|p| self.ordered_set.contains(p) || !certs.contains_key(p)))
                        && parents.iter().all(|p| {
                            if certs.contains_key(p) {
                                self.ordered_set.contains(p)
                            } else {
                                true
                            }
                        })
                })
                .cloned()
                .collect();

            if ready.is_empty() {
                // Break cycles / missing parents: force lowest hash.
                ready = remaining.iter().cloned().collect();
            }

            ready.sort_by(|a, b| {
                let ca = certs.get(a);
                let cb = certs.get(b);
                let ra = self.causal_round(a, certs, &mut memo);
                let rb = self.causal_round(b, certs, &mut memo);
                match self.tiebreaker {
                    BullsharkTiebreaker::CreatorLexHighestRound => {
                        let cra = ca.map(|c| c.creator.as_str()).unwrap_or("");
                        let crb = cb.map(|c| c.creator.as_str()).unwrap_or("");
                        let round_a = ca.map(|c| c.round).unwrap_or(0);
                        let round_b = cb.map(|c| c.round).unwrap_or(0);
                        ra.cmp(&rb)
                            .then_with(|| cra.cmp(crb))
                            .then_with(|| round_a.cmp(&round_b))
                            .then_with(|| a.cmp(b))
                    }
                    BullsharkTiebreaker::TimestampThenHash => {
                        let ta = ca.map(|c| c.timestamp).unwrap_or(0.0);
                        let tb = cb.map(|c| c.timestamp).unwrap_or(0.0);
                        ra.cmp(&rb)
                            .then_with(|| {
                                ta.partial_cmp(&tb).unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .then_with(|| a.cmp(b))
                    }
                }
            });

            let mut committed = false;
            for h in &ready {
                let parents = certs.get(h).map(|c| c.parents.clone()).unwrap_or_default();
                if !parents.is_empty()
                    && !parents.iter().all(|p| {
                        self.ordered_set.contains(p) || !certs.contains_key(p)
                    })
                {
                    continue;
                }
                if parents
                    .iter()
                    .any(|p| certs.contains_key(p) && !self.ordered_set.contains(p))
                {
                    continue;
                }
                self.ordered.push(h.clone());
                self.ordered_set.insert(h.clone());
                remaining.remove(h);
                committed = true;
            }
            if !committed {
                if let Some(h) = ready.first() {
                    self.ordered.push(h.clone());
                    self.ordered_set.insert(h.clone());
                    remaining.remove(h);
                }
            }
        }

        self.ordered_certs = self
            .ordered
            .iter()
            .map(|bh| {
                certs
                    .get(bh)
                    .map(|c| c.cert_id.clone())
                    .unwrap_or_else(|| bh.clone())
            })
            .collect();
        self.update_hard_finality();
        self.ordered.len().saturating_sub(before)
    }

    /// Legacy full recompute (tests / offline). Prefer Bullshark + expand_under_tip.
    pub fn recompute(&mut self, certs: &HashMap<HashHex, Certificate>) {
        // Reset to genesis then expand every tip in causal order of max-round tips.
        let g = genesis_batch_hash();
        self.ordered = vec![g.clone()];
        self.ordered_set = BTreeSet::from([g]);
        self.ordered_certs = vec![genesis_cert_id()];
        let tips = offline_tips(certs);
        // Expand all certs by treating each unreached cert as a successive tip.
        let mut all: Vec<_> = certs.keys().cloned().collect();
        all.sort_by(|a, b| {
            let mut memo = BTreeMap::new();
            let ra = self.causal_round(a, certs, &mut memo);
            let rb = self.causal_round(b, certs, &mut memo);
            ra.cmp(&rb).then_with(|| a.cmp(b))
        });
        for bh in all {
            if !self.ordered_set.contains(&bh) {
                self.expand_under_tip(certs, &bh);
            }
        }
        for t in tips {
            self.expand_under_tip(certs, &t);
        }
        self.update_hard_finality();
    }

    fn update_hard_finality(&mut self) {
        let n = self.ordered.len();
        let h = self.config.hard_finality_h as usize;
        self.hard_final.clear();
        self.hard_final.insert(genesis_batch_hash());
        for (i, bh) in self.ordered.iter().enumerate() {
            if n.saturating_sub(i + 1) >= h {
                self.hard_final.insert(bh.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fd_types::Certificate;

    fn cert(bh: &str, parents: Vec<&str>, ts: f64, round: u64) -> Certificate {
        cert_by(bh, "val1", parents, ts, round)
    }

    fn cert_by(bh: &str, creator: &str, parents: Vec<&str>, ts: f64, round: u64) -> Certificate {
        Certificate {
            cert_id: format!("c-{bh}"),
            batch_hash: bh.into(),
            signers: vec![creator.into()],
            parents: parents.into_iter().map(String::from).collect(),
            creator: creator.into(),
            timestamp: ts,
            round,
        }
    }

    fn genesis_cert() -> Certificate {
        Certificate {
            cert_id: genesis_cert_id(),
            batch_hash: genesis_batch_hash(),
            signers: vec![],
            parents: vec![],
            creator: "genesis".into(),
            timestamp: 0.0,
            round: 0,
        }
    }

    #[test]
    fn causal_order_and_hard_final() {
        let g = genesis_batch_hash();
        let mut certs = HashMap::new();
        certs.insert(g.clone(), genesis_cert());
        certs.insert("b1".into(), cert("b1", vec![&g], 1.0, 1));
        certs.insert("b2".into(), cert("b2", vec![&g], 2.0, 1));
        certs.insert("b3".into(), cert("b3", vec!["b1", "b2"], 3.0, 2));

        let mut cfg = ProtocolConfig::default();
        cfg.hard_finality_h = 1;
        let mut ord = OrderingEngine::new(cfg);
        ord.expand_under_tip(&certs, "b3");

        let i1 = ord.ordered.iter().position(|x| x == "b1").unwrap();
        let i2 = ord.ordered.iter().position(|x| x == "b2").unwrap();
        let i3 = ord.ordered.iter().position(|x| x == "b3").unwrap();
        assert!(i1 < i3 && i2 < i3);
        assert!(i1 < i2);
        assert!(ord.hard_final.contains(&g));
        assert!(!ord.hard_final.contains("b3"));
    }

    #[test]
    fn tip_bft_single_validator_commits() {
        let g = genesis_batch_hash();
        let mut certs = HashMap::new();
        certs.insert(g.clone(), genesis_cert());
        certs.insert("b1".into(), cert("b1", vec![&g], 1.0, 1));

        let mut unl = BTreeMap::new();
        unl.insert("val1".into(), 1.0);
        let sorted = TipBftEngine::sorted_unl(&unl);
        let mut tip = TipBftEngine::new();

        let prop = tip
            .try_propose("val1", &sorted, &certs, |_| Some("sig-p".into()))
            .expect("propose");
        assert_eq!(prop.tip_batch_hash, "b1");

        let vote = tip
            .on_proposal(
                prop,
                "val1",
                &sorted,
                &unl,
                &certs,
                |_, _, _| true,
                |_| Some("sig-v".into()),
            )
            .expect("vote");
        let commit = tip
            .on_vote(vote, &unl, 0.6, |_, _, _| true)
            .expect("commit");
        assert_eq!(commit.tip_batch_hash, "b1");
        assert_eq!(tip.next_round, 2);
        assert_eq!(tip.last_commit, "b1");
    }

    #[test]
    fn tip_bft_requires_quorum() {
        let g = genesis_batch_hash();
        let mut certs = HashMap::new();
        certs.insert(g.clone(), genesis_cert());
        certs.insert("b1".into(), cert("b1", vec![&g], 1.0, 1));

        let mut unl = BTreeMap::new();
        for i in 1..=5 {
            unl.insert(format!("val{i}"), 0.2);
        }
        let sorted = TipBftEngine::sorted_unl(&unl);
        let mut tip = TipBftEngine::new();
        // round 1 view 0 leader = val1
        let prop = tip
            .try_propose("val1", &sorted, &certs, |_| Some("p".into()))
            .unwrap();
        // two votes (0.4) < 0.6
        for v in ["val1", "val2"] {
            let vote = TipVote {
                round: 1,
                tip_batch_hash: "b1".into(),
                proposal_id: prop.proposal_id(),
                voter: v.into(),
                signature: "s".into(),
            };
            tip.proposals.insert(1, prop.clone());
            assert!(tip.on_vote(vote, &unl, 0.6, |_, _, _| true).is_none());
        }
        // third vote commits
        let vote = TipVote {
            round: 1,
            tip_batch_hash: "b1".into(),
            proposal_id: prop.proposal_id(),
            voter: "val3".into(),
            signature: "s".into(),
        };
        assert!(tip.on_vote(vote, &unl, 0.6, |_, _, _| true).is_some());
    }

    #[test]
    fn tip_bft_view_change_rotates_leader() {
        let mut unl = BTreeMap::new();
        for i in 1..=5 {
            unl.insert(format!("val{i}"), 0.2);
        }
        let sorted = TipBftEngine::sorted_unl(&unl);
        let mut tip = TipBftEngine::new();
        tip.view_change_after = 2;
        assert_eq!(TipBftEngine::leader(&sorted, 1, 0).unwrap(), "val1");
        tip.maybe_view_change();
        tip.maybe_view_change();
        assert_eq!(tip.view, 1);
        assert_eq!(TipBftEngine::leader(&sorted, 1, tip.view).unwrap(), "val2");
    }

    #[test]
    fn bullshark_even_leader_and_f_plus_1_commit() {
        // n=4 → f=1 → need 2 supporters
        let mut unl = BTreeMap::new();
        for i in 1..=4 {
            unl.insert(format!("val{i}"), 0.25);
        }
        let sorted = BullsharkEngine::sorted_unl(&unl);
        assert_eq!(bullshark_support_needed(4), 2);
        // round 2 leader = val1 (wave 0)
        assert_eq!(BullsharkEngine::leader(&sorted, 2).unwrap(), "val1");

        let g = genesis_batch_hash();
        let mut certs = HashMap::new();
        certs.insert(g.clone(), genesis_cert());
        // round 1 (odd): any
        certs.insert("r1".into(), cert_by("r1", "val2", vec![&g], 1.0, 1));
        // round 2 anchor: val1
        certs.insert(
            "anchor".into(),
            cert_by("anchor", "val1", vec!["r1"], 2.0, 2),
        );
        // round 3: one cert that parents the anchor, signed by 2 UNL members
        let mut s1 = cert_by("s1", "val2", vec!["anchor"], 3.0, 3);
        s1.signers = vec!["val2".into(), "val3".into()];
        certs.insert("s1".into(), s1);

        let mut bs = BullsharkEngine::new();
        let commits = bs.try_commit_from_dag(&certs, &unl);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].round, 2);
        assert_eq!(commits[0].batch_hash, "anchor");
        assert!(commits[0].supporters.len() >= 2);
        assert!(bs.committed_rounds.contains(&2));
    }

    #[test]
    fn bullshark_no_commit_without_support() {
        let mut unl = BTreeMap::new();
        for i in 1..=4 {
            unl.insert(format!("val{i}"), 0.25);
        }
        let g = genesis_batch_hash();
        let mut certs = HashMap::new();
        certs.insert(g.clone(), genesis_cert());
        certs.insert("r1".into(), cert_by("r1", "val2", vec![&g], 1.0, 1));
        certs.insert(
            "anchor".into(),
            cert_by("anchor", "val1", vec!["r1"], 2.0, 2),
        );
        // only one UNL name on the supporting cert (creator=signer val2)
        let mut s1 = cert_by("s1", "val2", vec!["anchor"], 3.0, 3);
        s1.signers = vec!["val2".into()];
        certs.insert("s1".into(), s1);
        let mut bs = BullsharkEngine::new();
        assert!(bs.try_commit_from_dag(&certs, &unl).is_empty());
    }

    #[test]
    fn bullshark_single_node_f0_one_support() {
        // n=1 → f=0 → need 1 supporter
        let mut unl = BTreeMap::new();
        unl.insert("val1".into(), 1.0);
        assert_eq!(bullshark_support_needed(1), 1);
        let g = genesis_batch_hash();
        let mut certs = HashMap::new();
        certs.insert(g.clone(), genesis_cert());
        certs.insert("r1".into(), cert_by("r1", "val1", vec![&g], 1.0, 1));
        certs.insert(
            "a2".into(),
            cert_by("a2", "val1", vec!["r1"], 2.0, 2),
        );
        certs.insert(
            "s3".into(),
            cert_by("s3", "val1", vec!["a2"], 3.0, 3),
        );
        let mut bs = BullsharkEngine::new();
        let c = bs.try_commit_from_dag(&certs, &unl);
        // Every-round: can commit r1 (via r2) and r2 (via r3).
        assert!(c.len() >= 1);
        assert!(bs.committed_rounds.contains(&2) || c.iter().any(|x| x.batch_hash == "a2"));
        // r2 anchor still commits
        assert!(
            bs.committed_rounds.contains(&2),
            "expected round-2 commit, got {:?}",
            bs.committed_rounds
        );
    }

    /// Multi-lane sparse: only rounds 1+2 — every-round commits odd r1 via r2 support.
    /// Classic even-only would try r=2 needing r3 and stay idle (Tip-BFT flake class).
    #[test]
    fn bullshark_odd_round_commits_with_next_support() {
        let mut unl = BTreeMap::new();
        for i in 1..=3 {
            unl.insert(format!("val{i}"), 1.0 / 3.0);
        }
        // n=3 → f=0 → need 1 supporter
        assert_eq!(bullshark_support_needed(3), 1);
        let g = genesis_batch_hash();
        let mut certs = HashMap::new();
        certs.insert(g.clone(), genesis_cert());
        let mut r1 = cert_by("r1", "val2", vec![&g], 1.0, 1);
        r1.signers = vec!["val1".into(), "val2".into(), "val3".into()];
        certs.insert("r1".into(), r1);
        // round 2 parents r1 — this is the support vote for odd anchor r1
        let mut r2 = cert_by("r2", "val3", vec!["r1"], 2.0, 2);
        r2.signers = vec!["val2".into(), "val3".into()];
        certs.insert("r2".into(), r2);

        let mut bs = BullsharkEngine::new();
        let commits = bs.try_commit_from_dag(&certs, &unl);
        assert!(
            !commits.is_empty(),
            "every-round should commit r1 when r2 supports; got empty"
        );
        assert!(
            bs.committed_rounds.contains(&1),
            "round 1 (odd) must commit, got {:?}",
            bs.committed_rounds
        );
        assert_eq!(
            commits.iter().find(|c| c.round == 1).map(|c| c.batch_hash.as_str()),
            Some("r1")
        );
        // No r3 → round 2 cannot commit yet
        assert!(!bs.committed_rounds.contains(&2));
    }

    #[test]
    fn option_a_ready_tips_per_creator_is_deterministic() {
        let g = genesis_batch_hash();
        let mut certs = HashMap::new();
        certs.insert(g.clone(), genesis_cert());
        // Concurrent multipack: three creators, each with two heights off genesis.
        certs.insert("a1".into(), cert_by("a1", "val1", vec![&g], 1.0, 1));
        certs.insert("a2".into(), cert_by("a2", "val1", vec!["a1"], 2.0, 2));
        certs.insert("b1".into(), cert_by("b1", "val2", vec![&g], 9.0, 1)); // late ts
        certs.insert("b2".into(), cert_by("b2", "val2", vec!["b1"], 0.5, 2)); // early ts
        certs.insert("c1".into(), cert_by("c1", "val3", vec![&g], 5.0, 1));

        let mut cfg = ProtocolConfig::default();
        cfg.hard_finality_h = 1;
        let mut ord = OrderingEngine::new(cfg);
        ord.tiebreaker = BullsharkTiebreaker::CreatorLexHighestRound;

        // Only genesis ordered → each creator's height-1 is ready (height-2 parents not ordered).
        let tips = ord.select_ready_tips_per_creator(&certs);
        assert_eq!(
            tips,
            vec!["a1".to_string(), "b1".to_string(), "c1".to_string()],
            "creators lex, one highest-ready each (all height 1)"
        );

        // After ordering a1/b1/c1, highest ready should advance to a2/b2 (c has no height 2).
        for t in ["a1", "b1", "c1"] {
            ord.expand_under_tip(&certs, t);
        }
        let tips2 = ord.select_ready_tips_per_creator(&certs);
        assert_eq!(tips2, vec!["a2".to_string(), "b2".to_string()]);
    }

    #[test]
    fn option_a_multipack_expand_ignores_receipt_and_timestamp_order() {
        let g = genesis_batch_hash();
        let mut certs = HashMap::new();
        certs.insert(g.clone(), genesis_cert());
        // val2 has earlier timestamp but must not reorder ahead of val1 by receipt/ts alone.
        certs.insert("late_val1".into(), cert_by("late_val1", "val1", vec![&g], 100.0, 1));
        certs.insert("early_val2".into(), cert_by("early_val2", "val2", vec![&g], 1.0, 1));

        let mut cfg = ProtocolConfig::default();
        cfg.hard_finality_h = 1;
        let mut ord_a = OrderingEngine::new(cfg.clone());
        ord_a.tiebreaker = BullsharkTiebreaker::CreatorLexHighestRound;
        let n = ord_a.expand_ready_multipack(&certs);
        assert_eq!(n, 2);
        let i1 = ord_a.ordered.iter().position(|x| x == "late_val1").unwrap();
        let i2 = ord_a.ordered.iter().position(|x| x == "early_val2").unwrap();
        assert!(i1 < i2, "creator lex: val1 before val2 regardless of timestamp");

        // Two engines that "see" certs inserted in opposite orders still agree.
        let mut certs_rev = HashMap::new();
        certs_rev.insert(g.clone(), genesis_cert());
        certs_rev.insert("early_val2".into(), cert_by("early_val2", "val2", vec![&g], 1.0, 1));
        certs_rev.insert("late_val1".into(), cert_by("late_val1", "val1", vec![&g], 100.0, 1));
        let mut ord_b = OrderingEngine::new(cfg);
        ord_b.tiebreaker = BullsharkTiebreaker::CreatorLexHighestRound;
        ord_b.expand_ready_multipack(&certs_rev);
        assert_eq!(ord_a.ordered, ord_b.ordered);
    }

    #[test]
    fn tiebreaker_parse_env_aliases() {
        assert_eq!(
            BullsharkTiebreaker::parse("ACCOUNT_ID_LEXICOGRAPHIC"),
            BullsharkTiebreaker::CreatorLexHighestRound
        );
        assert_eq!(
            BullsharkTiebreaker::parse("LEGACY"),
            BullsharkTiebreaker::TimestampThenHash
        );
    }
}
