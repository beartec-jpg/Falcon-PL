#!/usr/bin/env bash
# Falcon PL v2.2.0 — private testnet from genesis (host1 + host2 + droplet).
#
# Topology (defaults = start small; raise PL_F*_VALS to fill capacity with wiggle):
#   host1  : 1 full-history validator (v1) + extra vals   ports 19101+
#   host2  : validators                                 ports 19111+
#   droplet  : 1 validator + watcher agents               port  19121
#
# Mesh uses Tailscale IPs (droplet has no LAN to falcons). See pl-hosts.env.
#
#   bash crates/fd-pl/scripts/pl-private-testnet-v22.sh preflight
#   bash crates/fd-pl/scripts/pl-private-testnet-v22.sh genesis   # keys only
#   bash crates/fd-pl/scripts/pl-private-testnet-v22.sh start
#   bash crates/fd-pl/scripts/pl-private-testnet-v22.sh status
#   bash crates/fd-pl/scripts/pl-private-testnet-v22.sh load-light
#   bash crates/fd-pl/scripts/pl-private-testnet-v22.sh soak-12h  # bg light load + tip sync
#   bash crates/fd-pl/scripts/pl-private-testnet-v22.sh stop
#
# Capacity guide (light load, leave ~40% RAM free):
#   F1 15G/12c → up to ~1 hist + 4 vals   (default: 1 hist + 1 extra = 2)
#   F2 15G/6c  → up to ~3–4 vals          (default: 1)
#   Droplet 4G/2c → 1 val + watchers      (default: 1 val + 1 watcher)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=pl-hosts.env
# shellcheck disable=SC1091
source "$SCRIPT_DIR/pl-hosts.env"

RS="$(cd "$SCRIPT_DIR/../../.." && pwd)"
BIN="$RS/target/release"
ROOT="${PL_ROOT:-$HOME/falcon-pl-private-testnet-v22}"
KEYS="$ROOT/keys"
LOG="$ROOT/logs"
SNAP="$ROOT/snapshots"
RUN="$ROOT/run"
SCHEME="${PL_SCHEME:-hmac}"
LOAD_WALLETS="${PL_LOAD_WALLETS:-32}"
NETWORK_ID="${PL_NETWORK_ID:-2200}"
BOND="${PL_BOND:-1000}"
# Epoch: private soak default 1h (not 7d) so claim windows are exercisable; override for prod-like.
EPOCH_MS="${PL_EPOCH_MS:-3600000}"
UNBOND_CD_MS="${PL_UNBOND_COOLDOWN_MS:-86400000}"
SEAL_MS="${PL_SEAL_MS:-400}"
PACK_BUDGET_MS="${PL_PACK_BUDGET_MS:-1000}"
PROGRESS_TIMEOUT_MS="${PL_PROGRESS_TIMEOUT_MS:-1000}"
MAX_MEMPOOL_TXS="${PL_MAX_MEMPOOL_TXS:-20000}"
SNAPSHOT_INTERVAL_MS="${PL_SNAPSHOT_INTERVAL_MS:-60000}"
# Pruned vals keep last N ledgers; full-history node uses 0.
MAX_CHAIN_PRUNE="${PL_MAX_CHAIN_PRUNE:-20000}"

# Start-small defaults (raise later to fill hosts)
F1_N="${PL_F1_VALS:-2}"   # includes full-history v1
F2_N="${PL_F2_VALS:-1}"
F3_N="${PL_F3_VALS:-1}"
N_WATCHERS="${PL_WATCHERS:-1}"

F1_IP="${F1_IP:-$F1_TS}"
F2_IP="${F2_IP:-$F2_TS}"
F3_IP="${F3_IP:-$F3_TS}"
SSH_F2="${SSH_F2:-host2}"
SSH_F3="${SSH_F3:-host3}"
SSH_OPTS=(-o ConnectTimeout=12 -o BatchMode=yes -o StrictHostKeyChecking=accept-new)

# Port bases
F1_PORT0=19101
F2_PORT0=19111
F3_PORT0=19121

