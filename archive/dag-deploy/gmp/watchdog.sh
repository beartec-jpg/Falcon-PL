#!/usr/bin/env bash
# Keep the boring lab stack alive: health loop, smoke, log rotate, restart on DOWN.
#
#   export SSH_F2=100.64.0.3
#   bash deploy/gmp/watchdog.sh --daemon   # preferred
#   bash deploy/gmp/watchdog.sh            # foreground
#
# Stop: bash deploy/gmp/stop-stack.sh --all
#
# Single-instance via flock. No set -e — failed health/smoke must not kill us.
set -uo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"

# shellcheck disable=SC1091
if [[ -f "$DIR/stack.env.default" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$DIR/stack.env.default" || true
  set +a
fi

MESH="${MESH_ROOT:-$HOME/falcon-dag-mesh}"
LOGDIR="$MESH/logs"
STATEDIR="$MESH/stack"
LOCKFILE="$STATEDIR/watchdog.lock"
HBFILE="$STATEDIR/watchdog.heartbeat"
RESTART_STATE="$STATEDIR/watchdog_restarts.json"

INTERVAL="${WATCHDOG_INTERVAL_S:-60}"
SMOKE_EVERY="${SMOKE_EVERY_N:-5}"
LOG_MAX_MB="${LOG_MAX_MB:-50}"
RESTART_COOLDOWN_S="${RESTART_COOLDOWN_S:-120}"
RESTART_MAX_PER_HOUR="${RESTART_MAX_PER_HOUR:-3}"
STACK_SHARDS="${STACK_SHARDS:-2}"

mkdir -p "$LOGDIR" "$STATEDIR"

DAEMON=0
if [[ "${1:-}" == "--daemon" || "${1:-}" == "-d" ]]; then
  DAEMON=1
fi

log() {
  local line
  line="$(date -Is) $*"
  echo "$line" >>"$LOGDIR/watchdog.log"
  if [[ -t 1 ]]; then
    echo "$line"
  fi
}

heartbeat() {
  printf '%s\n' "$(date +%s)" >"$HBFILE"
  echo $$ >"$LOGDIR/watchdog.pid"
}

rotate_logs() {
  local max_bytes=$((LOG_MAX_MB * 1024 * 1024))
  shopt -s nullglob
  for f in "$LOGDIR"/*.log; do
    local base
    base=$(basename "$f")
    [[ "$base" == "watchdog.log" ]] && continue
    local sz
    sz=$(stat -c%s "$f" 2>/dev/null || echo 0)
    if [[ "$sz" -gt "$max_bytes" ]]; then
      mv -f "$f" "${f}.1" || true
      : >"$f"
      log "rotated $f ($sz bytes)"
    fi
  done
  if [[ -f "$LOGDIR/watchdog.log" ]]; then
    local wsz
    wsz=$(stat -c%s "$LOGDIR/watchdog.log" 2>/dev/null || echo 0)
    if [[ "$wsz" -gt "$max_bytes" ]]; then
      mv -f "$LOGDIR/watchdog.log" "$LOGDIR/watchdog.log.1" || true
      : >"$LOGDIR/watchdog.log"
      log "rotated watchdog.log ($wsz bytes)"
    fi
  fi
}

# Prints reason and exits 1 if blocked; exits 0 if restart allowed.
restart_gate() {
  python3 - "$RESTART_STATE" "$RESTART_COOLDOWN_S" "$RESTART_MAX_PER_HOUR" <<'PY'
import json, sys, time
path, cool, max_h = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
now = time.time()
try:
    d = json.loads(open(path).read())
except Exception:
    d = {"times": []}
times = [t for t in d.get("times", []) if now - t < 3600]
if times and (now - max(times)) < cool:
    left = int(cool - (now - max(times)))
    print(f"cooldown {left}s remaining")
    sys.exit(1)
if len(times) >= max_h:
    print(f"max {max_h} restarts/hour reached ({len(times)} in last hour)")
    sys.exit(1)
sys.exit(0)
PY
}

record_restart() {
  python3 - "$RESTART_STATE" <<'PY'
import json, sys, time
path = sys.argv[1]
now = time.time()
try:
    d = json.loads(open(path).read())
except Exception:
    d = {"times": []}
times = [t for t in d.get("times", []) if now - t < 3600]
times.append(now)
open(path, "w").write(json.dumps({"times": times, "last": now}, indent=2) + "\n")
PY
}

restart_stack() {
  local reason
  if ! reason=$(restart_gate 2>&1); then
    log "RESTART deferred: ${reason:-limit}"
    return 1
  fi
  record_restart
  log "RESTART stack (shards=${STACK_SHARDS})"
  rm -f "$STATEDIR/smoke_seq.json" 2>/dev/null || true
  if bash "$DIR/start-stack.sh" --shards "${STACK_SHARDS}"; then
    log "RESTART start-stack OK"
    sleep 3
    return 0
  fi
  log "RESTART start-stack FAILED"
  return 1
}

run_loop() {
  local LOOP=0 HC=0
  log "watchdog start interval=${INTERVAL}s smoke_every=${SMOKE_EVERY} cooldown=${RESTART_COOLDOWN_S}s max_restarts/h=${RESTART_MAX_PER_HOUR} pid=$$"
  heartbeat

  while true; do
    LOOP=$((LOOP + 1))
    heartbeat
    rotate_logs || true

    HC=0
    if python3 "$DIR/health.py" >"$STATEDIR/last_health.json" 2>"$STATEDIR/last_health.err"; then
      HC=0
    else
      HC=$?
    fi

    if [[ "$HC" -eq 2 ]]; then
      log "health DOWN — attempting restart"
      restart_stack || true
      python3 "$DIR/health.py" >"$STATEDIR/last_health.json" 2>"$STATEDIR/last_health.err" || true
    elif [[ "$HC" -eq 1 ]]; then
      log "health DEGRADED — see $STATEDIR/last_health.json (no auto-restart)"
    else
      log "health OK"
    fi

    if [[ $((LOOP % SMOKE_EVERY)) -eq 0 ]]; then
      log "smoke…"
      if python3 "$DIR/smoke.py" >"$STATEDIR/last_smoke.out" 2>"$STATEDIR/last_smoke.err"; then
        log "smoke PASS"
      else
        log "smoke FAIL — see $STATEDIR/last_smoke.json"
      fi
    fi

    sleep "$INTERVAL" || true
  done
}

# --- daemon mode: background a single instance ---
if [[ "$DAEMON" -eq 1 ]]; then
  # If already locked/running, report and exit 0
  exec 9>"$LOCKFILE"
  if ! flock -n 9; then
    other=$(cat "$LOGDIR/watchdog.pid" 2>/dev/null || echo "?")
    echo "watchdog already running (pid=${other})"
    exit 0
  fi
  # Release so the child can hold the lock for life
  exec 9>&-
  nohup bash "$DIR/watchdog.sh" >>"$LOGDIR/watchdog.log" 2>&1 &
  for _ in $(seq 1 20); do
    if [[ -f "$LOGDIR/watchdog.pid" ]]; then
      wpid=$(cat "$LOGDIR/watchdog.pid" 2>/dev/null || true)
      if [[ -n "${wpid:-}" && -d "/proc/$wpid" ]]; then
        echo "watchdog daemon pid=$wpid"
        exit 0
      fi
    fi
    sleep 0.15
  done
  echo "watchdog daemon launched; check $LOGDIR/watchdog.log"
  exit 0
fi

# --- foreground / nohup child: hold lock for process lifetime ---
exec 9>"$LOCKFILE"
if ! flock -n 9; then
  other=$(cat "$LOGDIR/watchdog.pid" 2>/dev/null || echo "?")
  echo "watchdog already running (pid=${other}, lock=$LOCKFILE)" >&2
  exit 0
fi

run_loop
