#!/usr/bin/env bash
# Isolated multi-PROCESS UTC Hello/status rehearsal on loopback.
# NOT a production soak. Does not touch live seats/ports/keys/Tailscale.
#
# Requires QC v4 (UTC binds on v5/v4 pack path) ⇒ 6 bonded seats (v4 committee).
# Private net 2200 only; FPL_* env ignored on 2300.
#
# Usage (on beartec-brain, low priority):
#   nice -n19 ionice -c3 bash docs/ops/tools/utc_multinode_rehearsal.sh \
#     /path/to/falcon-pl-rs [/path/to/artifact-dir]
set -euo pipefail
ROOT="${1:?falcon-pl-rs root}"
ART="${2:-/tmp/falcon-utc-mn-artifacts}"
mkdir -p "$ART"
SCRATCH="$(mktemp -d /tmp/falcon-utc-mn-XXXXXX)"
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

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/falcon-cargo-target}"
BIN="$CARGO_TARGET_DIR/debug/falcon-pl-node"
CTL="$CARGO_TARGET_DIR/debug/falcon-pl-ctl"
if [[ ! -x "$BIN" || ! -x "$CTL" ]]; then
  echo "building falcon-pl-node/ctl (debug, jobs=2)..."
  (cd "$ROOT" && CARGO_BUILD_JOBS=2 nice -n19 ionice -c3 cargo build -p fd-pl --bin falcon-pl-node --bin falcon-pl-ctl)
fi
test -x "$BIN" && test -x "$CTL"
sha256sum "$BIN" | tee "$ART/binary.sha256"

PORT_BASE=29101
N=6
NET=2200
H_UTC=2
VALS="v1,v2,v3,v4,v5,v6"
export FPL_UTC_FROM_HEIGHT="$H_UTC"
export FPL_UTC_HELLO_ENFORCE_FROM_HEIGHT="$H_UTC"
export FPL_EMPTY_BLOCK_FROM_HEIGHT=1
export ART PORT_BASE

KEYS="$SCRATCH/keys"
mkdir -p "$KEYS/lc"
"$BIN" --gen-keys-only --keys-dir "$KEYS" --scheme hmac --allow-dev-crypto \
  --validators "$VALS" --id v1 >/dev/null
# QC v4 from height 1 — UTC seal path is seal_v4_ledger (v5 domain when armed).
printf '%s\n' '{"qc_header_version":4,"qc_start_height":1,"network_id":2200,"start_height":1}' >"$KEYS/lc/bridge.json"

for i in $(seq 0 $((N-1))); do
  id="v$((i+1))"
  port=$((PORT_BASE+i))
  dat="$SCRATCH/$id"
  mkdir -p "$dat"/{run,snapshots,logs}
  peers=""
  for j in $(seq 0 $((N-1))); do
    if [[ $j -ne $i ]]; then
      p=$((PORT_BASE+j))
      [[ -n "$peers" ]] && peers+=","
      peers+="127.0.0.1:$p"
    fi
  done
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
    --seal-interval-ms 300 \
    --pack-budget-ms 500 \
    --progress-timeout-ms 4000 \
    --max-chain-ledgers 128 \
    --lock-public-wire \
    >"$dat/logs/node.log" 2>&1 &
  PIDS="$PIDS $!"
  echo "started $id :$port pid=$!" | tee -a "$ART/meta.txt"
done

sleep 4

export KEYS CTL PORT_BASE NET
python3 - <<'PY'
import subprocess, os, time
ctl=os.environ["CTL"]; keys=os.environ["KEYS"]; net=os.environ["NET"]; base=int(os.environ["PORT_BASE"])
for seq in range(1, 12):
    cmd=[ctl,"--addr",f"127.0.0.1:{base}","pay",
         "--from","alice","--to","bob","--amount","1","--fee","1",
         "--sequence",str(seq),"--scheme","hmac","--keys-dir",keys,
         "--network-id",net]
    try:
        out=subprocess.check_output(cmd, text=True, timeout=20, stderr=subprocess.STDOUT)
        print("pay seq",seq, out.splitlines()[0] if out else "ok")
    except Exception as e:
        print("pay seq",seq,"err",e)
    time.sleep(0.15)
PY

python3 - "$CTL" "$PORT_BASE" "$N" "$H_UTC" "$ART" <<'PY'
import json, subprocess, sys, time
ctl, base, n, h_utc, art = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]

