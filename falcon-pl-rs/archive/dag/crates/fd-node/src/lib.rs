//! Falcon DAG node library (engine used by `falcon-dagd` and tests).

#![recursion_limit = "256"]

pub mod conf_pool;
pub mod engine;
pub mod parts;
pub mod subsystems;

pub use engine::{AdmitSource, GossipEvents, LagMetrics, MeshDiag, NodeEngine, SubmitResult};
pub use parts::EngineParts;
pub use subsystems::{
    subsystem_map_json, AdmissionFacade, ConfPoolFacade, DagFacade, ExecFacade, OrderFacade,
    TRACE_KEYS,
};
