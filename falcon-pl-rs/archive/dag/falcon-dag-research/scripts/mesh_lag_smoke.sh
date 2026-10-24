#!/usr/bin/env bash
# Phase 49d — multi-node mesh lag measurement.
#
# Answers before production:
#   1) How fast does a cert propagate val1 → val5? (cert apply lag + round catch-up)
#   2) Can secondaries pull multiple missing ancestors in parallel?
#   3) In-order vs speculative validation?
#
# Env:
#   NTX=80          flood size (default 80)
#   SETTLE_S=25     max seconds to wait for mesh catch-up
#   PACK_SKIP_HOLE_AFTER=0
#   BIN=...
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true
BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
BASE_RPC=29800
BASE_P2P=29900
N=5
UNL="val1,val2,val3,val4,val5"
NTX="${NTX:-80}"
SETTLE_S="${SETTLE_S:-25}"
PACK_SKIP_HOLE_AFTER="${PACK_SKIP_HOLE_AFTER:-0}"
CRYPTO="${CRYPTO:-hmac-sha256-dev}"
LOGDIR=$(mktemp -d /tmp/fd-mesh.XXXXXX)
PIDS=()
KEYS=""
WALLETS=""

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
  (cd "$ROOT" && cargo build --release -p fd-node)
  BIN="$ROOT/target/release/falcon-dagd"
fi

if [[ "$CRYPTO" == falcon* ]]; then
  KEYS="$LOGDIR/keys"
  WALLETS="$LOGDIR/wallets"
  "$BIN" --gen-keys --keys-dir "$KEYS" --unl "$UNL"
  "$BIN" --gen-wallet-keys --wallet-dir "$WALLETS" --faucet-accounts 20
  echo "generated Falcon-512 UNL keys in $KEYS"
  echo "generated Falcon-512 faucet wallets in $WALLETS"
fi

for i in $(seq 0 $((N-1))); do
  fuser -k $((BASE_RPC+i))/tcp >/dev/null 2>&1 || true
  fuser -k $((BASE_P2P+i))/tcp >/dev/null 2>&1 || true
done
sleep 0.3

echo "mesh_lag_smoke N=$N ntx=$NTX settle_s=$SETTLE_S crypto=$CRYPTO pack_skip_hole_after=$PACK_SKIP_HOLE_AFTER logdir=$LOGDIR"

for i in $(seq 0 $((N-1))); do
  id="val$((i+1))"
  peers=""
  for j in $(seq 0 $((N-1))); do
    [[ $j -eq $i ]] && continue
    peers+="127.0.0.1:$((BASE_P2P+j)),"
  done
  peers=${peers%,}
  EXTRA=()
  if [[ -n "${KEYS:-}" ]]; then
    EXTRA+=(--keys-dir "$KEYS")
  fi
  if [[ -n "${WALLETS:-}" ]]; then
    EXTRA+=(--wallet-dir "$WALLETS")
  fi
  "$BIN" \
    --id "$id" \
    --role full \
    --rpc "127.0.0.1:$((BASE_RPC+i))" \
    --p2p "127.0.0.1:$((BASE_P2P+i))" \
    --peers "$peers" \
    --unl "$UNL" \
    --worker-index "$i" \
    --worker-count "$N" \
    --network-id 9003 \
    --crypto "$CRYPTO" \
    --require-tx-sigs \
    --auto-sign \
    --batch-interval 0.05 \
    --max-batch-txs 256 \
    --unl-threshold 0.6 \
    --pack-skip-hole-after "$PACK_SKIP_HOLE_AFTER" \
    --data-dir "$LOGDIR/$id" \
    "${EXTRA[@]}" \
    >"$LOGDIR/$id.log" 2>&1 &
  PIDS+=($!)
done

for i in $(seq 0 $((N-1))); do
  for _ in $(seq 1 100); do
    if curl -sf -m1 -X POST "http://127.0.0.1:$((BASE_RPC+i))" \
      -H 'Content-Type: application/json' \
      -d '{"method":"ping","params":[{}]}' 2>/dev/null | grep -q pong; then
      break
    fi
    sleep 0.1
  done
done

python3 - <<PY
import json, urllib.request, time, os, sys

base = $BASE_RPC
n = $N
ntx = $NTX
settle_s = float("$SETTLE_S")

def rpc(port, method, params=None, timeout=5.0):
    body = json.dumps({"method": method, "params": [params or {}]}).encode()
    last = None
    for attempt in range(8):
        try:
            req = urllib.request.Request(
                f"http://127.0.0.1:{port}",
                data=body,
                headers={
                    "Content-Type": "application/json",
                    "Content-Length": str(len(body)),
                    "Connection": "close",
                },
                method="POST",
            )
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                raw = resp.read()
            if not raw or not raw.strip():
                raise RuntimeError("empty")
            return json.loads(raw)
        except Exception as e:
            last = e
            time.sleep(0.03 * (attempt + 1))
    raise RuntimeError(f"rpc {method}:{port} {last}")

