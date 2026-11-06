#!/usr/bin/env bash
# Isolated installer-style join-snap (loopback, scratch keys). beartec-brain only.
# Forces NeedSnapshot via tip-0 joiner + archive/hub ads after mesh tip>strip window.
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
  sleep 0.5
  for p in $PIDS; do kill -9 "$p" 2>/dev/null || true; done
  mkdir -p "$ART/scratch-copy"
  cp -a "$SCRATCH/." "$ART/scratch-copy/" 2>/dev/null || true
  # Final port sweep for our range only
  for port in $(seq 29401 29408); do
    pids=$(ss -lntp 2>/dev/null | awk -v p=":$port" '$4 ~ p"$" {print}' | sed -n 's/.*pid=\([0-9]*\).*/\1/p' | sort -u)
    for pid in $pids; do kill -9 "$pid" 2>/dev/null || true; done
  done
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
# Include joiner seats in validator set / keys so Hello validates.
VALS="v1,v2,v3,v4,v5,v6,v7,v8"
LOAD_WALLETS=800
MESH_MCL=16
export FPL_EMPTY_BLOCK_FROM_HEIGHT=1
export ART PORT_BASE NET CTL KEYS SCRATCH LOAD_WALLETS
export RUST_LOG="${RUST_LOG:-falcon_pl_node=info}"

KEYS="$SCRATCH/keys"
mkdir -p "$KEYS/lc"
"$BIN" --gen-keys-only --keys-dir "$KEYS" --scheme hmac --allow-dev-crypto \
  --validators "$VALS" --id v1 >/dev/null
printf '%s\n' '{"qc_header_version":4,"qc_start_height":1,"network_id":2200,"start_height":1}' >"$KEYS/lc/bridge.json"
export KEYS

start_node() {
  local i="$1"
  local mode="${2:-mesh}"
  local id="v$((i+1))"
  local port=$((PORT_BASE+i))
  local dat="$SCRATCH/$id"
  mkdir -p "$dat"/{run,snapshots,logs}
  local peers="" j p
  local n_total=8
  for j in $(seq 0 $((n_total-1))); do
    if [[ $j -ne $i ]]; then
      p=$((PORT_BASE+j))
      [[ -n "$peers" ]] && peers+=","
      peers+="127.0.0.1:$p"
    fi
  done
  local extra=()
  local mcl=$MESH_MCL
  if [[ "$mode" == "archive" ]]; then
    extra+=(--archive --hub)
    mcl=256
  fi
  if [[ "$mode" == "fresh" || "$mode" == "fresh_join" ]]; then
    rm -f "$dat/snapshots/snap.json" 2>/dev/null || true
  fi
  local join_args=()
  if [[ "$mode" == "fresh_join" ]]; then
    join_args+=(--join)
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
    --seal-interval-ms 200 \
    --pack-budget-ms 400 \
    --progress-timeout-ms 4000 \
    --max-chain-ledgers "$mcl" \
    --load-wallets "$LOAD_WALLETS" \
    --lock-public-wire \
    "${extra[@]}" \
    "${join_args[@]}" \
    >"$dat/logs/node.log" 2>&1 &
  PIDS="$PIDS $!"
  echo "started $id :$port pid=$! mode=$mode mcl=$mcl" | tee -a "$ART/meta.txt"
}

start_node 0 archive
for i in $(seq 1 $((N_MESH-1))); do start_node "$i"; done
sleep 6

# Pays to create non-trivial state + empty-block tip growth
python3 - <<'PY'
import subprocess, os, time
ctl=os.environ["CTL"]; keys=os.environ["KEYS"]; net=os.environ["NET"]; base=int(os.environ["PORT_BASE"])
for seq in range(1, 40):
    cmd=[ctl,"--addr",f"127.0.0.1:{base}","pay","--from","alice","--to","bob","--amount","1","--fee","1",
         "--sequence",str(seq),"--scheme","hmac","--keys-dir",keys,"--network-id",net]
    try:
        out=subprocess.check_output(cmd,text=True,timeout=20,stderr=subprocess.STDOUT)
        if seq<=3 or seq%10==0: print("pay",seq, out.splitlines()[0] if out else "ok")
    except Exception as e:
        print("pay",seq,"err",e)
    time.sleep(0.05)
print("pays done")
PY

