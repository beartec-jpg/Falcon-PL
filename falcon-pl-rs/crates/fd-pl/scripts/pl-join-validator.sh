#!/usr/bin/env bash
# Live bond-in: add a validator to a *running* private testnet without restarting the mesh.
#
#   bash crates/fd-pl/scripts/pl-join-validator.sh \
#     --id v5 --host host2 --port 19112 \
#     --primary 127.0.0.1:19101 \
#     --peers 100.64.0.1:19101,100.64.0.1:19102,100.64.0.3:19111,100.64.0.5:19121
#
# Steps: gen key → sync keys → faucet fund bonder → bond ledger → start --join node → wait catch-up
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=pl-hosts.env
# shellcheck disable=SC1091
[[ -f "$SCRIPT_DIR/pl-hosts.env" ]] && source "$SCRIPT_DIR/pl-hosts.env" || true

RS="$(cd "$SCRIPT_DIR/../../.." && pwd)"
BIN="${PL_BIN:-$RS/target/release}"
ROOT="${PL_ROOT:-$HOME/falcon-pl-private-testnet-v22}"
KEYS="${PL_KEYS:-$ROOT/keys}"
SCHEME="${PL_SCHEME:-hmac}"
NETWORK_ID="${PL_NETWORK_ID:-2200}"
BOND_AMT="${PL_BOND_AMT:-1000}"
BONDER="${PL_BONDER:-alice}"
PRIMARY="${PL_PRIMARY:-127.0.0.1:19101}"

JOIN_ID=""
JOIN_HOST="local"   # local | host2 | host3 | user@host
JOIN_PORT=""
PEERS=""
GENESIS_VALS=""     # comma list of existing ids, e.g. v1,v2,v3,v4

SSH_OPTS=(-o ConnectTimeout=12 -o BatchMode=yes -o StrictHostKeyChecking=accept-new)

usage() {
  cat <<EOF
usage: $0 --id v5 --port 19112 --peers host:port,... [options]

required:
  --id ID              new validator id (e.g. v5)
  --port PORT          listen port on join host
  --peers LIST         seed peers (comma host:port of existing mesh)

optional:
  --host local|host2|host3|SSH  (default local)
  --primary ADDR       live RPC for bond (default $PRIMARY)
  --bond AMT           bond amount (default $BOND_AMT)
  --bonder ACCOUNT     who pays bond (default $BONDER)
  --genesis-vals LIST  existing validators (default: read \$ROOT/run/validators.txt or v1,v2,v3,v4)
  --keys DIR           keys dir (default $KEYS)
  --root DIR           data root (default $ROOT)
EOF
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --id) JOIN_ID="$2"; shift 2 ;;
    --host) JOIN_HOST="$2"; shift 2 ;;
    --port) JOIN_PORT="$2"; shift 2 ;;
    --peers) PEERS="$2"; shift 2 ;;
    --primary) PRIMARY="$2"; shift 2 ;;
    --bond) BOND_AMT="$2"; shift 2 ;;
    --bonder) BONDER="$2"; shift 2 ;;
    --genesis-vals) GENESIS_VALS="$2"; shift 2 ;;
    --keys) KEYS="$2"; shift 2 ;;
    --root) ROOT="$2"; shift 2 ;;
    -h|--help) usage ;;
    *) echo "unknown arg: $1"; usage ;;
  esac
done

[[ -n "$JOIN_ID" && -n "$JOIN_PORT" && -n "$PEERS" ]] || usage

if [[ -z "$GENESIS_VALS" ]]; then
  if [[ -f "$ROOT/run/validators.txt" ]]; then
    GENESIS_VALS=$(tr -d '[:space:]' <"$ROOT/run/validators.txt")
  else
    GENESIS_VALS="v1,v2,v3,v4"
  fi
fi

# Full validator list after join (genesis + new id if not already listed)
if [[ ",$GENESIS_VALS," == *",$JOIN_ID,"* ]]; then
  ALL_VALS="$GENESIS_VALS"
else
  ALL_VALS="${GENESIS_VALS},${JOIN_ID}"
fi

CTL=("$BIN/falcon-pl-ctl")
ssh_h() {
  ssh "${SSH_OPTS[@]}" "$1" "${@:2}"
}

echo "======== live bond-in ========"
echo "id=$JOIN_ID host=$JOIN_HOST port=$JOIN_PORT"
echo "primary=$PRIMARY peers=$PEERS"
echo "genesis_vals=$GENESIS_VALS → all=$ALL_VALS bond=$BOND_AMT from=$BONDER"

# 1) Ensure binary
[[ -x "$BIN/falcon-pl-node" && -x "$BIN/falcon-pl-ctl" ]] || {
  echo "building fd-pl..."
  (cd "$RS" && cargo build -p fd-pl --release 2>&1 | tail -5)
}

