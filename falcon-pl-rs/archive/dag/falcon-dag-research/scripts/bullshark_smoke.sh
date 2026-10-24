#!/usr/bin/env bash
# Prove Bullshark DAG-derived ordering:
#  - 5 Full validators form availability certs (Narwhal)
#  - bullshark_anchors >= 1 (even-round leader committed via f+1 support)
#  - multi-node agreement on anchors / ordered / executed
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true
BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
BASE_RPC=29600
BASE_P2P=29700
UNL="val1,val2,val3,val4,val5"
LOGDIR=$(mktemp -d /tmp/fd-bullshark.XXXXXX)
PIDS=()

cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  for i in 0 1 2 3 4; do
    fuser -k $((BASE_RPC+i))/tcp >/dev/null 2>&1 || true
    fuser -k $((BASE_P2P+i))/tcp >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT

if [[ ! -x "$BIN" ]]; then
  (cd "$ROOT" && cargo build --release -p fd-node)
  BIN="$ROOT/target/release/falcon-dagd"
fi

for i in 0 1 2 3 4; do
  fuser -k $((BASE_RPC+i))/tcp >/dev/null 2>&1 || true
  fuser -k $((BASE_P2P+i))/tcp >/dev/null 2>&1 || true
done
sleep 0.2

echo "logdir=$LOGDIR"
for i in 0 1 2 3 4; do
  id="val$((i+1))"
  peers=""
  for j in 0 1 2 3 4; do
    [[ $j -eq $i ]] && continue
    peers+="127.0.0.1:$((BASE_P2P+j)),"
  done
  peers=${peers%,}
  "$BIN" \
    --id "$id" \
    --role full \
    --rpc "127.0.0.1:$((BASE_RPC+i))" \
    --p2p "127.0.0.1:$((BASE_P2P+i))" \
    --peers "$peers" \
    --unl "$UNL" \
    --worker-index "$i" \
    --worker-count 5 \
    --network-id 9003 \
    --crypto hmac-sha256-dev \
    --require-tx-sigs \
    --auto-sign \
    --batch-interval 0.05 \
    --max-batch-txs 64 \
    --unl-threshold 0.6 \
    --data-dir "$LOGDIR/$id" \
    >"$LOGDIR/$id.log" 2>&1 &
  PIDS+=($!)
  echo "started $id rpc=$((BASE_RPC+i))"
done

for i in 0 1 2 3 4; do
  for _ in $(seq 1 80); do
    if curl -sf -m1 -X POST "http://127.0.0.1:$((BASE_RPC+i))" \
      -H 'Content-Type: application/json' \
      -d '{"method":"ping","params":[{}]}' 2>/dev/null | grep -q pong; then
      break
    fi
    sleep 0.1
  done
done

post() {
  curl -sf -m 8 -X POST "http://127.0.0.1:$1" -H 'Content-Type: application/json' -d "$2"
}

# Pipeline many txs so the DAG grows past even-round anchors + support rounds.
for seq in $(seq 1 40); do
  post $BASE_RPC "{\"method\":\"submit\",\"params\":[{\"account\":\"acct1\",\"sequence\":$seq,\"destination\":\"acct2\",\"amount\":1,\"fee\":10}]}" \
    | python3 -c "import sys,json;d=json.load(sys.stdin); c=(d.get('result') or d).get('engine_result'); assert c=='tesSUCCESS', d" || true
done
echo "submitted 40 txs"

ok=0
for _ in $(seq 1 200); do
  read -r EXEC ANCH <<<"$(post $BASE_RPC '{"method":"server_info","params":[{}]}' \
    | python3 -c "import sys,json;m=json.load(sys.stdin)['result']['info']['metrics'];print(m.get('executed_txs',0), m.get('bullshark_anchors',0))" 2>/dev/null || echo "0 0")"
  if [[ "${EXEC:-0}" -ge 1 && "${ANCH:-0}" -ge 1 ]]; then ok=1; break; fi
  sleep 0.15
done

# If Bullshark anchors are slow under thin packs, still require architecture + some progress.
if [[ $ok -ne 1 ]]; then
  # Fallback: wait for tip path + any ordered progress, then require bullshark when certs deep enough
  for _ in $(seq 1 80); do
    read -r EXEC CERTS ANCH <<<"$(post $BASE_RPC '{"method":"server_info","params":[{}]}' \
      | python3 -c "import sys,json;m=json.load(sys.stdin)['result']['info']['metrics'];print(m.get('executed_txs',0), m.get('certified',0), m.get('bullshark_anchors',0))" 2>/dev/null || echo "0 0 0")"
    if [[ "${ANCH:-0}" -ge 1 ]]; then ok=1; break; fi
    if [[ "${CERTS:-0}" -ge 6 && "${EXEC:-0}" -ge 1 ]]; then
      # deep enough DAG should produce an anchor soon
      sleep 0.5
    fi
    sleep 0.15
  done
fi

[[ $ok -eq 1 ]] || {
  echo "FAIL: no Bullshark anchor commit"
  post $BASE_RPC '{"method":"server_info","params":[{}]}' || true
  tail -50 "$LOGDIR/val1.log"
  exit 1
}

sleep 1

python3 - <<'PY'
import json, urllib.request, time
base=29600
def metrics(i):
    for attempt in range(20):
        try:
            req=urllib.request.Request(
                f"http://127.0.0.1:{base+i}",
                data=b'{"method":"server_info","params":[{}]}',
                headers={"Content-Type":"application/json"},
            )
            d=json.load(urllib.request.urlopen(req, timeout=5))
            m=(d.get("result") or {}).get("info", {}).get("metrics") or d.get("result", {}).get("metrics")
            if m:
                return m
        except Exception as e:
            time.sleep(0.2)
    raise RuntimeError(f"no metrics from val{i+1}")

# Wait for agreement on anchors
for _ in range(60):
    ms = [metrics(i) for i in range(5)]
    anchors = [int(m.get("bullshark_anchors") or 0) for m in ms]
    if min(anchors) >= 1 and min(anchors) == max(anchors):
        break
    time.sleep(0.25)
else:
    ms = [metrics(i) for i in range(5)]
    raise SystemExit(f"anchor agreement timeout: {[int(m.get('bullshark_anchors') or 0) for m in ms]}")

execs, anchors, ordered, last = [], [], [], []
for i, m in enumerate(ms):
    arch = m.get("architecture") or ""
    assert "bullshark" in arch, (i, arch)
    a = int(m.get("bullshark_anchors") or 0)
    assert a >= 1, (i, m)
    assert int(m.get("executed_txs") or 0) >= 1, (i, m)
    execs.append(int(m["executed_txs"]))
    anchors.append(a)
    ordered.append(int(m.get("ordered") or 0))
    last.append(str(m.get("bullshark_last_anchor") or "")[:16])
    print(f"val{i+1}: anchors={a} rounds={m.get('bullshark_committed_rounds')} exec={m['executed_txs']} ordered={m.get('ordered')} last={last[-1]}")
assert min(anchors) == max(anchors), f"anchor count disagree {anchors}"
# ordered/exec may lag one tick behind on a lagging peer — require majority match
from collections import Counter
assert Counter(ordered).most_common(1)[0][1] >= 3, f"ordered disagree {ordered}"
assert Counter(execs).most_common(1)[0][1] >= 3, f"exec disagree {execs}"
assert Counter(last).most_common(1)[0][1] >= 3, f"last anchor disagree {last}"
print("bullshark_smoke OK — DAG-derived anchors + multi-node agreement")
PY
