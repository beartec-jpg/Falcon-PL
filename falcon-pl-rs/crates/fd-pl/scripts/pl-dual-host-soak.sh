#!/usr/bin/env bash
# Falcon PL v2.2 — dual-host private soak (host1 + host2).
#
# Layout (same as pl-testnet-v1):
#   F1: v1 :19101, v2 :19102
#   F2: v3 :19103
#
#   bash crates/fd-pl/scripts/pl-dual-host-soak.sh start
#   bash crates/fd-pl/scripts/pl-dual-host-soak.sh soak
#   bash crates/fd-pl/scripts/pl-dual-host-soak.sh payday
#   bash crates/fd-pl/scripts/pl-dual-host-soak.sh bond-smoke
#   bash crates/fd-pl/scripts/pl-dual-host-soak.sh stop
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# Dual-host may use LAN; optional env file for overrides (does not force Tailscale).
if [[ -f "$SCRIPT_DIR/pl-hosts.env" ]]; then
  # shellcheck source=pl-hosts.env
  # shellcheck disable=SC1091
  source "$SCRIPT_DIR/pl-hosts.env" || true
fi
RS="$(cd "$SCRIPT_DIR/../../.." && pwd)"
BIN="$RS/target/release"
ROOT="${PL_ROOT:-${PL_ROOT_DUAL:-$HOME/falcon-pl-private-v22-dual}}"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
SCHEME="${PL_SCHEME:-hmac}"
LOAD_WALLETS="${PL_LOAD_WALLETS:-32}"
# SSH alias from ~/.ssh/config (not bare IP only)
SSH_F2="${SSH_F2:-host2}"
# Dual-host F1↔F2 on LAN by default (override with F1_IP/F2_IP or Tailscale)
F1_IP="${F1_IP:-${F1_LAN:-192.0.2.1}}"
F2_IP="${F2_IP:-${F2_LAN:-192.0.2.2}}"
SEAL_MS="${PL_SEAL_MS:-400}"
PACK_BUDGET_MS="${PL_PACK_BUDGET_MS:-1000}"
# Shared-skip progress timeout (1s): detect → apply_shared_skip + gossip SkipPacker;
# standby packs next seal tick so peers can apply first.
PROGRESS_TIMEOUT_MS="${PL_PROGRESS_TIMEOUT_MS:-1000}"
MAX_MEMPOOL_TXS="${PL_MAX_MEMPOOL_TXS:-50000}"
MAX_TXS="${PL_MAX_TXS:-0}"
EPOCH_MS="${PL_EPOCH_MS:-10000}"
UNBOND_CD_MS="${PL_UNBOND_COOLDOWN_MS:-5000}"
SNAPSHOT_INTERVAL_MS="${PL_SNAPSHOT_INTERVAL_MS:-30000}"
SOAK_SEC="${PL_SOAK_SEC:-60}"
SOAK_TPS="${PL_SOAK_TPS:-30}"
BOND=1000
NETWORK_ID="${PL_NETWORK_ID:-2200}"
VALS="v1,v2,v3"
CTL=("$BIN/falcon-pl-ctl")

cmd="${1:-start}"
mkdir -p "$ROOT" "$LOG" "$KEYS" "$SNAP"

# Hard fail if host2 unreachable — never pretend dual-host on one box.
require_remote() {
  if ! ssh -o ConnectTimeout=10 -o BatchMode=yes "$SSH_F2" "echo ok" >/dev/null 2>&1; then
    echo "FAIL: cannot SSH to F2 ($SSH_F2). Dual-host refuses single-machine fallback." >&2
    exit 3
  fi
  if [[ "$F2_IP" == "127.0.0.1" || "$F2_IP" == "localhost" || "$F2_IP" == "$F1_IP" ]]; then
    echo "FAIL: F2_IP must be a distinct remote address (got F1=$F1_IP F2=$F2_IP)" >&2
    exit 3
  fi
  local me
  me=$(hostname -I 2>/dev/null || true)
  if [[ " $me " == *" $F2_IP "* ]]; then
    echo "FAIL: F2_IP=$F2_IP is local — both validators would land on this machine" >&2
    exit 3
  fi
  echo "[dual] F2 ok via $SSH_F2 mesh F1=$F1_IP F2=$F2_IP"
}

