#!/usr/bin/env bash
# Process-level: fresh tip-0 NeedLedgers catch-up (non-archive RAM window vs archive bodies).
# Distinct from same-seat snapshot restart. Loopback only. Private net 2200.
# Usage (beartec-brain, low priority):
#   nice -n19 ionice -c3 bash docs/ops/tools/utc_fresh_catchup_rehearsal.sh \
#     /path/to/falcon-pl-rs [/path/to/artifact-dir]
set -euo pipefail
ROOT="${1:?falcon-pl-rs root}"
ART="${2:-/tmp/falcon-utc-fresh-artifacts}"
mkdir -p "$ART"
SCRATCH="$(mktemp -d /tmp/falcon-utc-fc-XXXXXX)"
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

PORT_BASE=29301
N_MESH=6
NET=2200
H_UTC=2
VALS="v1,v2,v3,v4,v5,v6,v7"
export FPL_UTC_FROM_HEIGHT="$H_UTC"
export FPL_UTC_HELLO_ENFORCE_FROM_HEIGHT="$H_UTC"
export FPL_EMPTY_BLOCK_FROM_HEIGHT=1
export ART PORT_BASE NET H_UTC CTL KEYS SCRATCH
export RUST_LOG="${RUST_LOG:-falcon_pl_node=debug}"

KEYS="$SCRATCH/keys"
mkdir -p "$KEYS/lc"
"$BIN" --gen-keys-only --keys-dir "$KEYS" --scheme hmac --allow-dev-crypto \
  --validators "$VALS" --id v1 >/dev/null
printf '%s\n' '{"qc_header_version":4,"qc_start_height":1,"network_id":2200,"start_height":1}' >"$KEYS/lc/bridge.json"

# Document modes in art before run
cat > "$ART/BODY_AVAILABILITY.md" <<'MD'
# Body availability for tip-0 catch-up (honest)

## Non-archive RAM window (this rehearsal Mode A)
- Seats run **without** `--archive`. Bodies live in RAM until `max_body_ledgers` (default 64) strips them.
- `NeedLedgers from=1` succeeds **only while** every height in `[1..tip]` still has a full body on at least one peer (`ledgers_from` stops at the first hole).
- Tip-0 joiners prefer `NeedSnapshot` when archive/hub snap sources advertise; with **no** archive/hub flags, `snap_sources_available()==false` so residual NeedLedgers is the path.
- This rehearsal keeps tip well below 64 so bodies exist. We do **not** claim bodies exist after strip / on long chains without an archive.

## Archive body store (Mode B, optional)
- `--archive` persists bodies under the snapshot parent `bodies/` so tip-0 can pull history past the RAM window.
- Production beartec-brain falcon-node (read-only check): `--role val`, **no** `--archive`, **no** `bodies/` dir; join snaps (~118MB) exist under `data/` — that is **not** a full body archive.

## What we never claim
- That missing bodies exist.
- That `latest`/`RECENT_FILLS=200` equals archive history.
- That join.serve.json is a substitute for NeedLedgers body continuity.
MD

start_node() {
  local i="$1"
  local id="v$((i+1))"
  local port=$((PORT_BASE+i))
  local dat="$SCRATCH/$id"
  mkdir -p "$dat"/{run,snapshots,logs}
  local peers="" j p
  local n_total=$((N_MESH)) # mesh only; v7 added later
  if [[ "${3:-}" == "with_v7" ]]; then n_total=7; fi
  for j in $(seq 0 $((n_total-1))); do
    if [[ $j -ne $i ]]; then
      p=$((PORT_BASE+j))
      [[ -n "$peers" ]] && peers+=","
      peers+="127.0.0.1:$p"
    fi
  done
  local extra=()
  if [[ "${2:-}" == "archive" ]]; then
    extra+=(--archive)
  fi
  if [[ "${2:-}" == "hub" || "${2:-}" == "archive" ]]; then
    extra+=(--hub)
  fi
  if [[ "${2:-}" == "fresh" || "${2:-}" == "fresh_join" ]]; then
    rm -f "$dat/snapshots/snap.json" "$dat/snapshots/snap.ring.json" 2>/dev/null || true
  fi
  local join_args=()
  if [[ "${2:-}" == "fresh_join" ]]; then
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
    --seal-interval-ms 300 \
    --pack-budget-ms 500 \
    --progress-timeout-ms 4000 \
    --max-chain-ledgers 128 \
    --lock-public-wire \
    "${extra[@]}" \
    "${join_args[@]}" \
    >"$dat/logs/node.log" 2>&1 &
  local pid=$!
  PIDS="$PIDS $pid"
  NODE_PID[$id]=$pid
  echo "started $id :$port pid=$pid mode=${2:-mesh}" | tee -a "$ART/meta.txt"
}

