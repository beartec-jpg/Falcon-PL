//! Task separation façades (PR8): same-machine first, network extraction later.
//!
//! Subsystems communicate through named handoffs. Today these are in-process
//! method calls on [`NodeEngine`] / [`EngineParts`]. Future IPC maps:
//!
//! | Façade method | Sync today | Future transport |
//! |---------------|------------|------------------|
//! | `AdmissionFacade::submit` | sync | RPC / admit microservice |
//! | `ConfPoolFacade::check_step` | sync | DA worker message |
//! | `DagFacade::ingest_batch` | sync | body store API |
//! | `OrderFacade::advance` | sync | order-exec service |
//! | `ExecFacade::apply_ordered` | sync | exec worker |
//! | `StoreFacade::snapshot` | sync | cold store service |
//!
//! Lock order (parts path): meta → mempool → dag → ordering → execution.
//! Mutation windows stay short; broadcasts happen outside locks when possible.

use crate::engine::{AdmitSource, NodeEngine, SubmitResult};
use crate::conf_pool::DutyPhase;
use fd_types::{Batch, Transaction};
use serde_json::Value;

/// Cross-subsystem span keys for tracing a tx/batch (presence smoke for digs).
pub const TRACE_KEYS: &[&str] = &[
    "admit",
    "route_owner",
    "pack",
    "conf_pool_check",
    "sign",
    "certify",
    "order",
    "execute",
    "snapshot",
];

/// Admission subsystem boundary (fd-mempool + engine submit).
pub struct AdmissionFacade<'a> {
    pub eng: &'a mut NodeEngine,
}

impl<'a> AdmissionFacade<'a> {
    pub fn submit_local(&mut self, tx: Transaction) -> SubmitResult {
        self.eng.submit_tx_from(tx, AdmitSource::Local)
    }

    pub fn submit_mesh(&mut self, tx: Transaction) -> SubmitResult {
        self.eng.submit_tx_from(tx, AdmitSource::Mesh)
    }

    pub fn mempool_len(&self) -> usize {
        self.eng.mempool.len()
    }
}

/// Conf-pool / DA scheduling boundary.
pub struct ConfPoolFacade<'a> {
    pub eng: &'a mut NodeEngine,
}

impl<'a> ConfPoolFacade<'a> {
    pub fn duty_phase(&self, now: f64) -> DutyPhase {
        self.eng.duty_phase(now)
    }

    pub fn check_step(&mut self, now: f64) -> crate::engine::GossipEvents {
        self.eng.conf_pool_check_step(now)
    }

    pub fn put_body(&mut self, batch: &Batch, now: f64) {
        self.eng.conf_pool_put_body(batch, now);
    }
}

/// DAG boundary (bodies, sigs, certs).
pub struct DagFacade<'a> {
    pub eng: &'a mut NodeEngine,
}

impl<'a> DagFacade<'a> {
    pub fn has_batch(&self, h: &str) -> bool {
        self.eng.has_batch(h)
    }

    pub fn has_cert(&self, h: &str) -> bool {
        self.eng.has_cert_for_batch(h)
    }
}

/// Ordering boundary (Bullshark / tip-BFT).
pub struct OrderFacade<'a> {
    pub eng: &'a mut NodeEngine,
}

impl<'a> OrderFacade<'a> {
    pub fn ordered_len(&self) -> usize {
        self.eng.ordering.ordered.len()
    }

    pub fn hard_final_len(&self) -> usize {
        self.eng.ordering.hard_final.len()
    }
}

/// Execution boundary.
pub struct ExecFacade<'a> {
    pub eng: &'a mut NodeEngine,
}

impl<'a> ExecFacade<'a> {
    pub fn executed_count(&self) -> usize {
        self.eng.execution.executed_txs.len()
    }
}

/// Orchestrator view: which façades are active for the current profile/role.
pub fn subsystem_map_json(eng: &NodeEngine) -> Value {
    let profile = eng.config.profile_status_json();
    serde_json::json!({
        "pr": "PR8",
        "mode": "in_process",
        "future_extraction": "map façades to IPC without rewriting business logic",
        "lock_order": ["meta", "mempool", "dag", "ordering", "execution"],
        "trace_keys": TRACE_KEYS,
        "profile": profile,
        "role": eng.role.as_str(),
        "sync_apis": ["submit", "check_step", "pack", "advance", "snapshot"],
        "async_message_candidates": ["directed_txs", "partial_sigs", "certs", "bodies"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fd_types::ProtocolConfig;

    #[test]
    fn façades_cover_admit_path() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig {
                require_tx_sigs: false,
                admit_circuit_breaker: false,
                ..Default::default()
            },
            0,
            1,
        );
        let mut admit = AdmissionFacade { eng: &mut eng };
        let tx = Transaction::create("acct1", 1, "dest", 1, 10, 0.0);
        assert!(admit.submit_local(tx).is_ok());
        assert_eq!(admit.mempool_len(), 1);
        let map = subsystem_map_json(&eng);
        assert_eq!(map["mode"], "in_process");
        assert!(map["trace_keys"].as_array().unwrap().len() >= 5);
    }

    #[test]
    fn conf_pool_façade_duty() {
        let mut eng = NodeEngine::new(
            "val1".into(),
            vec!["val1".into()],
            ProtocolConfig::default(),
            0,
            1,
        );
        let mut cp = ConfPoolFacade { eng: &mut eng };
        let _ = cp.duty_phase(0.0);
        let g = cp.check_step(0.25);
        let _ = g;
    }
}