build() {
  cd "$RS"
  cargo build -p fd-pl --release 2>&1 | tail -8
}

gen_keys() {
  rm -rf "$KEYS"
  mkdir -p "$KEYS"
  "$BIN/falcon-pl-ctl" gen-keys \
    --keys-dir "$KEYS" \
    --scheme "$SCHEME" \
    --validators v1,v2,v3 \
    --load-wallets "$LOAD_WALLETS"
  "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account faucet --scheme "$SCHEME" || true
}

stop_local() {
  for f in "$LOG"/v1.pid "$LOG"/v2.pid; do
    if [[ -f "$f" ]]; then
      kill "$(cat "$f")" 2>/dev/null || true
      rm -f "$f"
    fi
  done
  for p in 19101 19102; do fuser -k "${p}/tcp" 2>/dev/null || true; done
}

stop_remote() {
  ssh -o ConnectTimeout=10 -o BatchMode=yes "$SSH_F2" \
    "if [[ -f $ROOT/logs/v3.pid ]]; then kill \$(cat $ROOT/logs/v3.pid) 2>/dev/null || true; rm -f $ROOT/logs/v3.pid; fi; fuser -k 19103/tcp 2>/dev/null || true; sleep 0.5; fuser -k 19103/tcp 2>/dev/null || true" \
    2>/dev/null || true
}

sync_remote() {
  ssh -o ConnectTimeout=10 -o BatchMode=yes "$SSH_F2" "mkdir -p $ROOT/bin $ROOT/keys $ROOT/logs $ROOT/snapshots"
  rsync -az "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$SSH_F2:$ROOT/bin/"
  rsync -az --delete "$KEYS/" "$SSH_F2:$ROOT/keys/"
}

common_args() {
  echo \
    --validators "$VALS" \
    --scheme "$SCHEME" \
    --keys-dir "$KEYS" \
    --load-wallets "$LOAD_WALLETS" \
    --seal-interval-ms "$SEAL_MS" \
    --max-txs "$MAX_TXS" \
    --pack-budget-ms "$PACK_BUDGET_MS" \
    --progress-timeout-ms "$PROGRESS_TIMEOUT_MS" \
    --max-mempool-txs "$MAX_MEMPOOL_TXS" \
    --bond "$BOND" \
    --epoch-ms "$EPOCH_MS" \
    --unbond-cooldown-ms "$UNBOND_CD_MS" \
    --snapshot-interval-ms "$SNAPSHOT_INTERVAL_MS" \
    --network-id "$NETWORK_ID"
}

start_local() {
  # shellcheck disable=SC2206
  local common=($(common_args))
  nohup "$BIN/falcon-pl-node" --id v1 \
    --listen "0.0.0.0:19101" \
    --peers "${F1_IP}:19102,${F2_IP}:19103" \
    --snapshot "$SNAP/v1.json" \
    --snapshot-out "$SNAP/v1.json" \
    "${common[@]}" >>"$LOG/v1.log" 2>&1 &
  echo $! >"$LOG/v1.pid"
  nohup "$BIN/falcon-pl-node" --id v2 \
    --listen "0.0.0.0:19102" \
    --peers "${F1_IP}:19101,${F2_IP}:19103" \
    --snapshot "$SNAP/v2.json" \
    --snapshot-out "$SNAP/v2.json" \
    "${common[@]}" >>"$LOG/v2.log" 2>&1 &
  echo $! >"$LOG/v2.pid"
  echo "F1 v1/v2 pids $(cat "$LOG/v1.pid") $(cat "$LOG/v2.pid")"
}

