#!/usr/bin/env bash
# Process-level UTC restart/catch-up + Hello peer-compat + skew unit evidence binder.
# Distinct from utc_multinode_rehearsal.sh tip-agreement smoke.
# Loopback only. Private net 2200. Does NOT touch production.
#
# Usage (beartec-brain, low priority):
#   nice -n19 ionice -c3 bash docs/ops/tools/utc_restart_compat_rehearsal.sh \
#     /path/to/falcon-pl-rs [/path/to/artifact-dir]
set -euo pipefail
ROOT="${1:?falcon-pl-rs root}"
ART="${2:-/tmp/falcon-utc-restart-artifacts}"
mkdir -p "$ART"
SCRATCH="$(mktemp -d /tmp/falcon-utc-rr-XXXXXX)"
PIDS=""
declare -A NODE_PID
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

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/falcon-cargo-target}"
BIN="$CARGO_TARGET_DIR/debug/falcon-pl-node"
CTL="$CARGO_TARGET_DIR/debug/falcon-pl-ctl"
if [[ ! -x "$BIN" || ! -x "$CTL" ]]; then
  (cd "$ROOT" && CARGO_BUILD_JOBS=2 nice -n19 ionice -c3 cargo build -p fd-pl --bin falcon-pl-node --bin falcon-pl-ctl)
fi
test -x "$BIN" && test -x "$CTL"
sha256sum "$BIN" | tee "$ART/binary.sha256"

PORT_BASE=29201
N=6
NET=2200
H_UTC=2
VALS="v1,v2,v3,v4,v5,v6"
export FPL_UTC_FROM_HEIGHT="$H_UTC"
export FPL_UTC_HELLO_ENFORCE_FROM_HEIGHT="$H_UTC"
export FPL_EMPTY_BLOCK_FROM_HEIGHT=1
export ART PORT_BASE NET H_UTC CTL

KEYS="$SCRATCH/keys"
mkdir -p "$KEYS/lc"
"$BIN" --gen-keys-only --keys-dir "$KEYS" --scheme hmac --allow-dev-crypto \
  --validators "$VALS" --id v1 >/dev/null
printf '%s\n' '{"qc_header_version":4,"qc_start_height":1,"network_id":2200,"start_height":1}' >"$KEYS/lc/bridge.json"

start_node() {
  local i="$1"
  local id="v$((i+1))"
  local port=$((PORT_BASE+i))
  local dat="$SCRATCH/$id"
  mkdir -p "$dat"/{run,snapshots,logs}
  local peers="" j p
  for j in $(seq 0 $((N-1))); do
    if [[ $j -ne $i ]]; then
      p=$((PORT_BASE+j))
      [[ -n "$peers" ]] && peers+=","
      peers+="127.0.0.1:$p"
    fi
  done
  # Optional extra args (e.g. force empty snap for catch-up)
  local snap_args=(--snapshot "$dat/snapshots/snap.json" --snapshot-out "$dat/snapshots/snap.json")
  if [[ "${2:-}" == "fresh" ]]; then
    rm -f "$dat/snapshots/snap.json" "$dat/snapshots/snap.ring.json" 2>/dev/null || true
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
    "${snap_args[@]}" \
    --seal-interval-ms 300 \
    --pack-budget-ms 500 \
    --progress-timeout-ms 4000 \
    --max-chain-ledgers 128 \
    --snapshot-interval-ms 5000 \
    --lock-public-wire \
    >"$dat/logs/node.log" 2>&1 &
  local pid=$!
  PIDS="$PIDS $pid"
  NODE_PID[$id]=$pid
  echo "started $id :$port pid=$pid ${2:-}" | tee -a "$ART/meta.txt"
}

for i in $(seq 0 $((N-1))); do start_node "$i"; done
sleep 4

