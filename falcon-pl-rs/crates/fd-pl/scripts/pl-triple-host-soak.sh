#!/usr/bin/env bash
# Falcon PL v2.2 — triple-host private soak (host1 + host2 + droplet).
#
# One validator per machine (real multi-host mesh — not local loopback):
#   F1 host1 : v1 :19101
#   F2 host2 : v2 :19102
#   F3 droplet : v3 :19103
#
# Peer mesh uses Tailscale IPs by default (see pl-hosts.env) so the droplet
# can reach both falcons. LAN dual-host is a separate script.
#
#   bash crates/fd-pl/scripts/pl-triple-host-soak.sh preflight
#   bash crates/fd-pl/scripts/pl-triple-host-soak.sh start
#   bash crates/fd-pl/scripts/pl-triple-host-soak.sh soak
#   bash crates/fd-pl/scripts/pl-triple-host-soak.sh all
#   bash crates/fd-pl/scripts/pl-triple-host-soak.sh stop
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=pl-hosts.env
# shellcheck disable=SC1091
source "$SCRIPT_DIR/pl-hosts.env"

RS="$(cd "$SCRIPT_DIR/../../.." && pwd)"
BIN="$RS/target/release"
ROOT="${PL_ROOT:-$PL_ROOT_TRIPLE}"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
SCHEME="${PL_SCHEME:-hmac}"
LOAD_WALLETS="${PL_LOAD_WALLETS:-32}"
# Triple mesh MUST use Tailscale (droplet has no LAN path to falcons)
F1_IP="${F1_IP:-$F1_TS}"
F2_IP="${F2_IP:-$F2_TS}"
F3_IP="${F3_IP:-$F3_TS}"
SEAL_MS="${PL_SEAL_MS:-400}"
PACK_BUDGET_MS="${PL_PACK_BUDGET_MS:-1000}"
PROGRESS_TIMEOUT_MS="${PL_PROGRESS_TIMEOUT_MS:-1000}"
MAX_MEMPOOL_TXS="${PL_MAX_MEMPOOL_TXS:-50000}"
MAX_TXS="${PL_MAX_TXS:-0}"
EPOCH_MS="${PL_EPOCH_MS:-10000}"
UNBOND_CD_MS="${PL_UNBOND_COOLDOWN_MS:-5000}"
SNAPSHOT_INTERVAL_MS="${PL_SNAPSHOT_INTERVAL_MS:-30000}"
SOAK_SEC="${PL_SOAK_SEC:-60}"
SOAK_TPS="${PL_SOAK_TPS:-20}"
BOND=1000
NETWORK_ID="${PL_NETWORK_ID:-2200}"
VALS="v1,v2,v3"
CTL=("$BIN/falcon-pl-ctl")
SSH_OPTS=(-o ConnectTimeout=12 -o BatchMode=yes -o StrictHostKeyChecking=accept-new)

cmd="${1:-preflight}"
mkdir -p "$ROOT" "$LOG" "$KEYS" "$SNAP"

ssh_h() {
  local host="$1"
  shift
  ssh "${SSH_OPTS[@]}" "$host" "$@"
}

require_host() {
  local label="$1" host="$2"
  if ! ssh_h "$host" "echo ok" >/dev/null 2>&1; then
    echo "FAIL: cannot SSH to $label ($host) — multi-host tests refuse single-machine fallback" >&2
    exit 3
  fi
  local who hn
  who=$(ssh_h "$host" 'whoami')
  hn=$(ssh_h "$host" 'hostname')
  echo "[preflight] $label ok host=$host user=$who hostname=$hn"
}

