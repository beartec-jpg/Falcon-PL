# Falcon DAG — Master Implementation Guide

**Status:** north-star implementation plan for the Falcon-DAG architecture.

**Scope:** this guide consolidates architecture, implementation sequencing, hardware sizing, node-role separation, emissions, testing, and deployment planning into one operator and contributor reference.

**Audience:** protocol engineers, systems engineers, release managers, validator operators, and future public-network participants.

**Source alignment:** this guide is grounded in the current repository documentation and in the recent multi-lane reliability forensics that identified the dominant N=5 freeze mode as `PEER_SIGNED_CREATOR_MISSING_FOREIGN`.

**Primary references already in this repository:**
- `docs/DESIGN.md`
- `docs/ACCOUNT_LANES.md`
- `docs/BULLSHARK.md`
- `docs/NARWHAL_SPLIT.md`
- `docs/PULL_PATH_AND_LOCKS.md`
- `docs/REAL_BUILD.md`
- `docs/RUST_NEW_CHAIN_PLAN.md`
- `docs/NETWORK_ECONOMICS_AND_BOOTSTRAP.md`
- `docs/ECONOMICS.md`
- `docs/TRUST_AND_PROTOCOL_CONTROL.md`
- `docs/LANE_MODEL_STATUS.md`
- `docs/MESH_CONVERGENCE.md`
- `docs/MULTI_PACKER_STALL_DEBUG.md`

**What this document does:**
- Restates the architecture in implementation terms.
- Freezes the target design for a global mempool plus global conf-pool.
- Separates node roles into archival, validator, and RPC profiles.
- Defines an 11-PR execution plan with concrete tasks.
- Sizes hardware for prototype, live private net, and public expansion.
- Defines code structure and API boundaries for same-machine tasks first, network separation later.
- Defines the public-network emissions model and participation scoring shape.
- Defines testing, chaos, dig, and release gates.
- Defines the deployment roadmap from prototype to live private net to mainnet-grade operation to public participation.

**What this document does not do:**
- It does not change production Falcon Ledger.
- It does not replace the more focused protocol notes in the existing docs.
- It does not promise current N=5 multi-lane reliability before the listed work lands.

---

# 1. Executive summary

Falcon DAG is a two-layer system.

Layer A is the data-availability layer.

Layer A admits transactions into a global mempool, packs them into batches, spreads bodies, collects UNL signatures, and turns sufficiently signed batches into certificates.

Layer B is the ordering and execution layer.

Layer B orders certificates, expands the certificate DAG deterministically, and applies transactions to account state.

The major implementation lesson from the current codebase is that the architecture is directionally correct, but the reliability envelope is still constrained by creator-side absorption of foreign signatures and certificates under concurrent multi-lane load.

The most important immediate engineering objective is therefore not new feature surface.

It is making the existing global-mempool plus conf-pool plus multi-lane path reliable at N=5 under sustained load.

The most important medium-term objective is role separation.

The same binary can keep running all tasks on one machine at first, but the code must be structured as if mempool admission, DA, conf-pool, ordering, execution, archival, and RPC are separate services with crisp interfaces.

The most important long-term objective is a public network that remains protocol-controlled, post-quantum, and operationally boring.

That means:
- no admin backdoors,
- explicit node-role economics,
- predictable hardware envelopes,
- reproducible release gates,
- and staged deployment with conservative promotion criteria.

This plan turns those objectives into an implementation sequence.

---

# 2. Design principles

## 2.1 Non-negotiable principles

1. Production Falcon Ledger remains untouched.
2. Falcon DAG stays an isolated chain and deployment family.
3. Ordering and execution must be pure functions of certified state plus genesis.
4. The global mempool is admission and routing state, not consensus.
5. The conf-pool is scheduling and reliability state, not a second consensus protocol.
6. Only UNL weight inside the DAG produces certificates.
7. Soft finality and hard finality must remain explicit and operator-visible.
8. Account-sequence safety remains enforced at apply time.
9. System reliability is more important than peak TPS claims.
10. Public-network participation requires measurable, auditable scoring.

## 2.2 Delivery principles

1. Make the smallest implementation changes that close the largest reliability gaps.
2. Prefer explicit interfaces over implicit shared-state coupling.
3. Keep same-machine operation first-class during the split.
4. Treat network extraction as a later transport change, not a logic rewrite.
5. Add metrics before tuning.
6. Add directed recovery paths before raising concurrency.
7. Gate promotion on agreement, liveness, and operator observability.
8. Keep rollback paths simple.
9. Never hide ambiguous protocol state behind dashboards that only show aggregate TPS.
10. Prefer fail-closed behavior over clever but opaque auto-healing.

## 2.3 Reliability truths from the current forensics

The repository already fixed the earlier wire desynchronization issue.

That was the dominant `NO_BODY_ON_PEERS` failure mode.

The current dominant failure at N=5 multi-lane load is different.

Peers often have the body.

Peers often have signed.

Sometimes peers even have a certificate.

But the creator still shows foreign weight near zero and remains stuck in `need_seed`.

That is a creator-side integration, merge, delivery, or retry problem.

This guide therefore treats the global conf-pool and creator-directed return path as first-class architectural elements, not ad hoc reliability patches.

---

# 3. Current architecture baseline

## 3.1 Layered view

```text
Clients / wallets
    |
    v
Global mempool admission and routing
    |
    v
DA packing per lane / per validator
    |
    v
Global conf-pool scheduling and signature exchange
    |
    v
Certificates over batch hashes
    |
    v
Bullshark or tip-based ordering of certified DAG state
    |
    v
Deterministic expand
    |
    v
Sequential execution and state persistence
```

## 3.2 Repository baseline

The repository already contains both a Python executable specification and a Rust implementation path.

The Python path in `src/falcon_dag/` remains useful as a specification oracle, fast test harness, and design clarifier.

The Rust workspace in `falcon-dag-rs/` is the production path.

The Rust workspace is already structured into the following crates:
- `fd-types`
- `fd-crypto`
- `fd-mempool`
- `fd-dag`
- `fd-order`
- `fd-exec`
- `fd-net`
- `fd-store`
- `fd-node`

This crate split is directionally correct.

The remaining work is to align runtime responsibilities, interfaces, and operational roles with that crate split.

## 3.3 Runtime baseline

The current runtime already exposes several key ideas that should be preserved:
- same binary with role flags,
- conf-pool as a logical store for open DA work,
- pack/check duty phases,
- pull-path networking,
- Bullshark-oriented ordering,
- snapshot and restart support,
- protocol-controlled trust boundaries.

The runtime also already exposes several implementation truths that must be incorporated into the plan:
- `pack_open_cap` is currently acting as a reliability governor,
- `open_body_repool_after_s` is a recovery valve,
- `conf_pool_enabled` is a core part of multi-lane operation,
- creator-directed reseed and reannounce are not optional under load,
- and role separation cannot mean breaking current single-binary deployment.

---

# 4. Architecture north star

## 4.1 Summary statement

The north-star Falcon-DAG architecture is:

**global mempool | global conf-pool | multi-lane DA | certificate DAG | deterministic ordering | sequential execution | protocol-controlled economics | role-separated operations**

## 4.2 Why global mempool and global conf-pool both exist

A pure mempool is insufficient because admission order is not enough to make multi-lane batch creation reliable under PQ-signature scheduling pressure.

A pure conf-pool is insufficient because it cannot replace transaction admission, fee ordering, sequence-window validation, or owner routing.

The mempool and conf-pool serve different purposes.

The mempool answers:
- what transactions are eligible,
- where they should go,
- what priority they have,
- and whether they should still be considered for packing.

The conf-pool answers:
- which open bodies must be visible to signers,
- which signatures are still missing,
- which creators must be directly reseeded,
- which return-path retries are outstanding,
- and which work is in pack versus check phases.

## 4.3 What success looks like

A healthy N=5 run should show the following properties at the same time:
- negligible freeze events,
- negligible `PEER_SIGNED_CREATOR_MISSING_FOREIGN` gaps,
- negligible `NO_BODY_ON_PEERS` gaps,
- negligible open-body repools,
- low or zero end-of-run execution spread,
- stable hold-phase apply TPS,
- and consistent state roots across all honest nodes.

If freeze counters improve but `PEER_SIGNED` gaps remain large, the path is still unhealthy.

If body delivery improves but creator foreign merge does not, the path is still unhealthy.

If throughput improves only because the system is throttling into `terBUSY`, the path is not yet healthy.

---

# 5. Global mempool design

## 5.1 Purpose

The global mempool is the canonical admission and selection layer for unfinalized transactions.

It is global in the architectural sense, not necessarily as a single physical shared-memory object across the whole network.

Each node maintains a local replica that converges by gossip and directed owner delivery.

The protocol treats those local replicas as views over one logical global queue of candidate work.

## 5.2 Responsibilities

The global mempool is responsible for:
- admitting locally submitted transactions,
- rejecting obviously invalid transactions,
- enforcing bounded sequence windows,
- tracking first-seen time,
- tracking fee priority,
- tracking ownership routing,
- deduplicating exact transaction IDs,
- retaining higher-sequence candidates until earlier sequences clear,
- removing transactions once soft-final competitors are no longer relevant,
- and exposing deterministic selection order to the packer.

## 5.3 Non-responsibilities

The global mempool is not responsible for:
- final ordering of history,
- certifying availability,
- deciding final execution winners across the whole DAG,
- storing archival history forever,
- or acting as the place where foreign-signature liveness is recovered.

## 5.4 Data model

Each mempool entry should track at least:
- `tx_id`
- `account`
- `sequence`
- `fee`
- `fee_density` when applicable
- `first_seen`
- `admit_source`
- `owner_lane`
- `routed_to_owner_at`
- `last_rebroadcast_at`
- `soft_final_seen`
- `ordered_seen`
- `dropped_reason`

## 5.5 Deterministic ordering

The mempool ordering rule should remain deterministic.

A safe baseline is:
- fee density,
- plus age boost,
- then earlier first-seen,
- then transaction hash.

