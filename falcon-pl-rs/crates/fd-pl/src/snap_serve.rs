// Copyright (c) 2025-2026 BEARTEC. All rights reserved. See LICENSE.
//! Snapshot serve helpers: push budget under catch-up backpressure, and
//! separate stalled-source vs non-server skip lists (no shared wire_from).

use std::collections::HashMap;

/// Leave headroom on large catch-up reply lanes for Offer / SnapshotNo / hole-fill.
pub const SNAP_PUSH_RESERVE_SLOTS: usize = 32;
/// Conservative: 256 KiB chunk → ~512 KiB hex JSON, PIECE_THRESHOLD 64 KiB → ≥8 pieces; pad to 12.
pub const SNAP_EST_PIECES_PER_CHUNK: usize = 12;

/// How many SnapshotChunk messages to bulk-push after Offer given free lane slots.
///
/// Outbound catch lanes are only [`CATCH_QUEUE_CAP`]=32. A flat reserve of 32
/// made `usable==0` on every fresh session, so NeedSnapshot sent Offer with
/// zero chunks and joiners stalled. Reserve shrinks on small lanes so at least
/// one chunk can ship when the lane can hold its pieces.
pub fn snap_push_budget(free_slots: usize, max_push: u32) -> u32 {
    if max_push == 0 || free_slots == 0 {
        return 0;
    }
    let reserve = SNAP_PUSH_RESERVE_SLOTS.min(free_slots.saturating_sub(SNAP_EST_PIECES_PER_CHUNK));
    let usable = free_slots.saturating_sub(reserve);
    if usable < SNAP_EST_PIECES_PER_CHUNK {
        return u32::from(free_slots >= SNAP_EST_PIECES_PER_CHUNK);
    }
    let by_cap = usable / SNAP_EST_PIECES_PER_CHUNK;
    (by_cap as u32).min(max_push)
}

/// Expire both maps; return the union of ids still skipped.
pub fn snap_skip_ids(
    stalled: &mut HashMap<String, u64>,
    nonserve: &mut HashMap<String, u64>,
    now_ms: u64,
) -> Vec<String> {
    stalled.retain(|_, until| *until > now_ms);
    nonserve.retain(|_, until| *until > now_ms);
    let mut out: Vec<String> = stalled.keys().cloned().collect();
    for id in nonserve.keys() {
        if !out.iter().any(|x| x == id) {
            out.push(id.clone());
        }
    }
    out
}

/// All archives/hubs are slow: clear **only** non-server marks so rotation can
/// try other seats; **retain** stalled archive/hub cooldown (do not `clear()` stalled).
pub fn all_slow_forget_nonserve_only(
    stalled: &mut HashMap<String, u64>,
    nonserve: &mut HashMap<String, u64>,
    now_ms: u64,
) -> Vec<String> {
    stalled.retain(|_, until| *until > now_ms);
    nonserve.clear();
    stalled.keys().cloned().collect()
}

/// True if `id` is in the skip set.
pub fn is_snap_skipped(skip: &[String], id: &str) -> bool {
    !id.is_empty() && skip.iter().any(|s| s == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_budget_small_outbound_lane_allows_one_chunk() {
        // CATCH_QUEUE_CAP=32: must not reserve the entire lane.
        assert_eq!(snap_push_budget(10, 48), 0); // cannot fit one chunk's pieces
        assert_eq!(snap_push_budget(12, 48), 1);
        assert_eq!(snap_push_budget(32, 48), 1);
        assert_eq!(snap_push_budget(44, 48), 1); // reserve shrinks; usable=12
        assert_eq!(snap_push_budget(32 + 12 * 48, 48), 48);
        assert_eq!(snap_push_budget(10_000, 48), 48);
    }

    #[test]
    fn all_slow_keeps_stalled_clears_nonserve() {
        let mut stalled = HashMap::new();
        stalled.insert("v1".into(), 9_999_999);
        let mut nonserve = HashMap::new();
        nonserve.insert("v6".into(), 9_999_999);
        nonserve.insert("v10".into(), 9_999_999);
        let skip = all_slow_forget_nonserve_only(&mut stalled, &mut nonserve, 1_000);
        assert!(nonserve.is_empty());
        assert_eq!(stalled.get("v1"), Some(&9_999_999));
        assert_eq!(skip, vec!["v1".to_string()]);
    }

    #[test]
    fn snap_skip_union_expires() {
        let mut stalled = HashMap::new();
        stalled.insert("v1".into(), 50);
        let mut nonserve = HashMap::new();
        nonserve.insert("v6".into(), 200);
        let skip = snap_skip_ids(&mut stalled, &mut nonserve, 100);
        assert!(!is_snap_skipped(&skip, "v1")); // expired
        assert!(is_snap_skipped(&skip, "v6"));
    }
}