# Refuse if F2/F3 mesh IPs collapse to localhost / this host only.
assert_multi_host_ips() {
  local me
  me=$(hostname -I 2>/dev/null || true)
  for ip in "$F1_IP" "$F2_IP" "$F3_IP"; do
    if [[ "$ip" == "127.0.0.1" || "$ip" == "localhost" ]]; then
      echo "FAIL: peer IP must not be localhost ($ip). Source pl-hosts.env Tailscale IPs." >&2
      exit 3
    fi
  done
  if [[ "$F1_IP" == "$F2_IP" || "$F2_IP" == "$F3_IP" || "$F1_IP" == "$F3_IP" ]]; then
    echo "FAIL: F1/F2/F3 IPs must be distinct ($F1_IP $F2_IP $F3_IP)" >&2
    exit 3
  fi
  # F2 and F3 must not be this machine's addresses
  for ip in "$F2_IP" "$F3_IP"; do
    if [[ " $me " == *" $ip "* ]]; then
      echo "FAIL: $ip is local to this host — remote validators would not be multi-machine" >&2
      exit 3
    fi
  done
  echo "[preflight] mesh F1=$F1_IP F2=$F2_IP F3=$F3_IP"
}

preflight() {
  echo "======== triple-host preflight ========"
  echo "this host: $(hostname) $(hostname -I | awk '{print $1}')"
  assert_multi_host_ips
  require_host "F2 host2" "$SSH_F2"
  require_host "F3 droplet" "$SSH_F3"
  # TCP reachability for RPC ports once nodes run is checked after start;
  # here verify reverse path: remotes can open a socket toward F1 TS IP:22
  ssh_h "$SSH_F2" "timeout 3 bash -c 'echo >/dev/tcp/${F1_IP}/22'" >/dev/null
  ssh_h "$SSH_F3" "timeout 3 bash -c 'echo >/dev/tcp/${F1_IP}/22'" >/dev/null
  ssh_h "$SSH_F3" "timeout 3 bash -c 'echo >/dev/tcp/${F2_IP}/22'" >/dev/null || true
  echo "[preflight] remotes can reach F1 Tailscale SSH"
  echo "PASS preflight"
}

build() {
  cd "$RS"
  cargo build -p fd-pl --release 2>&1 | tail -8
  [[ -x "$BIN/falcon-pl-node" && -x "$BIN/falcon-pl-ctl" ]]
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
  if [[ -f "$LOG/v1.pid" ]]; then
    kill "$(cat "$LOG/v1.pid")" 2>/dev/null || true
    rm -f "$LOG/v1.pid"
  fi
  fuser -k 19101/tcp 2>/dev/null || true
}

stop_remote_f2() {
  ssh_h "$SSH_F2" \
    "if [[ -f $ROOT/logs/v2.pid ]]; then kill \$(cat $ROOT/logs/v2.pid) 2>/dev/null || true; rm -f $ROOT/logs/v2.pid; fi; fuser -k 19102/tcp 2>/dev/null || true" \
    2>/dev/null || true
}

stop_remote_f3() {
  ssh_h "$SSH_F3" \
    "if [[ -f $ROOT/logs/v3.pid ]]; then kill \$(cat $ROOT/logs/v3.pid) 2>/dev/null || true; rm -f $ROOT/logs/v3.pid; fi; fuser -k 19103/tcp 2>/dev/null || true" \
    2>/dev/null || true
}

stop_all() {
  stop_local
  stop_remote_f2
  stop_remote_f3
  echo stopped
}

sync_remote() {
  local host="$1"
  ssh_h "$host" "mkdir -p $ROOT/bin $ROOT/keys $ROOT/logs $ROOT/snapshots"
  rsync -az -e "ssh ${SSH_OPTS[*]}" "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$host:$ROOT/bin/"
  rsync -az --delete -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$host:$ROOT/keys/"
  echo "[sync] $host ← node+ctl+keys"
}

common_remote_flags() {
  # flags string for remote heredoc (keys path on remote = $ROOT/keys)
  echo \
    --validators "$VALS" \
    --scheme "$SCHEME" \
    --keys-dir "$ROOT/keys" \
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
  local common=($(
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
  ))
  nohup "$BIN/falcon-pl-node" --id v1 \
    --listen "0.0.0.0:19101" \
    --peers "${F2_IP}:19102,${F3_IP}:19103" \
    --snapshot "$SNAP/v1.json" \
    --snapshot-out "$SNAP/v1.json" \
    "${common[@]}" >>"$LOG/v1.log" 2>&1 &
  echo $! >"$LOG/v1.pid"
  echo "F1 v1 pid $(cat "$LOG/v1.pid") peers=${F2_IP}:19102,${F3_IP}:19103"
}