cmd="${1:-preflight}"
mkdir -p "$ROOT" "$KEYS" "$LOG" "$SNAP" "$RUN"

ssh_h() {
  local host="$1"; shift
  ssh "${SSH_OPTS[@]}" "$host" "$@"
}

# --- id / port / host maps ---------------------------------------------------

# Emit all validator ids: v1..vN
all_val_ids() {
  local n=$((F1_N + F2_N + F3_N)) i
  for ((i = 1; i <= n; i++)); do
    echo -n "v$i"
    [[ $i -lt $n ]] && echo -n ","
  done
  echo
}

# Print lines: id host port full_history(0|1)
layout_rows() {
  local i=1 p
  for ((p = 0; p < F1_N; p++)); do
    # first F1 node is full-history archive+val
    echo "v$i $F1_IP $((F1_PORT0 + p)) $([[ $p -eq 0 ]] && echo 1 || echo 0) local"
    i=$((i + 1))
  done
  for ((p = 0; p < F2_N; p++)); do
    echo "v$i $F2_IP $((F2_PORT0 + p)) 0 f2"
    i=$((i + 1))
  done
  for ((p = 0; p < F3_N; p++)); do
    echo "v$i $F3_IP $((F3_PORT0 + p)) 0 f3"
    i=$((i + 1))
  done
}

peer_list_for() {
  # all host:port except self
  local self_id="$1" peers="" id host port fh where
  while read -r id host port fh where; do
    [[ "$id" == "$self_id" ]] && continue
    if [[ -n "$peers" ]]; then peers+=","; fi
    peers+="${host}:${port}"
  done < <(layout_rows)
  echo "$peers"
}

vals_csv() { all_val_ids; }

# --- preflight ---------------------------------------------------------------

preflight() {
  echo "======== private testnet v2.2 preflight ========"
  echo "ROOT=$ROOT scheme=$SCHEME network_id=$NETWORK_ID"
  echo "layout: F1_vals=$F1_N (v1=full-history) F2_vals=$F2_N F3_vals=$F3_N watchers=$N_WATCHERS"
  echo "mesh: F1=$F1_IP F2=$F2_IP F3=$F3_IP"
  echo "--- planned nodes ---"
  layout_rows | while read -r id host port fh where; do
    echo "  $id  ${host}:${port}  hist=$fh  host=$where"
  done
  echo "validators: $(vals_csv)"
  if ! ssh_h "$SSH_F2" "echo ok" >/dev/null; then
    echo "FAIL: cannot SSH $SSH_F2"; exit 3
  fi
  if ! ssh_h "$SSH_F3" "echo ok" >/dev/null; then
    echo "FAIL: cannot SSH $SSH_F3"; exit 3
  fi
  echo "[preflight] SSH F2+F3 ok"
  # RAM hint
  echo "--- free RAM ---"
  free -h | head -2
  ssh_h "$SSH_F2" "free -h | head -2"
  ssh_h "$SSH_F3" "free -h | head -2"
  echo "PASS preflight"
}

# --- build / keys ------------------------------------------------------------

build() {
  cd "$RS"
  cargo build -p fd-pl --release 2>&1 | tail -6
  [[ -x "$BIN/falcon-pl-node" && -x "$BIN/falcon-pl-ctl" ]]
}

genesis() {
  build
  rm -rf "$KEYS"
  mkdir -p "$KEYS"
  local vals
  vals=$(vals_csv)
  # Validators + load wallets + explicit watcher accounts
  "$BIN/falcon-pl-ctl" gen-keys \
    --keys-dir "$KEYS" \
    --scheme "$SCHEME" \
    --validators "$vals" \
    --load-wallets "$LOAD_WALLETS"
  "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account faucet --scheme "$SCHEME" || true
  local w
  for ((w = 1; w <= N_WATCHERS; w++)); do
    "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account "watcher$w" --scheme "$SCHEME" || true
  done
  # Fund watchers from faucet later via ctl after start
  echo "$vals" >"$RUN/validators.txt"
  layout_rows >"$RUN/layout.txt"
  cat >"$RUN/genesis.env" <<EOF
NETWORK_ID=$NETWORK_ID
SCHEME=$SCHEME
VALS=$vals
F1_N=$F1_N
F2_N=$F2_N
F3_N=$F3_N
N_WATCHERS=$N_WATCHERS
EPOCH_MS=$EPOCH_MS
BOND=$BOND
F1_IP=$F1_IP
F2_IP=$F2_IP
F3_IP=$F3_IP
EOF
  echo "genesis keys → $KEYS"
  echo "layout → $RUN/layout.txt"
  cat "$RUN/layout.txt"
}