def poll():
    rows=[]; ok=True; tips=[]; hashes=set(); utcs=set()
    for i in range(n):
        port=base+i
        try:
            raw=subprocess.check_output([ctl,"--addr",f"127.0.0.1:{port}","status"], text=True, timeout=25)
            msg=json.loads(raw)
            b=msg.get("body") if isinstance(msg, dict) else None
            if not isinstance(b, dict):
                b = msg if isinstance(msg, dict) else {}
            tip=b.get("tip_height") or 0
            row={"port":port,"tip":tip,"hash":(b.get("tip_hash") or "")[:16],
                 "utc_from_height":b.get("utc_from_height"),"utc_active_next":b.get("utc_active_next"),
                 "tip_sealed_utc_ms":b.get("tip_sealed_utc_ms"),"tip_sealed_utc_quality":b.get("tip_sealed_utc_quality"),
                 "tip_sealed_at_quality":b.get("tip_sealed_at_quality"),
                 "qc_header_version":b.get("qc_header_version"),"qc_start_height":b.get("qc_start_height"),
                 "stall":b.get("consensus_stall"),"ver":b.get("product_version")}
            if row["utc_from_height"]!=h_utc:
                ok=False; row["fail"]="utc_from_height"
            tips.append(tip)
            if tip:
                hashes.add(b.get("tip_hash") or "")
                if b.get("tip_sealed_utc_ms"):
                    utcs.add(b.get("tip_sealed_utc_ms"))
        except Exception as e:
            ok=False; row={"port":port,"error":str(e)}; tips.append(0)
        rows.append(row)
    return rows, ok, tips, hashes, utcs

deadline=time.time()+150
last=None
while True:
    rows, ok, tips, hashes, utcs = poll()
    last=(rows, ok, tips, hashes, utcs)
    print("poll tips=%s hashes=%d utcs=%d ok=%s" % (tips, len(hashes), len(utcs), ok))
    # Require tip past H_utc, single tip hash, and non-zero sealed_utc on at least one seat
    if ok and min(tips)>=h_utc and len(hashes)==1 and len(utcs)==1:
        break
    if time.time()>=deadline:
        break
    time.sleep(2)

rows, ok, tips, hashes, utcs = last
open(art+"/status_round1.jsonl","w").write("\n".join(json.dumps(r) for r in rows)+"\n")
agree = ok and min(tips)>=h_utc and len(hashes)==1 and len(utcs)==1
open(art+"/status_round1_ok","w").write("ok\n" if agree else "fail\n")
open(art+"/tip_agreement.json","w").write(json.dumps({"tips":tips,"hashes":list(hashes),"utcs":list(utcs),"agree":agree},indent=2))
for r in rows: print(json.dumps(r,separators=(",",":")))
sys.exit(0 if agree else 2)
PY

python3 - <<'PY'
import json, socket, time, os, sys
port=int(os.environ["PORT_BASE"]); art=os.environ["ART"]
s=socket.create_connection(("127.0.0.1",port),timeout=5)
buf=b""; s.settimeout(2.0)
try:
  while b"\n" not in buf:
    c=s.recv(65536)
    if not c: break
    buf+=c
except Exception:
  pass
peer=None
if buf.strip():
  try: peer=json.loads(buf.split(b"\n",1)[0].decode())
  except Exception as e: peer={"parse_err":str(e),"raw":buf[:200].decode(errors="replace")}
hello={"type":"hello","id":"old-peer","public_key":"","validators":{},"publics":{},
       "tip_height":0,"tip_hash":"","is_hub":False,"mempool_len":0,
       "product_version":"2.9.100","hello_network_id":0,"archive":False,"snap_height":0,
       "release_version":"2.10.4"}
s.sendall((json.dumps(hello)+"\n").encode()); time.sleep(0.5)
resp=b""; s.settimeout(2.0)
try:
  while True:
    c=s.recv(65536)
    if not c: break
    resp+=c
    if len(resp)>8192: break
except Exception:
  pass
try: s.close()
except Exception: pass
caps=(peer or {}).get("capabilities") or []
out={"node_hello_type":(peer or {}).get("type"),"node_hello_caps":caps,
     "old_peer_response":resp.decode(errors="replace")[:600]}
print(json.dumps(out,indent=2))
open(art+"/hello_gate.json","w").write(json.dumps(out,indent=2))
if "utc-header-v5" not in caps:
  sys.exit("FAIL missing utc-header-v5: %r"% (caps,))
open(art+"/hello_gate_ok","w").write("ok\n")
print("hello_gate_ok")
PY

(ss -lntp 2>/dev/null || true) | grep -E ':2910[1-6]\b' | tee "$ART/listeners.txt" || true
if grep -E '0\.0\.0\.0:2910|\*:2910' "$ART/listeners.txt" >/dev/null 2>&1; then
  echo "FAIL non-loopback bind" | tee -a "$ART/meta.txt"; exit 3
fi
echo "ISOLATION_OK loopback:29101-29106 network_id=2200 qc_v4 scratch=$SCRATCH" | tee -a "$ART/meta.txt"
echo PASS | tee "$ART/RESULT.txt"
