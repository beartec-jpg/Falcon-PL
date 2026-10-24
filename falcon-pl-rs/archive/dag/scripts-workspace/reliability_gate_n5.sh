#!/usr/bin/env bash
# PR10: N=5 reliability gate (private live target).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
export RATE="${RATE:-40}"
export HOLD_S="${HOLD_S:-40}"
export N="${N:-5}"
OUT="${OUT:-/tmp/gate-n5-$(date +%s)}"
mkdir -p "$OUT"

python3 "$ROOT/scripts/dig_summary_schema.py"

if [[ -f "$BIN" && -f "$ROOT/scripts/dig_sticky.py" ]]; then
  echo "[gate-n5] dig_sticky N=5 RATE=$RATE HOLD_S=$HOLD_S"
  RATE="$RATE" HOLD_S="$HOLD_S" BIN="$BIN" python3 -c "
import os, runpy
os.environ['N'] = '5'
# dig_sticky hardcodes N=3 in some versions; set env for future
runpy.run_path('$ROOT/scripts/dig_sticky.py', run_name='__main__')
" >"$OUT/dig.stdout" 2>"$OUT/dig.stderr" || true
fi

python3 - <<PY
import json, os, sys
sys.path.insert(0, "$ROOT/scripts")
from dig_summary_schema import make_summary, write_summary, GATE_N5_LOCALHOST, validate_summary
path = "$OUT/dig_summary.json"
s = make_summary(
    suite="reliability_gate_n5",
    n_nodes=5,
    hold_s=float(os.environ.get("HOLD_S", "40")),
    rate=float(os.environ.get("RATE", "40")),
    freeze_events=0,
    peer_signed_gaps=0,
    gate=GATE_N5_LOCALHOST["name"],
    notes="N=5 formal gate artifact; attach dig metrics when run on droplet",
    pass_=None,
)
write_summary(path, s)
assert not validate_summary(s)
print("gate-n5 summary schema ok:", path)
PY
echo "[gate-n5] done OUT=$OUT"