start_remote() {
  # shellcheck disable=SC2206
  local common=($(common_args))
  # rebuild common string for remote (same flags; keys path on remote)
  local rcommon=(
    --validators "$VALS"
    --scheme "$SCHEME"
    --keys-dir "$ROOT/keys"
    --load-wallets "$LOAD_WALLETS"
    --seal-interval-ms "$SEAL_MS"
    --max-txs "$MAX_TXS"
    --pack-budget-ms "$PACK_BUDGET_MS"
    --progress-timeout-ms "$PROGRESS_TIMEOUT_MS"
    --max-mempool-txs "$MAX_MEMPOOL_TXS"
    --bond "$BOND"
    --epoch-ms "$EPOCH_MS"
    --unbond-cooldown-ms "$UNBOND_CD_MS"
    --snapshot-interval-ms "$SNAPSHOT_INTERVAL_MS"
    --network-id "$NETWORK_ID"
  )
  ssh -o ConnectTimeout=15 -o BatchMode=yes "$SSH_F2" bash -s <<EOF
set -euo pipefail
mkdir -p $ROOT/logs $ROOT/snapshots
nohup $ROOT/bin/falcon-pl-node --id v3 \
  --listen 0.0.0.0:19103 \
  --peers ${F1_IP}:19101,${F1_IP}:19102 \
  --snapshot $ROOT/snapshots/v3.json \
  --snapshot-out $ROOT/snapshots/v3.json \
  ${rcommon[*]} >>$ROOT/logs/v3.log 2>&1 &
echo \$! > $ROOT/logs/v3.pid
echo "F2 v3 pid \$(cat $ROOT/logs/v3.pid)"
EOF
}

parse_status() {
  python3 -c "
import sys,json
raw=sys.stdin.read()
i=raw.find('{')
if i<0:
  print('down'); sys.exit(0)
b=json.JSONDecoder().raw_decode(raw[i:])[0]
body=b.get('body',b)
print('id', body.get('id'), 'tip', body.get('tip_height'),
      'mp', body.get('mempool'), 'epoch', body.get('epoch'),
      'fee', body.get('suggested_min_fee'),
      'params', body.get('params_version'))
"
}

status_all() {
  for p in 19101 19102; do
    echo "=== F1 :$p ==="
    "${CTL[@]}" --addr "127.0.0.1:$p" status 2>/dev/null | parse_status || echo down
  done
  echo "=== F2 :19103 ==="
  ssh -o ConnectTimeout=5 -o BatchMode=yes "$SSH_F2" \
    "$ROOT/bin/falcon-pl-ctl --addr 127.0.0.1:19103 status" 2>/dev/null | parse_status || echo down
}

