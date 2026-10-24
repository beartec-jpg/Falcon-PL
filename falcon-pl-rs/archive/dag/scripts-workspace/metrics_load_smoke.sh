#!/usr/bin/env bash
# Realistic private-net load: 5 validators + payment flood + /metrics samples.
# Writes JSON report with TPS / lag / cert counts.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true
# shellcheck source=smoke_lib.sh
source "$ROOT/scripts/smoke_lib.sh" 2>/dev/null || true

BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
BASE_RPC="${BASE_RPC:-28200}"
BASE_P2P="${BASE_P2P:-29200}"
N=5
UNL="val1,val2,val3,val4,val5"
DURATION="${DURATION:-20}"       # seconds of load
TARGET_TPS="${TARGET_TPS:-50}"  # submit rate aim
ACCOUNTS="${ACCOUNTS:-100}"     # more accounts → more ready sequences in parallel
REPORT="${REPORT:-/tmp/fd-metrics-load-report.json}"
LOGDIR=$(mktemp -d /tmp/fd-metrics-load.XXXXXX)
PIDS=()
# Pin each account to one RPC so sequences stay contiguous on that node until gossip.
PIN_ACCOUNTS="${PIN_ACCOUNTS:-1}"

cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  for i in $(seq 0 $((N-1))); do
    fuser -k $((BASE_RPC+i))/tcp >/dev/null 2>&1 || true
    fuser -k $((BASE_P2P+i))/tcp >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT

if [[ ! -x "$BIN" ]]; then
  if command -v cargo >/dev/null 2>&1; then
    (cd "$ROOT" && cargo build --release -p fd-node)
  else
    echo "FAIL: no binary at $BIN and no cargo" >&2
    exit 1
  fi
  BIN="$ROOT/target/release/falcon-dagd"
fi

for i in $(seq 0 $((N-1))); do
  fuser -k $((BASE_RPC+i))/tcp >/dev/null 2>&1 || true
  fuser -k $((BASE_P2P+i))/tcp >/dev/null 2>&1 || true
done
sleep 0.3

echo "starting $N validators in $LOGDIR"
for i in $(seq 0 $((N-1))); do
  id="val$((i+1))"
  rpc="127.0.0.1:$((BASE_RPC+i))"
  p2p="127.0.0.1:$((BASE_P2P+i))"
  peers=""
  for j in $(seq 0 $((N-1))); do
    [[ $j -eq $i ]] && continue
    peers+="127.0.0.1:$((BASE_P2P+j)),"
  done
  peers=${peers%,}
  "$BIN" \
    --id "$id" \
    --rpc "$rpc" \
    --p2p "$p2p" \
    --peers "$peers" \
    --unl "$UNL" \
    --worker-index "$i" \
    --worker-count "$N" \
    --network-id 9003 \
    --crypto hmac-sha256-dev \
    --require-tx-sigs \
    --auto-sign \
    --batch-interval "${BATCH_INTERVAL:-0.05}" \
    --max-batch-txs "${MAX_BATCH_TXS:-256}" \
    --max-batch-parents 1 \
    --unl-threshold "${UNL_THRESHOLD:-0.6}" \
    --sequence-window "${SEQ_WINDOW:-256}" \
    --data-dir "$LOGDIR/data-$id" \
    --snapshot-interval 30 \
    >"$LOGDIR/$id.log" 2>&1 &
  PIDS+=($!)
done

for i in $(seq 0 $((N-1))); do
  rpc="127.0.0.1:$((BASE_RPC+i))"
  for _ in $(seq 1 80); do
    if curl -sf -m 1 -X POST "http://$rpc" -H 'Content-Type: application/json' \
      -d '{"method":"ping","params":[{}]}' 2>/dev/null | grep -q pong; then
      echo "up $rpc"
      break
    fi
    sleep 0.1
  done
done

RPC0="127.0.0.1:${BASE_RPC}"
post() { curl -sf -m 8 -X POST "http://$1" -H 'Content-Type: application/json' -d "$2"; }

