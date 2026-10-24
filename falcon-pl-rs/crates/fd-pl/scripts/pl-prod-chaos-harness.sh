#!/usr/bin/env bash
# Falcon PL — local multi-node production-chaos harness (L2).
#
# Spins N validators (default 6), injects fault scenarios, asserts the tip
# advances and seats agree. Does NOT touch live testnet 2300.
#
# Usage:
#   bash crates/fd-pl/scripts/pl-prod-chaos-harness.sh
#   bash crates/fd-pl/scripts/pl-prod-chaos-harness.sh baseline silent-leader
#   PL_SCHEME=falcon-512 bash .../pl-prod-chaos-harness.sh baseline
#   PL_SKIP_BUILD=1 bash .../pl-prod-chaos-harness.sh restart-while-pending
#
# Env:
#   PL_CHAOS_ROOT   working dir (default /tmp/falcon-pl-prod-chaos)
#   PL_SCHEME       hmac (default) | falcon-512
#   PL_N            validator count (default 6; min 4)
#   PL_BASE_PORT    first listen port (default 19401)
#   PL_NETWORK_ID   default 2299 (private; never 2300)
#   PL_VOTE_ROUNDS_FROM_HEIGHT  default 1
#   PL_SKIP_BUILD   if 1, reuse existing release binaries
#
set -euo pipefail

RS="$(cd "$(dirname "$0")/../../.." && pwd)"
BIN="$RS/target/release"
ROOT="${PL_CHAOS_ROOT:-/tmp/falcon-pl-prod-chaos}"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
SCHEME="${PL_SCHEME:-hmac}"
N="${PL_N:-6}"
BASE_PORT="${PL_BASE_PORT:-19401}"
NID="${PL_NETWORK_ID:-2299}"
ROUNDS_FROM="${PL_VOTE_ROUNDS_FROM_HEIGHT:-1}"
SEAL_MS="${PL_SEAL_MS:-400}"
PROGRESS_MS="${PL_PROGRESS_TIMEOUT_MS:-800}"
BOND=1000

if [[ "$NID" == "2300" ]]; then
  echo "refuse: harness must not use network_id 2300" >&2
  exit 2
fi
if [[ "$N" -lt 4 ]]; then
  echo "refuse: need at least 4 validators" >&2
  exit 2
fi

