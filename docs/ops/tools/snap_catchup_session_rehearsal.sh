#!/usr/bin/env bash
# Isolated loopback snapshot transfer test (scratch hmac keys, net 2200, ports 29401-29407).
# Usage: snap_catchup_session_rehearsal.sh TAG CHUNK_BYTES(0=default 256KiB) LOAD_WALLETS [PUSH_MAX]
# CHUNK_BYTES/PUSH_MAX set FALCON_PL_SNAPSHOT_CHUNK_BYTES / FALCON_PL_SNAPSHOT_PUSH_MAX (test hooks).
# PUSH_MAX=0 forces pure joiner hole-fill (NeedSnapshotChunk). Evaluate with the logs in $ART/logs.
set -uo pipefail
TAG="$1"; CHUNK="$2"; LOAD_WALLETS="$3"; PUSHMAX="${4:-}"
# Env: BIN, CTL (release falcon-pl-node / falcon-pl-ctl), SRC (repo, for commit stamp).
# Run on an isolated host under nice -n19 ionice -c3. Never point at live ports/keys.
BIN="${BIN:?falcon-pl-node path}"; CTL="${CTL:?falcon-pl-ctl path}"
COMMIT=$(cd "${SRC:-.}" && git rev-parse HEAD 2>/dev/null)
ART=/tmp/falcon-snap-session-art-$TAG
# Refuse to run if any scratch port is already owned (never kill unknown owners).
if ss -lnt | grep -qE '127\.0\.0\.1:2940[1-8]\b'; then echo "PORTS_BUSY"; ss -lntp | grep -E ':2940[1-8]\b'; exit 3; fi
rm -rf "$ART"; mkdir -p "$ART/logs"
date -u +'%Y-%m-%dT%H:%M:%SZ' > "$ART/started.txt"
echo "COMMIT=$COMMIT CHUNK=$CHUNK LOAD_WALLETS=$LOAD_WALLETS PUSHMAX=${PUSHMAX:-default}" | tee "$ART/meta.txt"
sha256sum "$BIN" >> "$ART/meta.txt"
SCRATCH=$(mktemp -d /tmp/falcon-snap-$TAG-XXXXXX); echo "scratch=$SCRATCH" | tee -a "$ART/meta.txt"
KEYS="$SCRATCH/keys"; mkdir -p "$KEYS/lc"; NET=2200; VALS="v1,v2,v3,v4,v5,v6"; JOIN=v7
export FPL_EMPTY_BLOCK_FROM_HEIGHT=1 RUST_LOG=falcon_pl_node=debug
[[ "$CHUNK" != 0 ]] && export FALCON_PL_SNAPSHOT_CHUNK_BYTES=$CHUNK
[[ -n "$PUSHMAX" ]] && export FALCON_PL_SNAPSHOT_PUSH_MAX=$PUSHMAX
export ART SCRATCH CTL KEYS NET
"$BIN" --gen-keys-only --keys-dir "$KEYS" --scheme hmac --allow-dev-crypto --validators "$VALS,$JOIN" --id v1 >/dev/null
printf '%s\n' '{"qc_header_version":4,"qc_start_height":1,"network_id":2200,"start_height":1}' >"$KEYS/lc/bridge.json"
PIDS=""
cleanup() { for p in $PIDS; do kill "$p" 2>/dev/null; done; sleep 0.7; for p in $PIDS; do kill -9 "$p" 2>/dev/null; done; kill "$SAMPLER" 2>/dev/null; }
SAMPLER=""
trap cleanup EXIT
start() {
  local id="$1" port="$2" mode="$3" peers="$4"; local dat="$SCRATCH/$id"; mkdir -p "$dat"/{run,snapshots,logs}
  local extra=() mcl=64
  [[ "$mode" == archive ]] && { extra+=(--archive --hub); mcl=256; }
  [[ "$mode" == join ]] && extra+=(--join)
  nice -n19 ionice -c3 "$BIN" --id "$id" --listen "127.0.0.1:$port" --peers "$peers" \
    --keys-dir "$KEYS" --scheme hmac --allow-dev-crypto --validators "$VALS,$JOIN" --bond 1000 \
    --network-id "$NET" --online-epoch-heights 0 --first-claim-epoch 1 --min-block-interval-ms 0 \
    --admin-sock "$dat/run/admin.sock" --snapshot "$dat/snapshots/snap.json" --snapshot-out "$dat/snapshots/snap.json" \
    --snapshot-interval-ms 4000 --seal-interval-ms 200 --pack-budget-ms 400 --progress-timeout-ms 4000 \
    --max-chain-ledgers "$mcl" --load-wallets "$LOAD_WALLETS" --lock-public-wire "${extra[@]}" \
    >"$dat/logs/node.log" 2>&1 &
  PIDS="$PIDS $!"; echo "started $id :$port pid=$! mode=$mode" | tee -a "$ART/meta.txt"; eval "PID_$id=$!"
}
MP() { local i=$1 o=() j; for j in 0 1 2 3 4 5; do [[ $j -eq $i ]]||o+=("127.0.0.1:$((29401+j))"); done; [[ $i -eq 0 ]] && o+=("127.0.0.1:29407"); local IFS=,; echo "${o[*]}"; }
for i in 0 1 2 3 4 5; do m=mesh; [[ $i -eq 0 ]] && m=archive; start v$((i+1)) $((29401+i)) $m "$(MP $i)"; done
sleep 6
"$CTL" --addr 127.0.0.1:29401 status >"$ART/v1_status0.json" || { tail -40 "$SCRATCH/v1/logs/node.log"; echo FAIL_BOOT > "$ART/RESULT.txt"; exit 2; }
python3 - <<'PY'
import subprocess,os,time
ctl=os.environ["CTL"]; keys=os.environ["KEYS"]; net=os.environ["NET"]
for seq in range(1,25):
  try: subprocess.check_output([ctl,"--addr","127.0.0.1:29401","pay","--from","alice","--to","bob","--amount","1","--fee","1","--sequence",str(seq),"--scheme","hmac","--keys-dir",keys,"--network-id",net],text=True,timeout=20,stderr=subprocess.STDOUT)
  except Exception as e: print("pay",seq,"err",e)
  time.sleep(0.05)