The point is not a perfect market.

The point is that all honest nodes can reason about the same candidate ordering shape even if their local mempools are not perfectly synchronized.

## 5.6 Ownership model

The lane-owner model remains:
- lane equals validator,
- account owner equals deterministic hash of account over sorted UNL,
- owner is the primary packer for that account’s sequence flow.

Non-owner nodes may admit a transaction.

Non-owner nodes must then direct it to the owner.

The owner should be able to pack even if the original submitter did not connect directly to it.

## 5.7 Global mempool invariants

The global mempool must satisfy the following invariants:
- no transaction is lost merely because it first arrived at a non-owner,
- exact duplicates are harmless,
- same-account same-sequence competitors are represented explicitly until execution authority resolves them,
- sequence holes are visible and measurable,
- and bounded memory limits are enforced without silently dropping operator-critical work.

## 5.8 Mempool failure modes that must stay visible

The metrics layer must continue to expose:
- mempool depth,
- owned versus not-owned pending counts,
- oldest owned age,
- oldest not-owned age,
- routing lag to owner,
- same-sequence competitor counts,
- and breaker-trigger reasons.

If those metrics go dark, future reliability regressions will be harder to diagnose.

## 5.9 Same-machine implementation target

On one machine, the mempool should be a separate logical subsystem with its own API surface.

It may remain in-process for performance.

But callers should interact with it through explicit methods or channels, not by reaching into mixed engine internals.

## 5.10 Future networked implementation target

Later, the mempool can be promoted to a network-visible service boundary for:
- admission gateways,
- RPC frontends,
- load balancers,
- and owner-routing helpers.

The consensus path must not depend on this extraction.

The protocol should behave the same whether the mempool lives in one process or behind a same-host IPC boundary.

---

# 6. Global conf-pool design

## 6.1 Purpose

The global conf-pool is the shared logical workspace for open DA batches that are waiting for enough foreign signatures and/or creator-visible foreign weight to become certified work.

It exists because multi-lane load produces reliability problems that a free-for-all mesh alone does not resolve deterministically.

## 6.2 Important architectural statement

The conf-pool is **not** a second consensus layer.

The conf-pool does **not** override the DAG.

The conf-pool does **not** create certificate authority outside UNL weight.

The conf-pool is a scheduling, visibility, retry, and liveness structure.

## 6.3 Why the conf-pool must be global

The conf-pool is global in the same logical sense as the mempool.

Each node has a local view.

But the protocol behaves as though there is one shared set of open bodies and partial-signature progress that every honest signer should be able to reason about.

This logical globality matters because the current failure mode is not simple local absence.

Peers often made progress.

The creator often did not absorb that progress.

A global conf-pool design therefore must prioritize state convergence around:
- body presence,
- signer presence,
- creator-visibility of foreign weight,
- and retry state.

## 6.4 Conf-pool entry model

Each conf-pool entry should track:
- `batch_hash`
- `creator`
- `parents`
- `first_seen`
- `last_touch`
- `body_present`
- `local_signed`
- `foreign_sig_count_visible`
- `foreign_sig_weight_visible`
- `creator_ack_state`
- `cert_state`
- `reannounce_count`
- `reseed_count`
- `parked_cert_present`
- `directed_creator_sig_last_at`
- `directed_creator_cert_last_at`
- `drop_reason`

## 6.5 Conf-pool states

A useful target state machine is:
- `OpenBodySeen`
- `OpenBodyDirected`
- `LocalSigAdded`
- `ForeignSigObserved`
- `CreatorAckPending`
- `CreatorForeignMerged`
- `CertParked`
- `Certified`
- `RepoolPending`
- `Dropped`

These states do not all need to be exposed as enums immediately.

But the implementation should evolve toward this model because it matches the real reliability questions operators ask.

## 6.6 Conf-pool invariants

The conf-pool must guarantee the following:
- if a signer has the body, the creator can eventually learn that fact,
- if a signer has signed, the creator can eventually learn that fact,
- if a signer can certify, the creator can eventually learn that fact,
- reannounce paths are bounded and observable,
- repool only happens after the system has exhausted defined recovery attempts,
- and open-body abandonment remains a deliberate, measured action.

## 6.7 Pack/check duty cycle

The pack/check duty split already exists and should remain.

The design intent is:
- pack windows create or extend work,
- check windows prioritize signature production, creator-directed return traffic, and parked-cert resolution.

The duty split reduces uncontrolled feedback loops.

It also gives operators a clearer mental model when analyzing under-load behavior.

## 6.8 Priority policy

The conf-pool should prioritize, in order:
1. foreign open bodies with zero creator-visible foreign signatures,
2. foreign open bodies with some foreign signatures but no creator acknowledgment,
3. foreign open bodies that are certifiable but not yet creator-visible as certified,
4. creator-local open bodies missing only one or two signatures,
5. maintenance retries for older open work,
6. and only then optional opportunistic work.

This priority order is different from naive fairness among already-half-signed opens.

It is designed around the observed failure mode.

## 6.9 Creator-directed return path

The conf-pool design explicitly requires directed return paths.

When a peer signs a foreign open body, it must not rely only on mesh chance to get that signature back to the creator.

The peer should:
- emit the partial signature,
- piggyback the body if useful,
- directly reannounce to the creator on a bounded interval,
- and escalate to directed certificate delivery if certification is reached before the creator has absorbed the foreign weight.

## 6.10 Parked certs are first-class

If a certificate arrives before the creator can fully apply it because of parent or body races, that state must be tracked explicitly.

A parked cert is not a success condition.

It is pending success.

The engine must retry parked-cert application for creator-local opens independently of new pack attempts.

## 6.11 Repool policy

Repool should stay as a last-resort recovery valve.

The protocol should not normalize repool as steady-state behavior.

Before an own open body is repooled, the node should have:
- reseeded the body,
- reannounced directed signatures,
- retried parked cert apply,
- retried local certification against merged state,
- and emitted enough metrics for operators to see why repool happened.

## 6.12 Conf-pool metrics

At minimum, expose:
- open entries,
- entries by creator,
- entries by age bucket,
- sign attempts,
- sign deferrals by cap,
- creator-directed sig reannounces,
- creator-directed cert reannounces,
- foreign sig merged from body,
- foreign sig merged from sig,
- parked cert applies,
- repool count,
- repool abandon count,
- cert latency by creator,
- and zero-foreign duration for own opens.

## 6.13 Same-machine implementation target

On one machine, the conf-pool should remain an in-process logical subsystem next to DA.

But it must have explicit APIs, metrics, and tests of its own.

It should not be a bag of incidental engine fields.

## 6.14 Future networked implementation target

Later, global conf-pool state can be partially externalized through:
- explicit gossip metadata,
- directed signer-to-creator subprotocols,
- optional peer recovery RPCs,
- or colocated DA worker sidecars.

The functional state machine should not change when the transport changes.

## 6.15 Conf-pool design conclusion

The conf-pool is where Falcon-DAG turns unreliable open-body mesh timing into a measurable, bounded, and eventually convergent certification workflow.

That is why it belongs in the master architecture guide.

---

# 7. Interaction between global mempool and global conf-pool

## 7.1 Hand-off model

The mempool hands candidate transactions to the packer.

The packer creates an open batch.

That open batch is inserted into the DAG and deposited into the conf-pool.

From that point onward, the conf-pool owns the visibility and signature-liveness workflow for that batch until it is certified, repooled, or dropped.

## 7.2 Removal model

Transactions remain in the mempool until one of the following is true:
- the transaction is safely finalized and no longer needs to compete,
- a conflicting transaction has already won at execution authority and policy says drop the loser,
- an operator breaker or TTL explicitly removes it,
- or a protocol rule invalidates it.

A batch entering the conf-pool must not implicitly mean the transaction disappears from admission accounting.

The system needs both views:
- transaction admission state,
- and batch certification state.

## 7.3 Critical joint metrics

A healthy deployment must be able to graph at least:
- mempool age versus conf-pool age,
- owner-routed pending versus own open pending,
- zero-foreign duration versus apply TPS,
- repool counts versus routed admission counts,
- and creator-local backlog versus foreign backlog.

## 7.4 Key design decision

The mempool and conf-pool should be coupled by identifiers and explicit lifecycle events, not by hidden mutation.

That means emitting events such as:
- `TxAdmitted`
- `TxRoutedToOwner`
- `BatchOpened`
- `BatchSeeded`
- `BatchSigned`
- `BatchForeignMerged`
- `BatchCertified`
- `BatchRepoolRequested`
- `BatchRepoolCompleted`
- `BatchDropped`

The current implementation does not need a full event bus immediately.

But the 11-PR plan should move toward this event-shaped architecture.

---

# 8. Node type separation

## 8.1 Why separate node types

The current binary already supports role separation in spirit.

The next step is to freeze the operational profiles the network will support.

Those profiles are:
- archival nodes,
- validator nodes,
- and RPC nodes.

## 8.2 Guiding rule

Role separation is operational, not consensus-fragmenting.

All node types verify enough protocol state to remain honest participants.

What changes is which subsystems they must run, how much data they must retain, and how they are rewarded.

## 8.3 Validator nodes

Validator nodes are consensus-critical participants.

They run:
- transaction admission needed for owned-lane work,
- DA packing,
- conf-pool check duties,
- batch signing,
- certificate generation and verification,
- ordering,
- execution,
- state persistence,
- and network scoring/reporting duties.

Validator nodes should expose RPC only if the operator wants to combine roles.

The validator role should not depend on public RPC load.

## 8.4 Validator node responsibilities

Validator nodes must:
- sign valid foreign opens they can verify,
- track own opens and foreign-sig visibility,
- participate in certificate gossip,
- derive the same order as other honest validators,
- produce identical executed state,
- store enough recent hot history to survive restart and catch-up,
- emit metrics needed for scoring and debugging,
- and remain stable under bounded load.

## 8.5 Validator node non-responsibilities

