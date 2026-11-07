#!/usr/bin/env bash
# Isolated mixed OLD-binary / NEW-binary UTC fork rehearsal on loopback.
# NOT a production soak. Private network_id 2200 ONLY. Never points at 2300
# seats, live ports, live keys, or Tailscale. FPL_UTC_* are ignored on 2300.
#
# Topology: 6 seats (QC v4 committee). NEW_SEATS run the UTC branch binary
# (advertises `utc-header-v5`, armed via FPL_UTC_FROM_HEIGHT on 2200);
# the rest run an OLD binary built from OLD_REF (no capability, v4 preimage
# forever).
#
# MODE=soft    — Hello enforce unarmed. Expected: at H_utc NEW packers stamp
#                v5 (sealed_utc_ms>0) while OLD seats hash v4 ⇒ ledger_hash /
#                OK signatures disagree ⇒ fork or stall at H_utc (no common QC
#                across the binary split). This is WHY enforce must be armed.
# MODE=enforce — FPL_UTC_HELLO_ENFORCE_FROM_HEIGHT armed. Effective close =
#                min(enforce, H_utc - PROPOSED_UTC_HELLO_LEAD_BLOCKS). Expected:
#                NEW seats Hello-close OLD peers ("utc-header-v5 capability
#                required"), OLD seats are isolated before H_utc; with only
#                NEW_SEATS of 6 the NEW side progresses iff NEW_SEATS meets
#                the QC threshold, otherwise it stalls safely (no fork).
#
# Usage (low priority; brain or box with RAM):
#   nice -n19 bash docs/ops/tools/utc_fork_rehearsal.sh /path/to/falcon-pl-rs \
#     [artifact-dir]   # env: MODE=soft|enforce OLD_REF=origin/main NEW_SEATS=4
#   OLD_BIN=/path/to/old/falcon-pl-node to skip building the old ref.
set -euo pipefail
ROOT="$(cd "${1:?falcon-pl-rs root}" && pwd)"
MODE="${MODE:-enforce}"
ART="${2:-/tmp/falcon-utc-fork-$MODE}"
OLD_REF="${OLD_REF:-origin/main}"
NEW_SEATS="${NEW_SEATS:-4}"
RUN_SECS="${RUN_SECS:-60}"
mkdir -p "$ART"
SCRATCH="$(mktemp -d /tmp/falcon-utc-fork-XXXXXX)"
PIDS=""
cleanup() {
  for p in $PIDS; do kill "$p" 2>/dev/null || true; done
  sleep 0.4
  for p in $PIDS; do kill -9 "$p" 2>/dev/null || true; done
  mkdir -p "$ART/logs"
  for d in "$SCRATCH"/v*; do [[ -f "$d/logs/node.log" ]] && cp "$d/logs/node.log" "$ART/logs/$(basename "$d").log"; done
}
trap cleanup EXIT
{ echo "mode=$MODE old_ref=$OLD_REF new_seats=$NEW_SEATS scratch=$SCRATCH"; date -u +%FT%TZ; } | tee "$ART/meta.txt"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/falcon-cargo-target}"
NEW_BIN="$CARGO_TARGET_DIR/debug/falcon-pl-node"
CTL="$CARGO_TARGET_DIR/debug/falcon-pl-ctl"
if [[ ! -x "$NEW_BIN" || ! -x "$CTL" ]]; then
  (cd "$ROOT" && CARGO_BUILD_JOBS=2 nice -n19 cargo build -p fd-pl --bin falcon-pl-node --bin falcon-pl-ctl)
fi
if [[ -z "${OLD_BIN:-}" ]]; then
  OLD_TREE="/tmp/falcon-utc-fork-oldtree"
  OLD_TARGET="/tmp/falcon-cargo-target-old"
  REPO="$(cd "$ROOT/.." && pwd)"
  if [[ ! -d "$OLD_TREE" ]]; then
    git -C "$REPO" fetch -q origin || true
    git -C "$REPO" worktree add -f --detach "$OLD_TREE" "$OLD_REF"
  fi
  OLD_BIN="$OLD_TARGET/debug/falcon-pl-node"
  [[ -x "$OLD_BIN" ]] || (cd "$OLD_TREE/falcon-pl-rs" && CARGO_TARGET_DIR="$OLD_TARGET" CARGO_BUILD_JOBS=2 nice -n19 cargo build -p fd-pl --bin falcon-pl-node)
  git -C "$OLD_TREE" rev-parse HEAD | tee "$ART/old_ref.sha"
fi
sha256sum "$NEW_BIN" "$OLD_BIN" | tee "$ART/binaries.sha256"

