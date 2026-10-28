# Security Policy

## Scope

Falcon PL is open for public technical and security review. The primary components in scope for review are:

- **Consensus, execution & node runtime:** `falcon-pl-rs/crates/fd-pl` (`2.10.0`, Falcon-512 post-quantum signatures, deterministic committee lottery + 4-of-6 QC, Rewards v2, on-chain key registry, ETH Gloas beacon light client, Bitcoin SPV & BitVM2 state machine)
- **Groth16 Falcon-512 QC prover & verifier circuits:** `falcon-pl-rs/crates/fd-pl-snark`
- **BitVM2 operator & graph crates:** `falcon-pl-rs/crates/fd-pl-bitvm2`, `falcon-pl-rs/crates/fd-pl-graph`, and `falcon-pl-rs/tools/fpl-operator`
- **Sepolia ETH/USDC V3 bridge contract:** `falcon-pl-rs/crates/fd-pl/scripts/FalconQcBridgeV3.sol` and `FalconQcBridgeV3Checks.sol`

Archived research prototypes under `archive/` and `falcon-pl-rs/archive/` are retained for historical reference only and are out of scope.

## Reporting a Vulnerability

Please **do not** open a public GitHub issue for suspected consensus, cryptographic, or bridge vulnerabilities that could affect live Testnet 2300 operations.

Instead, report vulnerabilities privately via:
1. **GitHub Private Vulnerability Reporting:** Use the **Report a vulnerability** button under the **Security** tab of this repository (`beartec-jpg/Falcon-PL`).

Please include:
- The affected component, crate, or contract and file/line references;
- A clear description of the invariant violated and the potential impact (e.g., consensus safety/liveness, signature verification, Groth16 circuit soundness, BitVM2 graph/timelock safety, or EVM bridge accounting);
- A minimal reproduction test (unit test, regtest scenario, or Foundry test).

## Testnet 2300 Rules of Engagement

- **Local & regtest verification first:** Reproduce consensus, SNARK, or BitVM2 findings in local unit tests (`cargo test`), regtest (`fpl-operator` / `fpl-bridge`), or local Foundry tests rather than against the shared public Testnet 2300 mesh.
- **No denial-of-service:** Do not flood public seed relays, RPC endpoints, or validator nodes with network/resource exhaustion traffic.
- Honest security researchers who follow this policy act within the Public Review grant in [`LICENSE`](LICENSE).
