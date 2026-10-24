#!/usr/bin/env bash
# Fetch BitVM upstream for Phase C.1b (shallow).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/vendor/BitVM"
REPO="${BITVM_REPO:-https://github.com/BitVM/BitVM.git}"
# Keep in sync with fd-pl-bitvm2 bn254_pack::UPSTREAM_COMMIT when bumping.
PIN="${BITVM_COMMIT:-7d1ca3660cac08aab62e76f3aa4daec0d7403ecc}"

mkdir -p "$ROOT/vendor"
if [[ -d "$DEST/.git" ]]; then
  git -C "$DEST" fetch --depth 1 origin "$PIN"
  git -C "$DEST" checkout --detach "$PIN"
else
  git clone --depth 1 "$REPO" "$DEST"
  git -C "$DEST" fetch --depth 1 origin "$PIN" 2>/dev/null || true
  git -C "$DEST" checkout --detach "$PIN" 2>/dev/null || git -C "$DEST" checkout --detach HEAD
fi
echo "BitVM at $(git -C "$DEST" rev-parse HEAD) → $DEST"