def metrics(i):
    d = rpc(base + i, "server_info")
    info = (d.get("result") or {}).get("info") or {}
    if "error" in (d.get("result") or {}) and not info:
        raise RuntimeError(d["result"]["error"])
    return info.get("metrics") or {}

# --- submit flood (with retries) ---
t0 = time.time()
admit_ok = admit_fail = 0
for seq in range(1, ntx + 1):
    code = ""
    for attempt in range(6):
        try:
            d = rpc(base, "submit", {
                "account": "acct1",
                "sequence": seq,
                "destination": "acct2",
                "amount": 1,
                "fee": 10,
            })
            res = d.get("result") or d
            code = res.get("engine_result") or ""
            err = res.get("error")
            if code == "tesSUCCESS":
                break
            if err and ("incomplete" in str(err).lower() or "empty_body" in str(err).lower() or "eof" in str(err).lower() or res.get("retryable")):
                time.sleep(0.02 * (attempt + 1))
                continue
            break
        except Exception:
            time.sleep(0.02 * (attempt + 1))
    if code == "tesSUCCESS":
        admit_ok += 1
    else:
        admit_fail += 1

t_submit = time.time()
print(f"submitted admit_ok={admit_ok} admit_fail={admit_fail} in {t_submit-t0:.3f}s", flush=True)

