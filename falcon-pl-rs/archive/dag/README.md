# Archived Falcon DAG Rust crates

Frozen Narwhal-style DA + ordering stack. **Not** Falcon PL.

| Crate | Former role |
|-------|-------------|
| `fd-types` | DAG architecture types |
| `fd-crypto` | DAG crypto |
| `fd-mempool` | Lane mempool |
| `fd-dag` | DA graph |
| `fd-order` | Bullshark ordering |
| `fd-exec` | Execution |
| `fd-net` | Network |
| `fd-store` | Storage |
| `fd-node` | `falcon-dagd` binary |

These are **not** workspace members. To experiment offline, create a separate
Cargo workspace under this tree — do not merge back into PL without an amendment.

Product path: `../../crates/fd-pl`.
