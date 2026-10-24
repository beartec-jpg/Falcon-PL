//! Architecture contract (PR1): frozen terms, metrics, gap classes, role profiles.
//!
//! This module is intentionally documentation + constants + pure classification.
//! Later PRs change behavior without renaming these contracts.

use serde::{Deserialize, Serialize};

/// Authoritative product architecture string (IMPLEMENTATION_GUIDE §4 / PR1).
pub const ARCHITECTURE_STRING: &str =
    "Narwhal-style multipack DA (logical-global mempool + conf-pool north star) \
     + UNL certificates + Bullshark ordering over finalized certs";

/// Master guide path relative to monorepo root.
pub const IMPLEMENTATION_GUIDE_PATH: &str = "docs/IMPLEMENTATION_GUIDE.md";

// ---------------------------------------------------------------------------
// Role taxonomy (deployment profiles — distinct from NodeRole da/ledger split)
// ---------------------------------------------------------------------------

/// Operator-facing node profile. Orthogonal to [`crate::NodeRole`] (DA vs ledger ticks).
///
/// - [`NodeRole`](crate::NodeRole) = process capability (pack / order+exec / both).
/// - [`NodeProfile`] = deployment intent (validator vs archival vs RPC front-end).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NodeProfile {
    /// Consensus participant: packs lane, signs, orders, executes (default dig profile).
    #[default]
    Validator,
    /// Long retention, replay, history; may run ledger without packing.
    Archival,
    /// Client-facing admission + query; may not pack or order.
    Rpc,
}

impl NodeProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Validator => "validator",
            Self::Archival => "archival",
            Self::Rpc => "rpc",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "validator" | "val" | "consensus" => Some(Self::Validator),
            "archival" | "archive" | "history" => Some(Self::Archival),
            "rpc" | "api" | "gateway" | "frontend" => Some(Self::Rpc),
            _ => None,
        }
    }

    /// Suggested [`crate::NodeRole`] for this profile (not forced; CLI may override).
    pub fn suggested_node_role(self) -> crate::NodeRole {
        match self {
            Self::Validator => crate::NodeRole::Full,
            Self::Archival => crate::NodeRole::OrderExec,
            // PR6: RPC front-end still runs DA for admit/owner-route; may skip pack
            // via single_packer + role override in ops.
            Self::Rpc => crate::NodeRole::Full,
        }
    }

    /// Subsystems this profile is expected to run (PR6 operator contract).
    /// Honest nodes still verify; this is enablement for duties/load, not skip-checks.
    pub fn enabled_subsystems(self) -> ProfileSubsystems {
        match self {
            Self::Validator => ProfileSubsystems {
                admit: true,
                pack: true,
                sign: true,
                order: true,
                execute: true,
                deep_store: false,
                public_rpc: false,
            },
            Self::Archival => ProfileSubsystems {
                admit: false,
                pack: false,
                sign: true, // may still verify/sign for mesh honesty
                order: true,
                execute: true,
                deep_store: true,
                public_rpc: false,
            },
            Self::Rpc => ProfileSubsystems {
                admit: true,
                pack: false, // client-facing; do not take pack load
                sign: false,
                order: false,
                execute: false,
                deep_store: false,
                public_rpc: true,
            },
        }
    }
}

/// Which duties a deployment profile enables (PR6). Verification remains required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSubsystems {
    pub admit: bool,
    pub pack: bool,
    pub sign: bool,
    pub order: bool,
    pub execute: bool,
    pub deep_store: bool,
    pub public_rpc: bool,
}

impl ProfileSubsystems {
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "admit": self.admit,
            "pack": self.pack,
            "sign": self.sign,
            "order": self.order,
            "execute": self.execute,
            "deep_store": self.deep_store,
            "public_rpc": self.public_rpc,
        })
    }
}

// ---------------------------------------------------------------------------
// Batch DA gap class (dig freeze forensics — frozen names)
// ---------------------------------------------------------------------------

/// Cross-node DA gap classification used by digs and `batch_status`.
///
/// Digs historically emitted string labels; these variants freeze the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BatchDaGapClass {
    /// No peer has the body.
    NoBodyOnPeers,
    /// Peers have body but none have signed yet.
    PeerHasBodyNoSig,
    /// ≥1 peer signed; creator still nsigs≤1 / not certified (Mode B).
    PeerSignedCreatorMissingForeign,
    /// Creator has foreign weight or multi-sig / certified / healthy.
    HasForeignOrOk,
    /// Local certified.
    Certified,
    /// Insufficient data to classify.
    Unknown,
}

