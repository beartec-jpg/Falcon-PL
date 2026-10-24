#!/usr/bin/env bash
# Falcon PL max private fleet — pack validators onto every reachable host.
#
#   bash crates/fd-pl/scripts/pl-fleet-max.sh preflight
#   bash crates/fd-pl/scripts/pl-fleet-max.sh start     # stop + genesis + deploy + start
#   bash crates/fd-pl/scripts/pl-fleet-max.sh status
#   bash crates/fd-pl/scripts/pl-fleet-max.sh lottery   # one-shot lottery board
#   bash crates/fd-pl/scripts/pl-fleet-max.sh monitor   # continuous lottery/scores
#   bash crates/fd-pl/scripts/pl-fleet-max.sh stop
#
# Capacity plan (leave headroom; prune chain on non-archive):
#   host1  15G/12c  → 4 vals (v1 archive + 3)
#   host2  15G/6c   → 3 vals
#   droplet   4G/2c   → 1 val + dashboard
#   hil-2-1   8G/4c   → 2 vals
#   fsn1-1    8G/2c   → 1 val
#   mhr       4G/2c   → 1 val
#   hel1-6    4G/2c   → 1 val
#   hel1-4 skipped (bitcoind already heavy)
# Total = 13 validators
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RS="$(cd "$SCRIPT_DIR/../../.." && pwd)"
BIN_SRC="${PL_BIN_SRC:-$RS/target/release}"
ROOT="${PL_ROOT:-$HOME/falcon-pl-private-testnet-v22}"
SCHEME="${PL_SCHEME:-hmac}"
LOAD_WALLETS="${PL_LOAD_WALLETS:-32}"
NETWORK_ID="${PL_NETWORK_ID:-2200}"
BOND="${PL_BOND:-1000}"
EPOCH_MS="${PL_EPOCH_MS:-3600000}"
UNBOND_CD_MS="${PL_UNBOND_COOLDOWN_MS:-86400000}"
SEAL_MS="${PL_SEAL_MS:-400}"
PACK_BUDGET_MS="${PL_PACK_BUDGET_MS:-1000}"
PROGRESS_TIMEOUT_MS="${PL_PROGRESS_TIMEOUT_MS:-1000}"
MAX_MEMPOOL_TXS="${PL_MAX_MEMPOOL_TXS:-20000}"
SNAPSHOT_INTERVAL_MS="${PL_SNAPSHOT_INTERVAL_MS:-60000}"
MAX_CHAIN_PRUNE="${PL_MAX_CHAIN_PRUNE:-20000}"

# host_key | ssh_spec | ts_ip | port0 | n_vals | hist_first(0|1) | remote_root
# ssh_spec: local | ssh-alias | tailscale:user@host
LAYOUT_SPEC=(
  "f1|local|100.64.0.1|19101|4|1|/home/falcon/falcon-pl-private-testnet-v22"
  "f2|ssh:host2|100.64.0.3|19111|3|0|/home/falcon/falcon-pl-private-testnet-v22"
  "f3|ssh:host3|100.64.0.5|19121|1|0|/home/falcon/falcon-pl-private-testnet-v22"
  "hil|ts:root@seat-host-4|100.64.0.2|19131|2|0|/root/falcon-pl-private-testnet-v22"
  "fsn|ts:root@seat-host-5|100.64.0.4|19141|1|0|/root/falcon-pl-private-testnet-v22"
  "mhr|ts:root@seat-host-6|100.64.0.6|19151|1|0|/root/falcon-pl-private-testnet-v22"
  "hel6|ts:root@seat-host-7|100.64.0.7|19161|1|0|/root/falcon-pl-private-testnet-v22"
)

SSH_OPTS=(-o ConnectTimeout=12 -o BatchMode=yes -o StrictHostKeyChecking=accept-new)
cmd="${1:-preflight}"

mkdir -p "$ROOT"/{bin,keys,logs,snapshots,run}

# --- helpers -----------------------------------------------------------------