Validator nodes should not be required to:
- store the entire historical body set forever,
- serve public RPC at internet scale,
- or absorb arbitrary client load directly.

## 8.6 Archival nodes

Archival nodes are history and audit machines.

They store:
- full batch bodies,
- full certificate history,
- checkpoints,
- state snapshots,
- and operator-grade query indexes.

Archival nodes may optionally run ordering and execution continuously.

They do not need to sign or pack.

Their primary purpose is data durability, indexing, auditability, and recovery support.

## 8.7 Archival node responsibilities

Archival nodes should:
- verify batches and certificates,
- persist full history,
- expose deep query APIs,
- support replay and export,
- and help bootstrap new RPC or validator nodes.

## 8.8 RPC nodes

RPC nodes are client-serving ingress and query edges.

They prioritize:
- transaction admission,
- fast account and transaction queries,
- mempool visibility,
- subscription streaming,
- and rate-limited public interfaces.

RPC nodes do not need to pack or sign.

RPC nodes must still validate enough local state to avoid serving nonsense.

## 8.9 Recommended role matrix

| Subsystem | Validator | Archival | RPC |
|-----------|-----------|----------|-----|
| Admission | yes | optional | yes |
| Owner routing | yes | optional | yes |
| DA packing | yes | no | no |
| Conf-pool check | yes | no | no |
| Batch signing | yes | no | no |
| Ordering | yes | yes | optional cached view |
| Execution | yes | yes | optional read replica |
| Hot state store | yes | yes | yes |
| Cold history store | optional | yes | no |
| Public RPC | optional | limited | yes |
| Metrics / scoring | yes | yes | yes |

## 8.10 Same-machine deployment pattern

On a single host, one operator may run:
- one validator instance,
- one archival sidecar process or embedded task,
- and one RPC frontend.

The north star is that these tasks can be colocated without being logically merged.

## 8.11 Future multi-host deployment pattern

On a larger network, role separation becomes physical:
- validators on low-latency trusted hosts,
- archival nodes on storage-heavy hosts,
- RPC nodes behind load balancers and DDoS controls.

## 8.12 Role-specific economics

The public network must pay validators and service nodes differently.

Validators earn emission-driven consensus rewards.

RPC and/or full-service infrastructure should earn from fee pools or service programs, not from pretending they are consensus signers.

## 8.13 Role-specific config profiles

The implementation should eventually ship explicit profiles such as:
- `--profile validator`
- `--profile archival`
- `--profile rpc`

These profiles should map to clear defaults for:
- enabled subsystems,
- storage retention,
- RPC exposure,
- metrics verbosity,
- and breaker thresholds.

---

# 9. Code structure and API boundaries

## 9.1 Goal

The codebase should look like a set of services even when it runs in one process.

## 9.2 Core subsystems

The target same-machine subsystems are:
- admission service,
- mempool service,
- lane ownership service,
- DA batch builder,
- conf-pool service,
- certificate DAG service,
- ordering service,
- execution service,
- snapshot service,
- archival store service,
- RPC query service,
- scoring and metrics service.

## 9.3 Boundary rule

Each subsystem should have:
- clear inputs,
- clear outputs,
- no hidden reverse dependency,
- and tests at the boundary.

## 9.4 Crate alignment target

| Crate | Primary responsibility | Should not own |
|-------|------------------------|----------------|
| `fd-types` | canonical protocol types and config | runtime logic |
| `fd-crypto` | signing and verification | routing or ordering |
| `fd-mempool` | tx admission and ordering | certificate logic |
| `fd-dag` | batches, signatures, certificates | client RPC |
| `fd-order` | Bullshark and deterministic expand | networking |
| `fd-exec` | apply and state transition | P2P |
| `fd-net` | transport, inventory, pull, directed delivery | consensus decisions |
| `fd-store` | hot/cold persistence and snapshots | network behavior |
| `fd-node` | wiring, profiles, orchestration | domain shortcuts that bypass crate APIs |

## 9.5 Same-machine process model

In the first production-quality shape, a full same-machine node can still run in one OS process.

Internally, it should behave like this:
- admission task,
- inbound network task,
- DA tick task,
- conf-pool check task,
- ordering tick task,
- execution apply task,
- snapshot flush task,
- metrics scrape task,
- RPC handler task.

## 9.6 Future multi-process model

Once the boundaries are stable, the system can graduate to multiple processes on one machine:
- `falcon-dagd-validator`
- `falcon-dagd-archival`
- `falcon-dagd-rpc`
- optional `falcon-dagd-ingress`

The logic should not need to be rewritten.

The boundary should already be defined by the same APIs used in-process.

## 9.7 Later multi-host model

Later still, those same boundaries can map to network interfaces:
- local channels become IPC,
- IPC becomes same-VPC gRPC or QUIC,
- storage calls become remote archival fetches,
- but protocol semantics stay identical.

## 9.8 Admission API boundary

Suggested input methods:
- `submit_tx(tx)`
- `submit_batch_candidate(tx_ids)` for tests only
- `get_pending(account)`
- `mark_finalized(tx_ids)`
- `route_to_owner(tx)`

Suggested outputs/events:
- `TxAccepted`
- `TxRejected`
- `TxOwnerRouted`
- `TxExpired`
- `TxConflictObserved`

## 9.9 Conf-pool API boundary

Suggested methods:
- `put_body(batch)`
- `put_sig(batch_hash, validator, sig)`
- `note_creator_ack(batch_hash, source)`
- `get_open(batch_hash)`
- `list_ready_for_sign()`
- `list_ready_for_creator_reannounce()`
- `list_ready_for_cert_retry()`
- `mark_certified(batch_hash)`
- `mark_repool(batch_hash)`

Suggested outputs/events:
- `BodyInserted`
- `SigInserted`
- `ForeignWeightVisible`
- `CreatorAckMissing`
- `CreatorAckObserved`
- `CertParked`
- `CertApplied`
- `RepoolTriggered`

## 9.10 DAG API boundary

Suggested methods:
- `ingest_batch(batch)`
- `sign_local(batch_hash)`
- `add_signature(batch_hash, validator, sig)`
- `try_certify(batch_hash)`
- `get_certificate(batch_hash)`
- `certified_tips()`
- `children(batch_hash)`
- `support_weight(batch_hash)`

## 9.11 Ordering API boundary

Suggested methods:
- `ingest_certificate(cert)`
- `compute_commits()`
- `expand_commits()`
- `ordered_since(index)`
- `hard_final_since(index)`

## 9.12 Execution API boundary

Suggested methods:
- `apply_ordered(batch_hashes)`
- `state_root()`
- `account_info(account)`
- `asset_info(asset)`
- `replay_from(snapshot, ordered_tail)`

## 9.13 Storage API boundary

Suggested methods:
- `put_hot_batch(batch)`
- `put_certificate(cert)`
- `get_body(batch_hash)`
- `write_snapshot(snapshot)`
- `load_snapshot()`
- `prune_hot(before)`
- `archive_export(range)`

## 9.14 RPC API boundary

Suggested public methods:
- `ping`
- `server_info`
- `dag_status`
- `submit`
- `canonical`
- `account_info`
- `asset_info`
- `economy`
- `tx_status`
- `batch_status`
- `metrics`

Suggested operator-only methods:
- `snapshot`
- `debug_conf_pool`
- `debug_pending_certs`
- `debug_lane_flow`
- `debug_reliability_gates`

## 9.15 Boundary anti-patterns to avoid

Do not:
- let DA directly mutate execution state,
- let RPC handlers reach into mixed lock stacks arbitrarily,
- let transport code decide certification semantics,
- let archival storage be required for hot-path execution,
- or let public RPC traffic block validator check windows.

## 9.16 Concurrency rules

The lock-hub work already points in the right direction.

Preserve the lock-order discipline.

Never hold blocking locks across awaits.

Keep composite mutations explicit.

Where possible, move toward message-passing plus scoped mutation windows.

## 9.17 Boundary tests required

Every boundary should eventually have:
- unit tests of valid paths,
- unit tests of invalid inputs,
- deterministic replay tests,
- concurrency safety tests,
- and serialization compatibility tests where relevant.

---

# 10. Hardware specification and expansion strategy

## 10.1 Planning assumptions

Hardware planning must optimize for reliability before marketing throughput.

The current reference host from the reliability reports is roughly:
- 12-core Intel i5-12400,
- 16 GB RAM,
- single host,
- localhost mesh.

That class of machine is adequate for prototype work.

It is not the baseline for a public network.

## 10.2 Phases

This guide defines three hardware phases:
- Phase 1: prototype and forensic lab,
- Phase 2: live private network and pre-public rollout,
- Phase 3: public-network and mainnet-grade expansion.

## 10.3 Phase 1 goals

Phase 1 is about:
- correctness,
- restartability,
- observability,
- and reproducing known failure modes.

Phase 1 is not about headline capacity.

## 10.4 Phase 1 recommended hardware by role

### Validator
- 8 to 12 physical cores or equivalent vCPU
- 32 GB RAM preferred, 16 GB minimum for lab use
- 1 TB NVMe SSD
- 1 Gbps network
- isolated CPU governor and no aggressive power saving

### Archival
- 8 cores
- 32 GB RAM
- 2 TB NVMe or SSD
- optional slower cold storage tier for exports

### RPC
- 4 to 8 cores
- 16 to 32 GB RAM
- 500 GB SSD
- 1 Gbps network

## 10.5 Phase 1 topology guidance

A good Phase 1 setup is:
- 3 to 5 validators,
- optionally colocated on one or two hosts for controlled digs,
- one archival observer,
- one RPC ingress.

## 10.6 Phase 2 goals

Phase 2 is live private-network reliability.

This is where:
- N=5 must be boring,
- restart and catch-up must be boring,
- snapshots must be routine,
- and RPC separation must be proven.

## 10.7 Phase 2 recommended hardware by role

