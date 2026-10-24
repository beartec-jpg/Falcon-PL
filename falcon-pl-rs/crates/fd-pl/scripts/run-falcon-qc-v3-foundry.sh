#!/usr/bin/env bash
# Foundry build + tests for FalconQcBridgeV3 + FalconQcBridgeV3Checks (mock verifier, mock V1/V2/DestLock).
# Also compiles Groth16Verifier.sol. testRealGroth16VerifierV2Fixture runs only when
# fixtures/v2-groth16/plus1-submit.json is present. No deploy, no RPC, no transactions.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
SCRIPTS="$REPO/crates/fd-pl/scripts"
export PATH="${HOME}/.foundry/bin:${PATH}"
if ! command -v forge >/dev/null; then
  echo "FATAL: forge not on PATH" >&2
  exit 2
fi
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$SCRIPTS/FalconQcBridgeV3.sol" "$SCRIPTS/FalconQcBridgeV3Checks.sol" "$SCRIPTS/FalconQcBridgeV3.t.sol" "$SCRIPTS/Groth16Verifier.sol" "$WORK/"
cd "$WORK"
forge init --force --no-git --quiet .
python3 - <<'PY'
from pathlib import Path
p = Path("foundry.toml")
text = p.read_text()
needle = 'libs = ["lib"]\n'
insert = needle + 'solc = "0.8.20"\nvia_ir = true\noptimizer = true\noptimizer_runs = 200\n'
# Foundry only allows vm.readFile/vm.exists under out/ unless this is set.
# solc, via_ir, and optimizer above are unchanged.
insert += 'fs_permissions = [{ access = "read", path = "./fixtures" }]\n'
if needle not in text:
    raise SystemExit("foundry.toml missing libs line")
p.write_text(text.replace(needle, insert, 1))
PY
rm -f src/Counter.sol test/Counter.t.sol script/Counter.s.sol
mkdir -p src test
mv FalconQcBridgeV3.sol FalconQcBridgeV3Checks.sol Groth16Verifier.sol src/
python3 - <<'PY'
from pathlib import Path
p = Path("FalconQcBridgeV3.t.sol")
t = p.read_text()
t = t.replace('import "./FalconQcBridgeV3.sol";', 'import "../src/FalconQcBridgeV3.sol";')
t = t.replace('import "./Groth16Verifier.sol";', 'import "../src/Groth16Verifier.sol";')
Path("test/FalconQcBridgeV3.t.sol").write_text(t)
p.unlink()
PY
echo "=== forge build (solc 0.8.20, via_ir, optimizer 200) ==="
forge build --sizes
FIX_SRC="$SCRIPTS/fixtures/v2-groth16"
if [ -d "$FIX_SRC" ]; then
  mkdir -p fixtures/v2-groth16
  (
    shopt -s nullglob
    jsons=("$FIX_SRC"/*.json)
    if [ ${#jsons[@]} -gt 0 ]; then
      cp "${jsons[@]}" fixtures/v2-groth16/
    fi
  )
fi
echo "=== forge test -vv ==="
forge test --match-contract FalconQcBridgeV3Test -vv