# Wait tip >= 5 (empty-block growth may stall; snap path forced below)
python3 - <<'PY'
import json,subprocess,os,time
ctl=os.environ["CTL"]; base=int(os.environ["PORT_BASE"]); art=os.environ["ART"]
n=6
deadline=time.time()+180
last=None
while time.time()<deadline:
    tips=[]; hashes=set()
    for i in range(n):
        raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{base+i}","status"],text=True,timeout=25)
        b=json.loads(raw).get("body") or {}
        tip=b.get("tip_height") or 0
        tips.append(tip)
        if tip: hashes.add(b.get("tip_hash") or "")
    last=tips
    print("mesh",tips,"hashes",len(hashes))
    # Majority (>=5/6) at same tip>=5 is enough; one laggard OK (archive still seals).
    from collections import Counter
    c=Counter(tips)
    tip_maj, n_maj = c.most_common(1)[0]
    if tip_maj>=5 and n_maj>=5 and len(hashes)==1:
        break
    time.sleep(2)
else:
    open(art+"/MESH_FAIL.json","w").write(json.dumps({"tips":last}))
    raise SystemExit(f"mesh tip timeout tips={last}")
open(art+"/mesh_ok.json","w").write(json.dumps({"tips":last}))
print("MESH_OK")
PY

"$CTL" --admin-sock "$SCRATCH/v1/run/admin.sock" snapshot-save --path "$SCRATCH/v1/snapshots/snap.json"
sleep 0.8
# Also force join.serve pin if present
ls -la "$SCRATCH/v1/snapshots/" | tee "$ART/v1_snap_dir.txt"
python3 - <<'PY'
import os,json,glob
art=os.environ["ART"]; scratch=os.environ["SCRATCH"]
paths=glob.glob(scratch+"/v1/snapshots/snap*.json")
meta=[]
for p in paths:
    try: st=os.stat(p); meta.append({"path":p,"bytes":st.st_size})
    except: pass
meta.sort(key=lambda x: -x["bytes"])
open(art+"/pin_meta.json","w").write(json.dumps({"files":meta,"largest":meta[0] if meta else None},indent=2))
print("pins",meta)
if not meta or meta[0]["bytes"] < 100_000:
    print("WARN pin <100KB — piece expansion may be weak")
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

# Pause non-archive mesh so joiners cannot live-gossip/NeedLedgers past tip-0;
# archive+hub remains the only snap source (forces NeedSnapshot attribution path).
echo "pausing mesh v2-v6 (keep archive v1)" | tee -a "$ART/meta.txt"
for id in v2 v3 v4 v5 v6; do
  if [[ -f "$SCRATCH/$id/run/admin.sock" ]]; then
    "$CTL" --admin-sock "$SCRATCH/$id/run/admin.sock" stop 2>/dev/null || true
  fi
done
sleep 1
# Kill by recorded pids for v2-v6 if still up (stop may be unimplemented)
python3 - <<'PY2'
import os,signal,time
scratch=os.environ["SCRATCH"]
# parse meta for pids
meta=open(os.environ["ART"]+"/meta.txt").read().splitlines()
keep=set()
for line in meta:
    if "mode=archive" in line and "pid=" in line:
        keep.add(int(line.split("pid=")[1].split()[0]))
# kill listeners 29402-29406
import subprocess
ss=subprocess.check_output(["bash","-lc","ss -lntp"],text=True)
for line in ss.splitlines():
    for port in range(29402,29407):
        if f":{port}" in line and "falcon-pl-node" in line:
            import re
            m=re.search(r"pid=(\d+)", line)
            if m:
                pid=int(m.group(1))
                if pid not in keep:
                    try: os.kill(pid, signal.SIGTERM)
                    except: pass
time.sleep(0.5)
for line in ss.splitlines():
    for port in range(29402,29407):
        if f":{port}" in line and "falcon-pl-node" in line:
            m=re.search(r"pid=(\d+)", line)
            if m:
                pid=int(m.group(1))
                if pid not in keep:
                    try: os.kill(pid, signal.SIGKILL)
                    except: pass
print("mesh_paused")
PY2
# Refresh PIDS to archive-only + later joiners (cleanup still kills leftovers via port sweep)
PIDS="$(pgrep -f 'falcon-pl-node.*29401' || true)"

# Two concurrent joiners for identity isolation
start_node 6 fresh_join
start_node 7 fresh_join
sleep 4