# --- Mode A mesh (non-archive) ---
start_node 0 hub
for i in $(seq 1 $((N_MESH-1))); do start_node "$i"; done
sleep 4

python3 - <<'PY'
import subprocess, os, time
ctl=os.environ["CTL"]; keys=os.environ["KEYS"]; net=os.environ["NET"]; base=int(os.environ["PORT_BASE"])
for seq in range(1, 12):
    cmd=[ctl,"--addr",f"127.0.0.1:{base}","pay","--from","alice","--to","bob","--amount","1","--fee","1",
         "--sequence",str(seq),"--scheme","hmac","--keys-dir",keys,"--network-id",net]
    try:
        out=subprocess.check_output(cmd,text=True,timeout=20,stderr=subprocess.STDOUT)
        print("pay",seq, out.splitlines()[0] if out else "ok")
    except Exception as e:
        print("pay",seq,"err",e)
    time.sleep(0.12)
PY

python3 - "$CTL" "$PORT_BASE" "$N_MESH" "$H_UTC" "$ART" <<'PY'
import json,subprocess,sys,time
ctl,base,n,h,art=sys.argv[1],int(sys.argv[2]),int(sys.argv[3]),int(sys.argv[4]),sys.argv[5]
deadline=time.time()+120
last=None
while time.time()<deadline:
    rows=[]; tips=[]; hashes=set(); utcs=set()
    for i in range(n):
        raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{base+i}","status"],text=True,timeout=25)
        b=json.loads(raw).get("body") or {}
        tip=b.get("tip_height") or 0
        tips.append(tip)
        if tip: hashes.add(b.get("tip_hash") or "")
        if b.get("tip_sealed_utc_ms"): utcs.add(b.get("tip_sealed_utc_ms"))
        rows.append({"port":base+i,"tip":tip,"hash":b.get("tip_hash"),"utc":b.get("tip_sealed_utc_ms"),"q":b.get("tip_sealed_utc_quality")})
    last=(rows,tips,hashes,utcs)
    print("mesh",tips,"hashes",len(hashes),"utcs",len(utcs))
    if min(tips)>=h and len(hashes)==1 and len(utcs)==1:
        break
    time.sleep(2)
rows,tips,hashes,utcs=last
open(art+"/mesh_status.json","w").write(json.dumps({"rows":rows,"tips":tips,"ok":min(tips)>=h and len(hashes)==1},indent=2))
assert min(tips)>=h and len(hashes)==1 and len(utcs)==1, (tips,hashes,utcs)
# Prove peers still hold contiguous bodies from h=1 via tip (short tip < max_body)
open(art+"/mode_a_assumption.json","w").write(json.dumps({
  "mode":"non_archive_ram_window",
  "tip":min(tips),
  "max_body_ledgers_default":64,
  "bodies_expected": True,
  "note":"NeedLedgers from=1 valid only while tip < strip window and bodies present"
},indent=2))
print("MESH_OK tip",min(tips))
PY

# Prove peer RAM window still has full bodies at h=1 (honest precondition)
python3 - "$SCRATCH/v1/snapshots/snap.json" "$ART" <<'PY'
import json,sys,os,time,subprocess
path,art=sys.argv[1],sys.argv[2]
ctl=os.environ["CTL"]
admin=os.path.join(os.environ["SCRATCH"],"v1/run/admin.sock")
subprocess.check_output([ctl,"--admin-sock",admin,"snapshot-save","--path",path],text=True,timeout=30)
time.sleep(0.5)
d=json.load(open(path))
chain=d.get("chain") or []
info={"tip":d.get("tip_height"),"chain_len":len(chain),"heights":[]}
for led in chain:
    txs=led.get("txs")
    ids=led.get("tx_ids") or []
    n_txs=len(txs) if isinstance(txs,list) else None
    info["heights"].append({"h":led.get("height"),"tx_ids":len(ids),"txs":n_txs,"has_body": n_txs==len(ids) if n_txs is not None else False, "utc": led.get("sealed_utc_ms")})
open(art+"/peer_body_inventory.json","w").write(json.dumps(info,indent=2))
print("body_inventory",info)
missing=[x for x in info["heights"] if not x["has_body"]]
if missing:
    open(art+"/BODY_HOLE.json","w").write(json.dumps(missing,indent=2))
    raise SystemExit(f"peer missing bodies (cannot claim NeedLedgers will work): {missing}")
assert info["tip"]>=2
PY


