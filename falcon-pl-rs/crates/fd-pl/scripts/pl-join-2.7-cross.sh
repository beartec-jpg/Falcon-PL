#!/usr/bin/env bash
# Live-join extra 2.7 validators across machines so soft=3 and hard=4 (n=6).
#
#   host1  v1 :19101 hub, v2 :19102 hub, v6 :19103 joiner
#   host2  v3 :19111 hub, v5 :19112 joiner
#   droplet  v4 :19121 joiner
#
# Does not restart the 3 hubs. Bond txs ride the soak load.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=pl-hosts.env
# shellcheck disable=SC1091
source "$SCRIPT_DIR/pl-hosts.env"

ROOT="${PL_ROOT:-$HOME/falcon-pl-private-testnet-v22}"
BIN="$ROOT/bin"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
RUN="$ROOT/run"
SCHEME=falcon-512
NID=2200
F1_IP="${F1_IP:-$F1_TS}"
F2_IP="${F2_IP:-$F2_TS}"
F3_IP="${F3_IP:-$F3_TS}"
SSH_F2="${SSH_F2:-host2}"
SSH_F3="${SSH_F3:-host3}"
SSH_OPTS=(-o ConnectTimeout=12 -o BatchMode=yes -o StrictHostKeyChecking=accept-new)
HUB_PEERS="${F1_IP}:19101,${F1_IP}:19102,${F2_IP}:19111"
HUBS="$HUB_PEERS"
SRC_BIN="${PL_SRC_BIN:-$HOME/falcon-pl/falcon-pl-rs/target/release}"

CTL=("$BIN/falcon-pl-ctl")

tip() {
  local a=${1:-127.0.0.1:19101}
  "${CTL[@]}" --addr "$a" status 2>/dev/null | python3 -c 'import sys,json
try: print(int(json.load(sys.stdin)["body"]["tip_height"]))
except: print(-1)' || echo -1
}

status_json() {
  "${CTL[@]}" --addr "${1:-127.0.0.1:19101}" status
}

has_bond() {
  local vid=$1
  status_json 127.0.0.1:19101 | python3 -c '
import sys,json
vid=sys.argv[1]
b=json.load(sys.stdin)["body"]
for v in b.get("validators") or []:
    if v.get("id")==vid and int(v.get("bond") or 0)>=1000:
        print(v.get("bond")); raise SystemExit(0)
raise SystemExit(1)
' "$vid" >/dev/null 2>&1
}

qc_line() {
  status_json 127.0.0.1:19101 | python3 -c '
import json,sys
b=json.load(sys.stdin)["body"]
vals=b.get("validators") or []
print("tip=%s hash=%s soft=%s hard=%s n=%d online=%s vals=%s" % (
    b.get("tip_height"),
    str(b.get("tip_hash",""))[:16],
    b.get("soft_need"),
    b.get("hard_need"),
    len(vals),
    ",".join(b.get("online_members") or []),
    ",".join("%s:%s"%(v.get("id"),v.get("bond")) for v in vals if isinstance(v,dict)),
))
'
}

echo "======== 2.7 cross-host join (v4 droplet, v5 host2, v6 local) ========"
qc_line

# Deploy current 2.7 bins
if [[ -x "$SRC_BIN/falcon-pl-node" ]]; then
  install -m 0755 "$SRC_BIN/falcon-pl-node" "$BIN/falcon-pl-node"
  install -m 0755 "$SRC_BIN/falcon-pl-ctl" "$BIN/falcon-pl-ctl"
fi
for h in "$SSH_F2" "$SSH_F3"; do
  echo "[deploy] $h"
  ssh "${SSH_OPTS[@]}" "$h" "mkdir -p '$BIN' '$KEYS' '$LOG' '$SNAP' '$RUN'"
  rsync -az -e "ssh ${SSH_OPTS[*]}" "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$h:$BIN/"
  rsync -az -e "ssh ${SSH_OPTS[*]}" "$KEYS/pl_keys.json" "$KEYS/v4.json" "$KEYS/v5.json" "$KEYS/v6.json" "$KEYS/alice.json" "$h:$KEYS/"
done

# Sequential bonds (auto-seq; wait so we do not collide)
for vid in v4 v5 v6; do
  if has_bond "$vid"; then
    echo "[bond] $vid already bonded"
    continue
  fi
  echo "[bond] $vid from alice"
  "${CTL[@]}" --addr 127.0.0.1:19101 bond \
    --id "$vid" --from alice --amount 1000 --fee 25 \
    --scheme "$SCHEME" --keys-dir "$KEYS" --network-id "$NID"
  for i in $(seq 1 40); do
    if has_bond "$vid"; then
      echo "[bond] $vid visible tip=$(tip)"
      break
    fi
    sleep 2
    if [[ "$i" -eq 40 ]]; then
      echo "FAIL: $vid bond not packed" >&2
      qc_line
      exit 2
    fi
  done
done
echo "[bond] all present"
qc_line

# Fresh snapshot that includes the bonds
echo "[snap] save v1 via admin sock"
"${CTL[@]}" --admin-sock "$RUN/v1.admin.sock" snapshot-save --path "$SNAP/v1.json"
python3 - "$SNAP/v1.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
vals=d.get("validators") or {}
print("snap tip=%s v4=%s v5=%s v6=%s" % (
    d.get("tip_height"),
    (vals.get("v4") or {}).get("bond"),
    (vals.get("v5") or {}).get("bond"),
    (vals.get("v6") or {}).get("bond"),
))
need=["v4","v5","v6"]
missing=[v for v in need if int((vals.get(v) or {}).get("bond") or 0)<1000]
if missing:
    raise SystemExit("snapshot missing bonds: " + ",".join(missing))
