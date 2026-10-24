#!/usr/bin/env bash
# Multi-config soak suite: start 5-val mesh per profile, run soak_ramp_load.py, collect reports.
#
# Default: FULL 30-minute profile (5m soak → ramp → hold → ramp down → ~10-15m idle).
# Series: several batch/window/sharding settings; each profile runs DURATION seconds.
#
# Usage:
#   ./scripts/soak_suite.sh                  # full 30m series (long!)
#   DURATION=180 QUICK=1 ./scripts/soak_suite.sh   # compressed ~3m phases for dry run
#   PROFILES=fast_batch,slow_batch ./scripts/soak_suite.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true

BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
OUT_DIR="${OUT_DIR:-/tmp/fd-soak-suite-$(date +%Y%m%d-%H%M%S)}"
DURATION="${DURATION:-1800}"   # 30 minutes
N=5
BASE_RPC="${BASE_RPC:-28600}"
BASE_P2P="${BASE_P2P:-29600}"
HOST=127.0.0.1

if [[ "${QUICK:-0}" == "1" ]]; then
  # scale phases to ~DURATION total with same shape
  SOAK=$(python3 -c "print(max(30, int($DURATION*5/30)))")
  RAMP=$(python3 -c "print(max(30, int($DURATION*5/30)))")
  HOLD=$(python3 -c "print(max(30, int($DURATION*5/30)))")
  RDOWN=$(python3 -c "print(max(30, int($DURATION*5/30)))")
else
  SOAK=300; RAMP=300; HOLD=300; RDOWN=300
fi

if [[ ! -x "$BIN" ]]; then
  (cd "$ROOT" && cargo build --release -p fd-node)
  BIN="$ROOT/target/release/falcon-dagd"
fi

mkdir -p "$OUT_DIR"
echo "suite out=$OUT_DIR duration=$DURATION quick=${QUICK:-0}"

# name|batch_interval|max_batch_txs|sequence_window|adaptive(0/1)|no_shard(0/1)|peak_tps
ALL_PROFILES=(
  "baseline_0.2s_128tx|0.2|128|256|0|1|60"
  "fast_close_0.05s|0.05|128|256|0|1|80"
  "slow_close_1.0s|1.0|256|256|0|1|60"
  "fat_batches_512|0.2|512|256|0|1|80"
  "adaptive_batch|0.2|128|256|1|1|80"
  "narrow_window_32|0.2|128|32|0|1|40"
  "sharded_workers|0.2|128|256|0|0|60"
)

if [[ -n "${PROFILES:-}" ]]; then
  SELECTED=()
  IFS=',' read -ra WANT <<< "$PROFILES"
  for p in "${ALL_PROFILES[@]}"; do
    name="${p%%|*}"
    for w in "${WANT[@]}"; do
      [[ "$name" == "$w" ]] && SELECTED+=("$p")
    done
  done
else
  SELECTED=("${ALL_PROFILES[@]}")
fi

SUMMARY="$OUT_DIR/SUMMARY.json"
echo '[]' >"$SUMMARY"

