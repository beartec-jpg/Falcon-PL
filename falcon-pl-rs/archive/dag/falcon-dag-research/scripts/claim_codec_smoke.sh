#!/usr/bin/env bash
# Phase 32: claim leaf/root codec parity — offline golden vectors + live node export.
# Matches fd_exec::withdrawal_claim_* and FalconRailLock.sol (sha256, not keccak).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=smoke_lib.sh
source "$ROOT/scripts/smoke_lib.sh"

RPC="${RPC:-127.0.0.1:18841}"
PORT="${RPC##*:}"
LOG="${LOG:-/tmp/fd-claim-codec.log}"
PID=""

cleanup() {
  [[ -n "${PID:-}" ]] && kill "$PID" 2>/dev/null || true
  smoke_kill_port "$PORT"
  wait 2>/dev/null || true
}
trap cleanup EXIT

# --- offline golden (no node) ---
python3 "$ROOT/scripts/rail_indexers/claim_codec_vectors.py"
echo "offline claim_codec_vectors OK"

smoke_require_bin "$ROOT"
PID=$(smoke_start_node "$RPC" "$LOG")
smoke_wait_rpc "$RPC" 80 || { tail -80 "$LOG" || true; exit 1; }

post() { smoke_post "$RPC" "$1"; }
wait_exec() { sleep 0.45; }
submit_ok() {
  smoke_require_engine "$(post "$1")" tesSUCCESS "${2:-submit}"
}

python3 "$ROOT/scripts/rail_agent.py" --rpc "http://$RPC" --rail ETH --indexer synthetic \
  --amount 5000 --recipient acct3
wait_exec
SEQ=$(post '{"method":"account_info","params":[{"account":"acct3"}]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['sequence'])")
submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"rail_withdraw\",\"account\":\"acct3\",\"sequence\":$SEQ,\"asset\":\"ETH\",\"amount\":1200,\"external_to\":\"0xclaim_codec_out\",\"fee\":1}]}" "rail_withdraw"
wait_exec
SEQ2=$(post '{"method":"account_info","params":[{"account":"acct3"}]}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['result']['sequence'])")
submit_ok "{\"method\":\"submit\",\"params\":[{\"kind\":\"rail_withdraw\",\"account\":\"acct3\",\"sequence\":$SEQ2,\"asset\":\"ETH\",\"amount\":800,\"external_to\":\"0xclaim_codec_out2\",\"fee\":1}]}" "rail_withdraw2"
wait_exec

# Live: agent recompute must match node claim_export + committed root
python3 - <<PY
import json, sys, urllib.request
from pathlib import Path
sys.path.insert(0, str(Path("$ROOT") / "scripts"))
from rail_indexers.claim_codec_vectors import claim_leaf, claim_root, verify_proof

RPC = "http://$RPC"

def rpc(method, params):
    body = json.dumps({"method": method, "params": [params]}).encode()
    req = urllib.request.Request(RPC, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.loads(r.read().decode()).get("result", {})

notes = rpc("withdrawal_notes", {"rail": "ETH"})
assert notes.get("count", 0) >= 2, notes
assert notes.get("committee") is None
local_root = claim_root(notes["notes"])
assert local_root == notes["claim_root"], (local_root, notes["claim_root"])
info = rpc("rail_info", {"rail": "ETH"})
assert info.get("claim_root_matches") is True, info
assert info.get("claim_root") == local_root

for n in notes["notes"]:
    leaf = claim_leaf(n)
    assert leaf == n["claim_leaf"], (leaf, n["claim_leaf"])
    exp = rpc("claim_export", {"note_id": n["note_id"]})
    assert exp.get("claim_leaf") == leaf
    assert exp.get("committee") is None
    path = exp.get("merkle_path") or []
    idx = int(exp.get("merkle_index") or exp.get("path_index") or 0)
    root = (exp.get("claim_root") or "").removeprefix("0x")
    assert verify_proof(leaf, path, idx, root), (n["note_id"], path, idx, root)
    assert exp.get("claim_root_matches_commit") is True or exp.get("claim_root_committed") == root

# outbound pack (Solidity calldata path)
import subprocess
out = subprocess.check_output(
    [sys.executable, str(Path("$ROOT") / "scripts/rail_indexers/evm_outbound_claim.py"),
     "--rpc", RPC, "--rail", "ETH"],
    text=True,
)
pack = json.loads(out)
assert pack.get("verified") is True, pack
assert pack["submitFalconRoot"]["root"].removeprefix("0x") == local_root
print("live claim_export + outbound pack OK notes", notes["count"], "root", local_root[:16])
PY

echo "claim_codec_smoke OK (golden + live ETH multi-note claim tree = FalconRailLock codec)"