PY
python3 - <<'PY' || { echo FAIL_MESH > "$ART/RESULT.txt"; exit 2; }
import json,subprocess,os,time
from collections import Counter
ctl=os.environ["CTL"]; art=os.environ["ART"]; deadline=time.time()+120
while time.time()<deadline:
  tips=[];hs=set()
  for i in range(6):
    b=json.loads(subprocess.check_output([ctl,"--addr",f"127.0.0.1:{29401+i}","status"],text=True,timeout=20)).get("body") or {}
    tips.append(b.get("tip_height") or 0); hs.add((tips[-1], b.get("tip_hash") or ""))
  t,n=Counter(tips).most_common(1)[0]
  hs={h for (tt,h) in hs if tt==t}  # hash agreement among majority-tip seats
  print("mesh",tips,len(hs),flush=True)
  if t>=3 and n>=5 and len(hs)==1: open(art+"/mesh_ok.json","w").write(json.dumps(tips)); raise SystemExit(0)
  time.sleep(2)
raise SystemExit(1)
PY
"$CTL" --admin-sock "$SCRATCH/v1/run/admin.sock" snapshot-save --path "$SCRATCH/v1/snapshots/snap.json" || true
sleep 1; ls -la "$SCRATCH/v1/snapshots/" | tee "$ART/pin_dir.txt"
"$CTL" --addr 127.0.0.1:29401 status > "$ART/v1_pre_joiner.json"
start v7 29407 join "127.0.0.1:29401"
# RSS sampler for v1 (server) and v7 (joiner), exact PIDs
( while true; do for n in v1 v7; do p=$(eval echo \$PID_$n); [[ -r /proc/$p/status ]] && echo "$(date +%s.%N) $n $(awk '/VmRSS|VmHWM/{printf "%s=%s ",$1,$2}' /proc/$p/status)"; done; sleep 0.2; done ) > "$ART/rss_samples.txt" 2>/dev/null &
SAMPLER=$!
sleep 2
python3 - <<'PY'
import json,subprocess,os,time
ctl=os.environ["CTL"]; art=os.environ["ART"]; deadline=time.time()+150; rows=[]
while time.time()<deadline:
  try:
    b1=json.loads(subprocess.check_output([ctl,"--addr","127.0.0.1:29401","status"],text=True,timeout=20)).get("body") or {}
    b7=json.loads(subprocess.check_output([ctl,"--addr","127.0.0.1:29407","status"],text=True,timeout=20)).get("body") or {}
    r={"t":time.time(),"v1":b1.get("tip_height"),"v7":b7.get("tip_height"),"h1":b1.get("tip_hash"),"h7":b7.get("tip_hash")}
    rows.append(r); print("join",r["v1"],r["v7"],flush=True)
    if (r["v7"] or 0)>=(r["v1"] or 1) and r["h1"]==r["h7"]: break
  except Exception as e: rows.append({"err":str(e)})
  time.sleep(2)
open(art+"/trace.jsonl","w").write("\n".join(json.dumps(r) for r in rows)+"\n")
PY
sleep 2
for n in v1 v7; do p=$(eval echo \$PID_$n); echo "$n $(grep -E 'VmHWM|VmRSS' /proc/$p/status | tr -s ' \n' ' ')"; done | tee "$ART/rss_final.txt"
cp -a "$SCRATCH/v1/logs/node.log" "$ART/logs/v1.log"; cp -a "$SCRATCH/v7/logs/node.log" "$ART/logs/v7.log"
cp -a "$SCRATCH/v1/snapshots" "$ART/v1_snapshots_listing" 2>/dev/null; ls -la "$SCRATCH/v7/snapshots" > "$ART/v7_snap_dir.txt" 2>&1
echo DONE > "$ART/DONE"