PORT_BASE=29201; N=6; NET=2200; H_UTC="${H_UTC:-8}"
VALS="v1,v2,v3,v4,v5,v6"
export FPL_UTC_FROM_HEIGHT="$H_UTC" FPL_EMPTY_BLOCK_FROM_HEIGHT=1
if [[ "$MODE" == enforce ]]; then export FPL_UTC_HELLO_ENFORCE_FROM_HEIGHT="$H_UTC"; else unset FPL_UTC_HELLO_ENFORCE_FROM_HEIGHT; fi

KEYS="$SCRATCH/keys"; mkdir -p "$KEYS/lc"
"$NEW_BIN" --gen-keys-only --keys-dir "$KEYS" --scheme hmac --allow-dev-crypto --validators "$VALS" --id v1 >/dev/null
printf '%s\n' '{"qc_header_version":4,"qc_start_height":1,"network_id":2200,"start_height":1}' >"$KEYS/lc/bridge.json"

for i in $(seq 0 $((N-1))); do
  id="v$((i+1))"; port=$((PORT_BASE+i)); dat="$SCRATCH/$id"
  mkdir -p "$dat"/{run,snapshots,logs}
  peers=""; for j in $(seq 0 $((N-1))); do [[ $j -ne $i ]] && { [[ -n "$peers" ]] && peers+=","; peers+="127.0.0.1:$((PORT_BASE+j))"; }; done
  if [[ $i -lt $NEW_SEATS ]]; then bin="$NEW_BIN"; kind=new; else bin="$OLD_BIN"; kind=old; fi
  nice -n19 "$bin" --id "$id" --listen "127.0.0.1:$port" --peers "$peers" --keys-dir "$KEYS" \
    --scheme hmac --allow-dev-crypto --validators "$VALS" --bond 1000 --network-id "$NET" \
    --online-epoch-heights 0 --first-claim-epoch 1 --min-block-interval-ms 0 \
    --admin-sock "$dat/run/admin.sock" --snapshot "$dat/snapshots/snap.json" \
    --snapshot-out "$dat/snapshots/snap.json" --seal-interval-ms 300 --pack-budget-ms 500 \
    --progress-timeout-ms 4000 --max-chain-ledgers 128 --lock-public-wire \
    >"$dat/logs/node.log" 2>&1 &
  PIDS="$PIDS $!"; echo "started $id ($kind) :$port pid=$!" | tee -a "$ART/meta.txt"
done
sleep 4
# Drive txs so packers seal past H_utc (same as utc_multinode_rehearsal.sh).
for seq in $(seq 1 $((H_UTC+6))); do
  "$CTL" --addr "127.0.0.1:$PORT_BASE" pay --from alice --to bob --amount 1 --fee 1 \
    --sequence "$seq" --scheme hmac --keys-dir "$KEYS" --network-id "$NET" >/dev/null 2>&1 \
    || echo "pay seq $seq err" | tee -a "$ART/meta.txt"
  sleep 1.5
done
sleep "$RUN_SECS"

python3 - "$CTL" "$PORT_BASE" "$N" "$NEW_SEATS" "$H_UTC" "$ART" "$SCRATCH" "$MODE" <<'PY'
import json, subprocess, sys, glob, os
ctl, base, n, new, h, art, scratch, mode = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5]), sys.argv[6], sys.argv[7], sys.argv[8]
rows=[]
for i in range(n):
    r={"seat":f"v{i+1}","kind":"new" if i<new else "old","port":base+i}
    try:
        m=json.loads(subprocess.check_output([ctl,"--addr",f"127.0.0.1:{base+i}","status"],text=True,timeout=25))
        b=m.get("body",m) if isinstance(m,dict) else {}
        r.update(tip=b.get("tip_height"),hash=(b.get("tip_hash") or "")[:16],utc=b.get("tip_sealed_utc_ms"),ver=b.get("product_version"))
    except Exception as e: r["error"]=str(e)[:200]
    log=open(f"{scratch}/v{i+1}/logs/node.log",errors="replace").read()
    r["hello_utc_closes"]=log.count("utc-header-v5 capability required")
    rows.append(r)
new_h={r.get("hash") for r in rows if r["kind"]=="new" and r.get("tip")}
old_h={r.get("hash") for r in rows if r["kind"]=="old" and r.get("tip")}
summary={"mode":mode,"h_utc":h,"rows":rows,
  "new_tips":[r.get("tip") for r in rows if r["kind"]=="new"],
  "old_tips":[r.get("tip") for r in rows if r["kind"]=="old"],
  "split_hashes": bool(new_h and old_h and not (new_h & old_h)),
  "hello_closes_on_new":sum(r["hello_utc_closes"] for r in rows if r["kind"]=="new")}
json.dump(summary,open(art+"/fork_summary.json","w"),indent=2)
print(json.dumps(summary,indent=2))
PY
