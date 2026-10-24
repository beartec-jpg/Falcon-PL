# Archive — not product

Everything under `archive/` is **historical research** from the Falcon DAG era
(Narwhal-style mempool DA + Bullshark ordering experiments).

**Do not** treat this as the Falcon PL product path.  
**Do not** add new product features here.  
**Do not** re-add archived crates to the live Cargo workspace without an explicit AMD.

| Directory | What |
|-----------|------|
| `dag-python/` | Original Python prototype (`src/`, `contracts/`, regtest) |
| `dag-docs/` | Design notes, TPS digs, architecture contracts |
| `dag-deploy/` | Old deploy layouts / public-testnet experiments |

Rust DAG crates live at `falcon-dag-rs/archive/dag/crates/` so they stay near the
historical workspace root but are **excluded from members**.

Active product: **`falcon-dag-rs/crates/fd-pl`** → GitHub **Falcon-PL**.
