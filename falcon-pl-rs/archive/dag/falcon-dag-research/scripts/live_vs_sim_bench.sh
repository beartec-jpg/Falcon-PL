#!/usr/bin/env bash
# Phase E: apples-to-apples sim vs live multi-node KPI report.
# Metrics: settle apply TPS, ordered depth, bullshark anchors, admit %, agreement.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true
BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
SIM="${SIM:-$ROOT/target/release/fd_sim}"
NTX="${NTX:-40}"
N="${N:-5}"
SCHEME="${SCHEME:-hmac-sha256-dev}"

echo "=== Phase E: live_vs_sim_bench ntx=$NTX n=$N scheme=$SCHEME ==="

if [[ ! -x "$BIN" ]]; then
  (cd "$ROOT" && cargo build --release -p fd-node)
  BIN="$ROOT/target/release/falcon-dagd"
  SIM="$ROOT/target/release/fd_sim"
fi
if [[ ! -x "$SIM" ]]; then
  (cd "$ROOT" && cargo build --release -p fd-node --bin fd_sim)
  SIM="$ROOT/target/release/fd_sim"
fi

# --- In-process sim ---
T0=$(date +%s.%N)
SIM_OUT=$("$SIM" "$N" "$NTX" "$SCHEME" 2>&1) || true
T1=$(date +%s.%N)
SIM_S=$(python3 -c "print(max(float('$T1')-float('$T0'),1e-6))")
echo "$SIM_OUT" | tail -5
SIM_EXEC=$(echo "$SIM_OUT" | grep -oE 'executed=[0-9]+' | head -1 | cut -d= -f2 || echo 0)
SIM_TPS=$(python3 -c "print(round(float('${SIM_EXEC:-0}')/float('$SIM_S'),2))")

# --- Live multi-node metrics harness ---
export NTX
LIVE_OUT=$(NTX="$NTX" "$ROOT/scripts/multinode_metrics.sh" 2>&1) || true
echo "$LIVE_OUT" | tail -20
LIVE_TPS=$(echo "$LIVE_OUT" | grep -oE 'apply_tps=[0-9.]+' | head -1 | cut -d= -f2 || echo 0)
LIVE_ADMIT=$(echo "$LIVE_OUT" | grep -oE 'admit_pct=[0-9.]+' | head -1 | cut -d= -f2 || echo 0)
LIVE_AGREE=$(echo "$LIVE_OUT" | grep -oE 'agreement=(YES|NO)' | head -1 | cut -d= -f2 || echo NO)
LIVE_ANCH=$(echo "$LIVE_OUT" | grep 'bullshark_anchors=' | head -1 || true)

echo
echo "=== KPI summary ==="
echo "sim_wall_s=$SIM_S sim_executed=$SIM_EXEC sim_tps=$SIM_TPS"
echo "live_apply_tps=$LIVE_TPS live_admit_pct=$LIVE_ADMIT live_agreement=$LIVE_AGREE"
echo "live_anchors_line: $LIVE_ANCH"
echo "KPI: sim/live gap shrinks as Phase A–C land (target live toward sim band)"
echo "live_vs_sim_bench done"