PY
cp -f "$SNAP/v1.json" "$SNAP/v4.json"
cp -f "$SNAP/v1.json" "$SNAP/v5.json"
cp -f "$SNAP/v1.json" "$SNAP/v6.json"
rsync -az -e "ssh ${SSH_OPTS[*]}" "$SNAP/v4.json" "$SSH_F3:$SNAP/v4.json"
rsync -az -e "ssh ${SSH_OPTS[*]}" "$SNAP/v5.json" "$SSH_F2:$SNAP/v5.json"

start_join_local() {
  local id=$1 port=$2
  nohup "$BIN/falcon-pl-node" --join --id "$id" --listen "0.0.0.0:$port" \
    --peers "$HUB_PEERS" \
    --validators v1,v2,v3 --scheme "$SCHEME" --keys-dir "$KEYS" \
    --load-wallets 1024 --bond 1000 --network-id "$NID" \
    --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000 \
    --max-mempool-txs 20000 --epoch-ms 3600000 --unbond-cooldown-ms 86400000 \
    --snapshot-interval-ms 60000 --max-chain-ledgers 20000 \
    --hubs "$HUBS" --online-epoch-heights 10 \
    --admin-sock "$RUN/${id}.admin.sock" --lock-public-wire \
    --snapshot "$SNAP/${id}.json" --snapshot-out "$SNAP/${id}.json" \
    >"$LOG/${id}.log" 2>&1 &
  echo $! >"$RUN/${id}.pid"
  echo "[start] local $id pid=$(cat "$RUN/${id}.pid") :$port"
}

start_join_remote() {
  local host=$1 id=$2 port=$3
  ssh "${SSH_OPTS[@]}" "$host" "rm -f '$RUN/${id}.admin.sock'; : >'$LOG/${id}.log'; nohup '$BIN/falcon-pl-node' --join --id $id --listen 0.0.0.0:$port \
    --peers $HUB_PEERS \
    --validators v1,v2,v3 --scheme $SCHEME --keys-dir '$KEYS' \
    --load-wallets 1024 --bond 1000 --network-id $NID \
    --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000 \
    --max-mempool-txs 20000 --epoch-ms 3600000 --unbond-cooldown-ms 86400000 \
    --snapshot-interval-ms 60000 --max-chain-ledgers 20000 \
    --hubs $HUBS --online-epoch-heights 10 \
    --admin-sock '$RUN/${id}.admin.sock' --lock-public-wire \
    --snapshot '$SNAP/${id}.json' --snapshot-out '$SNAP/${id}.json' \
    >'$LOG/${id}.log' 2>&1 & echo \$! >'$RUN/${id}.pid'; echo started $id pid=\$(cat '$RUN/${id}.pid')"
}

start_join_local v6 19103
start_join_remote "$SSH_F2" v5 19112
start_join_remote "$SSH_F3" v4 19121

echo "[wait] joiners catch up"
ok=0
for i in $(seq 1 60); do
  python3 - <<'PY'
import json, subprocess, sys
BIN="/home/falcon/falcon-pl-private-testnet-v22/bin/falcon-pl-ctl"
F2="100.64.0.3"
F3="100.64.0.5"
rows=[]
for name,a in [("v1","127.0.0.1:19101"),("v2","127.0.0.1:19102"),("v3",f"{F2}:19111"),
               ("v6","127.0.0.1:19103"),("v5",f"{F2}:19112"),("v4",f"{F3}:19121")]:
    try:
        b=json.loads(subprocess.check_output([BIN,"--addr",a,"status"], text=True, timeout=8))["body"]
        rows.append((name,int(b.get("tip_height") or -1), str(b.get("tip_hash") or "")[:16],
                     b.get("soft_need"), b.get("hard_need"),
                     ",".join(b.get("online_members") or [])))
    except Exception as e:
        rows.append((name,-1,str(e)[:40],None,None,""))
for r in rows:
    print("  %s tip=%s hash=%s soft=%s hard=%s online=%s" % r)
tips={r[0]:r[1] for r in rows}
hashes={r[0]:r[2] for r in rows}
h1=hashes.get("v1")
good=all(tips.get(x,-1)>0 for x in ("v1","v2","v3","v4","v5","v6"))
same=all(hashes.get(x)==h1 for x in ("v2","v3","v4","v5","v6")) if good else False
# allow 2-height lag while packing
close=good and all(abs(tips[x]-tips["v1"])<=2 for x in ("v2","v3","v4","v5","v6"))
sys.exit(0 if good and close and same else 1)
PY
  if [[ $? -eq 0 ]]; then
    ok=1
    break
  fi
  sleep 3
done

echo "======== after join ========"
qc_line
if [[ "$ok" -eq 1 ]]; then
  echo "JOIN_OK: 6 validators across host1 / host2 / droplet"
else
  echo "JOIN_PARTIAL: check joiner logs (may still be residual catch-up)"
  tail -8 "$LOG/v6.log" || true
  ssh "${SSH_OPTS[@]}" "$SSH_F2" "tail -8 '$LOG/v5.log'" || true
  ssh "${SSH_OPTS[@]}" "$SSH_F3" "tail -8 '$LOG/v4.log'" || true
fi