### Validator
- 16 dedicated cores or 24 vCPU
- 64 GB RAM
- 2 TB Gen4 NVMe
- 1 to 10 Gbps network depending on host provider
- hardware cryptography support where useful
- separate metrics and logs disk budget if possible

### Archival
- 16 to 24 cores
- 128 GB RAM
- 8 TB NVMe or mixed NVMe plus HDD cold tier
- 1 to 10 Gbps network
- periodic off-host snapshot replication

### RPC
- 8 to 16 cores
- 64 GB RAM
- 1 to 2 TB NVMe
- fronted by load balancer or reverse proxy
- strict rate limits and DDoS controls

## 10.8 Phase 2 topology guidance

Recommended minimum Phase 2 topology:
- 5 validators on separate failure domains,
- 2 archival nodes,
- 2 RPC nodes,
- centralized metrics and alerting,
- snapshot replication to object storage,
- and synthetic traffic generation separated from validator hosts.

## 10.9 Phase 3 goals

Phase 3 is public participation and mainnet-grade posture.

The network must assume:
- hostile internet traffic,
- mixed operator quality,
- and long-lived storage obligations.

## 10.10 Phase 3 recommended hardware by role

### Validator
- 24 physical cores or 32 to 48 high-quality vCPU
- 128 GB RAM
- 4 TB high-end NVMe
- 10 Gbps network preferred
- dedicated monitoring sidecar
- reserved CPU for signing, ordering, and storage flushes

### Archival
- 24 to 32 cores
- 256 GB RAM
- 16 TB storage with hot/cold separation
- 10 Gbps network
- object-store backup and export pipelines

### RPC
- 16 cores per node
- 64 to 128 GB RAM
- 2 TB NVMe
- horizontally scaled pool
- regional replicas for latency

## 10.11 Expansion strategy

Phase 1 to Phase 2 expansion should happen by role separation first, not by simply increasing batch knobs.

Phase 2 to Phase 3 expansion should happen by:
- moving validators to distinct failure domains,
- scaling RPC horizontally,
- scaling archival storage vertically and with replication,
- and only then increasing concurrency and public traffic allowances.

## 10.12 Capacity planning rules

1. Size for sustained hold-phase load, not only short spikes.
2. Keep at least 30 percent headroom on validator CPU.
3. Keep at least 40 percent headroom on validator memory.
4. Keep enough NVMe budget for snapshots plus recent hot history.
5. Measure creator-side zero-foreign duration when evaluating CPU pressure.
6. Never infer public-network readiness from localhost-only TPS.

## 10.13 Hardware promotion gates

Do not promote a hardware profile unless it demonstrates:
- agreement under target N,
- boring restart recovery,
- acceptable cert latency percentiles,
- bounded storage growth,
- and stable RPC latency under expected public traffic.

---

# 11. Emissions model for public network participation

## 11.1 Why an emissions model belongs in the architecture guide

For a public network, participation incentives shape topology and behavior.

If rewards are implicit or ad hoc, operators optimize the wrong things.

The public-network model must therefore explicitly define who gets paid for what.

## 11.2 Current repository split that must be reconciled

The repository currently contains two economic strands:
- `docs/ECONOMICS.md` describes the chain substrate in terms of `FDAG`.
- `docs/NETWORK_ECONOMICS_AND_BOOTSTRAP.md` describes public-network participation in terms of `FALC` and a 200 billion supply model.

This guide treats that as a documentation split between:
- chain substrate mechanics,
- and public-network bootstrapping.

Until PR9 resolves the split, contributors should treat `FDAG` as the current chain-substrate working name in code and private-network configuration, and treat `FALC` as the provisional public-network participation label used by the bootstrap and emissions planning docs.

PR9 in the implementation plan must reconcile naming and freeze one public token model.

## 11.3 Public-network baseline

Until PR9 freezes a final naming choice, the public-network participation model should follow the more complete bootstrap and rotation document.

That model includes:
- a large protocol treasury,
- bootstrap airdrop scoring,
- validator reward scoring,
- service-node reward pathways,
- and fee replenishment.

## 11.4 Treasury model

The treasury should be protocol-controlled.

No discretionary human key should be able to drain it.

That matches the repository trust model.

Treasury outflows should be limited to protocol-defined reward, emission, or future spend modules adopted by consensus.

## 11.5 Bootstrap model

The bootstrap phase should remain time-bounded.

A clear example is:
- week 0 genesis,
- weeks 1 through 7 onboarding,
- week 8 snapshot and distribution,
- week 9 onward production emissions.

## 11.6 Bootstrap scoring components

Participants may earn bootstrap score through:
- validator behavior,
- full-node or service-node behavior,
- public infrastructure contribution,
- faucet and user activity where appropriate,
- and transaction activity with anti-sybil limits.

## 11.7 Production validator reward model

Validator rewards should be driven by measurable operational behavior, such as:
- uptime,
- vote accuracy,
- signature latency,
- consistency,
- and possibly quality-of-service metrics related to cert-path reliability.

## 11.8 Suggested validator score vector

A stable first public model can remain close to the existing repository plan:
- uptime: 40 percent,
- vote accuracy: 30 percent,
- latency: 20 percent,
- consistency: 10 percent.

## 11.9 Additional reliability-aware metric

Given the current architecture, a public-network score should eventually include a bounded reliability factor around creator-side liveness.

For example:
- foreign-signature visibility success rate,
- or median creator zero-foreign time for owned opens.

This should be added carefully to avoid gaming.

It belongs later, after the metrics are stable.

## 11.10 Service-node rewards

The network should distinguish validator rewards from service rewards.

Service rewards may target:
- full archival operators,
- public RPC operators,
- or explicit registered full nodes.

Those rewards should likely come from fee pools or separate service budgets, not from the same emission bucket used to compensate consensus risk.

## 11.11 Fee split guidance

A public-network fee model can preserve the substrate idea of:
- burn,
- treasury replenishment,
- and service-node distribution.

Exact percentages should be frozen in PR9.

The important architectural rule is that fees should replenish long-term sustainability while not making public RPC a purely charitable role.

## 11.12 Governance and emissions

Any emissions schedule must be:
- public,
- deterministic,
- auditable on-chain,
- and changeable only through explicit governance.

## 11.13 Suggested emission phases

1. Bootstrap distribution phase.
2. Early production decay phase.
3. Long-tail participation phase.
4. Fee-dominant mature phase.

## 11.14 Participation ethics

The emissions model should reward durable contribution.

It should not primarily reward:
- fake traffic,
- submit spam,
- vanity uptime without correctness,
- or centralized sybil fleets.

## 11.15 Emissions summary

The architecture guide therefore assumes a public participation model with:
- protocol treasury,
- explicit bootstrap scoring,
- validator reliability rewards,
- service-node incentives,
- fee replenishment,
- and governance-controlled evolution.

---

# 12. Testing, digs, chaos, and acceptance criteria

## 12.1 Testing philosophy

Falcon-DAG needs more than unit tests.

It needs progressive evidence that the architecture behaves under:
- ordinary load,
- concurrent multi-lane load,
- restarts,
- delayed or missing peers,
- and public-facing service patterns.

## 12.2 Test layers

The test pyramid should include:
- unit tests,
- component tests,
- deterministic simulator tests,
- local multi-node digs,
- long-run soak tests,
- chaos scenarios,
- role-separation deployment drills,
- and public-profile dry runs.

## 12.3 Minimum test categories

1. type and serialization tests,
2. mempool ordering tests,
3. lane ownership tests,
4. conf-pool state-machine tests,
5. creator-foreign-merge tests,
6. certificate DAG correctness tests,
7. ordering agreement tests,
8. execution conflict tests,
9. snapshot and replay tests,
10. network pull and directed-delivery tests,
11. role-profile startup tests,
12. public economics and scoring tests.

## 12.4 Dig philosophy

A dig is a focused forensic run with high-visibility metrics and bounded hypotheses.

Digs are not a substitute for CI.

But Falcon-DAG clearly needs them because the dominant failures are timing-sensitive and multi-node.

## 12.5 Required dig classes

- wire and body-availability digs,
- creator-foreign-merge digs,
- open-body repool digs,
- pack/check rhythm digs,
- lane-fairness digs,
- N scaling digs,
- dual-host digs,
- and restart-after-stall digs.

## 12.6 Acceptance gate structure

Every promotion gate should combine:
- correctness,
- liveness,
- observability,
- and operational simplicity.

## 12.7 Core correctness gate

All honest validators must agree on:
- ordered certificate set,
- executed transaction results,
- state root,
- and hard-final progression.

## 12.8 Core liveness gate

At the target N and target peak load for that phase:
- freeze events should be near zero,
- creator-side zero-foreign intervals should remain bounded,
- open-body repool should be near zero,
- and hold-phase apply throughput should stay healthy.

## 12.9 Core observability gate

Operators must be able to answer the following from metrics and logs alone:
- is the body on peers,
- is the peer signed,
- did the creator merge foreign weight,
- is a cert parked,
- is repool imminent,
- and is ordering or execution the limiting stage.

## 12.10 Core operational gate

Restart, snapshot restore, and role-separated startup should be routine.

If the network only behaves after a clean cold start or with ad hoc debug flags, it is not ready for the next phase.

## 12.11 Example N=3 gate

For a moderate continuous dig at N=3:
- freeze events approximately zero,
- repool approximately zero,
- execution spread zero,
- and sustained healthy apply TPS.

## 12.12 Example N=5 gate

For the chosen Phase 2 production envelope at N=5:
- freeze events approximately zero,
- `PEER_SIGNED_CREATOR_MISSING_FOREIGN` approximately zero or very low,
- repool approximately zero,
- final execution spread zero,
- and all nodes remain caught up after settle.

## 12.13 Chaos scenarios that must exist

1. one validator silent,
2. two validators silent,
3. delayed certificate delivery,
4. delayed creator-directed signature return,
5. snapshot while under load,
6. restart of one validator mid-hold,
7. RPC flood while validators continue normally,
8. archival node lag and catch-up,
9. network partition and heal where within safety assumptions,
10. storage pressure and prune behavior.

