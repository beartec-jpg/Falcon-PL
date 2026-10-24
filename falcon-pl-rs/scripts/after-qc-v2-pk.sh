#!/usr/bin/env bash
# Run AFTER falcon-qc-v2-one/pk.bin exists. Does not broadcast unless FPL_PRODUCTION=1.
# Does not arm qc_start_height. Does not restart the fleet.
set -euo pipefail
PL_ROOT="${PL_ROOT:-/home/falcon/falcon-pl-public-testnet-2300}"
REPO="${REPO:-/home/falcon/falcon-pl/falcon-pl-rs}"
SNARK_DIR="${SNARK_DIR:-$PL_ROOT/run/snark/falcon-qc-v2-one}"

if [[ ! -s "$SNARK_DIR/pk.bin" || ! -f "$SNARK_DIR/ptau.json" ]]; then
  echo "waiting: no v2 pk.bin yet at $SNARK_DIR" >&2
  exit 2
fi
if ! grep -q FplFalconQcCircuitV2 "$SNARK_DIR/ptau.json"; then
  echo "FATAL: $SNARK_DIR/ptau.json is not circuit v2" >&2
  exit 2
fi

echo "==> v2 keys"
ls -lh "$SNARK_DIR/pk.bin" "$SNARK_DIR/vk.json" "$SNARK_DIR/vk.sol.json" "$SNARK_DIR/ptau.json"

echo "==> ctor fields from live tip (expect missing until v4 armed)"
python3 "$REPO/scripts/dump-qc-v2-ctor-args.py" || true

echo
echo "Next, in order (do not skip):"
echo "  1. cargo build -p fd-pl --release   # after ceremony frees RAM"
echo "  2. rolling restart 6 seats with NEW binary, keys/lc/bridge.json still qc_start_height=0"
echo "  3. set qc_header_version=4 and qc_start_height=<agreed tip> on all seats, restart"
echo "  4. wait dump-qc-v2-ctor-args.py exit 0"
echo "  5. FPL_PRODUCTION=1 bash $REPO/scripts/deploy-eth-sepolia-qc-v2.sh"
echo "  6. set FALCON_QC_V2_BRIDGE / FPL_SEPOLIA_QC_V2_BRIDGE, start v2 prover"
echo "  7. wallet qc_version=2 only after headers move; Kickoff leftover via DESTLOCK_EXIT=1"
echo
echo "Refuses: live deploy without FPL_PRODUCTION=1, arming v4 in this script, dest-lock self-dest."
