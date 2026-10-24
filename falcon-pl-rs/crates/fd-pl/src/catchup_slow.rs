//! Joiner-side NeedLedgers peer rotation (v2.9.72). Node-local routing only:
//! no wire, state, or consensus effect.
//!
//! Root cause of the 2.9.69–2.9.71 fresh-install stall: the ledger slow list
//! was the same map as the snapshot slow list. A snapshot source skipped as
//! slow during the download (10 min) was therefore also banned for residual
//! NeedLedgers, and once the serving archive (v3) missed one window it was
//! banned too. Only peers whose RAM window no longer held the snapshot heights
//! were left; they reply nothing to a NeedLedgers they cannot serve, so the
//! tip sat at the snapshot height until a restart emptied the map.
//!
//! This list is separate, strikes back off exponentially per peer, every
//! entry is dropped once the tip has been stuck for `STUCK_RESET_MS`
//! (retry everyone), and it is cleared on snapshot adopt and on progress.
//!
//! v2.9.75: the 2.9.74 fresh-install stall after the snapshot adopt. The plan
//! always puts the hubs (v1, v3: one saturated home uplink) first. The 30 s
//! stuck reset un-benched them and progress cleared their marks, so every
//! pull went back to v1/v3 and the fast seats served only in the gaps
//! (minutes per 13-ledger batch, lag grew). Now:
//! - the peer whose reply last advanced the tip is asked first
//!   (`PREFER_MS` after its last batch, dropped once it is benched);
//! - strikes are remembered for `REPEAT_WINDOW_MS` across progress/reset; a
//!   peer with `REPEAT_STRIKES` there is "repeat-slow": the stuck reset does
//!   not un-bench it and `route` asks it last.

use std::collections::HashMap;

pub const BASE_SKIP_MS: u64 = 10_000;
pub const MAX_SKIP_MS: u64 = 60_000;
/// Tip unmoved this long with every peer tried → forget all marks.
pub const STUCK_RESET_MS: u64 = 30_000;
/// v2.9.75: keep asking the peer that served the last advancing batch.
pub const PREFER_MS: u64 = 60_000;
/// v2.9.75: strike history window and the count that makes a peer repeat-slow.
pub const REPEAT_WINDOW_MS: u64 = 600_000;
pub const REPEAT_STRIKES: usize = 2;

/// Key for a NeedLedgers target: the peer id, or `lane#<idx>` while the
/// outbound lane has no id (v2.9.73). An empty key was never timed out or
/// skipped, so every pull went to the same silent untagged seed lane.
pub fn target_key(id: &str, idx: usize) -> String {
    if id.is_empty() {
        format!("lane#{idx}")
    } else {
        id.to_string()
    }
}

#[derive(Debug, Default, Clone)]
pub struct LedgerSlowList {
    /// peer → (skip until ms, strikes)
    marks: HashMap<String, (u64, u32)>,
    /// Current target, send ms, tip at send.
    req: Option<(String, u64, u64)>,
    /// Last ms the tip advanced (or the list was reset).
    last_progress_ms: Option<u64>,
    last_tip: u64,
    /// v2.9.75: peer whose reply last advanced the tip, and when.
    preferred: Option<(String, u64)>,
    /// v2.9.75: strike times per peer (survives progress and reset).
    offenses: HashMap<String, Vec<u64>>,
}

