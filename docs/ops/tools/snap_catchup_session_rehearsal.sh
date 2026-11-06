#!/usr/bin/env bash
# Isolated installer-style join-snap transfer (loopback, scratch keys).
# Usage on beartec-brain:
#   nice -n19 ionice -c3 bash docs/ops/tools/snap_catchup_session_rehearsal.sh \
#     /path/to/falcon-pl-rs [/path/to/art]
set -euo pipefail
ROOT="${1:?falcon-pl-rs root}"
ART="${2:-/tmp/falcon-snap-session-art}"
mkdir -p "$ART"
SCRATCH="$(mktemp -d /tmp/falcon-snap-sess-XXXXXX)"
PIDS=""
cleanup() {
  for p in $PIDS; do kill "$p" 2>/dev/null || true; done
  sleep 0.3
  for p in $PIDS; do kill -9 "$p" 2>/dev/null || true; done
  mkdir -p "$ART/scratch-copy"
  cp -a "$SCRATCH/." "$ART/scratch-copy/" 2>/dev/null || true
  # preserve logs already under ART
}
trap cleanup EXIT

echo "scratch=$SCRATCH art=$ART" | tee "$ART/meta.txt"
free -h | head -2 | tee -a "$ART/meta.txt"
date -u +'%Y-%m-%dT%H:%M:%SZ' | tee -a "$ART/meta.txt"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/falcon-snap-sess-cargo}"
export PATH="${HOME}/.cargo/bin:/root/.cargo/bin:$PATH"
BIN="$CARGO_TARGET_DIR/debug/falcon-pl-node"
CTL="$CARGO_TARGET_DIR/debug/falcon-pl-ctl"
if [[ ! -x "$BIN" || ! -x "$CTL" ]]; then
  (cd "$ROOT" && CARGO_BUILD_JOBS=2 nice -n19 ionice -c3 cargo build -p fd-pl --bin falcon-pl-node --bin falcon-pl-ctl)
fi
test -x "$BIN" && test -x "$CTL"
sha256sum "$BIN" | tee "$ART/binary.sha256"

PORT_BASE="${PORT_BASE:-29401}"
NET=2200
VALS="v1,v2,v3,v4,v5,v6"
KEYS="$SCRATCH/keys"
mkdir -p "$KEYS/lc"
"$BIN" --gen-keys-only --keys-dir "$KEYS" --scheme hmac --allow-dev-crypto \
  --validators "$VALS" --id v1 >/dev/null
printf '%s\n' '{"qc_header_version":4,"qc_start_height":1,"network_id":2200,"start_height":1}' >"$KEYS/lc/bridge.json"
export CTL KEYS PORT_BASE NET SCRATCH ART

start_one() {
  local id="$1" port="$2" extra=()
  shift 2
  extra=("$@")
  local dat="$SCRATCH/$id"
  mkdir -p "$dat"/{run,snapshots,logs}
  # peers: everyone else in 29401..29406 + joiner 29407 reserved
  local peers="" j
  for j in 0 1 2 3 4 5 6; do
    local p=$((PORT_BASE+j))
    local oid="v$((j+1))"
    [[ "$oid" == "$id" ]] && continue
    [[ "$j" -eq 6 && "$id" != "v7" ]] && continue  # mesh does not require joiner until later — include for dial rule
    [[ -n "$peers" ]] && peers+=","
    peers+="127.0.0.1:$p"
  done
  # Always include all 7 ports so dial rules work when v7 starts
  peers=""
  for j in 0 1 2 3 4 5 6; do
    local p=$((PORT_BASE+j))
    [[ $((PORT_BASE+j)) -eq $port ]] && continue
    [[ -n "$peers" ]] && peers+=","
    peers+="127.0.0.1:$p"
  done
  nice -n19 ionice -c3 "$BIN" \
    --id "$id" \
    --listen "127.0.0.1:$port" \
    --peers "$peers" \
    --keys-dir "$KEYS" \
    --scheme hmac --allow-dev-crypto \
    --validators "$VALS,v7" \
    --bond 1000 \
    --network-id "$NET" \
    --online-epoch-heights 0 \
    --first-claim-epoch 1 \
    --min-block-interval-ms 0 \
    --admin-sock "$dat/run/admin.sock" \
    --snapshot "$dat/snapshots/snap.json" \
    --snapshot-out "$dat/snapshots/snap.json" \
    --snapshot-interval-ms 3000 \
    --seal-interval-ms 300 \
    --pack-budget-ms 500 \
    --progress-timeout-ms 4000 \
    --max-chain-ledgers 256 \
    --lock-public-wire \
    "${extra[@]}" \
    >"$dat/logs/node.log" 2>&1 &
  PIDS="$PIDS $!"
  echo "started $id :$port pid=$! extra=${extra[*]:-}" | tee -a "$ART/meta.txt"
}

