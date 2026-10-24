#!/usr/bin/env bash
# Private rewards soak: 30s epochs, emit from epoch 1 (no mainnet bootstrap).
# Does not change public 2300 defaults (still first_claim=8, 7d epoch).
set -euo pipefail
export PL_EPOCH_MS="${PL_EPOCH_MS:-30000}"
export PL_FIRST_CLAIM_EPOCH="${PL_FIRST_CLAIM_EPOCH:-1}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
echo "[rewards] epoch_ms=$PL_EPOCH_MS first_claim_epoch=$PL_FIRST_CLAIM_EPOCH (testnet; mainnet stays 8 / 7d)"
exec bash "$SCRIPT_DIR/pl-start-2.9.0-soak.sh" "${1:-start}"
