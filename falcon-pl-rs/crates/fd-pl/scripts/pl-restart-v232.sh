#!/usr/bin/env bash
# Falcon PL v2.3.2 private testnet — P0 safety + P1 liveness + P2 recovery.
#
# Fresh genesis, three hubs (v1/v2 local, v3 host2), then live bond-in lights.
# Always --max-chain-ledgers 20000 (never 0).
#
#   bash crates/fd-pl/scripts/pl-restart-v232.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RS="$(cd "$SCRIPT_DIR/../../.." && pwd)"
ROOT="${PL_ROOT:-$HOME/falcon-pl-private-testnet-v22}"
BIN_SRC="$RS/target/release"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
RUN="$ROOT/run"
SCHEME=hmac
NETWORK_ID=2200
BOND=1000
LOAD_WALLETS=32
PROGRESS=2500
SEAL=400
PACK=1000
CHAIN_CAP=20000

SSH_OPTS=(-o ConnectTimeout=12 -o BatchMode=yes -o StrictHostKeyChecking=accept-new)

HUBS="100.64.0.1:19101,100.64.0.1:19102,100.64.0.3:19111"
SEED_VALS="v1,v2,v3"
LOCAL_IP="100.64.0.1"

mkdir -p "$ROOT"/{bin,keys,logs,snapshots,run}

log() { echo "[v232] $*"; }

