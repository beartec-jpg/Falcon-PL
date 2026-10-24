#!/usr/bin/env bash
# Start default boring lab stack: 2 shards + gmpd×2 + facade.
#   export SSH_F2=100.64.0.3
#   bash deploy/gmp/start-stack.sh
#   bash deploy/gmp/start-stack.sh --shards 3
#   bash deploy/gmp/start-stack.sh --watchdog   # also ensure watchdog daemon
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1091
if [[ -f "$DIR/stack.env.default" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$DIR/stack.env.default"
  set +a
fi

MESH="${MESH_ROOT:-$HOME/falcon-dag-mesh}"
BIN_DIR="$MESH/bin"
RS="${FALCON_RS:-$HOME/falcon-dag/falcon-dag-rs}"
SSH_F2="${SSH_F2:-100.64.0.3}"
F2_IP="${F2_IP:-192.0.2.2}"
export SSH_F2 F2_IP
mkdir -p "$MESH/logs" "$MESH/stack" "$BIN_DIR"

NSHARDS="${STACK_SHARDS:-2}"
WANT_WATCHDOG=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --shards)
      NSHARDS="${2:-2}"
      shift 2
      ;;
    --watchdog|-w)
      WANT_WATCHDOG=1
      shift
      ;;
    [0-9]*)
      NSHARDS="$1"
      shift
      ;;
    *)
      echo "unknown arg: $1" >&2
      exit 2
      ;;
  esac
done

echo "== install bins =="
for b in falcon-dagd falcon-gmpd falcon-gmp-facade falcon-gcpd; do
  if [[ -x "$RS/target/release/$b" ]]; then
    cp -f "$RS/target/release/$b" "$BIN_DIR/"
    echo "  $b updated"
  elif [[ ! -x "$BIN_DIR/$b" ]]; then
    echo "WARN: missing $BIN_DIR/$b" >&2
  fi
done

echo "== stop previous =="
# Do not kill an outer watchdog that invoked us for recovery
bash "$DIR/stop-stack.sh"

# Fresh smoke sequences after wipe/boot
rm -f "$MESH/stack/smoke_seq.json" 2>/dev/null || true
rm -f "$MESH/stack/state.stopped.json" 2>/dev/null || true