layout_rows() {
  # id ip port hist host_key ssh remote_root
  local i=1 entry key ssh ip p0 n hist rroot p
  for entry in "${LAYOUT_SPEC[@]}"; do
    IFS='|' read -r key ssh ip p0 n hist rroot <<<"$entry"
    for ((p = 0; p < n; p++)); do
      echo "v$i $ip $((p0 + p)) $([[ $p -eq 0 && $hist -eq 1 ]] && echo 1 || echo 0) $key $ssh $rroot"
      i=$((i + 1))
    done
  done
}

vals_csv() {
  layout_rows | awk '{printf "%s%s", (NR>1?",":""), $1} END{print ""}'
}

total_vals() { layout_rows | wc -l; }

remote_run() {
  # remote_run <ssh_spec> <bash-script-body>
  local ssh_spec="$1"
  shift
  local body="$*"
  case "$ssh_spec" in
    local)
      bash -c "$body"
      ;;
    ssh:*)
      ssh "${SSH_OPTS[@]}" "${ssh_spec#ssh:}" "bash -s" <<<"$body"
      ;;
    ts:*)
      # tailscale ssh does not take OpenSSH -o the same way; use timeout
      timeout 90 tailscale ssh "${ssh_spec#ts:}" "bash -s" <<<"$body"
      ;;
    *)
      echo "bad ssh_spec $ssh_spec" >&2
      return 2
      ;;
  esac
}

remote_rsync() {
  # remote_rsync <ssh_spec> <local_src_dir_or_files...> <remote_dest>
  local ssh_spec="$1"
  shift
  local dest="${*: -1}"
  local src=("${@:1:$#-1}")
  case "$ssh_spec" in
    local)
      mkdir -p "$dest"
      rsync -a "${src[@]}" "$dest/"
      ;;
    ssh:*)
      local host="${ssh_spec#ssh:}"
      ssh "${SSH_OPTS[@]}" "$host" "mkdir -p $dest"
      rsync -az -e "ssh ${SSH_OPTS[*]}" "${src[@]}" "$host:$dest/"
      ;;
    ts:*)
      local thost="${ssh_spec#ts:}"
      # tailscale ssh has no native rsync transport; tar pipe
      timeout 60 tailscale ssh "$thost" "mkdir -p $dest"
      tar -C "$(dirname "${src[0]}")" -czf - $(for s in "${src[@]}"; do basename "$s"; done) \
        | timeout 120 tailscale ssh "$thost" "tar -C $dest -xzf -"
      # if src are absolute different dirs, fall back to per-file base names from ROOT/bin
      ;;
    *)
      return 2
      ;;
  esac
}

# --- commands ----------------------------------------------------------------

preflight() {
  echo "======== PL max-fleet preflight ========"
  echo "ROOT=$ROOT network_id=$NETWORK_ID total_vals=$(total_vals)"
  echo "validators: $(vals_csv)"
  echo "--- layout ---"
  layout_rows | while read -r id ip port hist key ssh rroot; do
    echo "  $id  $ip:$port  hist=$hist  host=$key  via=$ssh  root=$rroot"
  done
  echo "--- connectivity ---"
  local seen_ssh=""
  layout_rows | while read -r id ip port hist key ssh rroot; do
    case " $seen_ssh " in
      *" $ssh "*) continue ;;
    esac
    seen_ssh+=" $ssh"
    if remote_run "$ssh" "echo OK \$(hostname) \$(whoami); free -h | head -2; nproc"; then
      echo "  [$key] OK"
    else
      echo "  [$key] FAIL via $ssh"
    fi
  done
}

build() {
  cd "$RS"
  cargo build -p fd-pl --release 2>&1 | tail -6
  [[ -x "$BIN_SRC/falcon-pl-node" && -x "$BIN_SRC/falcon-pl-ctl" ]]
  cp -f "$BIN_SRC/falcon-pl-node" "$BIN_SRC/falcon-pl-ctl" "$ROOT/bin/"
  chmod +x "$ROOT/bin/"*
}

