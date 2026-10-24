# Falcon PL Rust Workspace (`2.9.81`)

Active workspace members for live public testnet **2300** (`v2.9.81`, height `692,000+`):

- **`crates/fd-pl`** — Falcon PL core node (`falcon-pl-node`), CLI (`falcon-pl-ctl`), Falcon-512 consensus, disk-backed body store, Rewards v2, ETH Gloas light client, and BTC/BTC4 SPV & BitVM2 state machine.
- **`crates/fd-pl-snark`** — Groth16 over BN254 circuits and CLI (`falcon-pl-snark`) for 4-of-6 Falcon-512 QC verification (`FalconQcBridgeV3`).
- **`crates/fd-pl-bitvm2`** — BitVM2 withdrawal validity circuit (`FplWithdrawValid`), verifier trace split, and BIP341 Disprove taptree engine.
- **`crates/fd-pl-graph`** — Presigned BitVM2 transaction graph primitives.

Standalone operator and bridge tools under `tools/`:
- **`tools/fpl-operator`** — `BTC4` BitVM2 operator daemon (`fpl-operatord`), peer gossip, capacity advertising, and automated challenge/disprove runner.
- **`tools/fpl-bridge`** — BitVM2 ceremony coordinator, participant, challenger, and regtest/testnet4 verification suite.
- **`tools/bitvm-c1b-export`** — BitVM BN254 script chunk digest exporter.

Docs: [crates/fd-pl/docs/README.md](crates/fd-pl/docs/README.md) and root [`docs/`](../docs/).

```bash
cargo check --workspace
cargo test -p fd-pl --lib
cargo build -p fd-pl --release
```

Historical DAG crates under [`archive/dag/`](archive/dag/) are frozen and excluded from the workspace.