# 2) Generate validator key into keys dir (shared keyring file)
if [[ ! -f "$KEYS/${JOIN_ID}.json" ]] && [[ ! -f "$KEYS/pl_keys.json" ]]; then
  echo "FAIL: no keys dir at $KEYS"; exit 1
fi
# Append id into keyring via gen-keys only for this validator if missing
python3 - "$KEYS" "$JOIN_ID" "$SCHEME" "$BIN" <<'PY' || true
import json, os, subprocess, sys
keys, vid, scheme, bindir = sys.argv[1:5]
pk = os.path.join(keys, "pl_keys.json")
if os.path.isfile(pk):
    data = json.load(open(pk))
    # structure may be scheme-specific; fall through to ctl gen if unknown
    if isinstance(data, dict) and vid in data.get("keys", data):
        print(f"key {vid} already in pl_keys.json")
        sys.exit(0)
# Use ctl to generate a wallet-style key file named by account when possible
r = subprocess.run(
    [f"{bindir}/falcon-pl-ctl", "gen-wallet", "--keys-dir", keys, "--account", vid, "--scheme", scheme],
    capture_output=True, text=True,
)
print(r.stdout or r.stderr)
sys.exit(0 if r.returncode == 0 else 1)
PY

# Prefer gen-keys merge: regenerate full set including new id into a temp then merge is hard;
# for private testnet, gen-wallet for vid stores a single key; node KeyRing::load_dir loads pl_keys.json.
# Ensure pl_keys includes JOIN_ID by running gen-keys with full ALL_VALS (preserves? rewrites).
# Safer: call gen-keys only if JOIN_ID missing from directory listing.
if [[ ! -f "$KEYS/${JOIN_ID}.json" ]]; then
  # Many installs use only pl_keys.json — re-gen keys for ALL_VALS would wipe secrets.
  # Instead use gen-wallet which adds account file; also try ctl path that inserts into ring.
  "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account "$JOIN_ID" --scheme "$SCHEME" || true
fi

# If pl_keys.json exists, ensure public for JOIN_ID via a small Rust-less approach:
# re-load is handled if gen-wallet writes into pl_keys — check crypto load.
echo "[join] keys dir prepared for $JOIN_ID"

# 3) Sync keys to remote hosts that run the mesh (so they can verify seals after Hello)
for h in host2 host3; do
  ssh_h "$h" "mkdir -p $ROOT/keys $ROOT/bin $ROOT/logs $ROOT/snapshots $ROOT/run" 2>/dev/null || true
  rsync -az -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$h:$ROOT/keys/" 2>/dev/null || true
  rsync -az -e "ssh ${SSH_OPTS[*]}" "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$h:$ROOT/bin/" 2>/dev/null || true
done

# 4) Fund bonder if needed (best-effort faucet)
echo "[join] ensure bonder $BONDER funded"
"${CTL[@]}" --addr "$PRIMARY" faucet --to "$BONDER" --amount 5000000 \
  --scheme "$SCHEME" --keys-dir "$KEYS" 2>/dev/null || true
for _ in 1 2 3 4 5 6; do
  "${CTL[@]}" --addr "$PRIMARY" seal >/dev/null 2>&1 || true
  sleep 0.2
done

# 5) Bond on live chain
echo "[join] bond $BOND_AMT → $JOIN_ID from $BONDER"
"${CTL[@]}" --addr "$PRIMARY" bond \
  --id "$JOIN_ID" --from "$BONDER" --amount "$BOND_AMT" \
  --scheme "$SCHEME" --keys-dir "$KEYS"
# Drive packing until tip moves
tip0=$("${CTL[@]}" --addr "$PRIMARY" status 2>/dev/null | python3 -c "import sys,json;r=sys.stdin.read();b=json.JSONDecoder().raw_decode(r[r.find('{'):])[0];print((b.get('body')or b).get('tip_height',0))")
for i in $(seq 1 40); do
  "${CTL[@]}" --addr "$PRIMARY" pay --from alice --to bob --amount 1 --fee 2 \
    --sequence 0 --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null 2>&1 || true
  "${CTL[@]}" --addr "$PRIMARY" seal >/dev/null 2>&1 || true
  "${CTL[@]}" --addr "127.0.0.1:19102" seal >/dev/null 2>&1 || true
  sleep 0.25
  tip=$("${CTL[@]}" --addr "$PRIMARY" status 2>/dev/null | python3 -c "import sys,json;r=sys.stdin.read();b=json.JSONDecoder().raw_decode(r[r.find('{'):])[0];print((b.get('body')or b).get('tip_height',0))" 2>/dev/null || echo 0)
  # Check bond present
  if "${CTL[@]}" --addr "$PRIMARY" status 2>/dev/null | python3 -c "