SCENARIOS=("$@")
if [[ ${#SCENARIOS[@]} -eq 0 ]]; then
  SCENARIOS=(baseline silent-leader restart-while-pending dual-proposer-pressure coordinated-restart)
fi

mkdir -p "$ROOT" "$KEYS" "$LOG" "$SNAP"
cd "$RS"

vals_csv() {
  local i out=
  for ((i=1; i<=N; i++)); do
    [[ -n "$out" ]] && out+=","
    out+="v$i"
  done
  echo "$out"
}
VALS="$(vals_csv)"

port_of() { echo $((BASE_PORT + $1 - 1)); }

peers_for() {
  local self=$1 i p out=
  for ((i=1; i<=N; i++)); do
    [[ "$i" -eq "$self" ]] && continue
    p=$(port_of "$i")
    [[ -n "$out" ]] && out+=","
    out+="127.0.0.1:$p"
  done
  echo "$out"
}

build() {
  if [[ "${PL_SKIP_BUILD:-0}" == "1" && -x "$BIN/falcon-pl-node" && -x "$BIN/falcon-pl-ctl" ]]; then
    echo "[chaos] skip build (PL_SKIP_BUILD=1)"
    return 0
  fi
  echo "[chaos] cargo build -p fd-pl --release"
  cargo build -p fd-pl --release 2>&1 | tail -6
}

stop_all() {
  for f in "$LOG"/v*.pid; do
    [[ -f "$f" ]] || continue
    kill "$(cat "$f")" 2>/dev/null || true
    rm -f "$f"
  done
  local i p
  for ((i=1; i<=N; i++)); do
    p=$(port_of "$i")
    fuser -k "${p}/tcp" 2>/dev/null || true
  done
  sleep 0.4
}

gen_keys() {
  rm -rf "$KEYS"
  mkdir -p "$KEYS"
  "$BIN/falcon-pl-ctl" gen-keys --keys-dir "$KEYS" --scheme "$SCHEME" \
    --validators "$VALS"
  for a in faucet alice bob; do
    "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account "$a" --scheme "$SCHEME" \
      2>/dev/null || true
  done
}

start_one() {
  local i=$1
  local id="v$i"
  local port peers
  port=$(port_of "$i")
  peers=$(peers_for "$i")
  local allow=()
  if [[ "$SCHEME" == "hmac" ]]; then
    allow=(--allow-dev-crypto)
  fi
  local role=()
  if [[ "$i" -eq 1 ]]; then
    role=(--hub --archive)
  fi
  nohup "$BIN/falcon-pl-node" \
    --id "$id" \
    --listen "127.0.0.1:$port" \
    --peers "$peers" \
    --validators "$VALS" \
    --scheme "$SCHEME" \
    --keys-dir "$KEYS" \
    --network-id "$NID" \
    --bond "$BOND" \
    --seal-interval-ms "$SEAL_MS" \
    --progress-timeout-ms "$PROGRESS_MS" \
    --vote-rounds-from-height "$ROUNDS_FROM" \
    --liveness-alert-secs 60 \
    --snapshot "$SNAP/${id}.json" \
    --snapshot-out "$SNAP/${id}.json" \
    "${role[@]}" \
    "${allow[@]}" \
    >>"$LOG/${id}.log" 2>&1 &
  echo $! >"$LOG/${id}.pid"
}

start_all() {
  local i
  for ((i=1; i<=N; i++)); do
    start_one "$i"
  done
  sleep 2.5
}

status_json() {
  local port=$1
  "$BIN/falcon-pl-ctl" --addr "127.0.0.1:$port" status 2>/dev/null
}

tip_of() {
  local port=$1
  status_json "$port" | python3 -c '
import sys,json
raw=sys.stdin.read(); i=raw.find("{")
if i<0: print(0); raise SystemExit
b=json.JSONDecoder().raw_decode(raw[i:])[0]
body=b.get("body") or b
print(int(body.get("tip_height") or 0))
' 2>/dev/null || echo 0
}

hash_of() {
  local port=$1
  status_json "$port" | python3 -c '
import sys,json
raw=sys.stdin.read(); i=raw.find("{")
if i<0: print(""); raise SystemExit
b=json.JSONDecoder().raw_decode(raw[i:])[0]
body=b.get("body") or b
print(body.get("tip_hash") or "")
' 2>/dev/null || echo ""
}

mempool_of() {
  local port=$1
  status_json "$port" | python3 -c '
import sys,json
raw=sys.stdin.read(); i=raw.find("{")
if i<0: print(0); raise SystemExit
b=json.JSONDecoder().raw_decode(raw[i:])[0]
body=b.get("body") or b
print(int(body.get("mempool") or 0))
' 2>/dev/null || echo 0
}

# Byte offsets of every seat log, so a scenario only greps its own lines.
declare -A LOG_MARK=()
logs_mark() {
  local f
  LOG_MARK=()
  for f in "$LOG"/v*.log; do
    [[ -f "$f" ]] || continue
    LOG_MARK["$f"]=$(stat -c %s "$f")
  done
}
logs_grep_since() {
  local pat=$1 f off
  for f in "$LOG"/v*.log; do
    [[ -f "$f" ]] || continue
    off=${LOG_MARK["$f"]:-0}
    if tail -c +$((off + 1)) "$f" | grep -q -- "$pat"; then
      return 0
    fi
  done
  return 1
}

# "none" when no pending proposal is staged.
pending_height_of() {
  local port=$1
  status_json "$port" | python3 -c '
import sys,json
raw=sys.stdin.read(); i=raw.find("{")
if i<0: print("none"); raise SystemExit
b=json.JSONDecoder().raw_decode(raw[i:])[0]
body=b.get("body") or b
p=body.get("pending_proposal_height")
print("none" if p is None else int(p))
' 2>/dev/null || echo none
}

view_round_of() {
  local port=$1
  status_json "$port" | python3 -c '
import sys,json
raw=sys.stdin.read(); i=raw.find("{")
if i<0: print(0); raise SystemExit
b=json.JSONDecoder().raw_decode(raw[i:])[0]
body=b.get("body") or b
print(int(body.get("height_view_round") or 0))
' 2>/dev/null || echo 0
}


lottery_winner() {
  local port=$1
  status_json "$port" | python3 -c '
import sys,json
raw=sys.stdin.read(); i=raw.find("{")
if i<0: print("v1"); raise SystemExit
b=json.JSONDecoder().raw_decode(raw[i:])[0]
body=b.get("body") or b
lo=body.get("lottery_order") or body.get("qc_committee") or []
print(lo[0] if lo else "v1")
' 2>/dev/null || echo "v1"
}

pay_burst() {
  local port=$1 count=${2:-12}
  local i
  for ((i=1; i<=count; i++)); do
    "$BIN/falcon-pl-ctl" --addr "127.0.0.1:$port" pay \
      --from alice --to bob --amount 1 --fee 2 --sequence 0 \
      --scheme "$SCHEME" --keys-dir "$KEYS" --network-id "$NID" >/dev/null 2>&1 || true
  done
}

wait_tip_ge() {
  local port=$1 want=$2 timeout_s=${3:-45}
  local t0 end tip
  t0=$(date +%s)
  end=$((t0 + timeout_s))
  while true; do
    tip=$(tip_of "$port")
    if [[ "$tip" -ge "$want" ]]; then
      echo "$tip"
      return 0
    fi
    if [[ "$(date +%s)" -ge "$end" ]]; then
      echo "$tip"
      return 1
    fi
    sleep 0.5
  done
}

assert_tips_agree() {
  local timeout_s=${1:-20}
  local t0 end i p tip0 hash0 tip hash
  t0=$(date +%s)
  end=$((t0 + timeout_s))
  while true; do
    tip0=$(tip_of "$(port_of 1)")
    hash0=$(hash_of "$(port_of 1)")
    local ok=1
    for ((i=2; i<=N; i++)); do
      p=$(port_of "$i")
      tip=$(tip_of "$p")
      hash=$(hash_of "$p")
      if [[ "$tip" != "$tip0" || "$hash" != "$hash0" ]]; then
        ok=0
        if [[ "$(date +%s)" -ge "$end" ]]; then
          echo "[chaos] FAIL tip disagree v1=$tip0/${hash0:0:12} v$i=$tip/${hash:0:12}" >&2
          return 1
        fi
        break
      fi
    done
    if [[ "$ok" -eq 1 ]]; then
      echo "[chaos] tips agree height=$tip0"
      return 0
    fi
    sleep 0.5
  done
}

fresh_mesh() {
  stop_all
  rm -rf "$SNAP"
  mkdir -p "$SNAP" "$LOG"
  : >"$LOG/harness.log"
  gen_keys
  start_all
}

scenario_baseline() {
  echo "[chaos] === baseline ==="
  fresh_mesh
  local p1 tip
  p1=$(port_of 1)
  pay_burst "$p1" 20
  tip=$(wait_tip_ge "$p1" 1 40) || {
    echo "[chaos] FAIL baseline tip=$tip mempool=$(mempool_of "$p1")" >&2
    status_json "$p1" | python3 -c '
import sys,json
raw=sys.stdin.read(); i=raw.find("{")
b=json.JSONDecoder().raw_decode(raw[i:])[0]; body=b.get("body") or b
for k in ("tip_height","mempool","has_applicable_tx","lottery_order","pending_proposal_height","height_view_round","round_scoped_votes","consensus_stall","liveness"):
  print(k, ":", body.get(k))
' 2>/dev/null || true
    return 1
  }
  sleep 1
  assert_tips_agree 15 || return 1
  echo "[chaos] PASS baseline tip=$tip"
}

scenario_silent_leader() {
  echo "[chaos] === silent-leader ==="
  fresh_mesh
  local p1 winner wid wport tip0 tip1
  p1=$(port_of 1)
  pay_burst "$p1" 8
  tip0=$(wait_tip_ge "$p1" 1 40) || {
    echo "[chaos] FAIL silent-leader could not seed tip" >&2
    return 1
  }
  winner=$(lottery_winner "$p1")
  wid=${winner#v}
  wport=$(port_of "$wid")
  echo "[chaos] killing lottery winner $winner (port $wport) at tip=$tip0"
  if [[ -f "$LOG/${winner}.pid" ]]; then
    kill "$(cat "$LOG/${winner}.pid")" 2>/dev/null || true
    rm -f "$LOG/${winner}.pid"
  fi
  fuser -k "${wport}/tcp" 2>/dev/null || true
  sleep 0.5
  local alt=1
  [[ "$alt" -eq "$wid" ]] && alt=2
  pay_burst "$(port_of "$alt")" 24
  tip1=$(wait_tip_ge "$(port_of "$alt")" $((tip0 + 1)) 45) || {
    echo "[chaos] FAIL silent-leader tip stuck at $(tip_of "$(port_of "$alt")") (was $tip0)" >&2
    return 1
  }
  echo "[chaos] PASS silent-leader tip $tip0 → $tip1 with $winner down"
  start_one "$wid"
  sleep 2
}

lottery_next() {
  local port=$1
  status_json "$port" | python3 -c '
import sys,json
raw=sys.stdin.read(); i=raw.find("{")
if i<0: print("v1"); raise SystemExit
b=json.JSONDecoder().raw_decode(raw[i:])[0]
body=b.get("body") or b
w=body.get("lottery_winner_next")
lo=body.get("lottery_order") or []
print(w or (lo[0] if lo else "v1"))
' 2>/dev/null || echo "v1"
}

scenario_restart_while_pending() {
  echo "[chaos] === restart-while-pending ==="
  fresh_mesh
  local p1 tip0 tip1 pend i
  p1=$(port_of 1)
  pay_burst "$p1" 10
  tip0=$(wait_tip_ge "$p1" 1 40) || {
    echo "[chaos] FAIL restart seed" >&2
    return 1
  }
  sleep 1
  # Hold a body pending deterministically: freeze (SIGSTOP) three seats that
  # are neither the hub, the next proposer nor the victim, so the proposal
  # can collect at most 3 of the 4 OKs it needs. Then restart the victim while
  # pending_proposal_height is set, thaw the frozen seats, and require the
  # mesh to commit and the victim to catch up. (A fixed sleep could land
  # after commit or before stage, so the scenario might never test this.)
  local w wid victim=""
  w=$(lottery_next "$p1")
  wid=${w#v}
  for ((i=N; i>=2; i--)); do
    if [[ "$i" != "$wid" ]]; then victim=$i; break; fi
  done
  local frozen=()
  for ((i=2; i<=N; i++)); do
    [[ "$i" == "$wid" || "$i" == "$victim" ]] && continue
    [[ ${#frozen[@]} -ge $((N - 3)) ]] && break
    frozen+=("$i")
  done
  echo "[chaos] next proposer=$w victim=v${victim} freezing: ${frozen[*]/#/v}"
  for i in "${frozen[@]}"; do
    kill -STOP "$(cat "$LOG/v${i}.pid")" 2>/dev/null || true
  done
  thaw() {
    for i in "${frozen[@]}"; do
      kill -CONT "$(cat "$LOG/v${i}.pid" 2>/dev/null)" 2>/dev/null || true
    done
  }
  local t_end=$(( $(date +%s) + 25 ))
  pend=none
  while [[ "$(date +%s)" -lt "$t_end" ]]; do
    pay_burst "$p1" 6
    pend=$(pending_height_of "$p1")
    [[ "$pend" == "none" ]] && pend=$(pending_height_of "$(port_of "$victim")")
    [[ "$pend" != "none" ]] && break
    sleep 0.3
  done
  if [[ "$pend" == "none" ]]; then
    thaw
    echo "[chaos] FAIL restart-while-pending: no pending_proposal_height with ${#frozen[@]} seats frozen (tip=$(tip_of "$p1") mp=$(mempool_of "$p1"))" >&2
    return 1
  fi
  tip0=$(tip_of "$p1")
  if [[ "$pend" -le "$tip0" ]]; then
    thaw
    echo "[chaos] FAIL restart-while-pending: pending=$pend not above tip=$tip0" >&2
    return 1
  fi
  echo "[chaos] restarting v${victim} with pending_proposal_height=$pend tip=$tip0 mp=$(mempool_of "$p1")"
  if [[ -f "$LOG/v${victim}.pid" ]]; then
    kill "$(cat "$LOG/v${victim}.pid")" 2>/dev/null || true
    rm -f "$LOG/v${victim}.pid"
  fi
  fuser -k "$(port_of "$victim")/tcp" 2>/dev/null || true
  sleep 0.5
  start_one "$victim"
  sleep 1
  thaw
  pay_burst "$p1" 20
  tip1=$(wait_tip_ge "$p1" $((tip0 + 1)) 50) || {
    echo "[chaos] FAIL restart-while-pending tip stuck at $(tip_of "$p1") (pending was $pend)" >&2
    return 1
  }
  local vt
  vt=$(wait_tip_ge "$(port_of "$victim")" "$tip1" 40) || {
    echo "[chaos] FAIL v${victim} lag tip=$vt want=$tip1" >&2
    return 1
  }
  assert_tips_agree 30 || return 1
  echo "[chaos] PASS restart-while-pending tip $tip0 → $tip1 (restarted v${victim} with pending=$pend)"
}

scenario_dual_proposer_pressure() {
  echo "[chaos] === dual-proposer-pressure ==="
  # Aggressive timers invite competing proposals / view-changes under Phase 2.
  local saved_seal=$SEAL_MS saved_prog=$PROGRESS_MS
  SEAL_MS=200
  PROGRESS_MS=300
  stop_all
  rm -rf "$SNAP"
  mkdir -p "$SNAP" "$LOG"
  gen_keys
  start_all
  local p1 tip0 tip1 i
  p1=$(port_of 1)
  tip0=$(tip_of "$p1")
  local saw_vc=0 max_vr=0 saw_stall=0
  logs_mark
  for i in $(seq 1 10); do
    pay_burst "$p1" 25
    sleep 0.2
    local vr
    vr=$(view_round_of "$p1")
    if [[ "$vr" -gt "$max_vr" ]]; then max_vr=$vr; fi
    if [[ $saw_vc -eq 0 ]] && logs_grep_since "view-change"; then
      saw_vc=1
    fi
    # Not "equivocation queued": under hmac (no vote sigs) a lagging seat
    # mis-attributes future-height OKs and logs bogus equivocation; that is
    # noise, not evidence of dual proposals.
    if [[ $saw_stall -eq 0 ]] && logs_grep_since "DualProposal\|SplitVote"; then
      saw_stall=1
    fi
    # Mid-run: briefly silence the current proposer (lottery winner, never the
    # v1 hub) so the round times out while its body may still be in flight,
    # then bring it back — two candidate bodies for one height.
    if [[ $i -eq 4 ]]; then
      local w victim
      w=$(lottery_next "$p1")
      victim=${w#v}
      [[ "$victim" == "1" || -z "$victim" ]] && victim=2
      echo "[chaos] dual: briefly killing proposer v${victim} to force round advance"
      if [[ -f "$LOG/v${victim}.pid" ]]; then
        kill "$(cat "$LOG/v${victim}.pid")" 2>/dev/null || true
        rm -f "$LOG/v${victim}.pid"
      fi
      fuser -k "$(port_of "$victim")/tcp" 2>/dev/null || true
      # Round clocks are work-gated: keep load on while the proposer is gone.
      local k
      for k in 1 2 3 4 5 6; do
        pay_burst "$p1" 6
        sleep 0.4
        vr=$(view_round_of "$p1")
        if [[ "$vr" -gt "$max_vr" ]]; then max_vr=$vr; fi
      done
      start_one "$victim"
    fi
  done
  SEAL_MS=$saved_seal
  PROGRESS_MS=$saved_prog
  tip1=$(wait_tip_ge "$p1" $((tip0 + 3)) 60) || {
    echo "[chaos] FAIL dual-proposer-pressure tip=$(tip_of "$p1") (started $tip0)" >&2
    status_json "$p1" | python3 -c '
import sys,json
raw=sys.stdin.read(); i=raw.find("{")
b=json.JSONDecoder().raw_decode(raw[i:])[0]
body=b.get("body") or b
for k in ("tip_height","mempool","consensus_stall","liveness","height_view_round","round_scoped_votes"):
  print(k, ":", body.get(k))
' 2>/dev/null || true
    return 1
  }
  if [[ $saw_vc -eq 0 && $max_vr -eq 0 && $saw_stall -eq 0 ]]; then
    echo "[chaos] FAIL dual-proposer-pressure: tip advanced but no view-change / elevated round / dual-stall observed (pressure did not fire)" >&2
    return 1
  fi
  assert_tips_agree 20 || return 1
  echo "[chaos] PASS dual-proposer-pressure tip $tip0 → $tip1 (view_change=$saw_vc max_view_round=$max_vr stall=$saw_stall)"
}

scenario_coordinated_restart() {
  echo "[chaos] === coordinated-restart ==="
  fresh_mesh
  local p1 tip0 tip1
  p1=$(port_of 1)
  pay_burst "$p1" 16
  tip0=$(wait_tip_ge "$p1" 1 50) || {
    echo "[chaos] FAIL coordinated seed tip=$(tip_of "$p1")" >&2
    return 1
  }
  echo "[chaos] stop-all at tip=$tip0 (simulates clearing stale votes)"
  stop_all
  sleep 1
  start_all
  # Snapshots restore tip; wait for ONLINE re-cert + mesh before load.
  sleep 4
  local i
  for i in $(seq 1 6); do
    pay_burst "$p1" 12
    tip1=$(tip_of "$p1")
    if [[ "$tip1" -gt "$tip0" ]]; then
      break
    fi
    sleep 2
  done
  tip1=$(wait_tip_ge "$p1" $((tip0 + 1)) 45) || {
    echo "[chaos] FAIL coordinated-restart tip stuck at $(tip_of "$p1") (was $tip0)" >&2
    return 1
  }
  if [[ "$tip1" -lt "$tip0" ]]; then
    echo "[chaos] FAIL height regressed $tip0 → $tip1" >&2
    return 1
  fi
  assert_tips_agree 30 || return 1
  echo "[chaos] PASS coordinated-restart tip $tip0 → $tip1"
}

echo "[chaos] root=$ROOT scheme=$SCHEME n=$N nid=$NID rounds_from=$ROUNDS_FROM"
echo "[chaos] scenarios: ${SCENARIOS[*]}"
build

FAILED=()
PASSED=()
for s in "${SCENARIOS[@]}"; do
  case "$s" in
    baseline) scenario_baseline && PASSED+=("$s") || FAILED+=("$s") ;;
    silent-leader) scenario_silent_leader && PASSED+=("$s") || FAILED+=("$s") ;;
    restart-while-pending) scenario_restart_while_pending && PASSED+=("$s") || FAILED+=("$s") ;;
    dual-proposer-pressure) scenario_dual_proposer_pressure && PASSED+=("$s") || FAILED+=("$s") ;;
    coordinated-restart) scenario_coordinated_restart && PASSED+=("$s") || FAILED+=("$s") ;;
    *) echo "[chaos] unknown scenario: $s" >&2; FAILED+=("$s") ;;
  esac
done

stop_all
echo
echo "[chaos] passed: ${PASSED[*]:-none}"
echo "[chaos] failed: ${FAILED[*]:-none}"
if [[ ${#FAILED[@]} -gt 0 ]]; then
  echo "[chaos] logs under $LOG"
  exit 1
fi
echo "[chaos] ALL PASS"
exit 0