impl BatchDaGapClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoBodyOnPeers => "NO_BODY_ON_PEERS",
            Self::PeerHasBodyNoSig => "PEER_HAS_BODY_NO_SIG",
            Self::PeerSignedCreatorMissingForeign => "PEER_SIGNED_CREATOR_MISSING_FOREIGN",
            Self::HasForeignOrOk => "HAS_FOREIGN_OR_OK",
            Self::Certified => "CERTIFIED",
            Self::Unknown => "UNKNOWN",
        }
    }

    /// Classify from dig-style cross-check inputs (creator view + peer aggregates).
    pub fn classify(
        creator_has_body: bool,
        creator_certified: bool,
        creator_nsigs: usize,
        any_peer_has_body: bool,
        any_peer_signed: bool,
    ) -> Self {
        if creator_certified {
            return Self::Certified;
        }
        if !any_peer_has_body {
            return Self::NoBodyOnPeers;
        }
        if !any_peer_signed {
            return Self::PeerHasBodyNoSig;
        }
        // Peers signed.
        if creator_has_body && creator_nsigs <= 1 {
            return Self::PeerSignedCreatorMissingForeign;
        }
        if creator_has_body {
            return Self::HasForeignOrOk;
        }
        // Peers signed but creator lacks body — still Mode B family.
        Self::PeerSignedCreatorMissingForeign
    }
}

// ---------------------------------------------------------------------------
// Frozen metric keys (digs + operators must not rename without PR1 bump)
// ---------------------------------------------------------------------------

/// Metric visibility tier (PR1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricTier {
    /// Safe for public explorers / coarse dashboards.
    Public,
    /// Operator-facing status (default dig + ops).
    Operator,
    /// Debug / forensic only.
    Debug,
}

/// One frozen metrics contract entry.
#[derive(Debug, Clone, Copy)]
pub struct MetricKey {
    pub name: &'static str,
    pub tier: MetricTier,
    pub description: &'static str,
}

/// Keys digs and operators rely on. Renaming requires a metrics-compat PR.
pub const FROZEN_METRIC_KEYS: &[MetricKey] = &[
    // Core liveness
    MetricKey {
        name: "executed_txs",
        tier: MetricTier::Public,
        description: "Count of executed transactions",
    },
    MetricKey {
        name: "mempool",
        tier: MetricTier::Operator,
        description: "Local mempool size",
    },
    MetricKey {
        name: "open_needs_body_seed",
        tier: MetricTier::Operator,
        description: "Own open has zero foreign UNL sigs",
    },
    MetricKey {
        name: "open_conf",
        tier: MetricTier::Operator,
        description: "Own open batch confirmation snapshot",
    },
    // Mempool / admit
    MetricKey {
        name: "admit_reasons",
        tier: MetricTier::Operator,
        description: "Admit outcome histogram",
    },
    MetricKey {
        name: "admit_exec_lag",
        tier: MetricTier::Operator,
        description: "local_admits_ok - executed_txs",
    },
    MetricKey {
        name: "shard_flow",
        tier: MetricTier::Operator,
        description: "Ownership / directed routing counters",
    },
    MetricKey {
        name: "pack_skip_reasons",
        tier: MetricTier::Debug,
        description: "create_batch / conf-pool reason counters",
    },
    // Conf-pool / Mode B
    MetricKey {
        name: "foreign_sig_merged_from_sig",
        tier: MetricTier::Operator,
        description: "Foreign sigs merged via PartialSignature path",
    },
    MetricKey {
        name: "foreign_sig_merged_from_body",
        tier: MetricTier::Operator,
        description: "Foreign sigs merged via body piggyback",
    },
    MetricKey {
        name: "foreign_sig_parked",
        tier: MetricTier::Debug,
        description: "Sigs parked awaiting body",
    },
    MetricKey {
        name: "foreign_sig_applied_parked",
        tier: MetricTier::Debug,
        description: "Parked sigs applied after body",
    },
    MetricKey {
        name: "on_remote_sig_calls",
        tier: MetricTier::Debug,
        description: "on_remote_sig entry count",
    },
    MetricKey {
        name: "sig_reannounce_to_creator",
        tier: MetricTier::Operator,
        description: "Directed sig reannounce to batch creator",
    },
    MetricKey {
        name: "try_certify_attempts",
        tier: MetricTier::Debug,
        description: "try_certify attempts",
    },
    MetricKey {
        name: "try_certify_ok",
        tier: MetricTier::Operator,
        description: "try_certify successes",
    },
    MetricKey {
        name: "force_open_try_certify",
        tier: MetricTier::Debug,
        description: "Forced open cert attempts",
    },
    MetricKey {
        name: "conf_pool",
        tier: MetricTier::Operator,
        description: "Conf-pool JSON snapshot",
    },
    // Parked certs
    MetricKey {
        name: "mesh_diag",
        tier: MetricTier::Debug,
        description: "Pending certs / mesh lag diagnostics",
    },
    // Frozen pack_skip reason strings used by digs
    MetricKey {
        name: "pack_skip:conf_pool_signed",
        tier: MetricTier::Operator,
        description: "pack_skip_reasons.conf_pool_signed",
    },
    MetricKey {
        name: "pack_skip:conf_pool_sign_deferred_cap",
        tier: MetricTier::Debug,
        description: "Sign deferred by per-tick cap",
    },
    MetricKey {
        name: "pack_skip:sig_reannounce_to_creator",
        tier: MetricTier::Operator,
        description: "Creator-directed reannounce counter key",
    },
    MetricKey {
        name: "pack_skip:open_body_repool",
        tier: MetricTier::Operator,
        description: "Open body repool events",
    },
];