## 12.14 CI expectations

CI does not need to run the heaviest long digs on every commit.

But CI must at least run:
- unit tests,
- targeted role-profile tests,
- deterministic small multi-node tests,
- and smoke checks for docs and startup commands where possible.

Longer forensic and soak suites can run nightly or before promotion.

## 12.15 Current CI note

As of 2026-08-07, recent workflow history showed recurring failures in the repository’s `falcon-dag-rs CI` and `live-indexer-smoke` workflows.

Those failures should be treated as a reminder that promotion gates must stay realistic and observable.

This implementation-plan change itself is documentation-only, but the implementation roadmap below assumes CI hardening is part of the delivery sequence.

---

# 13. Deployment roadmap

## 13.1 Roadmap stages

This guide defines four roadmap stages:
- Prototype,
- Live private network,
- Mainnet-grade internal operation,
- Public participation network.

## 13.2 Prototype stage

Prototype means:
- same repository,
- same binary family,
- aggressive instrumentation,
- controlled node counts,
- and rapid digs.

Prototype exit criteria:
- architecture frozen,
- key reliability gaps known,
- and first boring N=3 plus bounded N=5 path demonstrated.

## 13.3 Live private network stage

Live private means:
- operators can run a real multi-host network,
- snapshots and restart work,
- validators and RPC nodes can be separated,
- economic controls are active,
- and no real public participation is yet assumed.

Exit criteria:
- boring N=5 private operation,
- explicit hardware profile,
- clean role profiles,
- and proven deploy/rollback runbooks.

## 13.4 Mainnet-grade internal operation stage

Mainnet-grade internal operation means:
- still possibly invite-only,
- but engineered as if public abuse exists,
- with hardened validators,
- service separation,
- audit-grade archival retention,
- security review of critical paths,
- and release discipline suitable for real value later.

Exit criteria:
- security posture reviewed,
- economics frozen,
- and operator runbooks no longer depend on core developers watching logs live.

## 13.5 Public participation stage

Public participation means:
- open operator onboarding,
- explicit registration and scoring,
- faucet or bootstrap mechanisms where appropriate,
- published hardware guides,
- public RPC endpoints,
- and governance-visible emissions.

Exit criteria:
- network can survive ordinary churn,
- scoring is auditable,
- and service-node incentives are clearly defined.

## 13.6 Promotion rule

No stage promotion should be based on one successful benchmark run.

Promotion requires repeated passes across the defined gates.

---

# 14. The 11-PR implementation plan

This section is the execution spine of the guide.

Each PR is intentionally scoped so that reliability, role separation, and public-network readiness can advance without forcing an all-at-once rewrite.

## 14.1 General implementation and rollback rules for all PRs

These rules apply to every PR in this section unless a PR explicitly says otherwise.

- Preserve backward-compatible startup behavior where practical during the transition.
- Prefer additive interfaces and metrics first; remove old paths only after the new path is measured.
- Keep same-machine and multi-node lab operation working throughout the sequence.
- If a migration touches dig tooling, update the dig summary schema in the same PR.
- Keep conservative defaults and clear opt-outs for new config.
- Prefer revertible changes that do not invalidate on-disk data.
- If metrics or JSON output changes, provide compatibility notes for scripts and operators.


## PR1 — Freeze architecture and observability contract

### Objective

Create the explicit architecture, metrics, and interface baseline so later PRs change behavior without changing the mental model every week.


### Primary crates and surfaces

- `fd-types`
- `fd-node`
- `fd-mempool`
- `fd-dag`
- `fd-order`
- `fd-exec`

### Concrete tasks

- Document the authoritative architecture string and role taxonomy in code comments and docs.
- Freeze the names of key metrics for mempool, conf-pool, batch-status, parked certs, and creator-directed retries.
- Add or normalize `batch_status` style debug surfaces that distinguish no-body, body-no-sig, peer-signed-creator-missing-foreign, and healthy cases.
- Define explicit configuration defaults for validator, archival, and RPC role profiles without forcing transport extraction yet.
- Write boundary notes for admission, conf-pool, DAG, ordering, execution, store, and RPC subsystems.
- Record the current N=3 and N=5 reliability baselines as engineering targets, not marketing claims.
- Decide which metrics are public, operator-only, and debug-only.
- Ensure the README and docs index point to the master guide and role docs.


### Test plan

- Doc consistency check across README and major architecture docs.
- Small smoke validating role-profile config can be parsed.
- Snapshot of metrics keys to prevent accidental renames.
- Targeted test that batch-status classification remains stable for canned scenarios.

### Acceptance gates

- All core architecture docs agree on terms for mempool, conf-pool, cert, soft final, and hard final.
- Developers can answer where a feature belongs by subsystem without reading engine internals.
- Metrics names used by digs are frozen and tested.


### Out of scope

- Changing core protocol semantics.
- Public-network economics finalization.
- Transport-layer rewrite.

### Dependency position

- This PR is foundational and should land before the others where possible.

## PR2 — Global mempool hardening and owner-routing clarity

### Objective

Make the global mempool explicitly responsible for deterministic admission, directed owner delivery, and visibility into same-sequence competition.


### Primary crates and surfaces

- `fd-mempool`
- `fd-node`
- `fd-types`
- `fd-net`

### Concrete tasks

- Normalize mempool entry metadata needed for ownership, age, and routing decisions.
- Ensure non-owner admission always has a measured, directed path to the owner lane.
- Make owner and non-owner pending counts visible in status output and metrics.
- Preserve exact duplicates as harmless while tracking sequence competitors explicitly.
- Keep account-sequence window enforcement visible and configurable.
- Make breaker reasons more intelligible for operators so terBUSY is diagnosed rather than guessed at.
- Ensure local admission accounting survives role separation and future RPC frontends.
- Review mempool drop paths for interaction with repool and reinsert behavior.


### Test plan

- Directed owner-routing unit tests.
- Sequence-window admission tests.
- Competitor retention and cleanup tests.
- RPC submit to non-owner multi-node smoke.

### Acceptance gates

- No transaction submitted to a healthy non-owner gets stranded without a visible routed-to-owner attempt.
- Mempool status clearly separates owned and not-owned backlogs.
- Repool reinsertion does not silently lose sequence intent.


### Out of scope

- Fixing creator foreign-sig merge directly.
- Changing ordering rules.
- Introducing public RPC autoscaling.

### Dependency position

- PR2 depends on the architecture clarity established by earlier PRs, especially PR1.

## PR3 — Conf-pool state machine and first-class metrics

### Objective

Turn the conf-pool from a helpful structure into an explicit, testable state machine with metrics that match the real failure modes.


### Primary crates and surfaces

- `fd-node`
- `fd-types`
- `fd-dag`

### Concrete tasks

- Introduce explicit conf-pool entry state and timestamps for body, local-sig, foreign-visible, creator-ack, parked-cert, and repool phases.
- Separate conf-pool APIs from mixed engine mutation paths where practical.
- Record creator-directed reannounce timestamps and bounded retry counters.
- Track zero-foreign duration for own opens and expose it in status output.
- Track sign deferral by cap and by policy reason.
- Make conf-pool cleanup after certify or repool auditable through counters.
- Document the pack/check duty split in code and tests.
- Retain logical compatibility with current same-binary deployment.


### Test plan

- Conf-pool entry lifecycle unit tests.
- Duty-phase tests.
- Status JSON tests for conf-pool visibility fields.
- Regression test for cleanup after certification.

### Acceptance gates

- Operators can see whether an open is waiting on body, signature, creator-ack, or parked-cert apply.
- Conf-pool counters line up with dig classifiers.
- No conf-pool entry remains invisible after it becomes a long-lived open.


### Out of scope

- Public-network scoring changes.
- Multi-host deployment changes.
- Role-profile rollout.

### Dependency position

- PR3 depends on the architecture clarity established by earlier PRs, especially PR1.

## PR4 — Creator-side foreign-signature and cert return-path repair

### Objective

Close the dominant N=5 failure mode by making peer-side progress land reliably on the creator for open batches.


### Primary crates and surfaces

- `fd-node`
- `fd-dag`
- `fd-net`

### Concrete tasks

- Prioritize zero-foreign creator-visible recovery in the conf-pool check path.
- Strengthen directed signer-to-creator reannounce of partial signatures.
- Strengthen directed signer-to-creator reannounce of certificates when cert arrives before creator merge.
- Retry parked-cert application for creator-local opens independently of new pack attempts.
- Ensure body piggyback and signature piggyback do not race destructively.
- Add counters that distinguish foreign sig merged from body, foreign sig merged from sig, and parked cert applied.
- Audit suppressions that were added to prevent storms but may hide required creator-directed retries.
- Bound all retry loops to avoid turning recovery into a flood amplifier.


### Test plan

- Targeted multi-node regression covering peer-signed-creator-missing-foreign.
- Parked-cert replay test.
- Directed reannounce timing test.
- Failure injection test where creator misses first signature but later recovers via directed path.

### Acceptance gates

- At N=5 target load, peer-signed-creator-missing-foreign events fall dramatically versus current baseline.
- Open-body repool falls correspondingly.
- Creator-owned zero-foreign duration is bounded and observable.


### Out of scope

- Changing consensus thresholds.
- Changing Bullshark ordering.
- Public token naming freeze.

### Dependency position

- PR4 depends on the architecture clarity established by earlier PRs, especially PR1.
- This PR is directly tied to the reliability findings from recent multi-lane freeze forensics.

## PR5 — Open-work liveness policy, repool discipline, and recovery guardrails

### Objective

Define and harden the lifecycle of stuck own-open work so liveness recovery is deliberate, measured, and non-chaotic.


### Primary crates and surfaces

- `fd-node`
- `fd-types`
- `fd-mempool`

### Concrete tasks