# Drive tip
export KEYS
python3 - <<'PY'
import subprocess, os, time
ctl=os.environ["CTL"]; keys=os.environ["KEYS"]; net=os.environ["NET"]; base=int(os.environ["PORT_BASE"])
for seq in range(1, 10):
    cmd=[ctl,"--addr",f"127.0.0.1:{base}","pay","--from","alice","--to","bob","--amount","1","--fee","1",
         "--sequence",str(seq),"--scheme","hmac","--keys-dir",keys,"--network-id",net]
    try:
        out=subprocess.check_output(cmd,text=True,timeout=20,stderr=subprocess.STDOUT)
        print("pay",seq, out.splitlines()[0] if out else "ok")
    except Exception as e:
        print("pay",seq,"err",e)
    time.sleep(0.12)
PY

wait_tip() {
  python3 - "$CTL" "$PORT_BASE" "$N" "$H_UTC" "$ART" "$1" <<'PY'
import json,subprocess,sys,time
ctl,base,n,h_utc,art,tag = sys.argv[1],int(sys.argv[2]),int(sys.argv[3]),int(sys.argv[4]),sys.argv[5],sys.argv[6]

def poll():
    rows=[]; tips=[]; hashes=set(); utcs=set(); ok=True
    for i in range(n):
        port=base+i
        try:
            raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{port}","status"],text=True,timeout=25)
            msg=json.loads(raw); b=msg.get("body") if isinstance(msg,dict) else {}
            if not isinstance(b,dict): b={}
            tip=b.get("tip_height") or 0
            row={"port":port,"tip":tip,"hash":b.get("tip_hash") or "","utc":b.get("tip_sealed_utc_ms"),
                 "q":b.get("tip_sealed_utc_quality"),"phys":b.get("tip_sealed_at_quality")}
            tips.append(tip)
            if tip: hashes.add(row["hash"]); 
            if b.get("tip_sealed_utc_ms"): utcs.add(b.get("tip_sealed_utc_ms"))
            if b.get("utc_from_height")!=h_utc: ok=False; row["fail"]="utc_from"
            rows.append(row)
        except Exception as e:
            ok=False; tips.append(-1); rows.append({"port":port,"error":str(e)})
    return rows,ok,tips,hashes,utcs

deadline=time.time()+120
last=None
while True:
    rows,ok,tips,hashes,utcs=poll(); last=(rows,ok,tips,hashes,utcs)
    print(f"{tag} tips={tips} hashes={len(hashes)} utcs={len(utcs)} ok={ok}")
    if ok and min(tips)>=h_utc and len(hashes)==1 and len(utcs)==1:
        break
    if time.time()>=deadline: break
    time.sleep(2)
rows,ok,tips,hashes,utcs=last
open(art+f"/status_{tag}.jsonl","w").write("\n".join(json.dumps(r) for r in rows)+"\n")
open(art+f"/agree_{tag}.json","w").write(json.dumps({"tips":tips,"hashes":list(hashes),"utcs":list(utcs),"ok":ok and min(tips)>=h_utc and len(hashes)==1 and len(utcs)==1},indent=2))
sys.exit(0 if (ok and min(tips)>=h_utc and len(hashes)==1 and len(utcs)==1) else 2)
PY
}

wait_tip baseline

# --- PROCESS: kill v6, keep its own snapshot, restart same seat (UTC tip replay) ---
V6_PORT=$((PORT_BASE+5))
V6_PID=${NODE_PID[v6]}
V6_ADMIN="$SCRATCH/v6/run/admin.sock"
V6_SNAP="$SCRATCH/v6/snapshots/snap.json"
echo "forcing v6 SnapshotSave before kill pid=$V6_PID" | tee -a "$ART/meta.txt"
# Capture live tips before save/kill
python3 - "$CTL" "$PORT_BASE" "$V6_PORT" "$ART" <<'PY'
import json,subprocess,sys
ctl,base,v6,art=sys.argv[1],int(sys.argv[2]),int(sys.argv[3]),sys.argv[4]
def st(port):
    raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{port}","status"],text=True,timeout=25)
    b=json.loads(raw).get("body",{})
    return {"tip":b.get("tip_height"),"hash":b.get("tip_hash"),"utc":b.get("tip_sealed_utc_ms"),
            "q":b.get("tip_sealed_utc_quality"),"phys":b.get("tip_sealed_at_quality")}