start_remote_f2() {
  local flags
  flags=$(common_remote_flags)
  # shellcheck disable=SC2086
  ssh_h "$SSH_F2" bash -s <<EOF
set -euo pipefail
mkdir -p $ROOT/logs $ROOT/snapshots
nohup $ROOT/bin/falcon-pl-node --id v2 \
  --listen 0.0.0.0:19102 \
  --peers ${F1_IP}:19101,${F3_IP}:19103 \
  --snapshot $ROOT/snapshots/v2.json \
  --snapshot-out $ROOT/snapshots/v2.json \
  $flags >>$ROOT/logs/v2.log 2>&1 &
echo \$! > $ROOT/logs/v2.pid
echo "F2 v2 pid \$(cat $ROOT/logs/v2.pid)"
EOF
}

start_remote_f3() {
  local flags
  flags=$(common_remote_flags)
  # shellcheck disable=SC2086
  ssh_h "$SSH_F3" bash -s <<EOF
set -euo pipefail
mkdir -p $ROOT/logs $ROOT/snapshots
nohup $ROOT/bin/falcon-pl-node --id v3 \
  --listen 0.0.0.0:19103 \
  --peers ${F1_IP}:19101,${F2_IP}:19102 \
  --snapshot $ROOT/snapshots/v3.json \
  --snapshot-out $ROOT/snapshots/v3.json \
  $flags >>$ROOT/logs/v3.log 2>&1 &
echo \$! > $ROOT/logs/v3.pid
echo "F3 v3 pid \$(cat $ROOT/logs/v3.pid)"
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
  echo "=== F1 :19101 ($F1_IP) ==="
  "${CTL[@]}" --addr "127.0.0.1:19101" status 2>/dev/null | parse_status || echo down
  echo "=== F2 :19102 ($F2_IP via $SSH_F2) ==="
  ssh_h "$SSH_F2" \
    "$ROOT/bin/falcon-pl-ctl --addr 127.0.0.1:19102 status" 2>/dev/null | parse_status || echo down
  echo "=== F3 :19103 ($F3_IP via $SSH_F3) ==="
  ssh_h "$SSH_F3" \
    "$ROOT/bin/falcon-pl-ctl --addr 127.0.0.1:19103 status" 2>/dev/null | parse_status || echo down
}

# Require all three endpoints live or fail (no single-host PASS).
require_all_up() {
  local bad=0
  if ! "${CTL[@]}" --addr 127.0.0.1:19101 status >/dev/null 2>&1; then
    echo "FAIL: F1 v1 down"; bad=1
  fi
  if ! ssh_h "$SSH_F2" "$ROOT/bin/falcon-pl-ctl --addr 127.0.0.1:19102 status" >/dev/null 2>&1; then
    echo "FAIL: F2 v2 down"; bad=1
  fi
  if ! ssh_h "$SSH_F3" "$ROOT/bin/falcon-pl-ctl --addr 127.0.0.1:19103 status" >/dev/null 2>&1; then
    echo "FAIL: F3 v3 down"; bad=1
  fi
  [[ $bad -eq 0 ]] || exit 4
}

