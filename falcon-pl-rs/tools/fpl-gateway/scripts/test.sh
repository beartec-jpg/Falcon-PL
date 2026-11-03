#!/usr/bin/env bash
# One command: fmt check (if available), clippy-free build, unit + integration tests.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --locked --all-targets 2>&1
