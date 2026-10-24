#!/usr/bin/env bash
# Grep-gates for lottery-6 V2: no SAMPLE_GAP jump in v2 units; no default kickoff.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
fail=0
if grep -n 'SAMPLE_GAP\|pick_on_grid' "$ROOT/pl-falcon-qc-v2-prover.py"; then
  echo "FAIL: v2 prover must not use SAMPLE_GAP/pick_on_grid" >&2
  fail=1
fi
if grep -n 'SAMPLE_GAP\|pick_on_grid' "$ROOT/pl-falcon-qc-v2-catchup.py"; then
  echo "FAIL: v2 catchup must not use SAMPLE_GAP" >&2
  fail=1
fi
if grep -n 'kickoff' "$ROOT/pl-falcon-qc-v2-prover.py"; then
  echo "FAIL: v2 prover must not kickoff" >&2
  fail=1
fi
if ! grep -q 'FALCON_QC_V2' "$ROOT/pl-bridge-e2e-wait.py"; then
  echo "FAIL: e2e-wait missing FALCON_QC_V2 sequential path" >&2
  fail=1
fi
if grep -n 'kickoff(' "$ROOT/pl-eth-sepolia-e2e.py"; then
  echo "FAIL: e2e must not call kickoff()" >&2
  fail=1
fi
if [[ $fail -ne 0 ]]; then
  exit 1
fi
echo "ok lottery6 v2 grep-gates"
