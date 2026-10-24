#!/usr/bin/env bash
# Poll server_info on all 6 vals + 2 arch.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=/dev/null
source "$ROOT/env.sh"

rpc_json() {
  local host="$1" port="$2" method="$3"
  local params="${4:-{}}"
  python3 - "$host" "$port" "$method" "$params" <<'PY'
import json,sys,urllib.request
host,port,method,params=sys.argv[1:5]
body=json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":[json.loads(params)]}).encode()
req=urllib.request.Request(f"http://{host}:{port}/", data=body,
  headers={"Content-Type":"application/json"}, method="POST")
try:
  with urllib.request.urlopen(req, timeout=4) as r:
    print(r.read().decode())
except Exception as e:
  print(json.dumps({"error": str(e)}))
PY
}

echo "=== mesh status NETWORK_ID=$NETWORK_ID CRYPTO=$CRYPTO ==="
echo "UNL=$UNL workers=${WORKER_COUNT:-?} threshold=$UNL_THRESHOLD LAN F1=$F1_IP F2=$F2_IP"
printf "%-8s %-22s %-10s %-8s %-8s %-8s %-8s %s\n" \
  "NODE" "HOST:RPC" "PROFILE" "EXEC" "LAG" "ADMITS" "P2P" "ROLE"
for row in \
  "val1 $F1_IP $VAL1_RPC" \
  "val2 $F1_IP $VAL2_RPC" \
  "val3 $F1_IP $VAL3_RPC" \
  "arch1 $F1_IP $ARCH1_RPC" \
  "val4 $F2_IP $VAL4_RPC" \
  "val5 $F2_IP $VAL5_RPC" \
  "val6 $F2_IP $VAL6_RPC" \
  "arch2 $F2_IP $ARCH2_RPC"
do
  set -- $row
  id=$1 host=$2 port=$3
  raw=$(rpc_json "$host" "$port" "server_info")
  python3 - "$id" "$host" "$port" "$raw" <<'PY'
import json,sys
id,host,port,raw=sys.argv[1:5]
try:
  d=json.loads(raw)
except Exception:
  print(f"{id:8} {host}:{port:5} ERROR parse")
  sys.exit(0)
if "error" in d and "result" not in d:
  print(f"{id:8} {host}:{port:<5} DOWN {d.get('error')}")
  sys.exit(0)
info=(d.get("result") or {}).get("info") or {}
m=info.get("metrics") or {}
ps=m.get("pack_stall") or {}
prof=info.get("node_profile") or m.get("node_profile") or "?"
role=m.get("role") or "?"
exec_=m.get("executed_txs")
lag=ps.get("admit_exec_lag", m.get("admit_exec_lag"))
adm=ps.get("local_admits_ok", m.get("local_admits_ok"))
peers=m.get("p2p_peers")
if isinstance(peers, dict):
  pn=peers.get("n") or peers.get("count") or len(peers)
elif isinstance(peers, list):
  pn=len(peers)
else:
  pn=peers
print(f"{id:8} {host}:{port:<5} {prof:10} {str(exec_):8} {str(lag):8} {str(adm):8} {str(pn):8} {role}")
PY
done