stop_all() {
  echo "stopping all fleet nodes..."
  local seen_ssh=""
  layout_rows | while read -r id ip port hist key ssh rroot; do
    case " $seen_ssh " in
      *" $ssh "*) continue ;;
    esac
    seen_ssh+=" $ssh"
    remote_run "$ssh" "
      set +e
      RROOT='$rroot'
      if [[ -f \$RROOT/run/fleet.pids ]]; then
        while read -r pid; do kill \$pid 2>/dev/null; done < \$RROOT/run/fleet.pids
        rm -f \$RROOT/run/fleet.pids
      fi
      pkill -x falcon-pl-node 2>/dev/null || true
      # free port range roughly
      for p in \$(seq 19101 19170); do fuser -k \${p}/tcp 2>/dev/null; done
      true
    " || true
  done
  echo stopped
}

genesis() {
  build
  rm -rf "$ROOT/keys" "$ROOT/snapshots" "$ROOT/run"
  mkdir -p "$ROOT"/{keys,logs,snapshots,run}
  local vals
  vals=$(vals_csv)
  "$ROOT/bin/falcon-pl-ctl" gen-keys \
    --keys-dir "$ROOT/keys" \
    --scheme "$SCHEME" \
    --validators "$vals" \
    --load-wallets "$LOAD_WALLETS"
  "$ROOT/bin/falcon-pl-ctl" gen-wallet --keys-dir "$ROOT/keys" --account faucet --scheme "$SCHEME" || true
  layout_rows >"$ROOT/run/layout.txt"
  echo "$vals" >"$ROOT/run/validators.txt"
  cat >"$ROOT/run/genesis.env" <<EOF
NETWORK_ID=$NETWORK_ID
SCHEME=$SCHEME
VALS=$vals
N=$(total_vals)
EPOCH_MS=$EPOCH_MS
BOND=$BOND
PRODUCT=2.2.2
EOF
  echo "genesis → $vals ($(total_vals) validators)"
  cat "$ROOT/run/layout.txt"
}

deploy() {
  echo "deploy bin+keys to remotes..."
  # unique ssh targets
  local seen_ssh=""
  layout_rows | while read -r id ip port hist key ssh rroot; do
    case " $seen_ssh " in
      *" $ssh "*) continue ;;
    esac
    seen_ssh+=" $ssh"
    if [[ "$ssh" == "local" ]]; then
      mkdir -p "$rroot"/{bin,keys,logs,snapshots,run}
      rsync -a "$ROOT/bin/" "$rroot/bin/"
      rsync -a --delete "$ROOT/keys/" "$rroot/keys/"
      rsync -a "$ROOT/run/" "$rroot/run/"
      echo "  [$key] local sync"
      continue
    fi
    # package stage
    local stage="$ROOT/run/stage-$key"
    rm -rf "$stage"
    mkdir -p "$stage"/{bin,keys,run}
    cp -a "$ROOT/bin/." "$stage/bin/"
    cp -a "$ROOT/keys/." "$stage/keys/"
    cp -a "$ROOT/run/layout.txt" "$ROOT/run/validators.txt" "$ROOT/run/genesis.env" "$stage/run/" 2>/dev/null || true
    case "$ssh" in
      ssh:*)
        local host="${ssh#ssh:}"
        ssh "${SSH_OPTS[@]}" "$host" "mkdir -p $rroot/{bin,keys,logs,snapshots,run}"
        rsync -az --delete -e "ssh ${SSH_OPTS[*]}" "$stage/bin/" "$host:$rroot/bin/"
        rsync -az --delete -e "ssh ${SSH_OPTS[*]}" "$stage/keys/" "$host:$rroot/keys/"
        rsync -az -e "ssh ${SSH_OPTS[*]}" "$stage/run/" "$host:$rroot/run/"
        ;;
      ts:*)
        local thost="${ssh#ts:}"
        timeout 60 tailscale ssh "$thost" "mkdir -p $rroot/{bin,keys,logs,snapshots,run}"
        tar -C "$stage" -czf - bin keys run \
          | timeout 180 tailscale ssh "$thost" "tar -C $rroot -xzf - && chmod +x $rroot/bin/*"
        ;;
    esac
    echo "  [$key] deployed → $rroot"
  done
}