sync_remotes() {
  for host in "$SSH_F2" "$SSH_F3"; do
    ssh_h "$host" "mkdir -p $ROOT/{bin,keys,logs,snapshots,run}"
    rsync -az -e "ssh ${SSH_OPTS[*]}" \
      "$BIN/falcon-pl-node" "$BIN/falcon-pl-ctl" "$host:$ROOT/bin/"
    rsync -az --delete -e "ssh ${SSH_OPTS[*]}" "$KEYS/" "$host:$ROOT/keys/"
    rsync -az -e "ssh ${SSH_OPTS[*]}" "$RUN/" "$host:$ROOT/run/"
    echo "[sync] $host ← bin+keys+run"
  done
}

# --- start / stop ------------------------------------------------------------

common_flags() {
  local hist="$1" # 0|1
  local mcl="$MAX_CHAIN_PRUNE"
  [[ "$hist" == "1" ]] && mcl=0
  echo \
    --validators "$(vals_csv)" \
    --scheme "$SCHEME" \
    --keys-dir "$KEYS" \
    --load-wallets "$LOAD_WALLETS" \
    --bond "$BOND" \
    --network-id "$NETWORK_ID" \
    --seal-interval-ms "$SEAL_MS" \
    --pack-budget-ms "$PACK_BUDGET_MS" \
    --progress-timeout-ms "$PROGRESS_TIMEOUT_MS" \
    --max-mempool-txs "$MAX_MEMPOOL_TXS" \
    --epoch-ms "$EPOCH_MS" \
    --unbond-cooldown-ms "$UNBOND_CD_MS" \
    --snapshot-interval-ms "$SNAPSHOT_INTERVAL_MS" \
    --max-chain-ledgers "$mcl"
}

stop_local() {
  if [[ -f "$RUN/local.pids" ]]; then
    while read -r pid; do kill "$pid" 2>/dev/null || true; done <"$RUN/local.pids"
    rm -f "$RUN/local.pids"
  fi
  # free ports
  local p
  for ((p = 0; p < F1_N; p++)); do
    fuser -k "$((F1_PORT0 + p))/tcp" 2>/dev/null || true
  done
}

stop_remote() {
  local host="$1" port0="$2" n="$3"
  ssh_h "$host" bash -s <<EOF
set -e
if [[ -f $ROOT/run/remote.pids ]]; then
  while read -r pid; do kill \$pid 2>/dev/null || true; done < $ROOT/run/remote.pids
  rm -f $ROOT/run/remote.pids
fi
for ((p=0;p<$n;p++)); do fuser -k \$(($port0 + p))/tcp 2>/dev/null || true; done
# watchers
if [[ -f $ROOT/run/watcher.pids ]]; then
  while read -r pid; do kill \$pid 2>/dev/null || true; done < $ROOT/run/watcher.pids
  rm -f $ROOT/run/watcher.pids
fi
EOF
}

stop_all() {
  stop_local
  stop_remote "$SSH_F2" "$F2_PORT0" "$F2_N"
  stop_remote "$SSH_F3" "$F3_PORT0" "$F3_N"
  echo stopped
}

start_local_nodes() {
  : >"$RUN/local.pids"
  local id host port fh where peers flags
  while read -r id host port fh where; do
    [[ "$where" == "local" ]] || continue
    peers=$(peer_list_for "$id")
    # shellcheck disable=SC2046
    flags=($(common_flags "$fh"))
    nohup "$BIN/falcon-pl-node" \
      --id "$id" \
      --listen "0.0.0.0:${port}" \
      --peers "$peers" \
      --snapshot "$SNAP/${id}.json" \
      --snapshot-out "$SNAP/${id}.json" \
      "${flags[@]}" \
      >>"$LOG/${id}.log" 2>&1 &
    echo $! >>"$RUN/local.pids"
    echo "F1 start $id :$port hist=$fh pid=$!"
  done < <(layout_rows)
}

