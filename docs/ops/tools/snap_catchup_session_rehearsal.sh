#!/usr/bin/env bash
# Isolated installer-style join-snap (loopback, scratch keys). beartec-brain only.
# Usage: nice -n19 ionice -c3 bash docs/ops/tools/snap_catchup_session_rehearsal.sh \
#          /path/to/falcon-pl-rs [/path/to/art]
set -euo pipefail
ROOT="${1:?falcon-pl-rs root}"
ART="${2:-/tmp/falcon-snap-session-art}"
mkdir -p "$ART"
SCRATCH="$(mktemp -d /tmp/falcon-snap-sess-XXXXXX)"
PIDS=""
cleanup() {
  for p in $PIDS; do kill "$p" 2>/dev/null || true; done
  sleep 0.4
  for p in $PIDS; do kill -9 "$p" 2>/dev/null || true; done
  mkdir -p "$ART/scratch-copy"
  cp -a "$SCRATCH/." "$ART/scratch-copy/" 2>/dev/null || true
}
trap cleanup EXIT

echo "scratch=$SCRATCH art=$ART" | tee "$ART/meta.txt"
free -h | head -2 | tee -a "$ART/meta.txt"
date -u +'%Y-%m-%dT%H:%M:%SZ' | tee -a "$ART/meta.txt"
uptime | tee -a "$ART/meta.txt"

export PATH="${HOME}/.cargo/bin:/root/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/falcon-snap-sess-cargo}"
BIN="$CARGO_TARGET_DIR/debug/falcon-pl-node"
CTL="$CARGO_TARGET_DIR/debug/falcon-pl-ctl"
if [[ ! -x "$BIN" || ! -x "$CTL" ]]; then
  (cd "$ROOT" && CARGO_BUILD_JOBS=2 nice -n19 ionice -c3 cargo build -p fd-pl --bin falcon-pl-node --bin falcon-pl-ctl)
fi
test -x "$BIN" && test -x "$CTL"
sha256sum "$BIN" | tee "$ART/binary.sha256"

PORT_BASE=29401
N_MESH=6
NET=2200
VALS="v1,v2,v3,v4,v5,v6,v7"
export FPL_EMPTY_BLOCK_FROM_HEIGHT=1
export ART PORT_BASE NET CTL KEYS SCRATCH
export RUST_LOG="${RUST_LOG:-falcon_pl_node=info}"

KEYS="$SCRATCH/keys"
mkdir -p "$KEYS/lc"
"$BIN" --gen-keys-only --keys-dir "$KEYS" --scheme hmac --allow-dev-crypto \
  --validators "$VALS" --id v1 >/dev/null
printf '%s\n' '{"qc_header_version":4,"qc_start_height":1,"network_id":2200,"start_height":1}' >"$KEYS/lc/bridge.json"
export KEYS

start_node() {
  local i="$1"
  local id="v$((i+1))"
  local port=$((PORT_BASE+i))
  local dat="$SCRATCH/$id"
  mkdir -p "$dat"/{run,snapshots,logs}
  local peers="" j p
  local n_total=7
  for j in $(seq 0 $((n_total-1))); do
    if [[ $j -ne $i ]]; then
      p=$((PORT_BASE+j))
      [[ -n "$peers" ]] && peers+=","
      peers+="127.0.0.1:$p"
    fi
  done
  local extra=()
  if [[ "${2:-}" == "archive" ]]; then
    extra+=(--archive --hub)
  fi
  if [[ "${2:-}" == "fresh" ]]; then
    rm -f "$dat/snapshots/snap.json" 2>/dev/null || true
  fi
  nice -n19 ionice -c3 "$BIN" \
    --id "$id" \
    --listen "127.0.0.1:$port" \
    --peers "$peers" \
    --keys-dir "$KEYS" \
    --scheme hmac --allow-dev-crypto \
    --validators "$VALS" \
    --bond 1000 \
    --network-id "$NET" \
    --online-epoch-heights 0 \
    --first-claim-epoch 1 \
    --min-block-interval-ms 0 \
    --admin-sock "$dat/run/admin.sock" \
    --snapshot "$dat/snapshots/snap.json" \
    --snapshot-out "$dat/snapshots/snap.json" \
    --snapshot-interval-ms 5000 \
    --seal-interval-ms 300 \
    --pack-budget-ms 500 \
    --progress-timeout-ms 4000 \
    --max-chain-ledgers 256 \
    --lock-public-wire \
    "${extra[@]}" \
    >"$dat/logs/node.log" 2>&1 &
  PIDS="$PIDS $!"
  echo "started $id :$port pid=$! mode=${2:-mesh}" | tee -a "$ART/meta.txt"
}

start_node 0 archive
for i in $(seq 1 $((N_MESH-1))); do start_node "$i"; done
sleep 5