- Review and document `pack_open_cap`, `open_body_repool_after_s`, and related liveness knobs as one policy group.
- Make repool preconditions explicit: body reseed attempted, creator-directed retries attempted, parked cert retry attempted.
- Instrument repool reasons so abandoned work is diagnosable.
- Ensure tx reinsertion preserves priority and sequence semantics.
- Expose per-creator repool metrics and longest-open age.
- Add a visible distinction between softening knobs and root-cause fixes in docs and status output.
- Tune default thresholds for boring behavior at the target private-network envelope.
- Keep aggressive experimental knobs out of default production profiles until proven.


### Test plan

- Repool eligibility unit tests.
- Reinsert ordering tests.
- Config profile tests for liveness knobs.
- Longer dig ensuring defaults do not oscillate under moderate sustained load.

### Acceptance gates

- Repool becomes rare in steady healthy runs.
- When repool happens, the reason is obvious in status and metrics.
- Default liveness knobs are understandable and documented.


### Out of scope

- Role-profile process extraction.
- Archival query indexing.
- Public governance features.

### Dependency position

- PR5 depends on the architecture clarity established by earlier PRs, especially PR1.
- This PR is directly tied to the reliability findings from recent multi-lane freeze forensics.

## PR6 — Role-profile separation for validator, archival, and RPC nodes

### Objective

Make node-type separation a first-class operational concept without breaking same-binary deployment.


### Primary crates and surfaces

- `fd-node`
- `fd-store`
- `fd-order`
- `fd-exec`

### Concrete tasks

- Add explicit role or profile presets for validator, archival, and RPC operation.
- Map subsystem enablement and retention settings to those profiles.
- Ensure validator profile is protected from public RPC load by default.
- Ensure archival profile can run deep storage and replay without pack or sign duties.
- Ensure RPC profile can serve clients and owner-routing without pretending to be a validator.
- Document startup commands and expected metrics for each role.
- Add server-info visibility for profile and enabled subsystems.
- Keep existing full-role behavior available for lab and transition deployments.


### Test plan

- Startup smoke for each profile.
- Server-info profile reporting tests.
- Role-disabled subsystem behavior tests.
- Same-machine mixed deployment smoke.

### Acceptance gates

- Operators can run validator-only, archival-only, and rpc-only nodes with clear expectations.
- No profile accidentally disables verification that an honest node still needs.
- Full role remains backward compatible for lab use.


### Out of scope

- Separate binaries for every role.
- Cross-region public rollout.
- Emission scoring finalization.

### Dependency position

- PR6 depends on the architecture clarity established by earlier PRs, especially PR1.
- This PR is directly tied to role separation and deployment maturity goals.

## PR7 — Storage, snapshots, archival retention, and replay boundaries

### Objective

Make hot-state, cold-history, snapshots, and replay responsibilities explicit so role separation and recovery become routine.


### Primary crates and surfaces

- `fd-store`
- `fd-node`
- `fd-exec`
- `fd-dag`

### Concrete tasks

- Define hot versus cold retention boundaries for validator and archival profiles.
- Make snapshot metadata sufficient for deterministic replay of ordered tails.
- Ensure archival nodes can retain full body and cert history without affecting validator hot path.
- Add or clarify export and replay tooling expectations.
- Document pruning rules that preserve correctness while controlling disk growth.
- Ensure restart behavior from snapshot plus tail does not depend on manual intervention.
- Expose snapshot age and last successful restore metadata.
- Align retention strategy with hardware phase guidance.


### Test plan

- Snapshot round-trip tests.
- Replay-from-snapshot multi-node consistency tests.
- Hot/cold retention rule tests.
- Archival catch-up smoke.

### Acceptance gates

- Validator restart and catch-up are boring.
- Archival nodes can answer deep-history questions without custom scripts.
- Retention policies are explicit and measurable.


### Out of scope

- Public explorer UI.
- Third-party indexer integrations.
- Governance UX.

### Dependency position

- PR7 depends on the architecture clarity established by earlier PRs, especially PR1.
- This PR is directly tied to role separation and deployment maturity goals.

## PR8 — Task separation APIs for same-machine first, network extraction later

### Objective

Refactor runtime wiring so subsystems communicate through stable boundaries that can remain in-process now and move out-of-process later.


### Primary crates and surfaces

- `fd-node`
- `fd-net`
- `fd-store`
- `fd-mempool`
- `fd-dag`
- `fd-order`
- `fd-exec`

### Concrete tasks

- Introduce or tighten subsystem façades used by the node orchestrator.
- Replace direct cross-layer shortcuts with named calls or event-like handoffs where feasible.
- Define which APIs are sync, async, or message-driven.
- Add internal tracing that follows a tx and batch across subsystem boundaries.
- Keep same-machine performance acceptable by avoiding unnecessary serialization.
- Document the future mapping from in-process calls to IPC or network APIs.
- Ensure lock-order and mutation windows remain explicit after refactor.
- Preserve backward-compatible behavior for current scripts and tests.


### Test plan

- Boundary-conformance unit tests.
- Tracing or span-presence smoke for tx-to-batch-to-cert flow.
- Concurrent same-machine task smoke validating no deadlocks or hidden state corruption.
- Serialization tests only for boundaries intended for future transport extraction.

### Acceptance gates

- A reviewer can identify subsystem ownership from APIs instead of call-stack archaeology.
- No subsystem requires a future network extraction to function correctly today.
- Task boundaries are ready for later process or host separation.


### Out of scope

- Immediately turning everything into microservices.
- Replacing current transport stack wholesale.
- Public SDK work.

### Dependency position

- PR8 depends on the architecture clarity established by earlier PRs, especially PR1.
- This PR is directly tied to role separation and deployment maturity goals.

## PR9 — Economics freeze, emissions implementation, and public participation scoring

### Objective

Unify the public-network token, treasury, fee split, emissions, and participation scoring model into one implementation-ready plan.


### Primary crates and surfaces

- `fd-types`
- `fd-exec`
- `fd-node`
- `fd-store`

### Concrete tasks

- Reconcile FDAG substrate naming with FALC public-network docs and freeze one public model.
- Define treasury controls and no-admin-spend invariants in implementation terms.
- Freeze the fee split and treasury replenishment path.
- Freeze validator reward scoring inputs and reporting cadence.
- Freeze service-node reward pathway or explicitly defer it.
- Define bootstrap snapshot and distribution mechanics.
- Define on-ledger or auditable record shape for scoring and rotations.
- Add tests around economic state transitions where code already exists or is added.


### Test plan

- Economic config serialization tests.
- Fee split and treasury accounting tests.
- Emission schedule progression tests.
- Bootstrap snapshot determinism tests.

### Acceptance gates

- One public token model is documented and implemented consistently.
- Reward math is deterministic and auditable.
- Protocol-controlled treasury rules are enforceable by code rather than operator convention.


### Out of scope

- All future DeFi modules.
- Marketing distribution channels.
- Cross-chain production bridge finality.

### Dependency position

- PR9 depends on the architecture clarity established by earlier PRs, especially PR1.
- This PR freezes the public-network participation model and should happen before any true public rollout.

## PR10 — Reliability harnesses, digs, chaos suite, and release gates

### Objective

Turn the current valuable but partially ad hoc digs into a durable reliability program with defined promotion criteria.


### Primary crates and surfaces

- `fd-node`
- `fd-net`
- `fd-store`
- `scripts`
- `tests`

### Concrete tasks

- Standardize dig outputs and summary fields so results are comparable across runs.
- Bake current key classifications into tooling: no-body, body-no-sig, peer-signed-creator-missing-foreign, healthy.
- Add target scripts for N=3 and N=5 reliability gates.
- Add chaos variants for silent peers, delayed deliveries, restarts, and RPC pressure.
- Define which suites run per commit, nightly, and pre-release.
- Ensure role-separated topologies are covered, not only full combined nodes.
- Publish expected pass/fail criteria for prototype, private net, and public stages.
- Hook core result summaries into CI artifacts where practical.


### Test plan

- Smoke of dig tooling itself.
- Golden summary-schema tests.
- Nightly or on-demand multi-node gate runs.
- Chaos restart and catch-up drills.

### Acceptance gates

- Reliability claims are backed by repeatable scripts and summaries.
- N=5 target envelope is a formal gate, not an anecdote.
- Role-separated deployments are exercised before public rollout.


### Out of scope

- Third-party observability platform lock-in.
- Nonessential benchmark vanity runs.
- Unbounded matrix explosion in CI.

### Dependency position

- PR10 depends on the architecture clarity established by earlier PRs, especially PR1.
- This PR is directly tied to the reliability findings from recent multi-lane freeze forensics.

## PR11 — Deployment program from prototype to live private net to public participation

### Objective

Package the architecture into a staged release program with runbooks, rollback rules, and promotion evidence.


### Primary crates and surfaces

- `fd-node`
- `fd-store`
- `deploy`
- `docs`
- `scripts`

### Concrete tasks

- Define deploy profiles, startup commands, and environment assumptions for each stage.
- Write upgrade and rollback runbooks that match role separation.
- Define minimum monitoring and alerting for validator, archival, and RPC fleets.
- Freeze hardware recommendations and operator docs.
- Integrate emissions and participation guidance into deploy docs.
- Define stage-promotion checklists with required evidence artifacts.
- Run at least one full private-network dress rehearsal using the staged guidance.
- Publish public participation prerequisites only after prior gates pass.


### Test plan

- Deploy-script smoke where available.
- Config profile validation.
- Dress-rehearsal checklist review.
- Rollback dry-run verification.

### Acceptance gates

- Operators can stand up, upgrade, and recover the network with written guidance alone.
- Every stage has explicit go/no-go criteria.
- Public rollout depends on passing private-network and internal mainnet-grade gates first.


### Out of scope

- Open-ended future governance beyond documented public-participation scope.
- Feature work unrelated to deployment readiness.
- Cross-repository consolidation.

### Dependency position

- PR11 depends on the architecture clarity established by earlier PRs, especially PR1.
- This PR is directly tied to role separation and deployment maturity goals.


# 15. Detailed testing and acceptance matrix

Section 12 defines the testing philosophy and the broad acceptance gates.