start_remote_nodes() {
  local where_filter="$1" # f2|f3
  local ssh_host="$2"
  local port0="$3"
  # Build remote start script
  local rows="" id host port fh where peers mcl
  while read -r id host port fh where; do
    [[ "$where" == "$where_filter" ]] || continue
    peers=$(peer_list_for "$id")
    mcl=$MAX_CHAIN_PRUNE
    [[ "$fh" == "1" ]] && mcl=0
    rows+="start_one $id $port $mcl $peers"$'\n'
  done < <(layout_rows)

  ssh_h "$ssh_host" bash -s <<EOF
set -euo pipefail
ROOT="$ROOT"
BIN="\$ROOT/bin"
KEYS="\$ROOT/keys"
LOG="\$ROOT/logs"
SNAP="\$ROOT/snapshots"
RUN="\$ROOT/run"
mkdir -p "\$LOG" "\$SNAP" "\$RUN"
: > "\$RUN/remote.pids"
VALS="$(vals_csv)"
SCHEME="$SCHEME"
LOAD_WALLETS="$LOAD_WALLETS"
BOND="$BOND"
NETWORK_ID="$NETWORK_ID"
SEAL_MS="$SEAL_MS"
PACK_BUDGET_MS="$PACK_BUDGET_MS"
PROGRESS_TIMEOUT_MS="$PROGRESS_TIMEOUT_MS"
MAX_MEMPOOL_TXS="$MAX_MEMPOOL_TXS"
EPOCH_MS="$EPOCH_MS"
UNBOND_CD_MS="$UNBOND_CD_MS"
SNAPSHOT_INTERVAL_MS="$SNAPSHOT_INTERVAL_MS"

start_one() {
  local id="\$1" port="\$2" mcl="\$3" peers="\$4"
  nohup "\$BIN/falcon-pl-node" \\
    --id "\$id" \\
    --listen "0.0.0.0:\${port}" \\
    --peers "\$peers" \\
    --validators "\$VALS" \\
    --scheme "\$SCHEME" \\
    --keys-dir "\$KEYS" \\
    --load-wallets "\$LOAD_WALLETS" \\
    --bond "\$BOND" \\
    --network-id "\$NETWORK_ID" \\
    --seal-interval-ms "\$SEAL_MS" \\
    --pack-budget-ms "\$PACK_BUDGET_MS" \\
    --progress-timeout-ms "\$PROGRESS_TIMEOUT_MS" \\
    --max-mempool-txs "\$MAX_MEMPOOL_TXS" \\
    --epoch-ms "\$EPOCH_MS" \\
    --unbond-cooldown-ms "\$UNBOND_CD_MS" \\
    --snapshot-interval-ms "\$SNAPSHOT_INTERVAL_MS" \\
    --max-chain-ledgers "\$mcl" \\
    --snapshot "\$SNAP/\${id}.json" \\
    --snapshot-out "\$SNAP/\${id}.json" \\
    >>"\$LOG/\${id}.log" 2>&1 &
  echo \$! >> "\$RUN/remote.pids"
  echo "remote start \$id :\$port pid=\$!"
}
$rows
EOF
}

