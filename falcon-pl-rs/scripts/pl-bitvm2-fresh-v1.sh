#!/usr/bin/env bash
# Nuclear v1 recovery: wipe stale snapshot/bodies, restart archive, snap-join from mesh.
# Run ON host1.
set -euo pipefail
PL="${PL_ROOT:-$HOME/falcon-pl-public-testnet-2300}"
CTL="$PL/bin/falcon-pl-ctl"
RPC="${FPL_RPC:-127.0.0.1:19301}"
ts=$(date +%Y%m%d-%H%M%S)

echo "==> stop v1"
while read -r pid; do kill "$pid" 2>/dev/null || true; done < <(pgrep -f 'falcon-pl-archive --id v1 ' || true)
sleep 2

echo "==> archive stale v1 snapshot + bodies"
mkdir -p "$PL/snapshots/stale-$ts"
for f in v1.json v1.join.json v1.join.serve.json v1.pull.json; do
  mv "$PL/snapshots/$f" "$PL/snapshots/stale-$ts/" 2>/dev/null || true
done
rm -rf "$PL/snapshots/bodies/v1"
mkdir -p "$PL/snapshots/bodies/v1"

echo "==> restart v1 archive (fresh snap join from v2/v3/v4)"
bash "$PL/scripts/restart-v1-bitvm2.sh" 2>&1 | tail -3

echo "==> wait for snap join + catch-up (up to 15 min)"
for i in $(seq 1 90); do
  if "$CTL" --addr "$RPC" status > /tmp/v1-status.json 2>/dev/null; then
    python3 - <<'PY'
import json, sys
b = json.load(open("/tmp/v1-status.json")).get("body") or json.load(open("/tmp/v1-status.json"))
tip = b.get("tip_height") or 0
peer = b.get("best_peer_tip") or 0
lag = peer - tip
conf = (b.get("last_catchup_conflict") or "")[:80]
print(f"  tip={tip} peer={peer} lag={lag} conf={conf!r}")
if lag <= 3 and "state root mismatch" not in (b.get("last_catchup_conflict") or ""):
    sys.exit(0)
sys.exit(1)
PY
    if [ $? -eq 0 ]; then
      echo "==> v1 healthy"
      exit 0
    fi
  fi
  sleep 10
done
echo "WARN: inspect $PL/logs/v1.log"
exit 1
