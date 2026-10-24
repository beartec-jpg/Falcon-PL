#!/usr/bin/env bash
# Multi-process smoke: 3 nodes, shared keys_dir, auto-seal non-empty ledgers.
set -euo pipefail
RS="$(cd "$(dirname "$0")/../../.." && pwd)"
BIN="$RS/target/release"
KEYS="${PL_KEYS:-/tmp/falcon-pl-keys}"
SCHEME="${PL_SCHEME:-hmac}"
LOG=/tmp/falcon-pl-multi

mkdir -p "$LOG" "$KEYS"
cd "$RS"
cargo build -p fd-pl --release 2>&1 | tail -5

# stop prior
for p in $(pgrep -f 'target/release/falcon-pl-node' 2>/dev/null || true); do
  kill "$p" 2>/dev/null || true
done
sleep 0.5

# Always refresh shared keys for clean smoke (overwrite)
rm -rf "$KEYS"
mkdir -p "$KEYS"
$BIN/falcon-pl-ctl gen-keys --keys-dir "$KEYS" --scheme "$SCHEME" --validators v1,v2,v3
ls -la "$KEYS" | head -20

common=(--validators v1,v2,v3 --scheme "$SCHEME" --keys-dir "$KEYS" --seal-interval-ms 600 --bond 1000 --max-txs 8)

$BIN/falcon-pl-node --id v1 --listen 127.0.0.1:19101 \
  --peers 127.0.0.1:19102,127.0.0.1:19103 "${common[@]}" >"$LOG/v1.log" 2>&1 &
$BIN/falcon-pl-node --id v2 --listen 127.0.0.1:19102 \
  --peers 127.0.0.1:19101,127.0.0.1:19103 "${common[@]}" >"$LOG/v2.log" 2>&1 &
$BIN/falcon-pl-node --id v3 --listen 127.0.0.1:19103 \
  --peers 127.0.0.1:19101,127.0.0.1:19102 "${common[@]}" >"$LOG/v3.log" 2>&1 &

echo "nodes started, waiting for mesh…"
sleep 2

echo "== status v1 =="
$BIN/falcon-pl-ctl --addr 127.0.0.1:19101 status | head -40

echo "== pay 12 txs =="
for seq in $(seq 1 12); do
  $BIN/falcon-pl-ctl --addr 127.0.0.1:19101 pay \
    --from alice --to bob --amount 10 --fee 2 --sequence "$seq" \
    --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null
done

echo "== wait for auto-seal (non-empty only) =="
sleep 5

echo "== status v1 =="
$BIN/falcon-pl-ctl --addr 127.0.0.1:19101 status
echo "== status v2 =="
$BIN/falcon-pl-ctl --addr 127.0.0.1:19102 status
echo "== status v3 =="
$BIN/falcon-pl-ctl --addr 127.0.0.1:19103 status

# Compare tips via python
python3 - <<'PY'
import json, subprocess, sys
tips = []
for port in (19101, 19102, 19103):
    out = subprocess.check_output([
        "target/release/falcon-pl-ctl", "--addr", f"127.0.0.1:{port}", "status"
    ], cwd="/home/falcon/falcon-pl/falcon-pl-rs", text=True)
    # status prints WireMsg Status wrapper or body
    d = json.loads(out)
    body = d.get("body") or d
    tips.append((port, body.get("tip_height"), body.get("tip_hash"), body.get("mempool")))
print("tips:", tips)
hs = {t[2] for t in tips}
if len(hs) == 1 and tips[0][1] and tips[0][1] > 0:
    print("PASS: multi-node agreed tip_height=%s mempool=%s" % (tips[0][1], tips[0][3]))
    sys.exit(0)
print("FAIL: tips disagree or height 0")
sys.exit(1)
PY

echo
echo "logs=$LOG keys=$KEYS"
echo "stop: pkill -f 'target/release/falcon-pl-node' || true"