run_profile() {
  local spec="$1"
  IFS='|' read -r name batch maxb win adaptive noshard peak <<< "$spec"
  local rpc_base=$((BASE_RPC + RANDOM % 50 * 10))
  local p2p_base=$((BASE_P2P + RANDOM % 50 * 10))
  # use fixed offsets from name hash for stability
  local off
  off=$(python3 -c "print(sum(ord(c) for c in '$name') % 40)")
  rpc_base=$((BASE_RPC + off * 10))
  p2p_base=$((BASE_P2P + off * 10))

  local logdir="$OUT_DIR/$name"
  mkdir -p "$logdir/data"
  local pids=()
  cleanup_prof() {
    for p in "${pids[@]:-}"; do kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    for i in $(seq 0 $((N-1))); do
      fuser -k $((rpc_base+i))/tcp >/dev/null 2>&1 || true
      fuser -k $((p2p_base+i))/tcp >/dev/null 2>&1 || true
    done
  }
  trap cleanup_prof RETURN

  echo ""
  echo "======== PROFILE $name batch=$batch max_batch=$maxb window=$win adaptive=$adaptive no_shard=$noshard peak=$peak ========"

  for i in $(seq 0 $((N-1))); do
    fuser -k $((rpc_base+i))/tcp >/dev/null 2>&1 || true
    fuser -k $((p2p_base+i))/tcp >/dev/null 2>&1 || true
  done
  sleep 0.3

  extra=()
  [[ "$adaptive" == "1" ]] && extra+=(--adaptive-batch)
  # sharding is opt-in now; only enable when noshard=0
  [[ "$noshard" == "0" ]] && extra+=(--worker-sharding)

  for i in $(seq 0 $((N-1))); do
    id="val$((i+1))"
    peers=""
    for j in $(seq 0 $((N-1))); do
      [[ $j -eq $i ]] && continue
      peers+="127.0.0.1:$((p2p_base+j)),"
    done
    peers=${peers%,}
    "$BIN" \
      --id "$id" \
      --rpc "127.0.0.1:$((rpc_base+i))" \
      --p2p "127.0.0.1:$((p2p_base+i))" \
      --peers "$peers" \
      --unl val1,val2,val3,val4,val5 \
      --worker-index "$i" \
      --worker-count "$N" \
      --network-id 9003 \
      --crypto hmac-sha256-dev \
      --require-tx-sigs \
      --auto-sign \
      --batch-interval "$batch" \
      --max-batch-txs "$maxb" \
      --sequence-window "$win" \
      "${extra[@]}" \
      --data-dir "$logdir/data/$id" \
      --snapshot-interval 60 \
      >"$logdir/$id.log" 2>&1 &
    pids+=($!)
  done

  for i in $(seq 0 $((N-1))); do
    for _ in $(seq 1 80); do
      if curl -sf -m 1 -X POST "http://127.0.0.1:$((rpc_base+i))" \
        -H 'Content-Type: application/json' \
        -d '{"method":"ping","params":[{}]}' 2>/dev/null | grep -q pong; then
        break
      fi
      sleep 0.1
    done
  done

  local report="$logdir/report.json"
  python3 "$ROOT/scripts/soak_ramp_load.py" \
    --host "$HOST" \
    --base-rpc "$rpc_base" \
    --validators "$N" \
    --duration "$DURATION" \
    --soak-s "$SOAK" \
    --ramp-up-s "$RAMP" \
    --hold-s "$HOLD" \
    --ramp-down-s "$RDOWN" \
    --soak-tps 5 \
    --peak-tps "$peak" \
    --sample-s 5 \
    --label "$name" \
    --report "$report" | tee "$logdir/summary.txt"

  # append to suite summary
  python3 - <<PY
import json
from pathlib import Path
summary_path = Path("$SUMMARY")
rep = json.load(open("$report"))
row = {
  "name": "$name",
  "batch_interval": float("$batch"),
  "max_batch_txs": int("$maxb"),
  "sequence_window": int("$win"),
  "adaptive_batch": "$adaptive" == "1",
  "no_worker_sharding": "$noshard" == "1",
  "peak_tps_target": float("$peak"),
  "duration_s": rep.get("duration_s"),
  "submitted_ok": rep.get("submitted_ok"),
  "submitted_fail": rep.get("submitted_fail"),
  "admit_ok_rate": rep.get("admit_ok_rate"),
  "apply_tps_avg": rep.get("apply_tps_avg"),
  "executed_max": rep.get("executed_max"),
  "executed_spread": rep.get("executed_spread"),
  "latency_p50_ms": (rep.get("submit_latency_ms") or {}).get("p50"),
  "latency_p95_ms": (rep.get("submit_latency_ms") or {}).get("p95"),
  "engine_codes": rep.get("engine_codes"),
  "report": str(Path("$report")),
}
rows = json.loads(summary_path.read_text())
rows.append(row)
summary_path.write_text(json.dumps(rows, indent=2))
print("updated", summary_path)
PY

  cleanup_prof
  trap - RETURN
}

for spec in "${SELECTED[@]}"; do
  run_profile "$spec"
done

echo ""
echo "======== SUITE COMPLETE ========"
python3 - <<PY
import json
from pathlib import Path
rows = json.loads(Path("$SUMMARY").read_text())
print(f"{'profile':<28} {'admit%':>8} {'apply_tps':>10} {'exec_max':>10} {'spread':>8} {'p95ms':>8}")
for r in rows:
  print(f"{r['name']:<28} {100*r.get('admit_ok_rate',0):>7.1f}% {r.get('apply_tps_avg',0):>10} {r.get('executed_max',0):>10} {r.get('executed_spread'):>8} {r.get('latency_p95_ms'):>8}")
print("full summary:", "$SUMMARY")
print("dir:", "$OUT_DIR")
PY
