#!/usr/bin/env bash
# Foundry tests for FalconQcBridgeV2 (AlwaysTrueVerifier + dest-only take).
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
cp "$SCRIPTS/FalconQcBridgeV2.sol" "$SCRIPTS/FalconQcBridgeV2.t.sol" "$WORK/"
cd "$WORK"
forge init --force --no-git --quiet .
cat >> foundry.toml <<'TOML'
via_ir = true
optimizer = true
optimizer_runs = 200
TOML
# forge-std Test lives under lib/ after init; point the test import.
mkdir -p src test
mv FalconQcBridgeV2.sol src/
# Test file imports "./FalconQcBridgeV2.sol" and forge-std.
python3 - <<'PY'
from pathlib import Path
p = Path("FalconQcBridgeV2.t.sol")
t = p.read_text()
t = t.replace('import "./FalconQcBridgeV2.sol";', 'import "../src/FalconQcBridgeV2.sol";')
Path("test/FalconQcBridgeV2.t.sol").write_text(t)
p.unlink()
PY
forge test --match-contract FalconQcBridgeV2Test -vv
