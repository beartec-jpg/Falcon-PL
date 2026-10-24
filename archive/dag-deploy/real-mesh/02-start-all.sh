#!/usr/bin/env bash
# Start 6 vals + 2 arch: f1=val1-3+arch1, f2=val4-6+arch2
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=/dev/null
source "$ROOT/env.sh"
# shellcheck source=/dev/null
source "$ROOT/node-cmd.sh"

start_local() {
  local id="$1" role="$2" profile="$3" rpc="$4" p2p="$5" peers="$6" widx="$7"
  local mesh="$MESH_ROOT_FALCON"
  local log="$mesh/logs/${id}.log"
  local pidf="$mesh/logs/${id}.pid"
  mkdir -p "$mesh/logs" "$mesh/data/$id"
  if [[ -f "$pidf" ]] && kill -0 "$(cat "$pidf")" 2>/dev/null; then
    echo "already running $id pid=$(cat "$pidf")"
    return 0
  fi
  # shellcheck disable=SC2046
  nohup $(node_cmd "$id" "$role" "$profile" "$rpc" "$p2p" "$peers" "$widx" "$mesh" "0.0.0.0") \
    >"$log" 2>&1 &
  echo $! >"$pidf"
  echo "started $id pid=$! log=$log"
}

start_remote() {
  local host="$1" mesh="$2" id="$3" role="$4" profile="$5" rpc="$6" p2p="$7" peers="$8" widx="$9"
  ssh "$host" "bash -s" <<EOF
set -euo pipefail
MESH="$mesh"
BIN="\$MESH/bin/falcon-dagd"
ID="$id"
LOG="\$MESH/logs/\${ID}.log"
PIDF="\$MESH/logs/\${ID}.pid"
mkdir -p "\$MESH/data/\$ID" "\$MESH/logs"
if [[ -f "\$PIDF" ]] && kill -0 "\$(cat "\$PIDF")" 2>/dev/null; then
  echo "already running \$ID"
  exit 0
fi
FORCE=""
if [[ "$profile" != "validator" ]]; then FORCE="--force-node-profile"; fi
nohup "\$BIN" \\
  --id "\$ID" --role "$role" --node-profile "$profile" \$FORCE \\
  --rpc "0.0.0.0:${rpc}" --p2p "0.0.0.0:${p2p}" \\
  --peers "$peers" --unl "$UNL" \\
  --worker-index "$widx" --worker-count "$WORKER_COUNT" \\
  --network-id "$NETWORK_ID" --crypto "$CRYPTO" \\
  --keys-dir "\$MESH/keys" --wallet-dir "\$MESH/wallets" \\
  --require-tx-sigs --auto-sign \\
  --single-packer=$SINGLE_PACKER --adaptive-batch=$ADAPTIVE_BATCH \\
  --batch-interval "$BATCH_INTERVAL" --max-batch-txs "$MAX_BATCH_TXS" --max-batch-parents 3 \\
  --unl-threshold "$UNL_THRESHOLD" \\
  --max-uncertified-per-creator "$MAX_UNCERTIFIED_PER_CREATOR" --pack-open-cap "$PACK_OPEN_CAP" \\
  --uncertified-ttl 0 --open-body-repool-after "$OPEN_BODY_REPOOL_AFTER" \\
  --admit-ahead-cap "$ADMIT_AHEAD_CAP" --admit-busy-uncert-age 0 \\
  --conf-pool true --pack-check-slot "$PACK_CHECK_SLOT" \\
  --snapshot-interval "$SNAPSHOT_INTERVAL" --data-dir "\$MESH/data/\$ID" \\
  >"\$LOG" 2>&1 &
echo \$! >"\$PIDF"
echo "started \$ID pid=\$(cat "\$PIDF") on $host"
EOF
}

echo "=== topology: UNL=$UNL workers=$WORKER_COUNT network=$NETWORK_ID crypto=$CRYPTO ==="
echo "=== tuning: $TUNING_TAG admit=$ADMIT_AHEAD_CAP uncert=$MAX_UNCERTIFIED_PER_CREATOR batch=$MAX_BATCH_TXS open=$PACK_OPEN_CAP single=$SINGLE_PACKER adaptive=$ADAPTIVE_BATCH slot=$PACK_CHECK_SLOT ==="
echo "=== peers via LAN F1=$F1_IP F2=$F2_IP ==="
echo "=== host1: val1 val2 val3 arch1 ==="
start_local val1 full validator "$VAL1_RPC" "$VAL1_P2P" "$PEERS_VAL1" 0
start_local val2 full validator "$VAL2_RPC" "$VAL2_P2P" "$PEERS_VAL2" 1
start_local val3 full validator "$VAL3_RPC" "$VAL3_P2P" "$PEERS_VAL3" 2
start_local arch1 order-exec archival "$ARCH1_RPC" "$ARCH1_P2P" "$PEERS_ARCH1" 0

echo "=== host2: val4 val5 val6 arch2 ==="
start_remote host2 "$MESH_ROOT_FALCON" val4 full validator "$VAL4_RPC" "$VAL4_P2P" "$PEERS_VAL4" 3
start_remote host2 "$MESH_ROOT_FALCON" val5 full validator "$VAL5_RPC" "$VAL5_P2P" "$PEERS_VAL5" 4
start_remote host2 "$MESH_ROOT_FALCON" val6 full validator "$VAL6_RPC" "$VAL6_P2P" "$PEERS_VAL6" 5
start_remote host2 "$MESH_ROOT_FALCON" arch2 order-exec archival "$ARCH2_RPC" "$ARCH2_P2P" "$PEERS_ARCH2" 0

echo "waiting for RPC…"
sleep 4
"$ROOT/03-status.sh" || true
echo "start-all done"
