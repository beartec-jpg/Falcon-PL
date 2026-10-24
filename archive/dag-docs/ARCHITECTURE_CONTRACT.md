# Architecture & observability contract (PR1)

This document freezes terms and surfaces so later PRs change **behavior** without renaming the mental model every week.

**Code source of truth:** `falcon-dag-rs/crates/fd-types/src/architecture.rs`  
**Master plan:** [IMPLEMENTATION_GUIDE.md](IMPLEMENTATION_GUIDE.md)

## Architecture string

```
Narwhal-style multipack DA (logical-global mempool + conf-pool north star)
+ UNL certificates + Bullshark ordering over finalized certs
```

Exposed as `fd_types::ARCHITECTURE_STRING` and JSON-RPC method `architecture`.

## Terms

| Term | Meaning |
|------|---------|
| mempool | Pending txs awaiting pack (logical-global north star; process-local today) |
| conf_pool / conf-pool | Open batches awaiting UNL weight |
| certificate | Soft-final DA attestation when UNL weight ≥ `unl_threshold` |
| soft_final | Tx in a certified batch body |
| hard_final | Tx ordered under `hard_finality_h` commit depth |
| lane | Multipack packing domain owned by one UNL validator |

## Roles vs profiles

- **`NodeRole`** (`--role`): process capability — `da-worker` | `order-exec` | `full`.
- **`NodeProfile`** (`--node-profile`): deployment intent — `validator` | `archival` | `rpc`.

Profiles are orthogonal to roles. Use `--force-node-profile` to apply profile knob defaults (retention, admit cap, multipack).

## Batch DA gap classes (`batch_status.gap_class`)

Frozen SCREAMING_SNAKE labels used by digs:

| Class | Meaning |
|-------|---------|
| `NO_BODY_ON_PEERS` | No peer has the body |
| `PEER_HAS_BODY_NO_SIG` | Peers have body; none signed yet |
| `PEER_SIGNED_CREATOR_MISSING_FOREIGN` | ≥1 peer signed; creator still nsigs≤1 (Mode B) |
| `HAS_FOREIGN_OR_OK` | Creator has foreign weight / multi-sig path healthy |
| `CERTIFIED` | Local certified |
| `UNKNOWN` | Insufficient data |

`BatchDaGapClass::classify(...)` is the pure classifier. RPC `batch_status` accepts optional `any_peer_has_body` / `any_peer_signed` so digs can pass cross-node inputs; otherwise local view is used.

## Frozen metric keys

See `FROZEN_METRIC_KEYS` in code (tested for uniqueness). Dig suite depends on names such as:

- `executed_txs`, `mempool`, `open_needs_body_seed`, `open_conf`
- `foreign_sig_merged_from_sig`, `foreign_sig_merged_from_body`
- `sig_reannounce_to_creator`, `try_certify_ok`
- `pack_skip_reasons` / `admit_reasons` histograms

Tiers: **public** | **operator** | **debug**.

## Subsystem boundaries

| Subsystem | Owns |
|-----------|------|
| admission | `fd-mempool` + engine submit/admit — not ordering |
| conf_pool | `fd-node` conf_pool; scheduled sign of open bodies — not Bullshark |
| dag | `fd-dag` bodies, partial sigs, certificates |
| ordering | `fd-order` Bullshark over finalized certs |
| execution | `fd-exec` apply in ordered sequence |
| store | `fd-store` snapshots / durability |
| rpc | `fd-node` JSON-RPC surface only |

## Dig reliability baselines

Engineering targets (not marketing SLAs): `DIG_BASELINES` in code (`N3_localhost_p20`, `N5_localhost_p40`, dual-WAN, …).

## Out of scope for PR1

- Core protocol semantics changes
- Public-network economics finalization
- Transport-layer rewrite