Section 15 turns that philosophy into a concise matrix of recurring release artifacts, named gates, and operator checklists.

## 15.1 Per-stage gate summary

| Stage | Node count target | Primary gate | Secondary gate | Must not regress |
|-------|-------------------|--------------|----------------|------------------|
| Prototype | 3 | agreement and observability | restart and snapshot sanity | wire/body correctness |
| Private live | 5 | zero or near-zero freeze with healthy apply | role separation and replay | creator foreign-merge visibility |
| Internal mainnet-grade | 5 to 9 | boring operations and bounded storage | deploy and rollback drills | validator isolation from RPC load |
| Public participation | 5+ plus service nodes | auditable scoring and stable public RPC | hardware guide and operator onboarding | protocol-controlled treasury |

## 15.2 Functional test checklist

- transaction submit and query
- owner-routing visibility
- multi-account sequence progression
- same-sequence competition
- batch creation
- foreign signature collection
- creator foreign-merge
- certificate formation
- ordering agreement
- execution agreement
- soft-final reporting
- hard-final progression
- snapshot create and restore
- prune and retention correctness
- role-profile startup
- public economics accounting

## 15.3 Reliability metrics that must be tracked forever

- `freeze_events`
- `exec_spread`
- `hold_apply_tps`
- `open_needs_body_seed`
- `conf_pool_signed`
- `conf_pool_sign_deferred_cap`
- `foreign_sig_merged_from_sig`
- `foreign_sig_merged_from_body`
- `foreign_sig_parked`
- `foreign_sig_applied_parked`
- `sig_reannounce_to_creator`
- `reseed_suppressed_progress`
- `repool_abandoned`
- `pending_certs_n`
- `pack_limit_last`
- `pack_fair_capped`

## 15.4 Digs that should become named gates

### Gate G1 — N=3 continuous calm
- objective: prove basic boring reliability
- profile: multi-lane, standard defaults
- expected result: freeze approximately zero, repool approximately zero, full agreement

### Gate G2 — N=5 target private-net hold
- objective: prove the dominant multi-lane failure is closed at intended Phase 2 throughput
- profile: separated validator hosts preferred
- expected result: near-zero peer-signed creator-missing-foreign, zero spread, healthy hold TPS

### Gate G3 — restart under load
- objective: prove one validator restart does not wedge ordering or leave permanent creator-open ghosts
- expected result: catch-up complete, no lasting divergence

### Gate G4 — RPC pressure isolation
- objective: prove validator health is not coupled to public query load
- expected result: validator metrics remain healthy while RPC p95 rises gracefully

### Gate G5 — snapshot and replay
- objective: prove deterministic state restoration
- expected result: restored state root matches uninterrupted baseline

## 15.5 Chaos matrix

| Scenario | Safety expectation | Liveness expectation | Notes |
|----------|--------------------|----------------------|-------|
| one silent validator | cert path still safe | reduced but live where threshold allows | already aligned with existing tests |
| two silent validators | no false certification | liveness may stop correctly | must fail safely |
| delayed creator-directed sig return | no divergence | eventual creator recovery via bounded retries | key for current architecture |
| delayed body reseed | no bad cert | visibility lag measured | should not silently disappear |
| validator restart | no state divergence | bounded recovery | snapshot and replay path |
| archival lag | no effect on consensus | eventual catch-up | role separation proof |
| RPC flood | no consensus compromise | validators remain boring | infrastructure isolation |

## 15.6 Evidence artifacts for every release candidate

A release candidate should ship with:
- exact commit hash,
- role-profile config examples,
- dig summaries,
- restart drill result,
- snapshot restore result,
- storage growth snapshot,
- hardware profile used,
- and any known caveats.

---

# 16. Deployment runbook outline

## 16.1 Prototype runbook

1. Choose lab topology.
2. Generate keys.
3. Start validators.
4. Start optional archival observer.
5. Start RPC frontend if needed.
6. Run basic ping and health checks.
7. Run baseline dig.
8. Capture metrics and logs.
9. Stop, snapshot, restore, and verify state root.

## 16.2 Private live runbook

1. Provision dedicated validator hosts.
2. Apply validator profile.
3. Provision at least one archival node.
4. Provision at least two RPC nodes or one RPC plus one standby.
5. Wire centralized metrics and alerting.
6. Run controlled traffic test.
7. Run restart drill.
8. Run N=5 hold gate.
9. Record artifacts.
10. Approve or hold rollout.

## 16.3 Internal mainnet-grade runbook

1. Freeze config set.
2. Freeze release artifact hashes.
3. Deploy validators by rolling order.
4. Verify quorum health after each hop.
5. Keep RPC and archival capacity separate from validator maintenance window.
6. Run post-upgrade health dig.
7. Verify snapshots and exports.
8. Confirm alerting coverage.

## 16.4 Public participation runbook

1. Publish hardware and role docs.
2. Publish scoring and economics docs.
3. Open registration or onboarding flow.
4. Expose public RPC with rate limits.
5. Run public synthetic traffic monitoring.
6. Publish weekly or epoch evidence reports.
7. Keep rollback and emergency communication paths ready.

## 16.5 Rollback rules

Rollback should be triggered by:
- agreement failure,
- repeated freeze gate failure after rollout,
- unacceptable creator-foreign merge regressions,
- snapshot corruption,
- or uncontrolled public-service impact on validator health.

Rollback should avoid panic changes.

The release program should always keep:
- previous artifact hashes,
- previous configs,
- previous runbooks,
- and a known-good snapshot window.

---

# 17. Open questions that should be answered during the plan, not before it exists

## 17.1 Token naming freeze

Should the public network standardize on FDAG, FALC, or another single token name?

This guide does not force the answer.

PR9 must.

## 17.2 Service-node rewards

Should archival and RPC nodes share one service reward bucket or be treated separately?

This guide leaves both options open.

## 17.3 Future transport extraction

Should later subsystem separation use gRPC, QUIC, or another IPC-first abstraction?

This guide says only that APIs should be transport-ready.

It does not force the exact protocol now.

## 17.4 Public governance depth

How much of the public-governance model should ship before the first public participation phase?

This should remain conservative.

## 17.5 Mainnet terminology

This guide uses “mainnet-grade” as an engineering maturity label before true open public participation.

If the project later chooses different naming, update the stage labels but keep the staged discipline.

---

# 18. Immediate next actions after publishing this guide

1. Treat this guide as the master sequencing document.
2. Link it from the README and any future docs index.
3. Use PR1 to freeze terminology and observability contract.
4. Use PR4 and PR5 to attack the currently dominant N=5 reliability gap.
5. Use PR6 through PR8 to make role separation real without forcing premature microservices.
6. Use PR9 to reconcile economics and public participation into one auditable model.
7. Use PR10 and PR11 to turn technical improvements into a release program.

---

# 19. Final north-star statement

Falcon-DAG should evolve into a system where:
- the global mempool admits and routes work deterministically,
- the global conf-pool makes open-batch certification progress visible and recoverable,
- validators sign and order without being burdened by public-RPC noise,
- archival nodes preserve the full truth of history,
- RPC nodes serve clients without becoming hidden consensus dependencies,
- public-network rewards are protocol-defined and auditable,
- and deployment stages are earned through boring operational evidence.

That is the north star.

It is ambitious.

It is also compatible with the repository’s existing design direction.

The required work is now organized into a concrete sequence.

---
# Appendix A. Glossary and implementation shorthand

- **Global mempool** — Logical network-wide candidate transaction set represented by converging local replicas.
- **Global conf-pool** — Logical network-wide open-batch progress workspace represented by converging local replicas.
- **Open batch** — Uncertified batch currently in the DAG and conf-pool workflow.
- **Creator ack** — The creator-local observation that foreign signature or cert progress has landed.
- **Parked cert** — Certificate observed but not yet fully applied because prerequisites are still catching up.
- **Soft final** — Included in a certified batch.
- **Hard final** — Ordered and followed by the configured hard-final depth.
- **Owner lane** — Deterministic validator responsible for primary packing of an account flow.
- **Repool** — Reinsertion of txs from a stuck open batch back into the mempool after bounded recovery attempts.
- **Pack/check duty** — Alternating windows for creating work and checking or advancing open work.
- **Archival node** — History-retaining node profile optimized for replay and deep queries.
- **RPC node** — Client-facing node profile optimized for admission and query service.
- **Validator node** — Consensus-participating node profile that packs, signs, orders, and executes.

# Appendix B. Checklist view of the 11 PRs

## PR1 checklist

- [x] Confirm objective: Create the explicit architecture, metrics, and interface baseline so later PRs change behavior without changing the mental model every week.
- [x] Confirm crate boundaries and owner reviewers
- [x] Document the authoritative architecture string and role taxonomy in code comments and docs.
- [x] Freeze the names of key metrics for mempool, conf-pool, batch-status, parked certs, and creator-directed retries.
- [x] Add or normalize `batch_status` style debug surfaces that distinguish no-body, body-no-sig, peer-signed-creator-missing-foreign, and healthy cases.
- [x] Define explicit configuration defaults for validator, archival, and RPC role profiles without forcing transport extraction yet.
- [x] Write boundary notes for admission, conf-pool, DAG, ordering, execution, store, and RPC subsystems.
- [x] Record the current N=3 and N=5 reliability baselines as engineering targets, not marketing claims.
- [x] Decide which metrics are public, operator-only, and debug-only.
- [x] Ensure the README and docs index point to the master guide and role docs.
- [x] Gate: All core architecture docs agree on terms for mempool, conf-pool, cert, soft final, and hard final.
- [x] Gate: Developers can answer where a feature belongs by subsystem without reading engine internals.
- [x] Gate: Metrics names used by digs are frozen and tested.

## PR2 checklist