start_watchers() {
  # Lightweight watcher agents on droplet: periodic watcher-credit ledger txs.
  [[ "$N_WATCHERS" -gt 0 ]] || return 0
  local primary="${F1_IP}:${F1_PORT0}"
  # Write agent script locally then sync/run on droplet
  cat >"$RUN/watcher-agent.sh" <<'AGENT'
#!/usr/bin/env bash
set -euo pipefail
CTL="${1:?}"
ADDR="${2:?}"
ACCOUNT="${3:?}"
SCHEME="${4:?}"
KEYS="${5:?}"
while true; do
  "$CTL" --addr "$ADDR" watcher-credit \
    --account "$ACCOUNT" --work 10 --presence-slots 1 \
    --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null 2>&1 || true
  sleep 30
done
AGENT
  chmod +x "$RUN/watcher-agent.sh"
  rsync -az -e "ssh ${SSH_OPTS[*]}" "$RUN/watcher-agent.sh" "$SSH_F3:$ROOT/run/"
  ssh_h "$SSH_F3" bash -s <<EOF
set -euo pipefail
ROOT="$ROOT"
: > "\$ROOT/run/watcher.pids"
for ((w=1; w<=$N_WATCHERS; w++)); do
  nohup bash "\$ROOT/run/watcher-agent.sh" \
    "\$ROOT/bin/falcon-pl-ctl" "$primary" "watcher\$w" "$SCHEME" "\$ROOT/keys" \
    >>"\$ROOT/logs/watcher\$w.log" 2>&1 &
  echo \$! >> "\$ROOT/run/watcher.pids"
  echo "watcher\$w pid=\$! → $primary"
done
EOF
}

fund_watchers() {
  local primary="127.0.0.1:${F1_PORT0}" w
  for ((w = 1; w <= N_WATCHERS; w++)); do
    "$BIN/falcon-pl-ctl" --addr "$primary" faucet \
      --to "watcher$w" --amount 1000000 \
      --scheme "$SCHEME" --keys-dir "$KEYS" >/dev/null 2>&1 || true
  done
  # nudge pack
  for _ in 1 2 3 4 5; do
    "$BIN/falcon-pl-ctl" --addr "$primary" seal >/dev/null 2>&1 || true
    sleep 0.3
  done
  echo "funded $N_WATCHERS watcher accounts (best-effort)"
}

