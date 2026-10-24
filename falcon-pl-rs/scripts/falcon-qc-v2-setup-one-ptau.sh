#!/usr/bin/env bash
# OPEN=1 Falcon QC v2 Groth16 from a public Hermez/PSE .ptau, then δ contribute + beacon.
# New circuit (FplFalconQcCircuitV2, 16 public inputs, no free claimRoot).
# Do NOT copy run/snark/falcon-qc-one/pk.bin. Do not --test-seed.
#
# Required: FPL_PTAU=/path/to/powersOfTau28_hez_final_23.ptau  (or _24)
# Optional:
#   SNARK_DIR           default $PL_ROOT/run/snark/falcon-qc-v2-one
#   FPL_SNARK           falcon-pl-snark binary
#   FPL_GROTH16_TAG     contribute tag
#   FPL_GROTH16_BEACON  hex entropy; default = latest Sepolia block hash
#   FPL_GROTH16_SKIP_BEACON=1
#   FORCE=1
set -euo pipefail

PL_ROOT="${PL_ROOT:-/home/falcon/falcon-pl-public-testnet-2300}"
REPO="${REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
SNARK_DIR="${SNARK_DIR:-$PL_ROOT/run/snark/falcon-qc-v2-one}"
SNARK="${FPL_SNARK:-$PL_ROOT/bin/falcon-pl-snark}"
ETH_RPC="${ETH_RPC:-https://ethereum-sepolia-rpc.publicnode.com}"
CAST="${CAST:-$HOME/.foundry/bin/cast}"
TAG="${FPL_GROTH16_TAG:-$(hostname)-$(date -u +%Y%m%dT%H%M%SZ)}"

if [[ ! -x "$SNARK" && -x "$REPO/target/release/falcon-pl-snark" ]]; then
  SNARK="$REPO/target/release/falcon-pl-snark"
fi
if [[ ! -x "$SNARK" ]]; then
  echo "FATAL: no falcon-pl-snark at $SNARK" >&2
  exit 2
fi
if [[ -z "${FPL_PTAU:-}" || ! -f "${FPL_PTAU}" ]]; then
  echo "FATAL: set FPL_PTAU to powersOfTau28_hez_final_23 (or _24) .ptau" >&2
  exit 2
fi
if [[ "$SNARK_DIR" == *falcon-qc-one* && "$SNARK_DIR" != *falcon-qc-v2-one* ]]; then
  echo "FATAL: refusing to write v2 keys over v1 falcon-qc-one" >&2
  exit 2
fi

install -d "$SNARK_DIR" "$SNARK_DIR/ceremony"
WORK="$SNARK_DIR/ceremony"

install_keys() {
  local src="$1"
  local stem="${src%.bin}"
  cp -f "$src" "$SNARK_DIR/pk.bin"
  cp -f "${stem}.vk.bin" "$SNARK_DIR/vk.bin"
  cp -f "${stem}.vk.json" "$SNARK_DIR/vk.json"
  cp -f "${stem}.vk.sol.json" "$SNARK_DIR/vk.sol.json"
}

if [[ -f "$SNARK_DIR/ceremony.json" && -f "$SNARK_DIR/pk.bin" && "${FORCE:-0}" != "1" ]]; then
  echo "existing OPEN=1 v2 ceremony at $SNARK_DIR (set FORCE=1 to redo)"
  exit 0
fi
if [[ -f "$SNARK_DIR/pk.bin" && -f "$SNARK_DIR/ptau.json" && "${FORCE:-0}" != "1" ]]; then
  echo "ptau pk present (δ may still be 1); contribute + beacon only"
else
  echo "==> falcon-setup-one-from-ptau --domain v2"
  "$SNARK" falcon-setup-one-from-ptau --ptau "$FPL_PTAU" --out-dir "$SNARK_DIR" --domain v2
fi

echo "==> contribute tag=$TAG"
"$SNARK" contribute --pk "$SNARK_DIR/pk.bin" --out "$WORK/contrib.bin" --tag "$TAG" \
  | tee "$WORK/contribute-$TAG.log"
install_keys "$WORK/contrib.bin"
cp -f "$WORK/contrib.bin" "$WORK/after-contribute.pk.bin"

if [[ "${FPL_GROTH16_SKIP_BEACON:-0}" == "1" ]]; then
  echo "skipped beacon (δ contribution only). Not production."
  exit 0
fi

BEACON="${FPL_GROTH16_BEACON:-}"
if [[ -z "$BEACON" ]]; then
  if [[ ! -x "$CAST" ]]; then
    echo "FATAL: set FPL_GROTH16_BEACON (hex) or install cast for Sepolia block-hash beacon" >&2
    exit 2
  fi
  BLOCK_NUM=$("$CAST" block-number --rpc-url "$ETH_RPC")
  BHASH=$("$CAST" block "$BLOCK_NUM" --rpc-url "$ETH_RPC" --json | python3 -c "import sys,json; print(json.load(sys.stdin).get('hash') or '')")
  if [[ -z "$BHASH" ]]; then
    echo "FATAL: could not read Sepolia block hash for beacon" >&2
    exit 2
  fi
  echo "beacon entropy = Sepolia #$BLOCK_NUM $BHASH"
  BEACON="$BHASH"
fi
BEACON="${BEACON#0x}"

echo "==> beacon"
"$SNARK" beacon --pk "$SNARK_DIR/pk.bin" --out "$WORK/beacon.bin" --entropy "$BEACON" --tag "beacon" \
  | tee "$WORK/beacon.log"
install_keys "$WORK/beacon.bin"

python3 - "$SNARK_DIR/ceremony.json" "$FPL_PTAU" "$TAG" "$BEACON" <<'PY'
import json, sys
from pathlib import Path
out, ptau, tag, beacon = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
doc = {
    "circuit": "FplFalconQcCircuitV2",
    "domain": "v2",
    "n_public": 16,
    "ptau_path": ptau,
    "contribute_tag": tag,
    "beacon_entropy": "0x" + beacon.lower().removeprefix("0x"),
    "note": "τ from public ptau; δ from contribute then public beacon. Not a mainnet MPC. Do not reuse v1 pk.",
}
Path(out).write_text(json.dumps(doc, indent=2) + "\n")
print("wrote", out)
PY

echo "==> OPEN=1 v2 ceremony keys in $SNARK_DIR"
ls -l "$SNARK_DIR/pk.bin" "$SNARK_DIR/vk.sol.json" "$SNARK_DIR/ptau.json"
echo "next: FPL_PRODUCTION=1 bash falcon-pl-rs/scripts/deploy-eth-sepolia-qc-v2.sh"