import sys,json
r=sys.stdin.read();b=json.JSONDecoder().raw_decode(r[r.find('{'):])[0]
body=b.get('body',b)
for v in body.get('validators',[]):
  if v.get('id')=='$JOIN_ID' and int(v.get('bond') or 0)>=$BOND_AMT:
    print('bonded', v.get('bond')); sys.exit(0)
sys.exit(1)
" 2>/dev/null; then
    echo "[join] bond visible on primary tip=$tip"
    break
  fi
  if [[ "$i" -eq 40 ]]; then
    echo "FAIL: bond not visible after packing"; exit 2
  fi
done

# 6) State-sync: copy a live peer snapshot (tip state). Full history pull from
# height 1 fails after mesh snapshot restarts (chain_len ≪ tip). Residual
# NeedLedgers closes the small gap after copy.
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
mkdir -p "$LOG" "$SNAP" "$ROOT/run"
# Prefer primary's local snapshot if on this host; else first mesh snapshot we can read.
PRIMARY_SNAP="${PL_PRIMARY_SNAP:-$SNAP/v1.json}"
JOIN_SNAP="$SNAP/${JOIN_ID}.json"
echo "[join] state-sync snapshot → $JOIN_SNAP"
if [[ -f "$PRIMARY_SNAP" ]]; then
  # Wait briefly for snapshot tip to include bond (periodic save on primary).
  for _ in $(seq 1 90); do
    if python3 - "$PRIMARY_SNAP" "$JOIN_ID" "$BOND_AMT" <<'PY'
import json,sys
path, vid, need = sys.argv[1], sys.argv[2], int(sys.argv[3])
d=json.load(open(path))
vals=d.get("validators") or {}
v=vals.get(vid) or {}
bond=int(v.get("bond") or 0) if isinstance(v,dict) else 0
tip=int(d.get("tip_height") or 0)
print(f"snap tip={tip} {vid}.bond={bond}")
sys.exit(0 if bond >= need and tip > 0 else 1)
PY
    then
      break
    fi
    sleep 1
  done
  cp -f "$PRIMARY_SNAP" "$JOIN_SNAP"
else
  echo "WARN: no $PRIMARY_SNAP — joiner starts at genesis (only works if peers hold full history)"
  rm -f "$JOIN_SNAP"
fi

start_join_local() {
  local peers="$1"
  local snap_args=()
  if [[ -f "$JOIN_SNAP" ]]; then
    snap_args=(--snapshot "$JOIN_SNAP" --snapshot-out "$JOIN_SNAP")
  else
    snap_args=(--snapshot-out "$JOIN_SNAP")
  fi
  nohup "$BIN/falcon-pl-node" \
    --join \
    --id "$JOIN_ID" \
    --listen "0.0.0.0:${JOIN_PORT}" \
    --peers "$peers" \
    --validators "$GENESIS_VALS" \
    --scheme "$SCHEME" \
    --keys-dir "$KEYS" \
    --load-wallets 32 \
    --bond "$BOND_AMT" \
    --network-id "$NETWORK_ID" \
    --seal-interval-ms 400 \
    --pack-budget-ms 1000 \
    --progress-timeout-ms 1000 \
    --max-mempool-txs 20000 \
    --epoch-ms "${PL_EPOCH_MS:-3600000}" \
    --unbond-cooldown-ms "${PL_UNBOND_COOLDOWN_MS:-86400000}" \
    --snapshot-interval-ms 60000 \
    --max-chain-ledgers 20000 \
    "${snap_args[@]}" \
    >>"$LOG/${JOIN_ID}.log" 2>&1 &
  echo $! | tee -a "$ROOT/run/join.pids"
  echo "[join] local $JOIN_ID pid=$! :$JOIN_PORT snap=$([[ -f $JOIN_SNAP ]] && echo yes || echo no)"
}