/// Dig reliability baselines (engineering targets, IMPLEMENTATION_GUIDE / dig suite).
/// Not marketing SLAs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigReliabilityBaseline {
    pub setup: &'static str,
    pub freeze_events_max: u32,
    pub peer_signed_gaps_max: u32,
    pub hold_tps_min: f64,
    pub notes: &'static str,
}

pub const DIG_BASELINES: &[DigReliabilityBaseline] = &[
    DigReliabilityBaseline {
        setup: "N3_localhost_p20",
        freeze_events_max: 0,
        peer_signed_gaps_max: 10,
        hold_tps_min: 15.0,
        notes: "Healthy multipack baseline",
    },
    DigReliabilityBaseline {
        setup: "N3_localhost_p40",
        freeze_events_max: 1,
        peer_signed_gaps_max: 10,
        hold_tps_min: 25.0,
        notes: "Mostly healthy high load",
    },
    DigReliabilityBaseline {
        setup: "N5_localhost_p40",
        freeze_events_max: 2,
        peer_signed_gaps_max: 10,
        hold_tps_min: 8.0,
        notes: "Mode B gate — current stack often FAILS (PEER_SIGNED 400+); target after global pools",
    },
    DigReliabilityBaseline {
        setup: "N3_dual_wan_p20",
        freeze_events_max: 0,
        peer_signed_gaps_max: 5,
        hold_tps_min: 6.0,
        notes: "Dual-site healthy",
    },
];