# Steady load via pay loop (auto-seq) for SOAK_SEC at ~SOAK_TPS.
soak() {
  local A=127.0.0.1:19101
  local end=$((SECONDS + SOAK_SEC))
  local n=0 ok=0 err=0
  local interval
  interval=$(python3 -c "print(max(0.01, 1.0/${SOAK_TPS}))")
  echo "[soak] ${SOAK_SEC}s target ~${SOAK_TPS} tps interval=${interval}s"
  local tip0
  tip0=$("${CTL[@]}" --addr "$A" status 2>/dev/null | python3 -c "
import sys,json
b=json.load(sys.stdin); print((b.get('body') or b).get('tip_height',0))
" || echo 0)
  while (( SECONDS < end )); do
    if "${CTL[@]}" --addr "$A" pay --from alice --to bob --amount 1 --fee 2 \
      --sequence 0 --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null 2>&1; then
      ok=$((ok+1))
    else
      err=$((err+1))
    fi
    n=$((n+1))
    sleep "$interval"
  done
  echo "[soak] drain / pull catch-up (NeedLedgers via pongs)…"
  sleep 8
  local tip1
  tip1=$("${CTL[@]}" --addr "$A" status 2>/dev/null | python3 -c "
import sys,json
raw=sys.stdin.read(); i=raw.find('{')
b=json.JSONDecoder().raw_decode(raw[i:])[0]
print((b.get('body') or b).get('tip_height',0))
" || echo 0)
  echo "[soak] submits=$n ok=$ok err=$err tip $tip0→$tip1"
  status_all
  # tips must converge to same height+hash (pull catch-up + shared chain)
  PL_SOAK_BIN="$BIN" PL_SOAK_SSH="$SSH_F2" PL_SOAK_ROOT="$ROOT" python3 - <<'PY'
import json, os, subprocess, sys, time
BIN = os.environ["PL_SOAK_BIN"]
SSH = os.environ["PL_SOAK_SSH"]
ROOT = os.environ["PL_SOAK_ROOT"]

def tip_local(port):
    o = subprocess.check_output([f"{BIN}/falcon-pl-ctl", "--addr", f"127.0.0.1:{port}", "status"], text=True)
    b = json.JSONDecoder().raw_decode(o[o.find("{"):])[0]
    body = b.get("body", b)
    return int(body.get("tip_height") or 0), (body.get("tip_hash") or ""), int(body.get("tip_lag") or 0)

def tip_remote():
    o = subprocess.check_output(
        ["ssh", "-o", "ConnectTimeout=8", "-o", "BatchMode=yes", SSH,
         f"{ROOT}/bin/falcon-pl-ctl --addr 127.0.0.1:19103 status"],
        text=True,
    )
    b = json.JSONDecoder().raw_decode(o[o.find("{"):])[0]
    body = b.get("body", b)
    return int(body.get("tip_height") or 0), (body.get("tip_hash") or ""), int(body.get("tip_lag") or 0)

last = None
for attempt in range(25):
    tips = []
    for p in (19101, 19102):
        try:
            h, hh, lag = tip_local(p)
            tips.append((str(p), h, hh[:12], lag))
        except Exception as e:
            tips.append((str(p), None, str(e), None))
    try:
        h, hh, lag = tip_remote()
        tips.append(("19103", h, hh[:12], lag))
    except Exception as e:
        tips.append(("19103", None, str(e), None))
    last = tips
    heights = [t[1] for t in tips if isinstance(t[1], int)]
    hashes = {t[2] for t in tips if isinstance(t[1], int) and t[2]}
    if len(heights) >= 3 and max(heights) - min(heights) <= 1 and len(hashes) == 1 and max(heights) >= 1:
        print("tips", tips)
        print("soak tip-agree OK (height Δ≤1, same hash, tip≥1)")
        sys.exit(0)
    time.sleep(1.0)
print("FAIL tip disagree after catch-up", last)
sys.exit(2)
PY
}

bond_smoke() {
  local A=127.0.0.1:19101
  local A2=127.0.0.1:19102
  local kd=(--scheme "$SCHEME" --keys-dir "$KEYS")
  # Drive packing on both local validators (lottery can elect either).
  keep_packing() {
    "${CTL[@]}" --addr "$A" pay --from alice --to bob --amount 1 --fee 2 \
      --sequence 0 --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null 2>&1 || true
    "${CTL[@]}" --addr "$A" seal >/dev/null 2>&1 || true
    "${CTL[@]}" --addr "$A2" seal >/dev/null 2>&1 || true
  }
  wait_val() {
    local field="$1" want="$2" tries="${3:-40}"
    python3 - "$BIN" "$A" "$field" "$want" "$tries" <<'PY' || return 1
import json, subprocess, sys, time
bin, addr, field, want, tries = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]), int(sys.argv[5])
for _ in range(tries):
    o = subprocess.check_output([f"{bin}/falcon-pl-ctl", "--addr", addr, "status"], text=True)
    b = json.JSONDecoder().raw_decode(o[o.find("{"):])[0]
    body = b.get("body", b)
    for v in body.get("validators", []):
        if v.get("id") == "v1":
            got = int(v.get(field) or 0)
            print(f"v1 {field}={got} (want {want})")
            if got == want:
                sys.exit(0)
    time.sleep(0.4)
print(f"timeout waiting v1.{field}=={want}", file=sys.stderr)
sys.exit(1)
PY
  }
  echo "[bond] unbond 100 from v1 as ledger tx (cooldown ${UNBOND_CD_MS}ms)"
  "${CTL[@]}" --addr "$A" unbond --id v1 --amount 100 "${kd[@]}"
  for _ in $(seq 1 25); do keep_packing; sleep 0.25; done
  wait_val unbonding 100 60 || return 1
  wait_val bond 900 15 || return 1
  echo "[bond] wait cooldown ${UNBOND_CD_MS}ms then complete-unbond"
  # Wall sleep: node clocks advance on seal ticks; all hosts must pass ready_ms.
  sleep $(( (UNBOND_CD_MS / 1000) + 3 ))
  for _ in $(seq 1 15); do keep_packing; sleep 0.25; done
  # Retry complete a few times (may land as "not ready" until clocks pass).
  for _ in 1 2 3 4 5; do
    "${CTL[@]}" --addr "$A" complete-unbond --id v1 "${kd[@]}" 2>/dev/null || true
    for __ in $(seq 1 8); do keep_packing; sleep 0.25; done
    if wait_val unbonding 0 8; then break; fi
  done
  wait_val unbonding 0 30 || return 1
  echo "[bond] re-bond 100 from alice → v1 (ledger tx)"
  "${CTL[@]}" --addr "$A" bond --id v1 --from alice --amount 100 "${kd[@]}"
  for _ in $(seq 1 30); do keep_packing; sleep 0.25; done
  wait_val bond 1000 60 || return 1
  echo "[bond] OK"
  return 0
}