stop_all() {
  log "stop all nodes + load"
  ps -eo pid,cmd | awk '/falcon-pl-load-net/ && !/awk/ {print $1}' | while read -r p; do kill "$p" 2>/dev/null || true; done
  # kill by path (avoid pkill -f self-match)
  ps -eo pid,cmd | awk '/falcon-pl-node/ && !/awk/ {print $1}' | while read -r p; do kill "$p" 2>/dev/null || true; done
  for h in host2 host3; do
    ssh "${SSH_OPTS[@]}" "$h" 'ps -eo pid,cmd | awk "/falcon-pl-node/ && !/awk/ {print \$1}" | xargs -r kill 2>/dev/null; true' 2>/dev/null || true
  done
  for t in root@seat-host-4 root@seat-host-5 root@seat-host-6 root@seat-host-7; do
    timeout 20 ssh "${SSH_OPTS[@]}" "$t" 'ps -eo pid,cmd | awk "/falcon-pl-node/ && !/awk/ {print \$1}" | xargs -r kill 2>/dev/null; true' 2>/dev/null || true
  done
  for p in $(seq 19101 19170); do fuser -k "${p}/tcp" 2>/dev/null || true; done
  sleep 1
  # Fresh logs each cut (avoid multi-GB append from prior storms)
  rm -f "$ROOT"/logs/*.log
  for h in host2 host3; do
    ssh "${SSH_OPTS[@]}" "$h" "rm -f $ROOT/logs/*.log" 2>/dev/null || true
  done
}

build() {
  cd "$RS"
  log "build release fd-pl"
  cargo build -p fd-pl --release 2>&1 | tail -8
  cp -f "$BIN_SRC/falcon-pl-node" "$BIN_SRC/falcon-pl-ctl" "$ROOT/bin/"
  chmod +x "$ROOT/bin/"*
  # smoke product version string
  strings "$ROOT/bin/falcon-pl-node" | grep -q '2.3.2' && log "binary product 2.3.2 ok" || log "WARN: 2.3.2 string not found in binary"
}

genesis() {
  log "fresh genesis (wipe keys/snaps/load-seq)"
  rm -rf "$KEYS" "$SNAP"
  rm -f "$RUN/load-seq.json"
  mkdir -p "$KEYS" "$SNAP" "$LOG" "$RUN"
  "$ROOT/bin/falcon-pl-ctl" gen-keys \
    --keys-dir "$KEYS" --scheme "$SCHEME" \
    --validators "v1,v2,v3,v4,v5,v6,v7,v8,v9,v10,v11,v12,v13" \
    --load-wallets "$LOAD_WALLETS"
  "$ROOT/bin/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account faucet --scheme "$SCHEME" || true
  echo "$SEED_VALS" >"$RUN/validators.txt"
  cat >"$RUN/live-mesh.txt" <<EOF
v1 $LOCAL_IP 19101
v2 $LOCAL_IP 19102
v3 100.64.0.3 19111
EOF
  cat >"$RUN/layout.txt" <<EOF
v1 $LOCAL_IP 19101 1 f1 local $ROOT
v2 $LOCAL_IP 19102 0 f1 local $ROOT
v3 100.64.0.3 19111 0 f2 ssh:host2 $ROOT
v4 100.64.0.5 19121 0 f3 ssh:host3 $ROOT
v5 100.64.0.3 19112 0 f2 ssh:host2 $ROOT
v6 $LOCAL_IP 19103 0 f1 local $ROOT
v7 100.64.0.2 19131 0 hil ts:root@seat-host-4 /root/falcon-pl-private-testnet-v22
v8 $LOCAL_IP 19104 0 f1 local $ROOT
v9 100.64.0.3 19113 0 f2 ssh:host2 $ROOT
v10 100.64.0.2 19132 0 hil ts:root@seat-host-4 /root/falcon-pl-private-testnet-v22
v11 100.64.0.4 19141 0 fsn ts:root@seat-host-5 /root/falcon-pl-private-testnet-v22
v12 100.64.0.6 19151 0 mhr ts:root@seat-host-6 /root/falcon-pl-private-testnet-v22
v13 $LOCAL_IP 19105 0 f1 local $ROOT
EOF
}

deploy_remote() {
  local how="$1" rroot="$2"
  case "$how" in
    ssh:*)
      local h="${how#ssh:}"
      ssh "${SSH_OPTS[@]}" "$h" "mkdir -p $rroot/{bin,keys,logs,snapshots,run}"
      rsync -az -e "ssh ${SSH_OPTS[*]}" "$ROOT/bin/" "$h:$rroot/bin/"
      rsync -az --delete -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$h:$rroot/keys/"
      ;;
    ts:*)
      local th="${how#ts:}"
      timeout 40 ssh "${SSH_OPTS[@]}" "$th" "mkdir -p $rroot/{bin,keys,logs,snapshots,run}"
      tar -C "$ROOT/bin" -czf - falcon-pl-node falcon-pl-ctl \
        | timeout 90 ssh "${SSH_OPTS[@]}" "$th" "tar -C $rroot/bin -xzf - && chmod +x $rroot/bin/*"
      tar -C "$ROOT" -czf - keys \
        | timeout 90 ssh "${SSH_OPTS[@]}" "$th" "cd $rroot && rm -rf keys && tar -xzf -"
      ;;
  esac
}

start_seed() {
  local common=(
    --validators "$SEED_VALS" --scheme "$SCHEME" --keys-dir "$KEYS"
    --load-wallets "$LOAD_WALLETS" --bond "$BOND" --network-id "$NETWORK_ID"
    --seal-interval-ms "$SEAL" --pack-budget-ms "$PACK"
    --progress-timeout-ms "$PROGRESS" --max-mempool-txs 20000
    --epoch-ms 3600000 --unbond-cooldown-ms 86400000
    --snapshot-interval-ms 60000 --max-chain-ledgers "$CHAIN_CAP"
    --hubs "$HUBS"
  )
  : >"$RUN/seed.pids"

  rm -f "$SNAP/v1.json"
  nohup "$ROOT/bin/falcon-pl-node" --id v1 --hub --listen 0.0.0.0:19101 \
    --peers "100.64.0.1:19102,100.64.0.3:19111" \
    --snapshot "$SNAP/v1.json" --snapshot-out "$SNAP/v1.json" \
    "${common[@]}" >>"$LOG/v1.log" 2>&1 &
  echo $! >>"$RUN/seed.pids"
  log "start v1 hub pid=$!"

  rm -f "$SNAP/v2.json"
  nohup "$ROOT/bin/falcon-pl-node" --id v2 --hub --listen 0.0.0.0:19102 \
    --peers "100.64.0.1:19101,100.64.0.3:19111" \
    --snapshot "$SNAP/v2.json" --snapshot-out "$SNAP/v2.json" \
    "${common[@]}" >>"$LOG/v2.log" 2>&1 &
  echo $! >>"$RUN/seed.pids"
  log "start v2 hub pid=$!"

  deploy_remote "ssh:host2" "$ROOT"
  ssh "${SSH_OPTS[@]}" host2 bash -s <<EOF
set -e
R=$ROOT
mkdir -p \$R/logs \$R/snapshots \$R/run
rm -f \$R/snapshots/v3.json
nohup \$R/bin/falcon-pl-node --id v3 --hub --listen 0.0.0.0:19111 \
  --peers "100.64.0.1:19101,100.64.0.1:19102" \
  --validators $SEED_VALS --scheme $SCHEME --keys-dir \$R/keys \
  --load-wallets $LOAD_WALLETS --bond $BOND --network-id $NETWORK_ID \
  --seal-interval-ms $SEAL --pack-budget-ms $PACK --progress-timeout-ms $PROGRESS \
  --max-mempool-txs 20000 --epoch-ms 3600000 --unbond-cooldown-ms 86400000 \
  --snapshot-interval-ms 60000 --max-chain-ledgers $CHAIN_CAP --hubs "$HUBS" \
  --snapshot \$R/snapshots/v3.json --snapshot-out \$R/snapshots/v3.json \
  >>\$R/logs/v3.log 2>&1 &
echo \$! > \$R/run/v3.pid
echo start v3 hub pid=\$!
EOF
}

light_load() {
  local n="${1:-15}" i
  for ((i = 0; i < n; i++)); do
    "$ROOT/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 pay --from alice --to bob --amount 1 --fee 2 \
      --sequence 0 --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null 2>&1 || true
  done
  sleep 2
}

join_light() {
  local id="$1" how="$2" ip="$3" port="$4" rroot="$5"
  local peers="100.64.0.1:19101,100.64.0.1:19102,100.64.0.3:19111"
  log "bond-in light $id @ $ip:$port"
  "$ROOT/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 faucet --to alice --amount 1000000 \
    --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null 2>&1 || true
  light_load 8
  "$ROOT/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 bond \
    --id "$id" --from alice --amount "$BOND" \
    --scheme "$SCHEME" --keys-dir "$KEYS" || true
  local i
  for i in $(seq 1 40); do
    light_load 4
    if "$ROOT/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 status 2>/dev/null | python3 -c "
import sys,json
raw=sys.stdin.read(); b=json.JSONDecoder().raw_decode(raw[raw.find('{'):])[0]
for v in (b.get('body') or b).get('validators') or []:
  if v.get('id')=='$id' and int(v.get('bond') or 0)>=$BOND: raise SystemExit(0)
raise SystemExit(1)
" 2>/dev/null; then
      break
    fi
  done
  "$ROOT/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 snapshot-save --path "$SNAP/v1.json" >/dev/null 2>&1 || true
  sleep 1
  cp -f "$SNAP/v1.json" "$SNAP/${id}.json"

  case "$how" in
    local)
      nohup "$ROOT/bin/falcon-pl-node" --join --id "$id" --listen "0.0.0.0:$port" \
        --peers "$peers" --validators "$SEED_VALS" --scheme "$SCHEME" --keys-dir "$KEYS" \
        --load-wallets "$LOAD_WALLETS" --bond "$BOND" --network-id "$NETWORK_ID" \
        --seal-interval-ms "$SEAL" --pack-budget-ms "$PACK" --progress-timeout-ms "$PROGRESS" \
        --max-mempool-txs 20000 --epoch-ms 3600000 --unbond-cooldown-ms 86400000 \
        --snapshot-interval-ms 60000 --max-chain-ledgers "$CHAIN_CAP" --hubs "$HUBS" \
        --snapshot "$SNAP/${id}.json" --snapshot-out "$SNAP/${id}.json" \
        >>"$LOG/${id}.log" 2>&1 &
      log "start light $id pid=$!"
      ;;
    ssh:*)
      deploy_remote "$how" "$rroot"
      rsync -az -e "ssh ${SSH_OPTS[*]}" "$SNAP/${id}.json" "${how#ssh:}:$rroot/snapshots/${id}.json"
      ssh "${SSH_OPTS[@]}" "${how#ssh:}" bash -s <<EOF
set -e
R=$rroot
nohup \$R/bin/falcon-pl-node --join --id $id --listen 0.0.0.0:$port \
  --peers "$peers" --validators $SEED_VALS --scheme $SCHEME --keys-dir \$R/keys \
  --load-wallets $LOAD_WALLETS --bond $BOND --network-id $NETWORK_ID \
  --seal-interval-ms $SEAL --pack-budget-ms $PACK --progress-timeout-ms $PROGRESS \
  --max-mempool-txs 20000 --epoch-ms 3600000 --unbond-cooldown-ms 86400000 \
  --snapshot-interval-ms 60000 --max-chain-ledgers $CHAIN_CAP --hubs "$HUBS" \
  --snapshot \$R/snapshots/${id}.json --snapshot-out \$R/snapshots/${id}.json \
  >>\$R/logs/${id}.log 2>&1 &
echo light $id pid=\$!
EOF
      ;;
    ts:*)
      deploy_remote "$how" "$rroot"
      tar -C "$SNAP" -czf - "${id}.json" \
        | timeout 40 ssh "${SSH_OPTS[@]}" "${how#ts:}" "tar -C $rroot/snapshots -xzf -"
      timeout 40 ssh "${SSH_OPTS[@]}" "${how#ts:}" bash -s <<EOF
set -e
R=$rroot
nohup \$R/bin/falcon-pl-node --join --id $id --listen 0.0.0.0:$port \
  --peers "$peers" --validators $SEED_VALS --scheme $SCHEME --keys-dir \$R/keys \
  --load-wallets $LOAD_WALLETS --bond $BOND --network-id $NETWORK_ID \
  --seal-interval-ms $SEAL --pack-budget-ms $PACK --progress-timeout-ms $PROGRESS \
  --max-mempool-txs 20000 --epoch-ms 3600000 --unbond-cooldown-ms 86400000 \
  --snapshot-interval-ms 60000 --max-chain-ledgers $CHAIN_CAP --hubs "$HUBS" \
  --snapshot \$R/snapshots/${id}.json --snapshot-out \$R/snapshots/${id}.json \
  >>\$R/logs/${id}.log 2>&1 &
echo light $id pid=\$!
EOF
      ;;
  esac
  echo "$id $ip $port" >>"$RUN/live-mesh.txt"
  sleep 3
}

status() {
  "$ROOT/bin/falcon-pl-ctl" --addr 127.0.0.1:19101 status 2>/dev/null | python3 -c "
import sys,json
raw=sys.stdin.read(); b=json.JSONDecoder().raw_decode(raw[raw.find('{'):])[0]
body=b.get('body',b)
vals=body.get('validators') or []
print(f\"product={body.get('product_version')} role={body.get('role')} tip={body.get('tip_height')} tip_soft={body.get('tip_soft')} mp={body.get('mempool')}\")
print(f\"winner={body.get('lottery_winner_next')} applicable={body.get('has_applicable_tx')} soft_need={body.get('soft_need')} hard_need={body.get('hard_need')}\")
print(f\"need_hard_resync={body.get('need_hard_resync')} conflict={body.get('last_catchup_conflict')!r}\")
print(f\"ready={sum(1 for v in vals if v.get('lottery_ready'))}/{len(vals)}\")
for v in sorted(vals, key=lambda x: x.get('id') or ''):
  print(f\"  {v.get('id'):4} bond={v.get('bond')} ready={v.get('lottery_ready')} packs={v.get('pack_count')} pong_fresh={v.get('pong_fresh')}\")
"
}

mesh_span() {
  python3 - <<'PY'
import socket, json, time
hosts=[("v1","127.0.0.1",19101),("v2","127.0.0.1",19102),("v3","100.64.0.3",19111),
       ("v6","127.0.0.1",19103),("v4","100.64.0.5",19121),("v5","100.64.0.3",19112)]
def st(h,p,t=2.0):
    s=socket.socket(); s.settimeout(t)
    try:
        s.connect((h,p))
        try: s.recv(65536)
        except: pass
        s.sendall(b'{"type":"status_req"}\n')
        buf=b""; t0=time.time()
        while time.time()-t0<t:
            try:
                c=s.recv(262144)
                if not c: break
                buf+=c
                if b'"type":"status"' in buf: break
            except: break
        s.close()
        for line in buf.decode(errors='replace').splitlines():
            try:
                j=json.loads(line)
                if j.get('type')=='status' or 'body' in j:
                    b=j.get('body',j)
                    return b.get('tip_height'), b.get('product_version'), b.get('tip_hash','')[:12]
            except: pass
        return None, None, None
    except Exception as e:
        return f"ERR:{e}", None, None
tips=[]
for id,h,p in hosts:
    tip,pv,th=st(h,p)
    print(f"{id:4} tip={tip} product={pv} hash={th}")
    if isinstance(tip,int): tips.append(tip)
if tips:
    print(f"span={max(tips)-min(tips)} n={len(tips)} min={min(tips)} max={max(tips)}")
else:
    print("span=n/a (no tips)")
PY
}

# --- main ---
stop_all
build
genesis
start_seed
sleep 5
light_load 25
log "======== seed status ========"
status
mesh_span

# Core lights (reachable hosts)
join_light v6 local "$LOCAL_IP" 19103 "$ROOT"
join_light v8 local "$LOCAL_IP" 19104 "$ROOT"
join_light v5 ssh:host2 100.64.0.3 19112 "$ROOT"
join_light v4 ssh:host3 100.64.0.5 19121 "$ROOT"
join_light v7 ts:root@seat-host-4 100.64.0.2 19131 /root/falcon-pl-private-testnet-v22 || log "WARN: v7 join failed"

light_load 30
sleep 4
log "======== final status ========"
status
mesh_span
log "done — product 2.3.2 fleet up (load not started)"