# --- sample mesh every 100ms until rounds agree or timeout ---
samples = []
deadline = time.time() + settle_s
agree_at = None
while time.time() < deadline:
    try:
        ms = [metrics(i) for i in range(n)]
    except Exception as e:
        time.sleep(0.1)
        continue
    rounds = [int(m.get("max_cert_round") or m.get("cert_round_max") or 0) for m in ms]
    certs = [int(m.get("certified") or 0) for m in ms]
    execs = [int(m.get("executed_txs") or 0) for m in ms]
    anchors = [int(m.get("bullshark_anchors") or 0) for m in ms]
    lag = max(rounds) - min(rounds) if rounds else 0
    meshes = [m.get("mesh") or {} for m in ms]
    pulls = [m.get("pull") or {} for m in ms]
    row = {
        "t": time.time() - t0,
        "rounds": rounds,
        "certs": certs,
        "execs": execs,
        "anchors": anchors,
        "round_lag": lag,
        "mesh": meshes,
        "pull": pulls,
    }
    samples.append(row)
    if lag == 0 and min(certs) == max(certs) and min(certs) > 1:
        if agree_at is None:
            agree_at = time.time() - t0
        # also want exec agreement for full mesh settle signal
        if min(execs) == max(execs) and min(execs) >= max(1, ntx // 2):
            break
    time.sleep(0.1)

ms = samples[-1] if samples else None
if not ms:
    print("FATAL: no metrics samples")
    sys.exit(2)

# Final detailed snapshot
final = [metrics(i) for i in range(n)]
print()
print("=== Phase 49d mesh lag report ===")
print(f"validators={n} ntx={ntx} admit_ok={admit_ok} admit_fail={admit_fail}")
print(f"samples={len(samples)} wall_s={time.time()-t0:.2f} agree_at_s={agree_at}")
print()

# 1) Cert propagation val1 → val5
print("--- 1) Cert propagation (val1 → others) ---")
r0 = [s["rounds"][0] for s in samples]
print(f"val1 max_cert_round over time: start={r0[0] if r0 else 0} end={r0[-1] if r0 else 0} peak={max(r0) if r0 else 0}")
for i in range(n):
    catch = None
    target = max(r0) if r0 else 0
    for s in samples:
        if s["rounds"][i] >= target and target > 0:
            catch = s["t"]
            break
    print(f"  val{i+1}: final_round={samples[-1]['rounds'][i]} catch_up_to_val1_peak_s={catch}")

# Cross-node round lag timeline
lags = [s["round_lag"] for s in samples]
print(f"round_lag max={max(lags) if lags else 0} final={lags[-1] if lags else 0} "
      f"time_at_lag0={next((s['t'] for s in samples if s['round_lag']==0 and max(s['certs'])>1), None)}")

# Mesh diag aggregate
print()
print("--- mesh_diag (final per node) ---")
for i, m in enumerate(final):
    mesh = m.get("mesh") or {}
    pull = m.get("pull") or {}
    print(
        f"  val{i+1}: certified={m.get('certified')} round={m.get('max_cert_round')} "
        f"exec={m.get('executed_txs')} anchors={m.get('bullshark_anchors')} "
        f"deferred_batch={mesh.get('cert_deferred_missing_batch')} "
        f"deferred_parent={mesh.get('cert_deferred_missing_parent')} "
        f"cert_applied={mesh.get('cert_applied')} reapplied={mesh.get('cert_reapplied_from_pending')} "
        f"pending_certs={mesh.get('pending_certs')} "
        f"apply_lag_ms_avg={mesh.get('cert_apply_lag_ms_avg')} "
        f"apply_lag_ms_max={mesh.get('cert_apply_lag_ms_max')} "
        f"from_create_lag_ms_avg={mesh.get('cert_from_create_lag_ms_avg')} "
        f"from_create_lag_ms_max={mesh.get('cert_from_create_lag_ms_max')} "
        f"validation={mesh.get('validation_model')} "
        f"pull_inflight_peak={pull.get('inflight_peak') or mesh.get('pull_inflight_peak')} "
        f"want_batch_max={pull.get('want_batch_max') or mesh.get('pull_want_batch_max')} "
        f"pull_retries={pull.get('retries')} pending_pulls={pull.get('pending_pulls')} "
        f"parallelism={pull.get('batch_pull_parallelism') or mesh.get('batch_pull_parallelism')}"
    )

print()
print("--- 2) Batch pull parallelism ---")
want_max = max(int((m.get("pull") or {}).get("want_batch_max") or (m.get("mesh") or {}).get("pull_want_batch_max") or 0) for m in final)
inflight_peak = max(int((m.get("pull") or {}).get("inflight_peak") or (m.get("mesh") or {}).get("pull_inflight_peak") or 0) for m in final)
print(f"cluster want_batch_max={want_max} inflight_peak={inflight_peak}")
if want_max > 1 or inflight_peak > 1:
    print("RESULT: SECONDARIES CAN REQUEST MULTIPLE HASHES IN PARALLEL (multi-hash want / multi-inflight).")
else:
    print("RESULT: No multi-hash parallelism observed this run (idle mesh or single-object pulls only).")

print()
print("--- 3) Validation model ---")
models = set((m.get("mesh") or {}).get("validation_model") or "unknown" for m in final)
print(f"validation_model={models}")
print("Code path: CertificateDag::try_certify requires EVERY parent already in certificates.")
print("           on_remote_cert parks cert if batch/parents missing; does NOT apply speculatively.")
print("RESULT: IN-ORDER parent-cert validation (not speculative).")

deferred_b = sum(int((m.get("mesh") or {}).get("cert_deferred_missing_batch") or 0) for m in final)
deferred_p = sum(int((m.get("mesh") or {}).get("cert_deferred_missing_parent") or 0) for m in final)
reapplied = sum(int((m.get("mesh") or {}).get("cert_reapplied_from_pending") or 0) for m in final)
print(f"deferred_missing_batch_sum={deferred_b} deferred_missing_parent_sum={deferred_p} reapplied_sum={reapplied}")

print()
print("--- Ship / Phase 50 recommendation ---")
final_lag = lags[-1] if lags else 99
max_lag = max(lags) if lags else 99
execs = [int(m.get("executed_txs") or 0) for m in final]
agree = min(execs) == max(execs) and final_lag == 0
create_max = max(float((m.get("mesh") or {}).get("cert_from_create_lag_ms_max") or 0) for m in final)
apply_max = max(float((m.get("mesh") or {}).get("cert_apply_lag_ms_max") or 0) for m in final)

# Heuristic
if agree and max_lag <= 2 and create_max < 5000 and admit_fail == 0:
    print("SHIP_NOW: Mesh lag stays small under this load; Phase 49 is fine for main.")
    print("  (max_round_lag<=2, final agreement, cert create→apply max <5s, clean admits)")
    rc = 0
elif agree and max_lag <= 4:
    print("SHIP_WITH_NOTE: Mesh converges but lag spikes observed — track in Phase 50.")
    print(f"  max_round_lag={max_lag} create_lag_max_ms={create_max} apply_lag_max_ms={apply_max}")
    rc = 0
else:
    print("PHASE_50: Mesh lag / disagreement under load needs work before relying on multi-node settle.")
    print(f"  final_round_lag={final_lag} max_round_lag={max_lag} execs={execs} agree={agree}")
    print(f"  create_lag_max_ms={create_max} apply_lag_max_ms={apply_max} deferred_batch={deferred_b}")
    rc = 1

print()
print("mesh_lag_smoke DONE")
sys.exit(rc)
PY