/// JSON contract blob for `architecture` RPC / digs.
pub fn architecture_contract_json() -> serde_json::Value {
    serde_json::json!({
        "architecture_string": ARCHITECTURE_STRING,
        "implementation_guide": IMPLEMENTATION_GUIDE_PATH,
        "node_profiles": ["validator", "archival", "rpc"],
        "profile_subsystems": {
            "validator": NodeProfile::Validator.enabled_subsystems().to_json(),
            "archival": NodeProfile::Archival.enabled_subsystems().to_json(),
            "rpc": NodeProfile::Rpc.enabled_subsystems().to_json(),
        },
        "startup_examples": {
            "validator": "falcon-dagd --node-profile validator --force-node-profile --role full",
            "archival": "falcon-dagd --node-profile archival --force-node-profile --role order-exec",
            "rpc": "falcon-dagd --node-profile rpc --force-node-profile --role full",
            "lab_full": "falcon-dagd --role full  # backward-compatible combined lab node",
        },
        "node_roles": ["da-worker", "order-exec", "full"],
        "batch_da_gap_classes": [
            BatchDaGapClass::NoBodyOnPeers.as_str(),
            BatchDaGapClass::PeerHasBodyNoSig.as_str(),
            BatchDaGapClass::PeerSignedCreatorMissingForeign.as_str(),
            BatchDaGapClass::HasForeignOrOk.as_str(),
            BatchDaGapClass::Certified.as_str(),
            BatchDaGapClass::Unknown.as_str(),
        ],
        "frozen_metric_keys": FROZEN_METRIC_KEYS.iter().map(|k| {
            serde_json::json!({
                "name": k.name,
                "tier": match k.tier {
                    MetricTier::Public => "public",
                    MetricTier::Operator => "operator",
                    MetricTier::Debug => "debug",
                },
                "description": k.description,
            })
        }).collect::<Vec<_>>(),
        "dig_baselines": DIG_BASELINES.iter().map(|b| {
            serde_json::json!({
                "setup": b.setup,
                "freeze_events_max": b.freeze_events_max,
                "peer_signed_gaps_max": b.peer_signed_gaps_max,
                "hold_tps_min": b.hold_tps_min,
                "notes": b.notes,
            })
        }).collect::<Vec<_>>(),
        "terms": {
            "mempool": "Pending txs awaiting pack (logical-global north star; process-local today)",
            "conf_pool": "Open batches awaiting UNL weight (logical-global north star; process-local today)",
            "certificate": "Soft final DA attestation when UNL weight ≥ unl_threshold",
            "soft_final": "Tx in a certified batch body",
            "hard_final": "Tx ordered under hard_finality_h commit depth",
            "lane": "Multipack packing domain owned by one UNL validator",
            "PEER_SIGNED_CREATOR_MISSING_FOREIGN": BatchDaGapClass::PeerSignedCreatorMissingForeign.as_str(),
        },
        "subsystem_boundaries": {
            "admission": "fd-mempool + engine submit/admit; not ordering",
            "conf_pool": "fd-node conf_pool; scheduled sign of open bodies; not Bullshark",
            "dag": "fd-dag bodies, partial sigs, certificates",
            "ordering": "fd-order Bullshark over finalized certs",
            "execution": "fd-exec apply in ordered sequence",
            "store": "fd-store snapshots / durability",
            "rpc": "fd-node JSON-RPC surface only",
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_class_mode_b() {
        let g = BatchDaGapClass::classify(
            true,  // creator body
            false, // not cert
            1,     // only self sig
            true,  // peer body
            true,  // peer signed
        );
        assert_eq!(g, BatchDaGapClass::PeerSignedCreatorMissingForeign);
        assert_eq!(g.as_str(), "PEER_SIGNED_CREATOR_MISSING_FOREIGN");
    }

    #[test]
    fn gap_class_no_body() {
        let g = BatchDaGapClass::classify(true, false, 1, false, false);
        assert_eq!(g, BatchDaGapClass::NoBodyOnPeers);
    }

    #[test]
    fn gap_class_body_no_sig() {
        let g = BatchDaGapClass::classify(true, false, 1, true, false);
        assert_eq!(g, BatchDaGapClass::PeerHasBodyNoSig);
    }

    #[test]
    fn frozen_metric_keys_unique_and_stable() {
        let mut names: Vec<&str> = FROZEN_METRIC_KEYS.iter().map(|k| k.name).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n, "frozen metric keys must be unique");
        // Dig suite hard dependencies
        assert!(FROZEN_METRIC_KEYS.iter().any(|k| k.name == "executed_txs"));
        assert!(FROZEN_METRIC_KEYS
            .iter()
            .any(|k| k.name == "foreign_sig_merged_from_sig"));
        assert!(FROZEN_METRIC_KEYS
            .iter()
            .any(|k| k.name == "open_needs_body_seed"));
    }

    #[test]
    fn architecture_json_has_guide_and_terms() {
        let v = architecture_contract_json();
        assert_eq!(
            v["implementation_guide"],
            serde_json::json!(IMPLEMENTATION_GUIDE_PATH)
        );
        assert!(v["architecture_string"].as_str().unwrap().contains("multipack"));
        assert!(v["terms"]["conf_pool"].is_string());
    }

    #[test]
    fn node_profile_parse() {
        assert_eq!(NodeProfile::parse("validator"), Some(NodeProfile::Validator));
        assert_eq!(NodeProfile::parse("rpc"), Some(NodeProfile::Rpc));
        assert_eq!(NodeProfile::parse("archival"), Some(NodeProfile::Archival));
    }
}
