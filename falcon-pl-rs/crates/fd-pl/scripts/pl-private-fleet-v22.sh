#!/usr/bin/env bash
# Falcon PL v2.2.0 — private full-function fleet (local 3-node or dual-host).
#
# Surfaces: lottery pack, vault, AMM, lend, emissions, watchers, BTC SPV rails, snapshots.
#
#   bash crates/fd-pl/scripts/pl-private-fleet-v22.sh start
#   bash crates/fd-pl/scripts/pl-private-fleet-v22.sh status
#   bash crates/fd-pl/scripts/pl-private-fleet-v22.sh smoke
#   bash crates/fd-pl/scripts/pl-private-fleet-v22.sh stop
set -euo pipefail

RS="$(cd "$(dirname "$0")/../../.." && pwd)"
BIN="$RS/target/release"
ROOT="${PL_ROOT:-$HOME/falcon-pl-private-v22}"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
SCHEME="${PL_SCHEME:-hmac}"
LOAD_WALLETS="${PL_LOAD_WALLETS:-32}"
# Local-only mesh by default; set PL_MODE=dual + F1/F2 for multi-host.
MODE="${PL_MODE:-local}"
SSH_F2="${SSH_F2:-100.64.0.3}"
F1_IP="${F1_IP:-127.0.0.1}"
F2_IP="${F2_IP:-127.0.0.1}"
SEAL_MS="${PL_SEAL_MS:-400}"
PACK_BUDGET_MS="${PL_PACK_BUDGET_MS:-1000}"
PROGRESS_TIMEOUT_MS="${PL_PROGRESS_TIMEOUT_MS:-1000}"
MAX_MEMPOOL_TXS="${PL_MAX_MEMPOOL_TXS:-50000}"
MAX_TXS="${PL_MAX_TXS:-0}"
SNAPSHOT_INTERVAL_MS="${PL_SNAPSHOT_INTERVAL_MS:-30000}"
# 0 = default 7d; private payday soak uses e.g. PL_EPOCH_MS=1000
EPOCH_MS="${PL_EPOCH_MS:-0}"
UNBOND_CD_MS="${PL_UNBOND_COOLDOWN_MS:-0}"
NETWORK_ID="${PL_NETWORK_ID:-2200}"
BOND=1000
VALS="v1,v2,v3"
CTL=("$BIN/falcon-pl-ctl")

cmd="${1:-start}"

mkdir -p "$ROOT" "$LOG" "$KEYS" "$SNAP"

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
}

stop_local() {
  for f in "$LOG"/v1.pid "$LOG"/v2.pid "$LOG"/v3.pid; do
    if [[ -f "$f" ]]; then
      kill "$(cat "$f")" 2>/dev/null || true
      rm -f "$f"
    fi
  done
  for p in 19101 19102 19103; do fuser -k "${p}/tcp" 2>/dev/null || true; done
}