start_join_remote() {
  local ssh_target="$1" peers="$2"
  ssh_h "$ssh_target" "mkdir -p $ROOT/{logs,snapshots,run,bin,keys}"
  rsync -az -e "ssh ${SSH_OPTS[*]}" "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$ssh_target:$ROOT/bin/"
  rsync -az -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$ssh_target:$ROOT/keys/"
  if [[ -f "$JOIN_SNAP" ]]; then
    rsync -az -e "ssh ${SSH_OPTS[*]}" "$JOIN_SNAP" "$ssh_target:$ROOT/snapshots/${JOIN_ID}.json"
  fi
  local has_snap="false"
  [[ -f "$JOIN_SNAP" ]] && has_snap="true"
  ssh_h "$ssh_target" bash -s <<EOF
set -euo pipefail
ROOT="$ROOT"
SNAP_ARGS=(--snapshot-out "\$ROOT/snapshots/${JOIN_ID}.json")
if [[ "$has_snap" == "true" && -f "\$ROOT/snapshots/${JOIN_ID}.json" ]]; then
  SNAP_ARGS=(--snapshot "\$ROOT/snapshots/${JOIN_ID}.json" --snapshot-out "\$ROOT/snapshots/${JOIN_ID}.json")
fi
nohup "\$ROOT/bin/falcon-pl-node" \\
  --join \\
  --id "$JOIN_ID" \\
  --listen "0.0.0.0:${JOIN_PORT}" \\
  --peers "$peers" \\
  --validators "$GENESIS_VALS" \\
  --scheme "$SCHEME" \\
  --keys-dir "\$ROOT/keys" \\
  --load-wallets 32 \\
  --bond "$BOND_AMT" \\
  --network-id "$NETWORK_ID" \\
  --seal-interval-ms 400 \\
  --pack-budget-ms 1000 \\
  --progress-timeout-ms 1000 \\
  --max-mempool-txs 20000 \\
  --epoch-ms "${PL_EPOCH_MS:-3600000}" \\
  --unbond-cooldown-ms "${PL_UNBOND_COOLDOWN_MS:-86400000}" \\
  --snapshot-interval-ms 60000 \\
  --max-chain-ledgers 20000 \\
  "\${SNAP_ARGS[@]}" \\
  >>"\$ROOT/logs/${JOIN_ID}.log" 2>&1 &
echo \$! >> "\$ROOT/run/join.pids"
echo "[join] remote $JOIN_ID pid=\$! :$JOIN_PORT on $ssh_target snap=$has_snap"
EOF
}

# Existing mesh should learn JOIN_ID public key via Hello; also peer list for joiner.
case "$JOIN_HOST" in
  local|host1|"") start_join_local "$PEERS" ;;
  host2|f2) start_join_remote host2 "$PEERS" ;;
  host3|droplet|df3|f3) start_join_remote host3 "$PEERS" ;;
  *) start_join_remote "$JOIN_HOST" "$PEERS" ;;
esac

# 7) Wait catch-up
echo "[join] waiting for catch-up (tip_lag=0)…"
python3 - "$BIN" "$JOIN_HOST" "$JOIN_PORT" "$PRIMARY" "$ROOT" <<'PY'
import json, subprocess, sys, time
bindir, host, port, primary, root = sys.argv[1:6]
port = int(port)

def primary_tip():
    o = subprocess.check_output(
        [f"{bindir}/falcon-pl-ctl", "--addr", primary, "status"], text=True
    )
    b = json.JSONDecoder().raw_decode(o[o.find("{"):])[0]
    body = b.get("body", b)
    return int(body.get("tip_height") or 0), (body.get("tip_hash") or "")[:16]

def join_status():
    if host in ("local", "host1", ""):
        cmd = [f"{bindir}/falcon-pl-ctl", "--addr", f"127.0.0.1:{port}", "status"]
    else:
        ssh = "host2" if host in ("host2", "f2") else (
            "host3" if host in ("droplet", "host3", "df3", "f3") else host
        )
        cmd = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", ssh,
               f"{root}/bin/falcon-pl-ctl --addr 127.0.0.1:{port} status"]
    o = subprocess.check_output(cmd, text=True)
    b = json.JSONDecoder().raw_decode(o[o.find("{"):])[0]
    return b.get("body", b)

for i in range(90):
    try:
        pt, ph = primary_tip()
        body = join_status()
        jt = int(body.get("tip_height") or 0)
        jh = (body.get("tip_hash") or "")[:16]
        lag = int(body.get("tip_lag") or 0)
        vids = [v.get("id") for v in body.get("validators", [])]
        bonds = {v.get("id"): int(v.get("bond") or 0) for v in body.get("validators", [])}
        has_self = body.get("id") in bonds and bonds.get(body.get("id"), 0) > 0
        print(f"  try {i}: join tip={jt} lag={lag} primary={pt} hash_match={jh==ph} has_self={has_self}")
        if jt > 0 and abs(jt - pt) <= 1 and jh == ph and has_self:
            print("  validators", bonds)
            print("JOIN_OK tip", jt, jh)
            sys.exit(0)
    except Exception as e:
        print("  wait", e)
    # nudge pack on primary
    subprocess.run([f"{bindir}/falcon-pl-ctl", "--addr", primary, "seal"], capture_output=True)
    time.sleep(1.0)
print("JOIN_FAIL timeout")
sys.exit(2)
PY

echo "======== live bond-in complete: $JOIN_ID ========"
echo "status: ssh/local ctl --addr 127.0.0.1:$JOIN_PORT status"
echo "docs: crates/fd-pl/docs/LIVE_BOND_IN.md"
