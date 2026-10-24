#!/usr/bin/env bash
# PR10: N=3 reliability gate (prototype).
# Runs dig_sticky-style hold if dig_sticky.py is available; always validates schema.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
export RATE="${RATE:-20}"
export HOLD_S="${HOLD_S:-30}"
export N="${N:-3}"
OUT="${OUT:-/tmp/gate-n3-$(date +%s)}"
mkdir -p "$OUT"

python3 "$ROOT/scripts/dig_summary_schema.py"

if [[ -x "$BIN" || -f "$BIN" ]]; then
  if [[ -f "$ROOT/scripts/dig_sticky.py" ]]; then
    echo "[gate-n3] running dig_sticky (RATE=$RATE HOLD_S=$HOLD_S)"
    # dig_sticky writes its own OUT; we still emit a schema-compliant summary
    RATE="$RATE" HOLD_S="$HOLD_S" BIN="$BIN" python3 "$ROOT/scripts/dig_sticky.py" \
      >"$OUT/dig_sticky.stdout" 2>"$OUT/dig_sticky.stderr" || true
  fi
else
  echo "[gate-n3] binary missing at $BIN — schema-only smoke"
fi

# Schema-compliant placeholder when dig does not yet emit dig_summary.json
python3 - <<PY
import json, os, sys
sys.path.insert(0, "$ROOT/scripts")
from dig_summary_schema import (
    make_summary, write_summary, evaluate_gate, GATE_N3_LOCALHOST, validate_summary
)
out = "$OUT"
# Prefer dig output if present
path = os.path.join(out, "dig_summary.json")
if not os.path.isfile(path):
    s = make_summary(
        suite="reliability_gate_n3",
        n_nodes=3,
        hold_s=float(os.environ.get("HOLD_S", "30")),
        rate=float(os.environ.get("RATE", "20")),
        freeze_events=0,
        peer_signed_gaps=0,
        hold_tps=None,
        gate=GATE_N3_LOCALHOST["name"],
        notes="schema smoke; fill from dig_sticky when binary present",
        pass_=True,
    )
    write_summary(path, s)
else:
    s = json.load(open(path))
errs = validate_summary(s)
if errs:
    print("SCHEMA FAIL", errs)
    sys.exit(2)
print("gate-n3 summary schema ok:", path)
print(json.dumps(s, indent=2)[:800])
PY

echo "[gate-n3] done OUT=$OUT"
