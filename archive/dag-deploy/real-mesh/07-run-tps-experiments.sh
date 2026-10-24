#!/usr/bin/env bash
# Run TPS experiment series: baseline → density (100 acct + adaptive off) → single-packer.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=/dev/null
source "$ROOT/env.sh"

SUMMARY_DIR="${SUMMARY_DIR:-/tmp/real-mesh-tps-exp-$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$SUMMARY_DIR"
echo "SUMMARY_DIR=$SUMMARY_DIR"

wipe_data() {
  echo "=== wipe data dirs (genesis reset) ==="
  bash "$ROOT/05-stop-all.sh" || true
  sleep 1
  rm -rf "$MESH_ROOT_FALCON/data"/*
  mkdir -p "$MESH_ROOT_FALCON/data"
  ssh host2 "rm -rf $MESH_ROOT_FALCON/data/*; mkdir -p $MESH_ROOT_FALCON/data" || true
}

ensure_wallets() {
  local n="$1"
  local have
  have="$(find "$MESH_ROOT_FALCON/wallets" -name 'acct*.json' 2>/dev/null | wc -l | tr -d ' ')"
  if [[ "$have" -ge "$n" ]]; then
    echo "wallets ok: $have >= $n"
    return 0
  fi
  echo "=== regenerating $n faucet wallets (had $have) ==="
  "$BIN_FALCON" --gen-wallet-keys --wallet-dir "$MESH_ROOT_FALCON/wallets" --faucet-accounts "$n"
  bash "$ROOT/01-distribute.sh"
}

start_mesh() {
  echo "=== start mesh tag=$TUNING_TAG single=$SINGLE_PACKER adaptive=$ADAPTIVE_BATCH accounts=${ACCOUNTS:-40} ==="
  bash "$ROOT/02-start-all.sh"
  # wait for all RPC
  local tries=0
  while (( tries < 40 )); do
    if curl -s -m 1 -X POST "http://${F1_IP}:${VAL1_RPC}/" \
      -H 'Content-Type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"server_info","params":[{}]}' \
      | grep -q executed_txs; then
      break
    fi
    sleep 1
    tries=$((tries + 1))
  done
  bash "$ROOT/03-status.sh" || true
}

run_ramp() {
  local tag="$1"
  local out="$SUMMARY_DIR/$tag"
  mkdir -p "$out"
  export OUT="$out"
  echo "=== measure ramp tag=$tag OUT=$out ACCOUNTS=${ACCOUNTS:-40} ==="
  python3 "$ROOT/06-measure-ramp.py" 2>&1 | tee "$SUMMARY_DIR/${tag}-run.log"
  # extract headline
  if [[ -f "$out/REPORT.md" ]]; then
    cp "$out/REPORT.md" "$SUMMARY_DIR/${tag}-REPORT.md"
  fi
  python3 - "$out" "$SUMMARY_DIR/${tag}-summary.json" <<'PY'
import json, sys
from pathlib import Path
out = Path(sys.argv[1])
dest = Path(sys.argv[2])
r = json.loads((out / "REPORT.json").read_text())
phases = r.get("phases") or []
rows = []
for p in phases:
    bd = p.get("batch_delta") or {}
    batches = sum(int(v or 0) for k, v in bd.items() if str(k).startswith("val"))
    ok = int(p.get("ok") or 0)
    elapsed = float(p.get("elapsed_s") or 0) or 1.0
    txs_per_batch = (ok / batches) if batches else None
    rows.append({
        "label": p.get("label"),
        "target": p.get("target_rate"),
        "submit_tps": p.get("submit_tps"),
        "exec_tps_min": p.get("exec_tps_min"),
        "spread": p.get("spread_after"),
        "pass": p.get("pass"),
        "ok": ok,
        "batches": batches,
        "txs_per_batch": round(txs_per_batch, 2) if txs_per_batch else None,
        "batch_rate": round(batches / elapsed, 2),
    })
best = max(rows, key=lambda x: x.get("exec_tps_min") or 0) if rows else None
summary = {
    "stop_reason": r.get("stop_reason"),
    "accounts": (r.get("topology") or {}).get("accounts"),
    "best": best,
    "phases": rows,
    "final_health": r.get("final_health"),
}
dest.write_text(json.dumps(summary, indent=2) + "\n")
print(json.dumps(summary, indent=2))
PY
}

# ---------- Exp 1: baseline v1 multipack, 40 accounts ----------
export TUNING_TAG="exp1-baseline-v1"
export ADMIT_AHEAD_CAP=2000
export MAX_UNCERTIFIED_PER_CREATOR=8
export MAX_BATCH_TXS=64
export PACK_OPEN_CAP=0
export BATCH_INTERVAL=0.05
export SINGLE_PACKER=false
export ADAPTIVE_BATCH=true
export PACK_CHECK_SLOT=0.25
export FAUCET_ACCOUNTS=40
export ACCOUNTS=40

wipe_data
ensure_wallets 40
# keep existing 40 if already present; distribute binary/keys
bash "$ROOT/01-distribute.sh"
start_mesh
run_ramp "exp1-baseline"

# ---------- Exp 2: 100 accounts + adaptive off (density) ----------
export TUNING_TAG="exp2-acct100-adaptive-off"
export ADAPTIVE_BATCH=false
export FAUCET_ACCOUNTS=100
export ACCOUNTS=100
# keep multipack + v1 pack knobs
export SINGLE_PACKER=false
export ADMIT_AHEAD_CAP=2000
export MAX_UNCERTIFIED_PER_CREATOR=8
export MAX_BATCH_TXS=64
export PACK_OPEN_CAP=0
export PACK_CHECK_SLOT=0.25

wipe_data
ensure_wallets 100
bash "$ROOT/01-distribute.sh"
start_mesh
run_ramp "exp2-density"

# ---------- Exp 3: single-packer multi-host, 100 accounts, adaptive off ----------
export TUNING_TAG="exp3-single-packer"
export SINGLE_PACKER=true
export ADAPTIVE_BATCH=false
export FAUCET_ACCOUNTS=100
export ACCOUNTS=100
export ADMIT_AHEAD_CAP=2000
export MAX_UNCERTIFIED_PER_CREATOR=8
export MAX_BATCH_TXS=256
export PACK_OPEN_CAP=0
export PACK_CHECK_SLOT=0.25

wipe_data
ensure_wallets 100
bash "$ROOT/01-distribute.sh"
start_mesh
run_ramp "exp3-single-packer"

# ---------- Compare ----------
python3 - "$SUMMARY_DIR" <<'PY'
import json, sys
from pathlib import Path
root = Path(sys.argv[1])
print("\n" + "=" * 72)
print("TPS EXPERIMENT COMPARISON")
print("=" * 72)
print(f"{'exp':<22} {'best_exec':>10} {'phase':>12} {'txs/batch':>10} {'batch/s':>8} {'pass_band'}")
for name in ["exp1-baseline", "exp2-density", "exp3-single-packer"]:
    p = root / f"{name}-summary.json"
    if not p.exists():
        print(f"{name:<22} MISSING")
        continue
    s = json.loads(p.read_text())
    b = s.get("best") or {}
    passes = [r["label"] for r in s.get("phases") or [] if r.get("pass")]
    print(
        f"{name:<22} {b.get('exec_tps_min', 0)!s:>10} "
        f"{str(b.get('label','')):>12} "
        f"{str(b.get('txs_per_batch','')):>10} "
        f"{str(b.get('batch_rate','')):>8} "
        f"{','.join(passes) or '-'}"
    )
print("=" * 72)
print(f"artifacts: {root}")
PY

echo "ALL EXPERIMENTS DONE → $SUMMARY_DIR"
