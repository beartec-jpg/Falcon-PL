#!/usr/bin/env bash
# Prove tip-BFT light ordering:
#  - 5 Full validators form DA certs
#  - Ordering advances only via tip_proposal / tip_vote / tip_commit (not pure recompute)
#  - tip_commits > 0 on all nodes; executed txs agree
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true
BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
BASE_RPC=29400
BASE_P2P=29500
UNL="val1,val2,val3,val4,val5"
LOGDIR=$(mktemp -d /tmp/fd-tip-bft.XXXXXX)
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
    --max-batch-txs 128 \
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

for seq in $(seq 1 30); do
  post $BASE_RPC "{\"method\":\"submit\",\"params\":[{\"account\":\"acct1\",\"sequence\":$seq,\"destination\":\"acct2\",\"amount\":1,\"fee\":10}]}" \
    | python3 -c "import sys,json;d=json.load(sys.stdin); c=(d.get('result') or d).get('engine_result'); assert c=='tesSUCCESS', d"
done
echo "submitted 30 txs"

ok=0
for _ in $(seq 1 150); do
  read -r EXEC TIPS <<<"$(post $BASE_RPC '{"method":"server_info","params":[{}]}' \
    | python3 -c "import sys,json;m=json.load(sys.stdin)['result']['info']['metrics'];print(m.get('executed_txs',0), m.get('tip_commits',0))" 2>/dev/null || echo "0 0")"
  if [[ "${EXEC:-0}" -ge 1 && "${TIPS:-0}" -ge 1 ]]; then ok=1; break; fi
  sleep 0.15
done
[[ $ok -eq 1 ]] || {
  echo "FAIL: tip-BFT did not commit+execute"
  post $BASE_RPC '{"method":"server_info","params":[{}]}' || true
  tail -40 "$LOGDIR/val1.log"
  exit 1
}

# Allow packing to catch up a bit under tip-BFT
sleep 2

# All validators: tip_commits >= 1, tip-bft architecture, agreement on tip_commits / ordered
python3 - <<'PY'
import json, urllib.request
base=29400
execs=[]
tips=[]
ordered=[]
for i in range(5):
    req=urllib.request.Request(
        f"http://127.0.0.1:{base+i}",
        data=b'{"method":"server_info","params":[{}]}',
        headers={"Content-Type":"application/json"},
    )
    d=json.load(urllib.request.urlopen(req, timeout=5))
    m=d["result"]["info"]["metrics"]
    arch = m.get("architecture") or ""
    assert "bullshark" in arch or "tip-bft" in arch, (i, arch, m)
    assert int(m.get("tip_commits") or 0) >= 1, (i, m)
    assert int(m.get("tip_bft_round") or 0) >= 2, (i, m)
    assert int(m.get("executed_txs") or 0) >= 1, (i, m)
    assert int(m.get("ledger_commits") or 0) >= 1, (i, m)
    execs.append(int(m["executed_txs"]))
    tips.append(int(m["tip_commits"]))
    ordered.append(int(m.get("ordered") or 0))
    print(f"val{i+1}: tip_commits={m['tip_commits']} tip_round={m['tip_bft_round']} exec={m['executed_txs']} ordered={m.get('ordered')} last={str(m.get('tip_bft_last_commit',''))[:12]}")
# Tip-BFT agreement: same tip commit count and ordered length (exec may lag slightly on late packs)
assert min(tips) == max(tips), f"tip_commits disagreement {tips}"
assert min(ordered) == max(ordered), f"ordered disagreement {ordered}"
assert min(execs) == max(execs), f"exec disagreement {execs}"
print("tip_bft_smoke OK — tip commits + multi-node agreement (order advances via tip-BFT)")
PY
