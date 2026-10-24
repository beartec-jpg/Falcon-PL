# Contributing & Public Review Guidelines

Falcon PL is open for public code review and testnet participation. We welcome technical feedback, bug reports, test cases, and pull requests.

## How to Submit Review Findings or Bug Reports

- **Security vulnerabilities:** Follow [`SECURITY.md`](SECURITY.md) and use GitHub Private Vulnerability Reporting rather than public issues.
- **Non-security bugs, documentation fixes & review notes:** Open a GitHub Issue or Pull Request with:
  - Exact crate/file/line references;
  - Steps or a `cargo test` command to reproduce;
  - Expected vs. actual behavior.

## Building & Running Tests Locally

From `falcon-pl-rs/`:

```bash
# Typecheck all active workspace crates
cargo check --workspace

# Run core consensus, key-registry, bridge, and rewards tests
cargo test -p fd-pl --lib
cargo test -p fd-pl-bitvm2
cargo test -p fd-pl-graph
```

## Running a Node or Bridge Operator on Testnet 2300

- **Lite node, watcher, or validator:** See [`docs/PUBLIC_NODE.md`](docs/PUBLIC_NODE.md) and [`docs/SEAT_PAYOUT.md`](docs/SEAT_PAYOUT.md).
- **Bitcoin Testnet4 (`BTC4`) BitVM2 operator:** See [`docs/bridge/OPERATOR_GUIDE.md`](docs/bridge/OPERATOR_GUIDE.md) and [`docs/bridge/OPERATOR_REWARDS.md`](docs/bridge/OPERATOR_REWARDS.md).

## License & Copyright

All code and documentation in this repository are `Copyright (c) BEARTEC. All Rights Reserved.` (see [`LICENSE`](LICENSE)). By submitting a pull request or patch to this repository, you agree that BEARTEC may use, modify, incorporate, and distribute your contribution as part of Falcon PL under BEARTEC's copyright and license terms.
