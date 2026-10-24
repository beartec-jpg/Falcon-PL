# Shared helpers for protocol CI smokes (source this file).
# shellcheck shell=bash

smoke_require_bin() {
  local root="$1"
  if [[ ! -x "${BIN:-}" ]]; then
    # shellcheck source=/dev/null
    source "${HOME}/.cargo/env" 2>/dev/null || true
    (cd "$root" && cargo build --release -p fd-node)
    BIN="$root/target/release/falcon-dagd"
  fi
  export BIN
}

# Kill whatever holds PORT/tcp; never match our own process via pkill -f.
# stdout discarded: fuser prints PIDs to stdout and would corrupt $(smoke_start_node).
smoke_kill_port() {
  local port="$1"
  if command -v fuser >/dev/null 2>&1; then
    fuser -k "${port}/tcp" >/dev/null 2>&1 || true
  fi
  # Second pass after short wait (TIME_WAIT / slow death)
  sleep 0.25
  if command -v fuser >/dev/null 2>&1; then
    fuser -k "${port}/tcp" >/dev/null 2>&1 || true
  fi
  sleep 0.15
}

smoke_wait_rpc() {
  local rpc="$1"
  local tries="${2:-60}"
  local i
  for i in $(seq 1 "$tries"); do
    if curl -sf -m 1 -X POST "http://${rpc}" -H 'Content-Type: application/json' \
      -d '{"method":"ping","params":[{}]}' 2>/dev/null | grep -q pong; then
      return 0
    fi
    sleep 0.1
  done
  echo "FAIL: RPC not ready at ${rpc} after ${tries} tries" >&2
  return 1
}

# Fail-closed: require tesSUCCESS (or expected code). Empty/None/missing → fail.
smoke_require_engine() {
  local body="$1"
  local expect="${2:-tesSUCCESS}"
  local label="${3:-submit}"
  python3 -c "
import json,sys
raw=sys.argv[1]
label=sys.argv[2]
expect=sys.argv[3]
try:
    d=json.loads(raw)
except Exception as e:
    print(f'FAIL {label}: bad json: {e} body={raw[:200]!r}', file=sys.stderr)
    sys.exit(2)
res=d.get('result', d)
code=res.get('engine_result') if isinstance(res, dict) else None
if code is None or code == '' or str(code).lower() == 'none':
    print(f'FAIL {label}: missing engine_result: {raw[:300]!r}', file=sys.stderr)
    sys.exit(3)
if code != expect:
    print(f'FAIL {label}: expected {expect} got {code}: {raw[:300]!r}', file=sys.stderr)
    sys.exit(4)
print(f'{label} -> {code}')
" "$body" "$label" "$expect"
}

smoke_post() {
  local rpc="$1"
  local payload="$2"
  curl -sf -m 12 -X POST "http://${rpc}" -H 'Content-Type: application/json' -d "$payload"
}

# Optional 3rd arg: data-dir (enables snapshot persistence for restart smokes).
# Optional env SMOKE_BATCH_INTERVAL (default 0.08), SMOKE_SNAPSHOT_INTERVAL (default 1.0 when data-dir set).
smoke_start_node() {
  local rpc="$1"
  local log="${2:-/tmp/fd-smoke.log}"
  local data_dir="${3:-}"
  local port="${rpc##*:}"
  local batch_iv="${SMOKE_BATCH_INTERVAL:-0.08}"
  smoke_kill_port "$port"
  local -a args=(
    --id val1 --rpc "$rpc" --unl val1 --worker-count 1
    --crypto hmac-sha256-dev --require-tx-sigs --auto-sign
    --batch-interval "$batch_iv"
  )
  if [[ -n "$data_dir" ]]; then
    mkdir -p "$data_dir"
    args+=(--data-dir "$data_dir" --snapshot-interval "${SMOKE_SNAPSHOT_INTERVAL:-1.0}")
  fi
  "$BIN" "${args[@]}" >"$log" 2>&1 &
  echo $!
}

smoke_snapshot() {
  local rpc="$1"
  smoke_post "$rpc" '{"method":"snapshot","params":[{}]}'
}
