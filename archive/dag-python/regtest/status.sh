#!/usr/bin/env bash
set -euo pipefail
for i in 1 2 3 4 5; do
  port=$((18000+i))
  echo "======== val${i} :${port} ========"
  curl -s -X POST "http://127.0.0.1:${port}" \
    -H 'Content-Type: application/json' \
    -d '{"method":"server_info","params":[{}]}' | python3 -c '
import sys,json
d=json.load(sys.stdin)
info=d.get("result",{}).get("info",{})
m=info.get("metrics",{})
da=m.get("da",{})
ord_=m.get("ordering",{})
ex=m.get("execution",{})
print("  arch=%s  state=%s"%(info.get("architecture"), info.get("server_state")))
print("  certified=%s ordered=%s exec_txs=%s soft=%s hard_batches=%s"%(
  da.get("certified"), ord_.get("ordered"), ex.get("executed_txs"),
  ex.get("soft_final_txs"), m.get("hard_final_batches")))
print("  mempool=%s batches_created=%s tips=%s"%(
  m.get("mempool"), m.get("batches_created"), da.get("tip_count")))
' 2>/dev/null || echo "  (unreachable)"
  echo
done