out={"peer":st(base),"v6":st(v6)}
open(art+"/peer_tip_before_restart.json","w").write(json.dumps({**out["v6"], "peer": out["peer"], "v6": out["v6"]}, indent=2))
print(json.dumps(out))
assert (out["v6"]["tip"] or 0) >= 2, out
PY
# Operator SnapshotSave so snap.json carries tip/hash/utc (interval alone may still be tip-0)
"$CTL" --admin-sock "$V6_ADMIN" snapshot-save --path "$V6_SNAP" | tee -a "$ART/meta.txt"
# Poll snap tip (also covers interval write)
python3 - "$V6_SNAP" "$ART" "$H_UTC" <<'PY'
import json,sys,time,os
path,art,h=sys.argv[1],sys.argv[2],int(sys.argv[3])
deadline=time.time()+30
last=None
while time.time()<deadline:
    if os.path.exists(path):
        d=json.load(open(path))
        info={"snap_tip":d.get("tip_height"),"snap_hash":d.get("tip_hash"),"snap_id":d.get("id"),
              "chain":len(d.get("chain") or []),
              "last_utc": (d.get("chain") or [{}])[-1].get("sealed_utc_ms") if d.get("chain") else None}
        last=info
        print("snap_poll", info)
        if (info["snap_tip"] or 0) >= h and info.get("last_utc"):
            open(art+"/v6_snap_before_restart.json","w").write(json.dumps(info,indent=2))
            break
    time.sleep(1)
else:
    open(art+"/v6_snap_before_restart.json","w").write(json.dumps(last or {},indent=2))
    raise SystemExit(f"snap tip < H_utc before kill: {last}")
PY
echo "killing v6 pid=$V6_PID for same-seat snapshot restart" | tee -a "$ART/meta.txt"
kill "$V6_PID" 2>/dev/null || true
sleep 1
kill -9 "$V6_PID" 2>/dev/null || true
NEW_PIDS=""
for p in $PIDS; do [[ "$p" == "$V6_PID" ]] || NEW_PIDS="$NEW_PIDS $p"; done
PIDS="$NEW_PIDS"
rm -rf "$SCRATCH/v6/run"
mkdir -p "$SCRATCH/v6/run"
# Re-verify snap still tip>=H after kill (no overwrite race)
python3 - "$V6_SNAP" "$ART" <<'PY'
import json,sys
p,art=sys.argv[1],sys.argv[2]
d=json.load(open(p))
info={"snap_tip":d.get("tip_height"),"snap_hash":d.get("tip_hash"),"snap_id":d.get("id"),
      "chain":len(d.get("chain") or []),
      "last_utc": (d.get("chain") or [{}])[-1].get("sealed_utc_ms") if d.get("chain") else None}
print("snap_at_kill", json.dumps(info))
open(art+"/v6_snap_at_kill.json","w").write(json.dumps(info,indent=2))
assert (d.get("tip_height") or 0) >= 2 and d.get("id")=="v6", info
PY
start_node 5
sleep 3


python3 - "$CTL" "$PORT_BASE" "$V6_PORT" "$ART" <<'PY'
import json, subprocess, sys, time
ctl, base, v6port, art = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
blob = json.load(open(art + "/peer_tip_before_restart.json"))
want = blob.get("v6") or blob
snap = json.load(open(art + "/v6_snap_before_restart.json"))
deadline = time.time() + 60
rows = []
ok = False
while time.time() < deadline:
    try:
        raw6 = subprocess.check_output([ctl, "--addr", f"127.0.0.1:{v6port}", "status"], text=True, timeout=25)
        b6 = json.loads(raw6).get("body", {})
        rawp = subprocess.check_output([ctl, "--addr", f"127.0.0.1:{base}", "status"], text=True, timeout=25)
        bp = json.loads(rawp).get("body", {})
        row = {
            "v6_tip": b6.get("tip_height"),
            "v6_hash": b6.get("tip_hash"),
            "v6_utc": b6.get("tip_sealed_utc_ms"),
            "v6_q": b6.get("tip_sealed_utc_quality"),
            "peer_tip": bp.get("tip_height"),
            "peer_hash": bp.get("tip_hash"),
            "peer_utc": bp.get("tip_sealed_utc_ms"),
            "want_tip": want.get("tip"),
            "snap_tip": snap.get("snap_tip"),
        }
        rows.append(row)
        print("restart_replay", row)
        # Same-seat snapshot restart must restore tip>=H_utc with sealed_utc_ms
        if (b6.get("tip_height") or 0) >= max(2, snap.get("snap_tip") or 0) and b6.get("tip_sealed_utc_ms"):
            # Prefer exact snap hash match; else live-agree with peer
            if b6.get("tip_hash") == snap.get("snap_hash") or (
                b6.get("tip_hash") == bp.get("tip_hash") and b6.get("tip_sealed_utc_ms") == bp.get("tip_sealed_utc_ms")
            ):
                ok = True
                break
    except Exception as e:
        rows.append({"error": str(e)})
        print("restart_replay err", e)
    time.sleep(2)