peer_list_for() {
  local self_id="$1"
  layout_rows | awk -v s="$self_id" '$1!=s {printf "%s%s:%s", (n++?",":""), $2, $3} END{print ""}'
}

start_all() {
  local vals
  vals=$(vals_csv)

  # Unique hosts (preserve order)
  local hosts
  hosts=$(layout_rows | awk '{print $5"|"$6"|"$7}' | awk '!s[$0]++')

  while IFS='|' read -r key ssh rroot; do
    [[ -n "$key" ]] || continue
    local rows="" id ip port hist key2 ssh2 rroot2 peers mcl
    while read -r id ip port hist key2 ssh2 rroot2; do
      [[ "$key2" == "$key" ]] || continue
      peers=$(peer_list_for "$id")
      mcl=$MAX_CHAIN_PRUNE
      [[ "$hist" == "1" ]] && mcl=0
      # peers may contain commas — pass as single quoted arg via start_one
      rows+="start_one $(printf '%q' "$id") $(printf '%q' "$port") $(printf '%q' "$mcl") $(printf '%q' "$peers")"$'\n'
    done < <(layout_rows)

    echo "starting host $key via $ssh ..."
    remote_run "$ssh" "
set -euo pipefail
RROOT=$(printf '%q' "$rroot")
BIN=\"\$RROOT/bin\"
KEYS=\"\$RROOT/keys\"
LOG=\"\$RROOT/logs\"
SNAP=\"\$RROOT/snapshots\"
RUN=\"\$RROOT/run\"
mkdir -p \"\$LOG\" \"\$SNAP\" \"\$RUN\"
: > \"\$RUN/fleet.pids\"
VALS=$(printf '%q' "$vals")
SCHEME=$(printf '%q' "$SCHEME")
LOAD_WALLETS=$(printf '%q' "$LOAD_WALLETS")
BOND=$(printf '%q' "$BOND")
NETWORK_ID=$(printf '%q' "$NETWORK_ID")
SEAL_MS=$(printf '%q' "$SEAL_MS")
PACK_BUDGET_MS=$(printf '%q' "$PACK_BUDGET_MS")
PROGRESS_TIMEOUT_MS=$(printf '%q' "$PROGRESS_TIMEOUT_MS")
MAX_MEMPOOL_TXS=$(printf '%q' "$MAX_MEMPOOL_TXS")
EPOCH_MS=$(printf '%q' "$EPOCH_MS")
UNBOND_CD_MS=$(printf '%q' "$UNBOND_CD_MS")
SNAPSHOT_INTERVAL_MS=$(printf '%q' "$SNAPSHOT_INTERVAL_MS")

start_one() {
  local id=\"\$1\" port=\"\$2\" mcl=\"\$3\" peers=\"\$4\"
  rm -f \"\$SNAP/\${id}.json\"
  nohup \"\$BIN/falcon-pl-node\" \\
    --id \"\$id\" \\
    --listen \"0.0.0.0:\${port}\" \\
    --peers \"\$peers\" \\
    --validators \"\$VALS\" \\
    --scheme \"\$SCHEME\" \\
    --keys-dir \"\$KEYS\" \\
    --load-wallets \"\$LOAD_WALLETS\" \\
    --bond \"\$BOND\" \\
    --network-id \"\$NETWORK_ID\" \\
    --seal-interval-ms \"\$SEAL_MS\" \\
    --pack-budget-ms \"\$PACK_BUDGET_MS\" \\
    --progress-timeout-ms \"\$PROGRESS_TIMEOUT_MS\" \\
    --max-mempool-txs \"\$MAX_MEMPOOL_TXS\" \\
    --epoch-ms \"\$EPOCH_MS\" \\
    --unbond-cooldown-ms \"\$UNBOND_CD_MS\" \\
    --snapshot-interval-ms \"\$SNAPSHOT_INTERVAL_MS\" \\
    --max-chain-ledgers \"\$mcl\" \\
    --snapshot \"\$SNAP/\${id}.json\" \\
    --snapshot-out \"\$SNAP/\${id}.json\" \\
    >>\"\$LOG/\${id}.log\" 2>&1 &
  echo \$! >> \"\$RUN/fleet.pids\"
  echo \"start \$id :\$port pid=\$! mcl=\$mcl\"
}
$rows
"
  done <<<"$hosts"
  sleep 2
  echo "fleet started"
}

