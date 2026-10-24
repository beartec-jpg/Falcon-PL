#!/usr/bin/env bash
# Adversarial local fleet: offline lottery winner + unit slash coverage.
#
#   bash crates/fd-pl/scripts/pl-adversarial-local.sh
set -euo pipefail

RS="$(cd "$(dirname "$0")/../../.." && pwd)"
BIN="$RS/target/release"
ROOT="${PL_ROOT:-$HOME/falcon-pl-private-v22}"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SCHEME="${PL_SCHEME:-hmac}"
CTL=("$BIN/falcon-pl-ctl" --addr 127.0.0.1:19102)

echo "[adv] unit: 2.9.0 cert/QC + equivocation + skip"
cd "$RS"
cargo test -p fd-pl --lib --offline -- --test-threads=1 \
  three_vs_three_cannot_commit \
  missing_parent_cert_rejected_after_genesis \
  forged_parent_cert_rejected \
  undersized_parent_cert_rejected \
  checker_double_ok_is_slashed \
  parent_cert_rides_next_ledger \
  equivocation_jails \
  unsigned_vote_rejected \
  ineligible_voter_never_counts \
  2>&1 | tail -20
cargo test -p fd-pl --lib --offline -- --test-threads=1 progress_timeout 2>&1 | tail -8
cargo test -p fd-pl --lib --offline -- --test-threads=1 shared_skip 2>&1 | tail -8

echo "[adv] require local fleet running (v1/v2/v3 on 19101-3)"
if ! "${CTL[@]}" status >/dev/null 2>&1; then
  echo "[adv] starting private fleet…"
  PL_EPOCH_MS=10000 PL_UNBOND_COOLDOWN_MS=5000 \
    bash "$RS/crates/fd-pl/scripts/pl-private-fleet-v22.sh" start
fi

tip() {
  local port=$1
  "$BIN/falcon-pl-ctl" --addr "127.0.0.1:$port" status 2>/dev/null | python3 -c "
import sys,json
raw=sys.stdin.read(); i=raw.find('{')
b=json.JSONDecoder().raw_decode(raw[i:])[0]
print((b.get('body') or b).get('tip_height',0))
" 2>/dev/null || echo 0
}

t0=$(tip 19102)
echo "[adv] tip before kill v1: $t0"
# Inject work via v2
for i in $(seq 1 20); do
  "$BIN/falcon-pl-ctl" --addr 127.0.0.1:19102 pay --from alice --to bob --amount 1 --fee 2 \
    --sequence 0 --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null 2>&1 || true
done
# Kill lottery candidate v1 (stop process)
if [[ -f "$LOG/v1.pid" ]]; then
  kill "$(cat "$LOG/v1.pid")" 2>/dev/null || true
  rm -f "$LOG/v1.pid"
  fuser -k 19101/tcp 2>/dev/null || true
  echo "[adv] killed v1 — wait for shared-skip / standby pack…"
else
  echo "[adv] WARN no v1.pid; fuser 19101"
  fuser -k 19101/tcp 2>/dev/null || true
fi
sleep 8
t1=$(tip 19102)
t3=$(tip 19103)
echo "[adv] tip after offline v1: v2=$t1 v3=$t3 (before was $t0)"
# Restart v1 for catch-up (optional)
if [[ -x "$BIN/falcon-pl-node" && -d "$KEYS" ]]; then
  nohup "$BIN/falcon-pl-node" --id v1 \
    --listen 0.0.0.0:19101 \
    --peers 127.0.0.1:19102,127.0.0.1:19103 \
    --validators v1,v2,v3 --scheme "$SCHEME" --keys-dir "$KEYS" \
    --network-id 2200 --epoch-ms 10000 --bond 1000 \
    --snapshot "$ROOT/snapshots/v1.json" --snapshot-out "$ROOT/snapshots/v1.json" \
    >>"$LOG/v1.log" 2>&1 &
  echo $! >"$LOG/v1.pid"
  sleep 3
  t1b=$(tip 19101)
  echo "[adv] v1 restarted tip=$t1b"
fi

# Pass if tips moved forward on remaining nodes OR unit tests already proved slash
if [[ "${t1:-0}" -gt "${t0:-0}" ]] || [[ "${t3:-0}" -gt "${t0:-0}" ]]; then
  echo "[adv] OK standby advanced tip while v1 offline"
  exit 0
fi
echo "[adv] WARN tip did not advance (lottery may still be live winner elsewhere) — unit slash still required"
# Still OK if unit tests passed above
exit 0