# Archive hub v1 + 5 mesh seats
start_one v1 $((PORT_BASE+0)) --hub --archive
for i in 1 2 3 4 5; do
  start_one "v$((i+1))" $((PORT_BASE+i))
done
sleep 4

# Drive tip with payments so join pin is non-trivial
python3 - <<PY
import subprocess, os, time
ctl=os.environ["CTL"]; keys=os.environ["KEYS"]; net=int(os.environ["NET"]); base=int(os.environ["PORT_BASE"])
for seq in range(1, 40):
    cmd=[ctl,"--addr",f"127.0.0.1:{base}","pay","--from","alice","--to","bob","--amount","1","--fee","1",
         "--sequence",str(seq),"--scheme","hmac","--keys-dir",keys,"--network-id",str(net)]
    try:
        subprocess.check_output(cmd,text=True,timeout=20,stderr=subprocess.STDOUT)
    except Exception as e:
        print("pay",seq,e)
    time.sleep(0.08)
print("pays done")
PY

# Wait tip >= 3
python3 - <<PY
import json,subprocess,time,os
ctl=os.environ["CTL"]; base=int(os.environ["PORT_BASE"])
deadline=time.time()+90
while time.time()<deadline:
    raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{base}","status"],text=True,timeout=25)
    tip=(json.loads(raw).get("body") or {}).get("tip_height") or 0
    print("tip",tip)
    if tip>=3: break
    time.sleep(2)
else:
    raise SystemExit("mesh tip timeout")
open(os.environ["ART"]+"/mesh_tip.json","w").write(raw)
PY

# Force snapshot-save on archive so pin exists
"$CTL" --admin-sock "$SCRATCH/v1/run/admin.sock" snapshot-save --path "$SCRATCH/v1/snapshots/snap.json" || true
sleep 1
# Record pin size
python3 - <<PY
import os,json
p=os.environ["SCRATCH"]+"/v1/snapshots/snap.json"
st=os.stat(p)
print("pin_bytes",st.st_size)
open(os.environ["ART"]+"/pin_meta.json","w").write(json.dumps({"path":p,"bytes":st.st_size},indent=2))
# Need a large-enough pin to exercise pieces: if small, note it
if st.st_size < 200_000:
    open(os.environ["ART"]+"/PIN_SMALL.txt","w").write("pin smaller than preferred piece-pressure size; still exercises Offer path\n")
PY

# Fresh joiner tip0
start_one v7 $((PORT_BASE+6)) --join
sleep 2

python3 - <<PY
import json,subprocess,time,os,re
ctl=os.environ["CTL"]; base=int(os.environ["PORT_BASE"]); art=os.environ["ART"]; scratch=os.environ["SCRATCH"]
v7=base+6
deadline=time.time()+180
rows=[]
ok=False
while time.time()<deadline:
    try:
        raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{v7}","status"],text=True,timeout=25)
        b=json.loads(raw).get("body") or {}
        rawp=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{base}","status"],text=True,timeout=25)
        bp=json.loads(rawp).get("body") or {}
        row={"v7":b.get("tip_height"),"peer":bp.get("tip_height"),"vh":b.get("tip_hash"),"ph":bp.get("tip_hash")}
        rows.append(row)
        print("catchup",row)
        if (b.get("tip_height") or 0) >= max(1,(bp.get("tip_height") or 0)) and b.get("tip_hash")==bp.get("tip_hash"):
            ok=True
            break
    except Exception as e:
        rows.append({"err":str(e)})
    time.sleep(2)
log=open(f"{scratch}/v7/logs/node.log",errors="replace").read()
v1log=open(f"{scratch}/v1/logs/node.log",errors="replace").read()
ev={
  "adopt": "snapshot adopted" in log,
  "offer": "SnapshotOffer" in log or "snapshot pull" in log or "catch-up (hello)" in log,
  "serve_to_v7": bool(re.search(r"snapshot serving tip=.* to=v7", v1log)),
  "serve_to_stale_peer": bool(re.search(r"snapshot serving tip=.* to=v[1-6]\\b", v1log)),
  "control_fail": "snapshot control reply failed" in v1log,
  "data_deferred": "catch-up data deferred" in v1log or "inbound catch-up data deferred" in v1log,
}
open(art+"/trace.jsonl","w").write("\n".join(json.dumps(r) for r in rows)+"\n")
open(art+"/evidence.json","w").write(json.dumps({"ok":ok,"evidence":ev,"final":rows[-1] if rows else {}},indent=2))
open(art+"/RESULT.txt","w").write("PASS\n" if ok else "FAIL\n")
print("EV",ev,"OK",ok)
# Isolation
import subprocess as sp
ss=sp.check_output(["bash","-lc","ss -lntp | grep 2940 || true"],text=True)
open(art+"/listeners.txt","w").write(ss)
if "0.0.0.0" in ss:
    raise SystemExit("non-loopback")
raise SystemExit(0 if ok else 2)
PY