start_all() {
  preflight
  if [[ ! -f "$KEYS/pl_keys.json" ]]; then
    genesis
  else
    build
    layout_rows >"$RUN/layout.txt"
    vals_csv >"$RUN/validators.txt"
  fi
  stop_all
  rm -rf "$SNAP"/* 2>/dev/null || true
  : >"$LOG/.keep"
  # wipe remote snaps
  ssh_h "$SSH_F2" "mkdir -p $ROOT/logs $ROOT/snapshots; rm -rf $ROOT/snapshots/*" || true
  ssh_h "$SSH_F3" "mkdir -p $ROOT/logs $ROOT/snapshots; rm -rf $ROOT/snapshots/*" || true

  sync_remotes
  start_local_nodes
  sleep 1
  start_remote_nodes f2 "$SSH_F2" "$F2_PORT0"
  sleep 1
  start_remote_nodes f3 "$SSH_F3" "$F3_PORT0"
  sleep 4
  fund_watchers
  start_watchers
  sleep 2
  status_all
  echo "private testnet up (genesis mesh)"
  echo "primary RPC: 127.0.0.1:${F1_PORT0}  (also ${F1_IP}:${F1_PORT0})"
  echo "stop: $0 stop"
}

# --- status / load / soak ----------------------------------------------------

parse_tip() {
  python3 -c "
import sys,json
raw=sys.stdin.read(); i=raw.find('{')
if i<0:
  print('down'); sys.exit(0)
b=json.JSONDecoder().raw_decode(raw[i:])[0]
body=b.get('body',b)
print('id', body.get('id'), 'tip', body.get('tip_height'),
      'hash', (body.get('tip_hash') or '')[:12],
      'mp', body.get('mempool'), 'epoch', body.get('epoch'),
      'chain', body.get('chain_len'), 'lag', body.get('tip_lag'))
"
}

status_one() {
  local where="$1" port="$2" out
  set +e
  if [[ "$where" == "local" ]]; then
    out=$("$BIN/falcon-pl-ctl" --addr "127.0.0.1:${port}" status 2>/dev/null)
  elif [[ "$where" == "f2" ]]; then
    out=$(ssh_h "$SSH_F2" "$ROOT/bin/falcon-pl-ctl --addr 127.0.0.1:${port} status" 2>/dev/null)
  else
    out=$(ssh_h "$SSH_F3" "$ROOT/bin/falcon-pl-ctl --addr 127.0.0.1:${port} status" 2>/dev/null)
  fi
  set -e
  if [[ -z "${out:-}" ]]; then
    echo down
    return 0
  fi
  printf '%s\n' "$out" | parse_tip || echo down
}

status_all() {
  local id host port fh where layoutf
  layoutf="$RUN/layout.txt"
  if [[ ! -s "$layoutf" ]]; then
    layout_rows >"$layoutf"
  fi
  while read -r id host port fh where; do
    [[ -n "${id:-}" ]] || continue
    echo "=== $id ${host}:${port} hist=$fh ==="
    status_one "$where" "$port"
  done <"$layoutf"
}

# Check all tips agree (Δ≤1, same hash)
sync_check() {
  PL_BIN="$BIN" PL_ROOT="$ROOT" PL_SSH_F2="$SSH_F2" PL_SSH_F3="$SSH_F3" \
  PL_F1_N="$F1_N" PL_F2_N="$F2_N" PL_F3_N="$F3_N" \
  PL_F1_PORT0="$F1_PORT0" PL_F2_PORT0="$F2_PORT0" PL_F3_PORT0="$F3_PORT0" \
  python3 - <<'PY'
import json, os, subprocess, sys

def tip_local(port):
    o = subprocess.check_output(
        [os.environ["PL_BIN"] + "/falcon-pl-ctl", "--addr", f"127.0.0.1:{port}", "status"],
        text=True,
    )
    b = json.JSONDecoder().raw_decode(o[o.find("{"):])[0]
    body = b.get("body", b)
    return int(body.get("tip_height") or 0), (body.get("tip_hash") or "")[:16]

def tip_ssh(host, port):
    o = subprocess.check_output(
        ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host,
         f"{os.environ['PL_ROOT']}/bin/falcon-pl-ctl --addr 127.0.0.1:{port} status"],
        text=True,
    )
    b = json.JSONDecoder().raw_decode(o[o.find("{"):])[0]
    body = b.get("body", b)
    return int(body.get("tip_height") or 0), (body.get("tip_hash") or "")[:16]

rows = []
f1n, f2n, f3n = int(os.environ["PL_F1_N"]), int(os.environ["PL_F2_N"]), int(os.environ["PL_F3_N"])
p0 = int(os.environ["PL_F1_PORT0"])
for i in range(f1n):
    h, hh = tip_local(p0 + i)
    rows.append((f"F1:v{i+1}:{p0+i}", h, hh))
base = f1n
p0 = int(os.environ["PL_F2_PORT0"])
for i in range(f2n):
    h, hh = tip_ssh(os.environ["PL_SSH_F2"], p0 + i)
    rows.append((f"F2:v{base+i+1}:{p0+i}", h, hh))
base += f2n
p0 = int(os.environ["PL_F3_PORT0"])
for i in range(f3n):
    h, hh = tip_ssh(os.environ["PL_SSH_F3"], p0 + i)
    rows.append((f"F3:v{base+i+1}:{p0+i}", h, hh))

# Optional live joiners: run/joiners.txt lines "id host port" (e.g. v6 host2 19113)
joiners = os.path.join(os.environ["PL_ROOT"], "run", "joiners.txt")
if os.path.isfile(joiners):
    for line in open(joiners):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) < 3:
            continue
        jid, host, port = parts[0], parts[1], int(parts[2])
        try:
            if host in ("local", "host1", "127.0.0.1"):
                h, hh = tip_local(port)
            else:
                h, hh = tip_ssh(host, port)
            rows.append((f"JOIN:{jid}:{host}:{port}", h, hh))
        except Exception as e:
            print(f"JOIN:{jid}:{host}:{port} DOWN {e}")
            rows.append((f"JOIN:{jid}:{host}:{port}", -1, ""))

for r in rows:
    print(r)
heights = [r[1] for r in rows if r[1] >= 0]
if not heights:
    print("SYNC_FAIL no_nodes")
    sys.exit(2)
hashes = {r[2] for r in rows if r[1] >= 0 and r[2]}
ok = max(heights) - min(heights) <= 1 and len(hashes) <= 1
print("SYNC_OK" if ok else "SYNC_FAIL", f"height_span={max(heights)-min(heights)} hashes={len(hashes)}")
sys.exit(0 if ok else 2)
PY
}

load_light() {
  # Small steady load on primary (~2 tps for SOAK_SEC or default 60s)
  local primary="127.0.0.1:${F1_PORT0}"
  local sec="${PL_LOAD_SEC:-60}"
  local tps="${PL_LOAD_TPS:-2}"
  echo "[load-light] ${sec}s @ ~${tps} tps → $primary"
  python3 - "$BIN" "$primary" "$KEYS" "$SCHEME" "$sec" "$tps" <<'PY'
import subprocess, sys, time
bin, addr, keys, scheme, sec, tps = sys.argv[1:7]
sec, tps = int(sec), float(tps)
interval = max(0.05, 1.0 / tps)
end = time.time() + sec
ok = err = 0
while time.time() < end:
    r = subprocess.run(
        [f"{bin}/falcon-pl-ctl", "--addr", addr, "pay",
         "--from", "alice", "--to", "bob", "--amount", "1", "--fee", "2",
         "--sequence", "0", "--scheme", scheme, "--keys-dir", keys],
        capture_output=True, text=True,
    )
    if r.returncode == 0 and "ok" in (r.stdout + r.stderr).lower():
        ok += 1
    else:
        err += 1
    time.sleep(interval)
print(f"[load-light] done ok={ok} err={err}")
PY
  status_all
  sync_check || true
}

soak_12h() {
  # Background: light load + tip sync every ~5 min for 12 hours
  local hours="${PL_SOAK_HOURS:-12}"
  local tps="${PL_LOAD_TPS:-1}"
  local logf="$LOG/soak-${hours}h.log"
  local self="$SCRIPT_DIR/pl-private-testnet-v22.sh"
  echo "[soak] starting ${hours}h light load (~${tps} tps) + sync monitor → $logf"
  nohup bash -c "
    set +e
    SELF='$self'
    HOURS='$hours'
    TPS='$tps'
    END=\$((SECONDS + HOURS*3600))
    while (( SECONDS < END )); do
      date -u +%Y-%m-%dT%H:%M:%SZ
      PL_LOAD_SEC=60 PL_LOAD_TPS=\$TPS bash \"\$SELF\" load-light
      bash \"\$SELF\" sync-check || echo SYNC_FAIL
      bash \"\$SELF\" status
      sleep 240
    done
    echo SOAK_DONE
  " >>"$logf" 2>&1 &
  echo $! >"$RUN/soak.pid"
  echo "soak pid $(cat "$RUN/soak.pid") log $logf"
  echo "tail: tail -f $logf"
}

# --- CLI ---------------------------------------------------------------------

case "$cmd" in
  preflight) preflight ;;
  build) build ;;
  genesis) genesis ;;
  start) start_all ;;
  stop) stop_all ;;
  status) status_all ;;
  sync-check|sync_check) sync_check ;;
  load-light|load) load_light ;;
  soak-12h|soak) soak_12h ;;
  fund-watchers) fund_watchers ;;
  layout) layout_rows ;;
  *)
    cat <<EOF
usage: $0 {preflight|genesis|start|stop|status|sync-check|load-light|soak-12h|fund-watchers|layout}

env (defaults start small):
  PL_F1_VALS=$F1_N   # host1 validators (v1 = full history)
  PL_F2_VALS=$F2_N   # host2 validators
  PL_F3_VALS=$F3_N   # droplet validators
  PL_WATCHERS=$N_WATCHERS
  PL_SCHEME=$SCHEME  PL_NETWORK_ID=$NETWORK_ID  PL_EPOCH_MS=$EPOCH_MS
  PL_LOAD_TPS=2  PL_LOAD_SEC=60  PL_SOAK_HOURS=12
  PL_ROOT=$ROOT

capacity (raise when ready to grow the mesh):
  PL_F1_VALS=4 PL_F2_VALS=3 PL_F3_VALS=1 PL_WATCHERS=2
EOF
    exit 1
    ;;
esac