start_local() {
  local common=(
    --validators "$VALS"
    --scheme "$SCHEME"
    --keys-dir "$KEYS"
    --load-wallets "$LOAD_WALLETS"
    --seal-interval-ms "$SEAL_MS"
    --max-txs "$MAX_TXS"
    --pack-budget-ms "$PACK_BUDGET_MS"
    --progress-timeout-ms "$PROGRESS_TIMEOUT_MS"
    --max-mempool-txs "$MAX_MEMPOOL_TXS"
    --bond "$BOND"
    --snapshot-interval-ms "$SNAPSHOT_INTERVAL_MS"
  )
  if [[ "${EPOCH_MS}" != "0" && -n "${EPOCH_MS}" ]]; then
    common+=(--epoch-ms "$EPOCH_MS")
  fi
  if [[ "${UNBOND_CD_MS}" != "0" && -n "${UNBOND_CD_MS}" ]]; then
    common+=(--unbond-cooldown-ms "$UNBOND_CD_MS")
  fi
  common+=(--network-id "$NETWORK_ID")
  nohup "$BIN/falcon-pl-node" --id v1 \
    --listen "0.0.0.0:19101" \
    --peers "127.0.0.1:19102,127.0.0.1:19103" \
    --snapshot "$SNAP/v1.json" \
    --snapshot-out "$SNAP/v1.json" \
    "${common[@]}" >>"$LOG/v1.log" 2>&1 &
  echo $! >"$LOG/v1.pid"
  nohup "$BIN/falcon-pl-node" --id v2 \
    --listen "0.0.0.0:19102" \
    --peers "127.0.0.1:19101,127.0.0.1:19103" \
    --snapshot "$SNAP/v2.json" \
    --snapshot-out "$SNAP/v2.json" \
    "${common[@]}" >>"$LOG/v2.log" 2>&1 &
  echo $! >"$LOG/v2.pid"
  nohup "$BIN/falcon-pl-node" --id v3 \
    --listen "0.0.0.0:19103" \
    --peers "127.0.0.1:19101,127.0.0.1:19102" \
    --snapshot "$SNAP/v3.json" \
    --snapshot-out "$SNAP/v3.json" \
    "${common[@]}" >>"$LOG/v3.log" 2>&1 &
  echo $! >"$LOG/v3.pid"
  echo "local fleet v1/v2/v3 pids $(cat "$LOG/v1.pid") $(cat "$LOG/v2.pid") $(cat "$LOG/v3.pid") epoch_ms=${EPOCH_MS:-0}"
  sleep 1
}

status() {
  for p in 19101 19102 19103; do
    echo "=== :$p ==="
    "${CTL[@]}" --addr "127.0.0.1:$p" status 2>/dev/null | head -c 1200 || echo "(down)"
    echo
  done
}