payday() {
  # Dual-host: only seal local F1 ports; F2 packs via mesh lottery.
  python3 "$RS/crates/fd-pl/scripts/pl-epoch-payday.py" \
    --addr 127.0.0.1:19101 \
    --seal-addrs "127.0.0.1:19101,127.0.0.1:19102" \
    --keys-dir "$KEYS" \
    --scheme "$SCHEME" \
    --ctl "$BIN/falcon-pl-ctl" \
    --wait-seal-s 6 \
    --pack-wait-s 8
}

case "$cmd" in
  build) build ;;
  keys) gen_keys ;;
  start)
    require_remote
    build
    if [[ ! -f "$KEYS/pl_keys.json" ]]; then gen_keys; fi
    stop_local; stop_remote
    # Clean snaps so mesh starts at shared tip 0
    rm -rf "$SNAP"/* 2>/dev/null || true
    ssh -o ConnectTimeout=10 -o BatchMode=yes "$SSH_F2" "rm -rf $ROOT/snapshots/*" 2>/dev/null || true
    start_local
    sync_remote
    sleep 1
    start_remote
    sleep 3
    status_all
    ;;
  stop) stop_local; stop_remote; echo stopped ;;
  status) status_all ;;
  soak) soak ;;
  bond-smoke) bond_smoke ;;
  payday) payday ;;
  all)
    require_remote
    build
    if [[ ! -f "$KEYS/pl_keys.json" ]]; then gen_keys; fi
    stop_local; stop_remote
    rm -rf "$SNAP"/* 2>/dev/null || true
    ssh -o ConnectTimeout=10 -o BatchMode=yes "$SSH_F2" "rm -rf $ROOT/snapshots/*" 2>/dev/null || true
    start_local
    sync_remote
    sleep 1
    start_remote
    sleep 3
    status_all
    # Soak is the production gate (tip agree under load across hosts).
    soak
    # Bond/payday are feature checks; do not mask a soak failure, but warn only
    # if soak already proved multi-host tip agreement.
    if ! bond_smoke; then
      echo "WARN: bond-smoke did not complete (separate from tip-agree gate)"
    fi
    if ! payday; then
      echo "WARN: payday did not complete (separate from tip-agree gate)"
    fi
    status_all
    stop_local; stop_remote
    echo "dual-host all done"
    ;;
  *)
    echo "usage: $0 {build|keys|start|stop|status|soak|bond-smoke|payday|all}"
    exit 1
    ;;
esac
