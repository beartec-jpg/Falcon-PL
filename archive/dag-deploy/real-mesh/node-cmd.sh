#!/usr/bin/env bash
# Helper: print start command for a node (sourced by start scripts).
# Usage: node_cmd <id> <role> <profile> <rpc> <p2p> <peers> <worker_index> <mesh_root> <bind_ip>
set -euo pipefail

node_cmd() {
  local id="$1" role="$2" profile="$3" rpc_port="$4" p2p_port="$5"
  local peers="$6" widx="$7" mesh="$8" bind_ip="$9"
  local bin="$mesh/bin/falcon-dagd"
  local data="$mesh/data/$id"
  local force=()
  [[ "$profile" != "validator" || "${FORCE_PROFILE:-0}" == "1" ]] && force=(--force-node-profile)

  mkdir -p "$data"
  # shellcheck disable=SC2086
  echo "$bin" \
    --id "$id" \
    --role "$role" \
    --node-profile "$profile" \
    "${force[@]}" \
    --rpc "${bind_ip}:${rpc_port}" \
    --p2p "${bind_ip}:${p2p_port}" \
    --peers "$peers" \
    --unl "$UNL" \
    --worker-index "$widx" \
    --worker-count "${WORKER_COUNT:-6}" \
    --network-id "$NETWORK_ID" \
    --crypto "$CRYPTO" \
    --keys-dir "$mesh/keys" \
    --wallet-dir "$mesh/wallets" \
    --require-tx-sigs --auto-sign \
    --single-packer="${SINGLE_PACKER:-false}" \
    --adaptive-batch="${ADAPTIVE_BATCH:-true}" \
    --batch-interval "${BATCH_INTERVAL:-0.05}" \
    --max-batch-txs "${MAX_BATCH_TXS:-64}" \
    --max-batch-parents 3 \
    --unl-threshold "$UNL_THRESHOLD" \
    --max-uncertified-per-creator "${MAX_UNCERTIFIED_PER_CREATOR:-8}" \
    --pack-open-cap "${PACK_OPEN_CAP:-0}" \
    --uncertified-ttl 0 \
    --open-body-repool-after "${OPEN_BODY_REPOOL_AFTER:-8}" \
    --admit-ahead-cap "${ADMIT_AHEAD_CAP:-2000}" \
    --admit-busy-uncert-age 0 \
    --conf-pool true \
    --pack-check-slot "${PACK_CHECK_SLOT:-0.25}" \
    --snapshot-interval "${SNAPSHOT_INTERVAL:-15}" \
    --data-dir "$data"
}