# Capture mesh tip before launching fresh joiner
MESH_TIP_JSON=$(python3 - "$CTL" "$PORT_BASE" <<'PY'
import json,subprocess,sys
ctl,base=sys.argv[1],int(sys.argv[2])
raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{base}","status"],text=True,timeout=25)
b=json.loads(raw).get("body") or {}
print(json.dumps({"tip":b.get("tip_height"),"hash":b.get("tip_hash"),"utc":b.get("tip_sealed_utc_ms"),"q":b.get("tip_sealed_utc_quality")}))
PY
)
echo "$MESH_TIP_JSON" | tee "$ART/mesh_tip_before_joiner.json"

# Fresh v7: no snap file, tip0, joins mesh (validators include v7; not pre-registered as bonded if --join)
# Use fresh without --join first so v7 is bonded genesis (simpler QC); prove NeedLedgers catch-up.
# Restart mesh peer lists to include v7: kill none — start v7 with peers to all 6; existing seats also need v7 peer.
# Easier: start v7 listening; existing seats already have static peers without v7 — outbound dial from v7 to them is enough for catch-up.
start_node 6 fresh
sleep 2

python3 - "$CTL" "$PORT_BASE" "$ART" <<'PY'
import json,subprocess,sys,time,os
ctl,base,art=sys.argv[1],int(sys.argv[2]),sys.argv[3]
v7=base+6
want=json.load(open(art+"/mesh_tip_before_joiner.json"))
deadline=time.time()+90
rows=[]
ok=False
body_path=None
while time.time()<deadline:
    try:
        raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{v7}","status"],text=True,timeout=25)
        b=json.loads(raw).get("body") or {}
        rawp=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{base}","status"],text=True,timeout=25)
        bp=json.loads(rawp).get("body") or {}
        row={"v7_tip":b.get("tip_height"),"v7_hash":b.get("tip_hash"),"v7_utc":b.get("tip_sealed_utc_ms"),"v7_q":b.get("tip_sealed_utc_quality"),
             "peer_tip":bp.get("tip_height"),"peer_hash":bp.get("tip_hash"),"peer_utc":bp.get("tip_sealed_utc_ms")}
        rows.append(row)
        print("fresh_catchup",row)
        # Success: reached mesh tip (or advanced to >= H_utc) with matching hash/utc
        if (b.get("tip_height") or 0) >= max(2, want.get("tip") or 0) and b.get("tip_hash")==bp.get("tip_hash") and b.get("tip_sealed_utc_ms")==bp.get("tip_sealed_utc_ms"):
            ok=True
            break
    except Exception as e:
        rows.append({"error":str(e)})
        print("fresh_catchup err",e)
    time.sleep(2)
# Evidence from logs: NeedLedgers / catch-up batch / no pretend archive
log=os.path.join(os.environ["SCRATCH"],"v7/logs/node.log")
text=open(log,errors="replace").read() if os.path.exists(log) else ""
evidence={
  "need_ledgers_pull": "catch-up pull from_height=" in text or "NeedLedgers" in text,
  "catchup_batch_advanced": "catch-up batch" in text and "→" in text,
  "need_snapshot": "NeedSnapshot" in text or "loading snapshot" in text,
  "archive_body_store_line": "archive body store" in text,
}
open(art+"/fresh_catchup_trace.jsonl","w").write("\n".join(json.dumps(r) for r in rows)+"\n")
open(art+"/fresh_catchup_result.json","w").write(json.dumps({"ok":ok,"want":want,"final":rows[-1] if rows else {},"log_evidence":evidence},indent=2))
open(art+"/fresh_catchup_ok","w").write("ok\n" if ok else "fail\n")
# Classify path honestly
path="unknown"
if ok and evidence["catchup_batch_advanced"] and not evidence["archive_body_store_line"]:
    path="need_ledgers_ram_window"
elif ok and evidence.get("need_snapshot"):
    path="join_snapshot"
open(art+"/catchup_path.json","w").write(json.dumps({"path":path,"evidence":evidence},indent=2))
print("PATH",path,"OK",ok)
sys.exit(0 if ok else 2)
PY

# Isolation
(ss -lntp 2>/dev/null || true) | grep -E ':2930[1-7]\b' | tee "$ART/listeners.txt" || true
if grep -E '0\.0\.0\.0:2930|\*:2930' "$ART/listeners.txt" >/dev/null 2>&1; then
  echo FAIL_NON_LOOPBACK | tee -a "$ART/meta.txt"; exit 3
fi
echo "ISOLATION_OK loopback:29301-29307 network_id=2200" | tee -a "$ART/meta.txt"
echo PASS | tee "$ART/RESULT.txt"