# light log rotation on start
shopt -s nullglob
for f in "$MESH"/logs/*.log; do
  [[ -f "$f" ]] || continue
  base=$(basename "$f")
  [[ "$base" == "watchdog.log" ]] && continue
  sz=$(stat -c%s "$f" 2>/dev/null || echo 0)
  if [[ "$sz" -gt $((20 * 1024 * 1024)) ]]; then
    mv -f "$f" "${f}.1" || true
  fi
done
shopt -u nullglob

echo "== GMP daemons (shards=$NSHARDS) =="
nohup "$BIN_DIR/falcon-gmpd" --listen 0.0.0.0:18500 --shards 16 --lease-ttl-s 8 \
  >>"$MESH/logs/gmp0.log" 2>&1 &
echo $! >"$MESH/logs/gmp0.pid"
echo "  gmp0 pid=$(cat "$MESH/logs/gmp0.pid")"
nohup "$BIN_DIR/falcon-gmpd" --listen 0.0.0.0:18510 --shards 16 --lease-ttl-s 8 \
  >>"$MESH/logs/gmp1.log" 2>&1 &
echo $! >"$MESH/logs/gmp1.pid"
echo "  gmp1 pid=$(cat "$MESH/logs/gmp1.pid")"
if [[ "$NSHARDS" -ge 3 ]]; then
  nohup "$BIN_DIR/falcon-gmpd" --listen 0.0.0.0:18520 --shards 16 --lease-ttl-s 8 \
    >>"$MESH/logs/gmp2.log" 2>&1 &
  echo $! >"$MESH/logs/gmp2.pid"
  echo "  gmp2 pid=$(cat "$MESH/logs/gmp2.pid")"
fi
sleep 0.6

F1_IP="${F1_IP:-$(ip -4 -br addr 2>/dev/null | awk '/^lo /{next} /tailscale/{next} /docker/{next} /br-/{next} {print; exit}' | grep -oE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' | head -1)}"
F1_IP="${F1_IP:-127.0.0.1}"
export F1_IP
echo "F1_IP=$F1_IP F2_IP=$F2_IP"

echo "== boot mesh =="
NID_BASE="${NETWORK_ID_BASE:-9930}"
echo "  network_id_base=$NID_BASE (shard s → base+s)"
python3 "$DIR/boot_mesh.py" --shards "$NSHARDS" --network-id-base "$NID_BASE"

if [[ "$NSHARDS" -ge 3 ]]; then
  SHARD_ARGS=(
    --listen "${FACADE_LISTEN:-0.0.0.0:18499}"
    --xshard-timeout-s "${XSHARD_TIMEOUT_S:-45}"
    --shard "id=0,name=s0,gmp=http://${F1_IP}:18500,rpc=http://${F1_IP}:18001,bridge=acct14,lo=1,hi=14"
    --shard "id=1,name=s1,gmp=http://${F1_IP}:18510,rpc=http://${F1_IP}:18004,bridge=acct28,lo=15,hi=28"
    --shard "id=2,name=s2,gmp=http://${F1_IP}:18520,rpc=http://${F1_IP}:18007,bridge=acct40,lo=29,hi=40"
  )
  RPCS=$(python3 -c "import json; print(json.dumps([
    ['s0','127.0.0.1',18001],['s0','127.0.0.1',18002],['s0_remote','${F2_IP}',18003],
    ['s1','127.0.0.1',18004],['s1','127.0.0.1',18005],['s1_remote','${F2_IP}',18006],
    ['s2','127.0.0.1',18007],['s2','127.0.0.1',18008],['s2_remote','${F2_IP}',18009],
  ]))")
  GMPS='[["gmp0","http://127.0.0.1:18500"],["gmp1","http://127.0.0.1:18510"],["gmp2","http://127.0.0.1:18520"]]'
else
  SHARD_ARGS=(
    --listen "${FACADE_LISTEN:-0.0.0.0:18499}"
    --xshard-timeout-s "${XSHARD_TIMEOUT_S:-45}"
    --shard "id=0,name=s0,gmp=http://${F1_IP}:18500,rpc=http://${F1_IP}:18001,bridge=acct20,lo=1,hi=20"
    --shard "id=1,name=s1,gmp=http://${F1_IP}:18510,rpc=http://${F1_IP}:18004,bridge=acct40,lo=21,hi=40"
  )
  RPCS=$(python3 -c "import json; print(json.dumps([
    ['s0','127.0.0.1',18001],['s0','127.0.0.1',18002],['s0_remote','${F2_IP}',18003],
    ['s1','127.0.0.1',18004],['s1','127.0.0.1',18005],['s1_remote','${F2_IP}',18006],
  ]))")
  GMPS='[["gmp0","http://127.0.0.1:18500"],["gmp1","http://127.0.0.1:18510"]]'
fi

echo "== facade =="
nohup "$BIN_DIR/falcon-gmp-facade" "${SHARD_ARGS[@]}" >>"$MESH/logs/facade.log" 2>&1 &
echo $! >"$MESH/logs/facade.pid"
echo "  facade pid=$(cat "$MESH/logs/facade.pid")"
sleep 0.8

# Persist state for health/smoke
export NSHARDS F1_IP F2_IP RPCS GMPS
python3 - <<'PY'
import json, os, time
from pathlib import Path
mesh = Path(os.environ.get("MESH_ROOT", Path.home() / "falcon-dag-mesh"))
nshards = int(os.environ["NSHARDS"])
state = {
    "started_at": time.time(),
    "shards": nshards,
    "network_id_base": int(os.environ.get("NETWORK_ID_BASE", "9930")),
    "f1_ip": os.environ.get("F1_IP"),
    "f2_ip": os.environ.get("F2_IP"),
    "facade": os.environ.get("FACADE_URL", "http://127.0.0.1:18499"),
    "rpcs": json.loads(os.environ["RPCS"]),
    "gmps": json.loads(os.environ["GMPS"]),
    "bridges": (
        [{"shard": 0, "account": "acct14"}, {"shard": 1, "account": "acct28"}, {"shard": 2, "account": "acct40"}]
        if nshards >= 3
        else [{"shard": 0, "account": "acct20"}, {"shard": 1, "account": "acct40"}]
    ),
}
(mesh / "stack").mkdir(parents=True, exist_ok=True)
(mesh / "stack" / "state.json").write_text(json.dumps(state, indent=2) + "\n")
print("wrote", mesh / "stack" / "state.json")
PY

echo "== post-start health (wait up to 45s) =="
HC=2
for i in $(seq 1 15); do
  if python3 "$DIR/health.py" >"$MESH/stack/last_health.json" 2>"$MESH/stack/last_health.err"; then
    HC=0
    echo "  healthy after ${i} probe(s)"
    break
  else
    HC=$?
  fi
  # exit 1 = degraded still acceptable for boot (remote lag)
  if [[ "$HC" -eq 1 ]]; then
    echo "  degraded (acceptable) after ${i} probe(s)"
    break
  fi
  sleep 3
done

if [[ "$HC" -eq 2 ]]; then
  echo "ERROR: stack still DOWN after boot" >&2
  cat "$MESH/stack/last_health.err" 2>/dev/null || true
  exit 1
fi

if [[ "$WANT_WATCHDOG" -eq 1 ]]; then
  echo "== ensure watchdog =="
  bash "$DIR/watchdog.sh" --daemon
fi

echo
echo "Clients: POST http://127.0.0.1:18499/v1/submit"
echo "Status:  bash $DIR/status.sh"
echo "Health:  python3 $DIR/health.py"
echo "Smoke:   python3 $DIR/smoke.py"
echo "Watch:   bash $DIR/watchdog.sh --daemon"
echo "Stop:    bash $DIR/stop-stack.sh [--all]"