impl LedgerSlowList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything (snapshot adopt / restart-equivalent).
    /// Strike history and the preferred peer are kept: they describe the
    /// links, not the pull.
    pub fn reset(&mut self, now: u64, tip: u64) {
        self.marks.clear();
        self.req = None;
        self.last_progress_ms = Some(now);
        self.last_tip = tip;
    }

    /// Called before choosing a NeedLedgers target. Returns the peer newly
    /// marked slow (for logging) and its skip in ms.
    pub fn tick(&mut self, now: u64, tip: u64, judge_ms: u64) -> Option<(String, u64)> {
        if tip > self.last_tip || self.last_progress_ms.is_none() {
            if tip > self.last_tip {
                // Progress: whoever served is good; drop strikes.
                self.marks.clear();
                // Directed pulls go to one target at a time: credit it.
                if let Some((id, _, _)) = self.req.as_ref() {
                    self.preferred = Some((id.clone(), now));
                }
            }
            self.last_tip = tip;
            self.last_progress_ms = Some(now);
            self.req = None;
            return None;
        }
        let mut out = None;
        if let Some((id, sent, at)) = self.req.clone() {
            if tip <= at && now.saturating_sub(sent) >= judge_ms {
                let strikes = self.marks.get(&id).map(|m| m.1).unwrap_or(0).saturating_add(1);
                let skip = BASE_SKIP_MS
                    .saturating_mul(1u64 << (strikes - 1).min(6))
                    .min(MAX_SKIP_MS);
                self.marks.insert(id.clone(), (now + skip, strikes));
                let h = self.offenses.entry(id.clone()).or_default();
                h.retain(|t| now.saturating_sub(*t) < REPEAT_WINDOW_MS);
                h.push(now);
                if self.preferred.as_ref().is_some_and(|p| p.0 == id) {
                    self.preferred = None;
                }
                self.req = None;
                out = Some((id, skip));
            }
        }
        let since = now.saturating_sub(self.last_progress_ms.unwrap_or(now));
        if since >= STUCK_RESET_MS && !self.marks.is_empty() {
            // Stuck with peers on the bench: give everybody another go
            // (keep strikes so a repeat offender backs off longer).
            // v2.9.75: a repeat-slow peer stays benched until its own skip
            // ends; un-benching it here sent every pull back to v1/v3.
            let repeat: Vec<String> = self
                .marks
                .keys()
                .filter(|id| self.repeat_slow(id, now))
                .cloned()
                .collect();
            for (id, m) in self.marks.iter_mut() {
                if !repeat.contains(id) {
                    m.0 = 0;
                }
            }
            self.last_progress_ms = Some(now);
        }
        out
    }

    /// v2.9.75: `REPEAT_STRIKES` or more strikes in the last `REPEAT_WINDOW_MS`.
    pub fn repeat_slow(&self, id: &str, now: u64) -> bool {
        self.offenses.get(id).is_some_and(|h| {
            h.iter().filter(|t| now.saturating_sub(**t) < REPEAT_WINDOW_MS).count() >= REPEAT_STRIKES
        })
    }

    /// v2.9.75: the preferred peer while fresh and not benched.
    pub fn preferred(&self, now: u64) -> Option<&str> {
        self.preferred
            .as_ref()
            .filter(|(id, at)| now.saturating_sub(*at) < PREFER_MS && !self.is_slow(id, now))
            .map(|(id, _)| id.as_str())
    }

    /// v2.9.75: order to try `candidates` (positions into it): the preferred
    /// peer first, then the given order, repeat-slow peers last. Nothing is
    /// dropped; benching stays `skip_set`'s job.
    pub fn route(&self, candidates: &[String], now: u64) -> Vec<usize> {
        let pref = self.preferred(now);
        let mut first = Vec::new();
        let mut mid = Vec::new();
        let mut last = Vec::new();
        for (k, c) in candidates.iter().enumerate() {
            if pref.is_some_and(|p| p == c) && first.is_empty() {
                first.push(k);
            } else if !c.is_empty() && self.repeat_slow(c, now) {
                last.push(k);
            } else {
                mid.push(k);
            }
        }
        first.extend(mid);
        first.extend(last);
        first
    }

    pub fn is_slow(&self, id: &str, now: u64) -> bool {
        self.marks.get(id).is_some_and(|m| m.0 > now)
    }

    /// Slow ids among `candidates`, or empty if that would exclude them all.
    pub fn skip_set(&self, candidates: &[String], now: u64) -> Vec<String> {
        let slow: Vec<String> = candidates
            .iter()
            .filter(|c| !c.is_empty() && self.is_slow(c, now))
            .cloned()
            .collect();
        if candidates.iter().any(|c| !c.is_empty() && !slow.contains(c)) {
            slow
        } else {
            Vec::new()
        }
    }

    /// Record the peer a NeedLedgers was just sent to.
    pub fn note_sent(&mut self, id: &str, now: u64, tip: u64) {
        if id.is_empty() {
            return;
        }
        if self.req.as_ref().is_some_and(|r| r.0 == id) {
            return;
        }
        self.req = Some((id.to_string(), now, tip));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn silent_peer_rotates_then_recovers_without_restart() {
        let mut l = LedgerSlowList::new();
        let cands = ids(&["v3", "v1", "v6"]);
        let tip = 626_508;
        let j = 5_000; // judge window
        l.reset(0, tip); // snapshot adopt
        // v3 is asked and never answers.
        l.note_sent("v3", 1_000, tip);
        assert_eq!(l.tick(6_000, tip, j), Some(("v3".into(), BASE_SKIP_MS)));
        assert_eq!(l.skip_set(&cands, 6_000), ids(&["v3"]));
        // v1 also silent → both skipped, v6 still asked.
        l.note_sent("v1", 6_000, tip);
        assert!(l.tick(11_000, tip, j).is_some());
        assert_eq!(l.skip_set(&cands, 11_000), ids(&["v3", "v1"]));
        // v6 silent too: the candidate set never empties.
        l.note_sent("v6", 11_000, tip);
        assert!(l.tick(15_999, tip, j).is_none());
        assert!(l.tick(16_000, tip, j).is_some());
        assert!(l.skip_set(&cands, 15_999).len() < cands.len());
        // Stuck 30 s after adopt → everyone is retried.
        assert!(l.tick(30_000, tip, j).is_none());
        assert!(l.skip_set(&cands, 30_000).is_empty());
        // Second strike backs off longer.
        l.note_sent("v3", 30_000, tip);
        assert_eq!(l.tick(35_000, tip, j), Some(("v3".into(), 2 * BASE_SKIP_MS)));
        // Progress clears all marks.
        l.tick(36_000, tip + 64, j);
        assert!(l.skip_set(&cands, 36_000).is_empty());
    }

    #[test]
    fn snapshot_slow_marks_do_not_leak_and_reset_clears() {
        let mut l = LedgerSlowList::new();
        l.reset(0, 10);
        l.note_sent("v1", 0, 10);
        l.tick(10_000, 10, 10_000);
        assert!(l.is_slow("v1", 10_001));
        l.reset(10_001, 10);
        assert!(!l.is_slow("v1", 10_002));
    }

    #[test]
    fn backoff_caps() {
        let mut l = LedgerSlowList::new();
        l.reset(0, 1);
        let mut now = 0;
        let mut last = 0;
        for _ in 0..10 {
            l.note_sent("v3", now, 1);
            now += 10_000;
            if let Some((_, s)) = l.tick(now, 1, 10_000) {
                last = s;
            }
        }
        assert_eq!(last, MAX_SKIP_MS);
    }

    /// 557604-style stall: after the snapshot adopt every outbound lane is an
    /// untagged seed relay port except v3. v3 is benched, then the first
    /// silent untagged lane must be benched too and the next lane tried,
    /// without a restart.
    #[test]
    fn untagged_silent_lane_is_rotated() {
        let mut l = LedgerSlowList::new();
        let tip = 557_604;
        l.reset(500, tip);
        let peers = [(0usize, "v3"), (1, ""), (2, ""), (3, "")];
        let cands: Vec<String> = peers.iter().map(|(i, id)| target_key(id, *i)).collect();
        assert_eq!(cands, ids(&["v3", "lane#1", "lane#2", "lane#3"]));
        // Picks the first non-slow candidate, like fan_catchup.
        let pick = |l: &LedgerSlowList, now| {
            let slow = l.skip_set(&cands, now);
            cands.iter().find(|c| !slow.contains(c)).cloned().unwrap()
        };
        let mut now = 1_000;
        l.tick(now, tip, 2_000);
        let mut asked = Vec::new();
        for _ in 0..4 {
            let t = pick(&l, now);
            l.note_sent(&t, now, tip);
            asked.push(t);
            now += 2_000;
            l.tick(now, tip, 2_000);
        }
        // Every lane got a turn; lane#1 did not absorb every pull.
        assert_eq!(asked, ids(&["v3", "lane#1", "lane#2", "lane#3"]));
        // lane#3 serves → tip moves → marks cleared.
        l.tick(now + 1, tip + 64, 2_000);
        assert!(l.skip_set(&cands, now + 1).is_empty());
    }

    /// Pick like fan_catchup (v2.9.75): route order, minus benched peers.
    fn pick(l: &LedgerSlowList, cands: &[String], now: u64) -> String {
        let slow = l.skip_set(cands, now);
        l.route(cands, now)
            .into_iter()
            .map(|k| cands[k].clone())
            .find(|c| !slow.contains(c))
            .unwrap()
    }

    #[test]
    fn peer_that_advanced_tip_is_asked_first() {
        let mut l = LedgerSlowList::new();
        let cands = ids(&["v1", "v3", "v6", "v12"]);
        let j = 10_000;
        let mut tip = 641_346;
        l.reset(0, tip);
        assert_eq!(pick(&l, &cands, 0), "v1");
        // v12 is asked (v1 busy) and serves.
        l.note_sent("v12", 1_000, tip);
        tip += 13;
        l.tick(5_000, tip, j);
        assert_eq!(l.preferred(5_000), Some("v12"));
        assert_eq!(pick(&l, &cands, 5_000), "v12");
        // Still first while it keeps serving.
        l.note_sent("v12", 5_000, tip);
        tip += 13;
        l.tick(9_000, tip, j);
        assert_eq!(pick(&l, &cands, 9_000), "v12");
        // Goes stale without a new batch.
        assert_eq!(l.preferred(9_000 + PREFER_MS), None);
    }

    #[test]
    fn benched_preferred_is_dropped() {
        let mut l = LedgerSlowList::new();
        let cands = ids(&["v1", "v3", "v6"]);
        let j = 10_000;
        let tip = 100;
        l.reset(0, tip);
        l.note_sent("v6", 0, tip);
        l.tick(1_000, tip + 10, j);
        assert_eq!(l.preferred(1_000), Some("v6"));
        l.note_sent("v6", 1_000, tip + 10);
        assert!(l.tick(11_000, tip + 10, j).is_some());
        assert_eq!(l.preferred(11_000), None);
        assert_eq!(pick(&l, &cands, 11_000), "v1");
    }

    /// The 2.9.74 E2E run-3 stall: v1/v3 never answer, v6/v12 do. Before
    /// 2.9.75 the 30 s reset and progress put v1/v3 back at the front every
    /// cycle. Now after two strikes each they go last and stay benched.
    #[test]
    fn repeat_slow_hubs_not_put_back_in_front() {
        let mut l = LedgerSlowList::new();
        let cands = ids(&["v1", "v3", "v6", "v12"]);
        let j = 10_000;
        let mut tip = 641_346;
        let mut now = 0;
        l.reset(now, tip);
        let mut served = 0;
        let mut asked_hub = 0;
        for _ in 0..200 {
            let t = pick(&l, &cands, now);
            l.note_sent(&t, now, tip);
            if t == "v6" || t == "v12" {
                // fast seats answer in 2 s
                now += 2_000;
                tip += 13;
                served += 1;
            } else {
                asked_hub += 1;
                now += j;
            }
            l.tick(now, tip, j);
            now += 500;
            l.tick(now, tip, j);
        }
        assert!(l.repeat_slow("v1", now) && l.repeat_slow("v3", now));
        // Each hub gets its two strikes, then (almost) never again.
        assert!(asked_hub <= 6, "hubs asked {asked_hub} times");
        assert!(served >= 190, "served {served}");
        assert_eq!(l.route(&cands, now)[2..], [0, 1]);
    }

    #[test]
    fn stuck_reset_keeps_repeat_slow_benched_but_never_empties() {
        let mut l = LedgerSlowList::new();
        let cands = ids(&["v1", "v3", "v6"]);
        let j = 10_000;
        let tip = 5;
        l.reset(0, tip);
        // v3: one strike at 12 s (skip until 22 s, then strike again later).
        // v1: strikes at 10 s and 30 s → repeat-slow, skip until 50 s.
        l.note_sent("v1", 0, tip);
        l.tick(10_000, tip, j);
        l.note_sent("v3", 10_000, tip);
        assert!(l.tick(20_000, tip, j).is_some());
        l.note_sent("v1", 20_000, tip);
        // 30 s stuck: the strike lands and the stuck reset fires in one tick.
        assert_eq!(l.tick(30_000, tip, j), Some(("v1".into(), 2 * BASE_SKIP_MS)));
        assert!(l.repeat_slow("v1", 30_000));
        // Pre-2.9.75 this un-benched v1 too. Now only the one-strike peer.
        assert!(l.is_slow("v1", 30_001));
        assert!(!l.is_slow("v3", 30_001));
        assert_eq!(pick(&l, &cands, 30_001), "v3");
        // Only repeat-slow peers left → they are still asked (never empty).
        assert!(l.skip_set(&ids(&["v1"]), 45_000).is_empty());
    }

    #[test]
    fn repeat_slow_ages_out_and_reset_keeps_history() {
        let mut l = LedgerSlowList::new();
        l.reset(0, 1);
        l.note_sent("v3", 0, 1);
        l.tick(10_000, 1, 10_000);
        l.note_sent("v3", 30_000, 1);
        l.tick(40_000, 1, 10_000);
        assert!(l.repeat_slow("v3", 40_000));
        l.reset(41_000, 1); // snapshot adopt
        assert!(l.repeat_slow("v3", 41_000));
        assert!(!l.repeat_slow("v3", 10_000 + REPEAT_WINDOW_MS));
        // Progress also keeps it.
        l.tick(50_000, 2, 10_000);
        assert!(l.repeat_slow("v3", 50_000));
    }

    #[test]
    fn empty_key_is_never_recorded() {
        let mut l = LedgerSlowList::new();
        l.reset(500, 5);
        l.note_sent("", 500, 5);
        assert_eq!(l.tick(20_000, 5, 10_000), None);
    }
}