open(art + "/catchup_trace.jsonl", "w").write("\n".join(json.dumps(r) for r in rows) + "\n")
open(art + "/catchup_ok", "w").write("ok\n" if ok else "fail\n")
open(art + "/catchup_result.json", "w").write(json.dumps({"ok": ok, "want": want, "snap": snap, "final": rows[-1] if rows else {}}, indent=2))
sys.exit(0 if ok else 2)
PY

export SCRATCH_V1_LOG="$SCRATCH/v1/logs/node.log"

# Keep chain advancing a bit so restarted node stays live
python3 - <<'PY'
import subprocess,os,time
ctl=os.environ["CTL"]; keys=os.environ["KEYS"]; net=os.environ["NET"]; base=int(os.environ["PORT_BASE"])
for seq in range(20, 26):
    cmd=[ctl,"--addr",f"127.0.0.1:{base}","pay","--from","alice","--to","bob","--amount","1","--fee","1",
         "--sequence",str(seq),"--scheme","hmac","--keys-dir",keys,"--network-id",net]
    try: subprocess.check_output(cmd,text=True,timeout=20,stderr=subprocess.STDOUT)
    except Exception: pass
    time.sleep(0.1)
PY
sleep 2

# --- PROCESS: Hello peer-compat / rejection (sidecar) ---
python3 "$(dirname "$0")/utc_hello_compat_probe.py"

# Isolation proof
(ss -lntp 2>/dev/null || true) | grep -E ':2920[1-6]\b' | tee "$ART/listeners.txt" || true
if grep -E '0\.0\.0\.0:2920|\*:2920' "$ART/listeners.txt" >/dev/null 2>&1; then
  echo FAIL_NON_LOOPBACK | tee -a "$ART/meta.txt"; exit 3
fi
echo "ISOLATION_OK loopback:29201-29206 network_id=2200 scratch=$SCRATCH" | tee -a "$ART/meta.txt"

# Binder note: skew/malformed covered by unit filter (asserted separately in CI/brain cargo test)
cat > "$ART/UNIT_VS_PROCESS.md" <<'MD'
# Unit vs process evidence (t286u)

## Process-level (this script)
- 6-seat loopback tip agreement past H_utc with utc_committed
- Kill+snapshot-restore+restart one seat; assert tip hash + sealed_utc_ms replay, then live follow
- Hello: advertise utc-header-v5; old peer without caps → Close Err containing utc-header-v5
- Hello: capable peer with matching network_id does not get utc capability Close
- Hello: wrong network_id → Close (mixed-version / net reject)
- Isolation: 127.0.0.1 binds only

## Unit / in-process (cargo test -p fd-pl --lib utc_)
- Malformed UTC (nonzero pre-H, missing post-H)
- Future (ahead-of-wall) and backward (non-monotonic) live rejection
- Historical replay accepts stamps outside live skew window
- Snapshot save/load preserves sealed_utc_ms + hash
- Soft gap vs hard Hello enforce; 2300 env ignore
MD

echo PASS | tee "$ART/RESULT.txt"