# Sample helper
sample_metrics() {
  local rpc="$1"
  post "$rpc" '{"method":"server_info","params":[{}]}' 2>/dev/null \
    | python3 -c "
import sys,json
try:
  m=json.load(sys.stdin)['result']['info']['metrics']
  print(json.dumps({
    'executed_txs': m.get('executed_txs',0),
    'certified': m.get('certified',0),
    'batches_created': m.get('batches_created',0),
    'mempool': m.get('mempool',0),
    'hard_final': m.get('hard_final',0),
    'soft_final_txs': m.get('soft_final_txs',0),
    'crypto_scheme': m.get('crypto_scheme',''),
    'validator_id': m.get('validator_id',''),
  }))
except Exception as e:
  print(json.dumps({'error': str(e)}))
"
  curl -sf -m 3 "http://${rpc}/metrics" 2>/dev/null | python3 -c "
import sys
prom={}
for line in sys.stdin:
  line=line.strip()
  if not line or line.startswith('#'): continue
  parts=line.split()
  if len(parts)>=2 and parts[0].startswith('falcon_dag_'):
    try: prom[parts[0]]=float(parts[1])
    except: pass
print(json.dumps(prom))
" 2>/dev/null || echo '{}'
}

echo "=== load: ~${TARGET_TPS} submit/s for ${DURATION}s across ${N} RPCs ==="
# Flood in python for accurate timing
python3 - <<PY
import json, time, urllib.request, threading, statistics
from concurrent.futures import ThreadPoolExecutor, as_completed

BASE_RPC = $BASE_RPC
N = $N
DURATION = float("$DURATION")
TARGET_TPS = float("$TARGET_TPS")
REPORT = "$REPORT"
ACCOUNTS = int("$ACCOUNTS")
PIN = int("$PIN_ACCOUNTS")

def rpc(port, method, params):
    body = json.dumps({"method": method, "params": [params]}).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=8) as r:
        return json.loads(r.read().decode())

def server_metrics(port):
    d = rpc(port, "server_info", {})
    res = d.get("result") or d
    info = res.get("info") or res
    return info.get("metrics") or info

def prom(port):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/metrics", timeout=3) as r:
            text = r.read().decode()
    except Exception:
        return {}
    out = {}
    for line in text.splitlines():
        if line.startswith("falcon_dag_") and not line.startswith("#"):
            parts = line.split()
            if len(parts) >= 2:
                try:
                    out[parts[0]] = float(parts[1])
                except ValueError:
                    pass
    return out

# sequences per account (genesis accounts acct1..acctN)
seqs = {f"acct{i}": 1 for i in range(1, ACCOUNTS + 1)}
lock = threading.Lock()
submitted_ok = 0
submitted_fail = 0
latencies = []
t_end = time.time() + DURATION
interval = 1.0 / max(TARGET_TPS, 1.0)
i = 0
t0 = time.time()
samples = []

def one_submit(k):
    global submitted_ok, submitted_fail
    src = f"acct{(k % ACCOUNTS) + 1}"
    dst = f"acct{((k + 1) % ACCOUNTS) + 1}"
    # Pin account → fixed RPC so sequences stay contiguous for packing
    if PIN:
        port = BASE_RPC + (((k % ACCOUNTS)) % N)
    else:
        port = BASE_RPC + (k % N)
    with lock:
        seq = seqs[src]
        seqs[src] = seq + 1
    t_s = time.time()
    try:
        res = rpc(port, "submit", {
            "account": src,
            "sequence": seq,
            "destination": dst,
            "amount": 1,
            "fee": 10,
        })
        code = (res.get("result") or res).get("engine_result")
        dt = time.time() - t_s
        with lock:
            latencies.append(dt)
            if code == "tesSUCCESS":
                submitted_ok += 1
            else:
                submitted_fail += 1
        return code
    except Exception:
        with lock:
            submitted_fail += 1
        return "error"