# End-to-end private surface: pay, vault, pool, rail header+deposit, claim path tick, snapshot.
smoke() {
  local A=127.0.0.1:19101
  local TS SEQ
  TS=$(date +%s)
  # sequence from status if available (snapshot may have advanced seqs)
  SEQ=$("${CTL[@]}" --addr "$A" status 2>/dev/null | python3 -c "
import sys,json
b=json.load(sys.stdin)
body=b.get('body',b)
# no account seq in status yet — use time-based high seq only for uniqueness of attempts
print(1)
" 2>/dev/null || echo 1)
  echo "[smoke] account alice (seq)"
  "${CTL[@]}" --addr "$A" account --account alice || true
  echo "[smoke] pay alice→bob (auto-seq, fee escalate-aware)"
  "${CTL[@]}" --addr "$A" pay --from alice --to bob --amount 10 --fee 2 --sequence 0 \
    --scheme "$SCHEME" --keys-dir "$KEYS" || true
  sleep 1
  echo "[smoke] faucet drip → bob"
  "${CTL[@]}" --addr "$A" faucet --to bob --amount 100 --fee 2 --sequence 0 \
    --scheme "$SCHEME" --keys-dir "$KEYS" || true
  sleep 1
  echo "[smoke] vault open alice→bob (ledger tx)"
  "${CTL[@]}" --addr "$A" vault-open --account alice --destination bob \
    --scheme "$SCHEME" --keys-dir "$KEYS" || true
  for _ in 1 2 3; do "${CTL[@]}" --addr "$A" seal >/dev/null 2>&1 || true; done
  echo "[smoke] create pool FPL/USD (ledger tx)"
  "${CTL[@]}" --addr "$A" create-pool --pool-id "p-${TS}" --asset-a FPL --asset-b USD \
    --account alice --scheme "$SCHEME" --keys-dir "$KEYS" || true
  for _ in 1 2 3; do "${CTL[@]}" --addr "$A" seal >/dev/null 2>&1 || true; done
  echo "[smoke] BTC rail headers (+6 from tip) + unique deposit"
  local LOCK TIP_H TIP_HASH
  read -r LOCK TIP_H TIP_HASH < <("$BIN/falcon-pl-ctl" --addr "$A" status 2>/dev/null | python3 -c "
import sys,json
b=json.load(sys.stdin)
body=b.get('body',b)
for r in body.get('rails',[]):
  if r.get('asset')=='BTC':
    print(r.get('lock_id',''), r.get('tip_height',0), r.get('tip_hash',''))
    break
else:
    print('', 0, '')
" 2>/dev/null || echo " 0 ")
  if [[ -z "${LOCK// }" ]]; then
    LOCK=$(python3 -c "import hashlib; print(hashlib.sha256(b'fpl-spv-lock-v1:BTC').hexdigest())")
  fi
  local AMT=1000
  TS=$(date +%s)
  local TXID="smoke-btc-${TS}"
  local H=$((TIP_H + 1))
  local BH="smoke-bh-${TS}-1"
  local LEAF
  LEAF=$(python3 -c "import hashlib; print(hashlib.sha256(f'${TXID}|${AMT}|${LOCK}'.encode()).hexdigest())")
  local parent="${TIP_HASH}"
  local i hash root height
  for i in 1 2 3 4 5 6; do
    height=$((TIP_H + i))
    if [[ "$i" -eq 1 ]]; then
      hash="$BH"
      root="$LEAF"
    else
      hash="smoke-bh-${TS}-$i"
      root="pad-root-${TS}-$i"
    fi
    "${CTL[@]}" --addr "$A" rail-header \
      --asset BTC --height "$height" --hash "$hash" --parent-hash "$parent" \
      --merkle-root "$root" --watcher alice
    parent="$hash"
  done
  echo "[smoke] BTC rail deposit → bob (h=$H tip=$((TIP_H+6)))"
  "${CTL[@]}" --addr "$A" rail-deposit \
    --asset BTC --to bob --amount "$AMT" --height "$H" \
    --external-txid "$TXID" --block-hash "$BH" --lock-id "$LOCK" \
    --watcher alice --test-fixture
  echo "[smoke] BTC rail withdraw 100 from bob"
  "${CTL[@]}" --addr "$A" rail-withdraw \
    --asset BTC --from bob --amount 100 --external-to "bc1qsmoke-withdraw" || true
  echo "[smoke] watcher credit + tick"
  "${CTL[@]}" --addr "$A" watcher-credit --account alice --work 5
  "${CTL[@]}" --addr "$A" tick --dt-ms 2000
  echo "[smoke] snapshot-save"
  "${CTL[@]}" --addr "$A" snapshot-save --path "$SNAP/smoke-v1.json"
  echo "[smoke] status accounts + fee metrics"
  "${CTL[@]}" --addr "$A" status | python3 -c "
import sys,json
b=json.load(sys.stdin)
body=b.get('body',b)
m=body.get('metrics',{})
fm=m.get('fee_market',{})
accts=body.get('accounts') or {}
print('tip', body.get('tip_height'),
      'params_version', body.get('params_version'),
      'suggested_min_fee', body.get('suggested_min_fee'),
      'tier', fm.get('congestion_tier'),
      'alice_seq', (accts.get('alice') or {}).get('sequence'),
      'faucet_bal', (body.get('faucet') or {}).get('balance'),
      'submit_ok', m.get('submit_accepted'),
      'fees_burned', m.get('fees_burned_total'))
print('btc', [r for r in body.get('rails',[]) if r.get('asset')=='BTC'])
"
  echo "smoke done"
}

payday() {
  python3 "$RS/crates/fd-pl/scripts/pl-epoch-payday.py" \
    --addr 127.0.0.1:19101 \
    --keys-dir "$KEYS" \
    --scheme "$SCHEME" \
    --ctl "$BIN/falcon-pl-ctl"
}

case "$cmd" in
  build) build ;;
  keys) gen_keys ;;
  start)
    build
    if [[ ! -f "$KEYS/pl_keys.json" ]]; then gen_keys; fi
    # ensure faucet key if keys predate faucet account
    "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account faucet --scheme "$SCHEME" >/dev/null 2>&1 || true
    stop_local
    start_local
    status
    ;;
  stop) stop_local; echo stopped ;;
  status) status ;;
  smoke) smoke ;;
  payday) payday ;;
  wallet)
    shift || true
    bash "$RS/crates/fd-pl/scripts/pl-wallet-stage.sh" "$@"
    ;;
  restart)
    stop_local
    start_local
    status
    ;;
  *)
    echo "usage: $0 {build|keys|start|stop|status|smoke|payday|wallet|restart}"
    exit 1
    ;;
esac