- [x] Confirm objective: Make the global mempool explicitly responsible for deterministic admission, directed owner delivery, and visibility into same-sequence competition.
- [x] Confirm crate boundaries and owner reviewers
- [x] Normalize mempool entry metadata needed for ownership, age, and routing decisions.
- [x] Ensure non-owner admission always has a measured, directed path to the owner lane.
- [x] Make owner and non-owner pending counts visible in status output and metrics.
- [x] Preserve exact duplicates as harmless while tracking sequence competitors explicitly.
- [x] Keep account-sequence window enforcement visible and configurable.
- [x] Make breaker reasons more intelligible for operators so terBUSY is diagnosed rather than guessed at.
- [x] Ensure local admission accounting survives role separation and future RPC frontends.
- [x] Review mempool drop paths for interaction with repool and reinsert behavior.
- [x] Gate: No transaction submitted to a healthy non-owner gets stranded without a visible routed-to-owner attempt.
- [x] Gate: Mempool status clearly separates owned and not-owned backlogs.
- [x] Gate: Repool reinsertion does not silently lose sequence intent.

## PR3 checklist

- [x] Confirm objective: Turn the conf-pool from a helpful structure into an explicit, testable state machine with metrics that match the real failure modes.
- [x] Confirm crate boundaries and owner reviewers
- [x] Introduce explicit conf-pool entry state and timestamps for body, local-sig, foreign-visible, creator-ack, parked-cert, and repool phases.
- [x] Separate conf-pool APIs from mixed engine mutation paths where practical.
- [x] Record creator-directed reannounce timestamps and bounded retry counters.
- [x] Track zero-foreign duration for own opens and expose it in status output.
- [x] Track sign deferral by cap and by policy reason.
- [x] Make conf-pool cleanup after certify or repool auditable through counters.
- [x] Document the pack/check duty split in code and tests.
- [x] Retain logical compatibility with current same-binary deployment.
- [x] Gate: Operators can see whether an open is waiting on body, signature, creator-ack, or parked-cert apply.
- [x] Gate: Conf-pool counters line up with dig classifiers.
- [x] Gate: No conf-pool entry remains invisible after it becomes a long-lived open.

## PR4 checklist

- [x] Confirm objective: Close the dominant N=5 failure mode by making peer-side progress land reliably on the creator for open batches.
- [x] Confirm crate boundaries and owner reviewers
- [x] Prioritize zero-foreign creator-visible recovery in the conf-pool check path.
- [x] Strengthen directed signer-to-creator reannounce of partial signatures.
- [x] Strengthen directed signer-to-creator reannounce of certificates when cert arrives before creator merge.
- [x] Retry parked-cert application for creator-local opens independently of new pack attempts.
- [x] Ensure body piggyback and signature piggyback do not race destructively.
- [x] Add counters that distinguish foreign sig merged from body, foreign sig merged from sig, and parked cert applied.
- [x] Audit suppressions that were added to prevent storms but may hide required creator-directed retries.
- [x] Bound all retry loops to avoid turning recovery into a flood amplifier.
- [x] Gate: At N=5 target load, peer-signed-creator-missing-foreign events fall dramatically versus current baseline.
- [x] Gate: Open-body repool falls correspondingly.
- [x] Gate: Creator-owned zero-foreign duration is bounded and observable.

## PR5 checklist

- [x] Confirm objective: Define and harden the lifecycle of stuck own-open work so liveness recovery is deliberate, measured, and non-chaotic.
- [x] Confirm crate boundaries and owner reviewers
- [x] Review and document `pack_open_cap`, `open_body_repool_after_s`, and related liveness knobs as one policy group.
- [x] Make repool preconditions explicit: body reseed attempted, creator-directed retries attempted, parked cert retry attempted.
- [x] Instrument repool reasons so abandoned work is diagnosable.
- [x] Ensure tx reinsertion preserves priority and sequence semantics.
- [x] Expose per-creator repool metrics and longest-open age.
- [x] Add a visible distinction between softening knobs and root-cause fixes in docs and status output.
- [x] Tune default thresholds for boring behavior at the target private-network envelope.
- [x] Keep aggressive experimental knobs out of default production profiles until proven.
- [x] Gate: Repool becomes rare in steady healthy runs.
- [x] Gate: When repool happens, the reason is obvious in status and metrics.
- [x] Gate: Default liveness knobs are understandable and documented.

## PR6 checklist

- [x] Confirm objective: Make node-type separation a first-class operational concept without breaking same-binary deployment.
- [x] Confirm crate boundaries and owner reviewers
- [x] Add explicit role or profile presets for validator, archival, and RPC operation.
- [x] Map subsystem enablement and retention settings to those profiles.
- [x] Ensure validator profile is protected from public RPC load by default.
- [x] Ensure archival profile can run deep storage and replay without pack or sign duties.
- [x] Ensure RPC profile can serve clients and owner-routing without pretending to be a validator.
- [x] Document startup commands and expected metrics for each role.
- [x] Add server-info visibility for profile and enabled subsystems.
- [x] Keep existing full-role behavior available for lab and transition deployments.
- [x] Gate: Operators can run validator-only, archival-only, and rpc-only nodes with clear expectations.
- [x] Gate: No profile accidentally disables verification that an honest node still needs.
- [x] Gate: Full role remains backward compatible for lab use.

## PR7 checklist

- [x] Confirm objective: Make hot-state, cold-history, snapshots, and replay responsibilities explicit so role separation and recovery become routine.
- [x] Confirm crate boundaries and owner reviewers
- [x] Define hot versus cold retention boundaries for validator and archival profiles.
- [x] Make snapshot metadata sufficient for deterministic replay of ordered tails.
- [x] Ensure archival nodes can retain full body and cert history without affecting validator hot path.
- [x] Add or clarify export and replay tooling expectations.
- [x] Document pruning rules that preserve correctness while controlling disk growth.
- [x] Ensure restart behavior from snapshot plus tail does not depend on manual intervention.
- [x] Expose snapshot age and last successful restore metadata.
- [x] Align retention strategy with hardware phase guidance.
- [x] Gate: Validator restart and catch-up are boring.
- [x] Gate: Archival nodes can answer deep-history questions without custom scripts.
- [x] Gate: Retention policies are explicit and measurable.

## PR8 checklist

- [x] Confirm objective: Refactor runtime wiring so subsystems communicate through stable boundaries that can remain in-process now and move out-of-process later.
- [x] Confirm crate boundaries and owner reviewers
- [x] Introduce or tighten subsystem façades used by the node orchestrator.
- [x] Replace direct cross-layer shortcuts with named calls or event-like handoffs where feasible.
- [x] Define which APIs are sync, async, or message-driven.
- [x] Add internal tracing that follows a tx and batch across subsystem boundaries.
- [x] Keep same-machine performance acceptable by avoiding unnecessary serialization.
- [x] Document the future mapping from in-process calls to IPC or network APIs.
- [x] Ensure lock-order and mutation windows remain explicit after refactor.
- [x] Preserve backward-compatible behavior for current scripts and tests.
- [x] Gate: A reviewer can identify subsystem ownership from APIs instead of call-stack archaeology.
- [x] Gate: No subsystem requires a future network extraction to function correctly today.
- [x] Gate: Task boundaries are ready for later process or host separation.

## PR9 checklist

- [x] Confirm objective: Unify the public-network token, treasury, fee split, emissions, and participation scoring model into one implementation-ready plan.
- [x] Confirm crate boundaries and owner reviewers
- [x] Reconcile FDAG substrate naming with FALC public-network docs and freeze one public model.
- [x] Define treasury controls and no-admin-spend invariants in implementation terms.
- [x] Freeze the fee split and treasury replenishment path.
- [x] Freeze validator reward scoring inputs and reporting cadence.
- [x] Freeze service-node reward pathway or explicitly defer it.
- [x] Define bootstrap snapshot and distribution mechanics.
- [x] Define on-ledger or auditable record shape for scoring and rotations.
- [x] Add tests around economic state transitions where code already exists or is added.
- [x] Gate: One public token model is documented and implemented consistently.
- [x] Gate: Reward math is deterministic and auditable.
- [x] Gate: Protocol-controlled treasury rules are enforceable by code rather than operator convention.

## PR10 checklist

- [x] Confirm objective: Turn the current valuable but partially ad hoc digs into a durable reliability program with defined promotion criteria.
- [x] Confirm crate boundaries and owner reviewers
- [x] Standardize dig outputs and summary fields so results are comparable across runs.
- [x] Bake current key classifications into tooling: no-body, body-no-sig, peer-signed-creator-missing-foreign, healthy.
- [x] Add target scripts for N=3 and N=5 reliability gates.
- [x] Add chaos variants for silent peers, delayed deliveries, restarts, and RPC pressure.
- [x] Define which suites run per commit, nightly, and pre-release.
- [x] Ensure role-separated topologies are covered, not only full combined nodes.
- [x] Publish expected pass/fail criteria for prototype, private net, and public stages.
- [x] Hook core result summaries into CI artifacts where practical.
- [x] Gate: Reliability claims are backed by repeatable scripts and summaries.
- [x] Gate: N=5 target envelope is a formal gate, not an anecdote.
- [x] Gate: Role-separated deployments are exercised before public rollout.

## PR11 checklist

- [x] Confirm objective: Package the architecture into a staged release program with runbooks, rollback rules, and promotion evidence.
- [x] Confirm crate boundaries and owner reviewers
- [x] Define deploy profiles, startup commands, and environment assumptions for each stage.
- [x] Write upgrade and rollback runbooks that match role separation.
- [x] Define minimum monitoring and alerting for validator, archival, and RPC fleets.
- [x] Freeze hardware recommendations and operator docs.
- [x] Integrate emissions and participation guidance into deploy docs.
- [x] Define stage-promotion checklists with required evidence artifacts.
- [x] Run at least one full private-network dress rehearsal using the staged guidance.
- [x] Publish public participation prerequisites only after prior gates pass.
- [x] Gate: Operators can stand up, upgrade, and recover the network with written guidance alone.
- [x] Gate: Every stage has explicit go/no-go criteria.
- [x] Gate: Public rollout depends on passing private-network and internal mainnet-grade gates first.