status_all() {
  local CTL="$ROOT/bin/falcon-pl-ctl"
  layout_rows | while read -r id ip port hist key ssh rroot; do
    echo -n "=== $id $ip:$port === "
    if out=$("$CTL" --addr "$ip:$port" status 2>/dev/null); then
      echo "$out" | python3 -c "
import sys,json
raw=sys.stdin.read(); b=json.JSONDecoder().raw_decode(raw[raw.find('{'):])[0]
body=b.get('body',b)
vals=body.get('validators') or []
ready=sum(1 for v in vals if v.get('lottery_ready'))
print(f\"tip={body.get('tip_height')} lag={body.get('tip_lag')} chain={body.get('chain_len')} winner={body.get('lottery_winner_next')} ready={ready}/{len(vals)} pv={body.get('product_version')}\")
"
    else
      echo DOWN
    fi
  done
}

lottery_board() {
  local CTL="$ROOT/bin/falcon-pl-ctl"
  # use first up node as primary
  local primary=""
  while read -r id ip port hist key ssh rroot; do
    if "$CTL" --addr "$ip:$port" status >/dev/null 2>&1; then
      primary="$ip:$port"
      break
    fi
  done < <(layout_rows)
  if [[ -z "$primary" ]]; then
    echo "no primary up"
    return 1
  fi
  "$CTL" --addr "$primary" status 2>/dev/null | python3 -c "
import sys,json,time
raw=sys.stdin.read(); b=json.JSONDecoder().raw_decode(raw[raw.find('{'):])[0]
body=b.get('body',b)
vals=sorted(body.get('validators') or [], key=lambda v: -(v.get('pack_count') or 0))
print('='*72)
print(f\"Falcon PL lottery board  tip={body.get('tip_height')}  hash={(body.get('tip_hash') or '')[:16]}\")
print(f\"winner={body.get('lottery_winner_next')}  standby={body.get('lottery_standby_next')}  ranked={body.get('lottery_ranked_next')}\")
print(f\"network={body.get('network')}  product={body.get('product_version')}  params={body.get('params_version')}\")
print('-'*72)
print(f\"{'id':6} {'bond':>6} {'ready':>6} {'pong_ms':>8} {'packs':>7} {'perf':>7} {'tip_adv':>8}\")
for v in vals:
  age=v.get('pong_age_ms')
  age_s='—' if age is None else str(age)
  print(f\"{v.get('id'):6} {v.get('bond') or 0:6} {str(bool(v.get('lottery_ready'))):>6} {age_s:>8} {v.get('pack_count') or 0:7} {float(v.get('perf') or 0):7.3f} {str(v.get('last_advertised_tip') or '—'):>8}\")
print('='*72)
"
}

monitor_loop() {
  while true; do
    clear 2>/dev/null || true
    date -u +'%Y-%m-%d %H:%M:%S UTC'
    lottery_board || true
    echo
    status_all || true
    sleep "${PL_MONITOR_SEC:-5}"
  done
}

# --- main --------------------------------------------------------------------

case "$cmd" in
  preflight) preflight ;;
  build) build ;;
  genesis) genesis ;;
  deploy) deploy ;;
  stop) stop_all ;;
  start)
    stop_all
    genesis
    deploy
    start_all
    sleep 3
    status_all
    lottery_board
    ;;
  status) status_all ;;
  lottery) lottery_board ;;
  monitor) monitor_loop ;;
  layout) layout_rows ;;
  *)
    echo "usage: $0 {preflight|start|stop|status|lottery|monitor|genesis|deploy|layout}"
    exit 2
    ;;
esac