# sampler thread
stop_sample = threading.Event()

def sampler():
    while not stop_sample.is_set():
        try:
            m = server_metrics(BASE_RPC)
            p = prom(BASE_RPC)
            # lag across validators
            execs = []
            for vi in range(N):
                try:
                    execs.append(int(server_metrics(BASE_RPC + vi).get("executed_txs") or 0))
                except Exception:
                    execs.append(-1)
            samples.append({
                "t": time.time() - t0,
                "executed": m.get("executed_txs", 0),
                "certified": m.get("certified", 0),
                "batches": m.get("batches_created", 0),
                "mempool": m.get("mempool", 0),
                "hard_final": m.get("hard_final", 0),
                "exec_by_val": execs,
                "exec_spread": (max(execs) - min(e for e in execs if e >= 0)) if any(e >= 0 for e in execs) else None,
                "prom_executed": p.get("falcon_dag_executed_txs"),
            })
        except Exception as e:
            samples.append({"t": time.time() - t0, "error": str(e)})
        time.sleep(1.0)

th = threading.Thread(target=sampler, daemon=True)
th.start()

# submit loop
next_at = time.time()
while time.time() < t_end:
    one_submit(i)
    i += 1
    next_at += interval
    sleep = next_at - time.time()
    if sleep > 0:
        time.sleep(sleep)

# drain a bit
time.sleep(2.0)
stop_sample.set()
th.join(timeout=2)

t1 = time.time()
elapsed = t1 - t0
final = []
for vi in range(N):
    try:
        final.append(server_metrics(BASE_RPC + vi))
    except Exception as e:
        final.append({"error": str(e), "executed_txs": 0})
exec_final = [int(m.get("executed_txs") or 0) for m in final]
exec0 = samples[0]["executed"] if samples and "executed" in samples[0] else 0
exec_end = max(exec_final) if exec_final else 0
applied = max(0, exec_end - exec0)
tps = applied / elapsed if elapsed > 0 else 0
submit_tps = submitted_ok / elapsed if elapsed > 0 else 0

lat_ms = [x * 1000 for x in latencies]
lat_ms_sorted = sorted(lat_ms)
def pct(p):
    if not lat_ms_sorted:
        return None
    idx = min(len(lat_ms_sorted) - 1, int(p / 100.0 * (len(lat_ms_sorted) - 1)))
    return round(lat_ms_sorted[idx], 2)

report = {
    "ok": True,
    "kind": "private_5val_payment_load",
    "network_id": 9003,
    "validators": N,
    "accounts": ACCOUNTS,
    "duration_s": round(elapsed, 2),
    "target_submit_tps": TARGET_TPS,
    "submitted_ok": submitted_ok,
    "submitted_fail": submitted_fail,
    "submit_tps": round(submit_tps, 2),
    "executed_start": exec0,
    "executed_end_max": exec_end,
    "executed_by_validator": exec_final,
    "executed_spread": (max(exec_final) - min(exec_final)) if exec_final else 0,
    "applied_during_run": applied,
    "apply_tps": round(tps, 2),
    "submit_latency_ms": {
        "p50": pct(50),
        "p95": pct(95),
        "p99": pct(99),
        "mean": round(statistics.mean(lat_ms), 2) if lat_ms else None,
    },
    "final_metrics_val1": final[0] if final else {},
    "samples": samples,
    "notes": [
        "auto-sign faucet mode (dev); not client wallet signing load",
        "HMAC-dev crypto; Falcon-512 would be slower",
        "TPS = executed delta / wall time after load+drain",
        "Narwhal-style: async pack on tick; pin accounts to RPCs",
    ],
}
open(REPORT, "w").write(json.dumps(report, indent=2))
print(json.dumps({k: report[k] for k in report if k != "samples"}, indent=2))
print(f"full report: {REPORT} ({len(samples)} samples)")
if applied < 1:
    raise SystemExit("no txs applied")
PY


echo "metrics_load_smoke OK"
echo "report: $REPORT"