start_all() {
  preflight
  build
  if [[ ! -f "$KEYS/pl_keys.json" ]]; then gen_keys; fi
  stop_all
  rm -rf "$SNAP"/* 2>/dev/null || true
  ssh_h "$SSH_F2" "rm -rf $ROOT/snapshots/*" 2>/dev/null || true
  ssh_h "$SSH_F3" "rm -rf $ROOT/snapshots/*" 2>/dev/null || true
  : >"$LOG/v1.log" 2>/dev/null || true
  ssh_h "$SSH_F2" ": > $ROOT/logs/v2.log" 2>/dev/null || true
  ssh_h "$SSH_F3" ": > $ROOT/logs/v3.log" 2>/dev/null || true

  sync_remote "$SSH_F2"
  sync_remote "$SSH_F3"
  start_local
  sleep 1
  start_remote_f2
  sleep 1
  start_remote_f3
  sleep 4
  status_all
  require_all_up
  echo "triple-host mesh up"
}

soak() {
  require_all_up
  local A=127.0.0.1:19101
  local end=$((SECONDS + SOAK_SEC))
  local n=0 ok=0 err=0
  local interval
  interval=$(python3 -c "print(max(0.01, 1.0/${SOAK_TPS}))")
  echo "[soak] ${SOAK_SEC}s target ~${SOAK_TPS} tps across F1+F2+F3"
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
  echo "[soak] drain / pull catch-up…"
  sleep 12
  local tip1
  tip1=$("${CTL[@]}" --addr "$A" status 2>/dev/null | python3 -c "
import sys,json
raw=sys.stdin.read(); i=raw.find('{')
b=json.JSONDecoder().raw_decode(raw[i:])[0]
print((b.get('body') or b).get('tip_height',0))
" || echo 0)
  echo "[soak] submits=$n ok=$ok err=$err tip $tip0→$tip1"
  status_all
  PL_SOAK_BIN="$BIN" PL_SOAK_SSH_F2="$SSH_F2" PL_SOAK_SSH_F3="$SSH_F3" PL_SOAK_ROOT="$ROOT" \
  python3 - <<'PY'
import json, os, subprocess, sys, time
BIN = os.environ["PL_SOAK_BIN"]
SSH_F2 = os.environ["PL_SOAK_SSH_F2"]
SSH_F3 = os.environ["PL_SOAK_SSH_F3"]
ROOT = os.environ["PL_SOAK_ROOT"]
SSH_OPTS = ["-o", "ConnectTimeout=12", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new"]

def tip_local(port):
    o = subprocess.check_output([f"{BIN}/falcon-pl-ctl", "--addr", f"127.0.0.1:{port}", "status"], text=True)
    b = json.JSONDecoder().raw_decode(o[o.find("{"):])[0]
    body = b.get("body", b)
    return int(body.get("tip_height") or 0), (body.get("tip_hash") or "")[:12]

def tip_remote(ssh_host, port):
    o = subprocess.check_output(
        ["ssh", *SSH_OPTS, ssh_host, f"{ROOT}/bin/falcon-pl-ctl --addr 127.0.0.1:{port} status"],
        text=True,
    )
    b = json.JSONDecoder().raw_decode(o[o.find("{"):])[0]
    body = b.get("body", b)
    return int(body.get("tip_height") or 0), (body.get("tip_hash") or "")[:12]

last = None
for _ in range(30):
    tips = []
    try:
        tips.append(("F1:19101", *tip_local(19101)))
    except Exception as e:
        tips.append(("F1:19101", None, str(e)[:40]))
    try:
        tips.append(("F2:19102", *tip_remote(SSH_F2, 19102)))
    except Exception as e:
        tips.append(("F2:19102", None, str(e)[:40]))
    try:
        tips.append(("F3:19103", *tip_remote(SSH_F3, 19103)))
    except Exception as e:
        tips.append(("F3:19103", None, str(e)[:40]))
    last = tips
    heights = [t[1] for t in tips if isinstance(t[1], int)]
    hashes = {t[2] for t in tips if isinstance(t[1], int) and t[2]}
    if len(heights) >= 3 and max(heights) - min(heights) <= 2 and len(hashes) == 1 and max(heights) >= 1:
        print("tips", tips)
        print("soak tip-agree OK (height Δ≤2, same hash, tip≥1) across F1+F2+F3")
        sys.exit(0)
    time.sleep(1.0)
print("FAIL tip disagree after catch-up (triple-host)", last)
sys.exit(2)
PY
}

case "$cmd" in
  preflight) preflight ;;
  build) build ;;
  keys) gen_keys ;;
  start) start_all ;;
  stop) stop_all ;;
  status) status_all ;;
  soak) soak ;;
  all)
    start_all
    soak
    status_all
    stop_all
    echo "triple-host all done"
    ;;
  *)
    echo "usage: $0 {preflight|build|keys|start|stop|status|soak|all}"
    echo "hosts: F1=$(hostname) F2=$SSH_F2 F3=$SSH_F3"
    echo "mesh:  $F1_IP / $F2_IP / $F3_IP  (Tailscale — required for droplet)"
    exit 1
    ;;
esac