python3 - <<'PY'
import subprocess, os, time
ctl=os.environ["CTL"]; keys=os.environ["KEYS"]; net=os.environ["NET"]; base=int(os.environ["PORT_BASE"])
for seq in range(1, 24):
    cmd=[ctl,"--addr",f"127.0.0.1:{base}","pay","--from","alice","--to","bob","--amount","1","--fee","1",
         "--sequence",str(seq),"--scheme","hmac","--keys-dir",keys,"--network-id",net]
    try:
        out=subprocess.check_output(cmd,text=True,timeout=20,stderr=subprocess.STDOUT)
        print("pay",seq, out.splitlines()[0] if out else "ok")
    except Exception as e:
        print("pay",seq,"err",e)
    time.sleep(0.1)
PY

python3 - <<'PY'
import json,subprocess,sys,time,os
ctl=os.environ["CTL"]; base=int(os.environ["PORT_BASE"]); n=int(os.environ.get("N_MESH","6")); art=os.environ["ART"]
# N_MESH not exported — hardcode 6
n=6
deadline=time.time()+120
last=None
while time.time()<deadline:
    tips=[]
    hashes=set()
    for i in range(n):
        raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{base+i}","status"],text=True,timeout=25)
        b=json.loads(raw).get("body") or {}
        tip=b.get("tip_height") or 0
        tips.append(tip)
        if tip: hashes.add(b.get("tip_hash") or "")
    last=tips
    print("mesh",tips,"hashes",len(hashes))
    if min(tips)>=2 and len(hashes)==1:
        break
    time.sleep(2)
else:
    open(art+"/MESH_FAIL.json","w").write(json.dumps({"tips":last}))
    raise SystemExit(f"mesh tip timeout tips={last}")
open(art+"/mesh_ok.json","w").write(json.dumps({"tips":last}))
print("MESH_OK")
PY

# Snapshot save + pin size
"$CTL" --admin-sock "$SCRATCH/v1/run/admin.sock" snapshot-save --path "$SCRATCH/v1/snapshots/snap.json"
sleep 0.5
python3 - <<'PY'
import os,json
p=os.environ["SCRATCH"]+"/v1/snapshots/snap.json"
st=os.stat(p)
open(os.environ["ART"]+"/pin_meta.json","w").write(json.dumps({"bytes":st.st_size,"path":p},indent=2))
print("pin_bytes",st.st_size)
PY

MESH_TIP=$(python3 - <<'PY'
import json,subprocess,os
ctl=os.environ["CTL"]; base=int(os.environ["PORT_BASE"])
raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{base}","status"],text=True,timeout=25)
b=json.loads(raw).get("body") or {}
print(json.dumps({"tip":b.get("tip_height"),"hash":b.get("tip_hash")}))
PY
)
echo "$MESH_TIP" | tee "$ART/mesh_tip_before_joiner.json"

start_node 6 fresh
sleep 5

python3 - <<'PY'
import json,subprocess,os,time,re
ctl=os.environ["CTL"]; base=int(os.environ["PORT_BASE"]); art=os.environ["ART"]; scratch=os.environ["SCRATCH"]
want=json.load(open(art+"/mesh_tip_before_joiner.json"))
v7=base+6
deadline=time.time()+180
rows=[]; ok=False
while time.time()<deadline:
    try:
        raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{v7}","status"],text=True,timeout=25)
        b=json.loads(raw).get("body") or {}
        rawp=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{base}","status"],text=True,timeout=25)
        bp=json.loads(rawp).get("body") or {}
        row={"v7_tip":b.get("tip_height"),"peer_tip":bp.get("tip_height"),
             "v7_hash":b.get("tip_hash"),"peer_hash":bp.get("tip_hash")}
        rows.append(row); print("joiner",row)
        if (b.get("tip_height") or 0) >= max(2, want.get("tip") or 0) and b.get("tip_hash")==bp.get("tip_hash"):
            ok=True; break
    except Exception as e:
        rows.append({"err":str(e)})
    time.sleep(2)
log=open(f"{scratch}/v7/logs/node.log",errors="replace").read()
v1=open(f"{scratch}/v1/logs/node.log",errors="replace").read()
ev={
  "adopt": "snapshot adopted" in log,
  "serve_to_v7": bool(re.search(r"snapshot serving tip=.* to=v7", v1)),
  "serve_misattr_mesh": bool(re.search(r"snapshot serving tip=.* to=v[1-6]\b", v1)),
  "control_fail": "snapshot control reply failed" in v1,
  "data_deferred": "inbound catch-up data deferred" in v1 or "catch-up data deferred" in v1,
  "need_snapshot": "NeedSnapshot" in log or "snapshot pull" in log,
}
open(art+"/trace.jsonl","w").write("\n".join(json.dumps(r) for r in rows)+"\n")
open(art+"/evidence.json","w").write(json.dumps({"ok":ok,"want":want,"evidence":ev,"final":rows[-1] if rows else {}},indent=2))
open(art+"/RESULT.txt","w").write("PASS\n" if ok else "FAIL\n")
print("EV",ev,"OK",ok)
import subprocess as sp
ss=sp.check_output(["bash","-lc","ss -lntp | grep 2940 || true"],text=True)
open(art+"/listeners.txt","w").write(ss)
if "0.0.0.0" in ss:
    raise SystemExit("non-loopback")
raise SystemExit(0 if ok else 2)
PY

echo "ISOLATION_OK loopback:29401-29407" | tee -a "$ART/meta.txt"