python3 - <<'PY'
import json,subprocess,os,time,re
ctl=os.environ["CTL"]; base=int(os.environ["PORT_BASE"]); art=os.environ["ART"]; scratch=os.environ["SCRATCH"]
want=json.load(open(art+"/mesh_tip_before_joiner.json"))
deadline=time.time()+240
rows=[]; ok_v7=False; ok_v8=False
while time.time()<deadline:
    try:
        rawp=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{base}","status"],text=True,timeout=25)
        bp=json.loads(rawp).get("body") or {}
        row={"peer_tip":bp.get("tip_height"),"peer_hash":bp.get("tip_hash")}
        for name,off in [("v7",6),("v8",7)]:
            raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{base+off}","status"],text=True,timeout=25)
            b=json.loads(raw).get("body") or {}
            row[f"{name}_tip"]=b.get("tip_height"); row[f"{name}_hash"]=b.get("tip_hash")
        rows.append(row); print("joiners",row)
        want_tip=max(2, want.get("tip") or 0)
        if (row.get("v7_tip") or 0)>=want_tip and row.get("v7_hash")==row.get("peer_hash"): ok_v7=True
        if (row.get("v8_tip") or 0)>=want_tip and row.get("v8_hash")==row.get("peer_hash"): ok_v8=True
        if ok_v7 and ok_v8: break
    except Exception as e:
        rows.append({"err":str(e)})
    time.sleep(2)

def slog(id_):
    return open(f"{scratch}/{id_}/logs/node.log",errors="replace").read()
v1=slog("v1"); v7=slog("v7"); v8=slog("v8")
ev={
  "adopt_v7": "snapshot adopted" in v7,
  "adopt_v8": "snapshot adopted" in v8,
  "need_snapshot_v7": "NeedSnapshot" in v7 or "snapshot pull" in v7 or "need snapshot" in v7.lower(),
  "need_snapshot_v8": "NeedSnapshot" in v8 or "snapshot pull" in v8 or "need snapshot" in v8.lower(),
  "serve_to_v7": bool(re.search(r"snapshot serving tip=.* to=v7", v1)),
  "serve_to_v8": bool(re.search(r"snapshot serving tip=.* to=v8", v1)),
  "serve_misattr_mesh": bool(re.search(r"snapshot serving tip=.* to=v[1-6]\b", v1)),
  "control_fail": "snapshot control reply failed" in v1,
  "data_deferred": "inbound catch-up data deferred" in v1 or "catch-up data deferred" in v1,
  "offer_retry": "snapshot control" in v1 and ("retry" in v1 or "Offer" in v1),
}
# Count serve attributions
ev["serve_v7_count"]=len(re.findall(r"snapshot serving tip=.* to=v7", v1))
ev["serve_v8_count"]=len(re.findall(r"snapshot serving tip=.* to=v8", v1))

ok = ok_v7 and ok_v8 and (ev["serve_to_v7"] or ev["adopt_v7"] or ev["need_snapshot_v7"])
# Isolation: if both served, both to= distinct; never mesh misattr
iso_ok = (not ev["serve_misattr_mesh"]) and (not (ev["serve_to_v7"] and ev["serve_to_v8"]) or (ev["serve_v7_count"]>0 and ev["serve_v8_count"]>0))

open(art+"/trace.jsonl","w").write("\n".join(json.dumps(r) for r in rows)+"\n")
open(art+"/evidence.json","w").write(json.dumps({
  "ok": ok, "iso_ok": iso_ok, "ok_v7": ok_v7, "ok_v8": ok_v8,
  "want": want, "evidence": ev, "final": rows[-1] if rows else {}
},indent=2))

# Loopback isolation: listen address must be 127.0.0.1 for our ports (ignore ss peer wildcard 0.0.0.0:*)
ss=subprocess.check_output(["bash","-lc","ss -lntp | grep -E ':(2940[1-8])\\b' || true"],text=True)
open(art+"/listeners.txt","w").write(ss)
bad=False
for line in ss.splitlines():
    # local address is field 4 typically: 127.0.0.1:29401
    parts=line.split()
    if len(parts)>=4 and not parts[3].startswith("127.0.0.1:"):
        bad=True
open(art+"/RESULT.txt","w").write("PASS\n" if (ok and iso_ok and not bad) else "FAIL\n")
print("EV",ev,"OK",ok,"ISO",iso_ok,"BAD_LISTEN",bad)
if bad: raise SystemExit("non-loopback listen")
raise SystemExit(0 if (ok and iso_ok) else 2)
PY

echo "ISOLATION_OK loopback:29401-29408" | tee -a "$ART/meta.txt"
free -h | head -2 | tee "$ART/mem_after.txt"
