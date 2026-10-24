#!/usr/bin/env bash
# Real-mesh: 6 validators + 2 archivals on host1 + host2 (LAN)
# shellcheck disable=SC2034

# LAN addresses (same-site real Ethernet/WiFi path — not Tailscale)
export F1_IP="${F1_IP:-192.0.2.1}"
export F2_IP="${F2_IP:-192.0.2.2}"

export NETWORK_ID="${NETWORK_ID:-9210}"
export CRYPTO="${CRYPTO:-falcon-512}"
export UNL="${UNL:-val1,val2,val3,val4,val5,val6}"
export UNL_THRESHOLD="${UNL_THRESHOLD:-0.6}"   # ≈4/6 equal weight
export WORKER_COUNT="${WORKER_COUNT:-6}"
export ARCH_IDS="${ARCH_IDS:-arch1,arch2}"

# Ports: vals 18001-18006 / 19001-19006 ; arch 18011-12 / 19011-12
export VAL1_RPC=18001 VAL1_P2P=19001
export VAL2_RPC=18002 VAL2_P2P=19002
export VAL3_RPC=18003 VAL3_P2P=19003
export VAL4_RPC=18004 VAL4_P2P=19004
export VAL5_RPC=18005 VAL5_P2P=19005
export VAL6_RPC=18006 VAL6_P2P=19006
export ARCH1_RPC=18011 ARCH1_P2P=19011
export ARCH2_RPC=18012 ARCH2_P2P=19012

# Host placement:
#   host1: val1 val2 val3 arch1
#   host2: val4 val5 val6 arch2
_all_peers() {
  # every node dials every other P2P port (full mesh)
  echo "\
${F1_IP}:${VAL1_P2P},\
${F1_IP}:${VAL2_P2P},\
${F1_IP}:${VAL3_P2P},\
${F2_IP}:${VAL4_P2P},\
${F2_IP}:${VAL5_P2P},\
${F2_IP}:${VAL6_P2P},\
${F1_IP}:${ARCH1_P2P},\
${F2_IP}:${ARCH2_P2P}"
}

# Per-node peer list = all minus self (daemon tolerates self in list, but cleaner without)
export PEERS_VAL1="${F1_IP}:${VAL2_P2P},${F1_IP}:${VAL3_P2P},${F2_IP}:${VAL4_P2P},${F2_IP}:${VAL5_P2P},${F2_IP}:${VAL6_P2P},${F1_IP}:${ARCH1_P2P},${F2_IP}:${ARCH2_P2P}"
export PEERS_VAL2="${F1_IP}:${VAL1_P2P},${F1_IP}:${VAL3_P2P},${F2_IP}:${VAL4_P2P},${F2_IP}:${VAL5_P2P},${F2_IP}:${VAL6_P2P},${F1_IP}:${ARCH1_P2P},${F2_IP}:${ARCH2_P2P}"
export PEERS_VAL3="${F1_IP}:${VAL1_P2P},${F1_IP}:${VAL2_P2P},${F2_IP}:${VAL4_P2P},${F2_IP}:${VAL5_P2P},${F2_IP}:${VAL6_P2P},${F1_IP}:${ARCH1_P2P},${F2_IP}:${ARCH2_P2P}"
export PEERS_VAL4="${F1_IP}:${VAL1_P2P},${F1_IP}:${VAL2_P2P},${F1_IP}:${VAL3_P2P},${F2_IP}:${VAL5_P2P},${F2_IP}:${VAL6_P2P},${F1_IP}:${ARCH1_P2P},${F2_IP}:${ARCH2_P2P}"
export PEERS_VAL5="${F1_IP}:${VAL1_P2P},${F1_IP}:${VAL2_P2P},${F1_IP}:${VAL3_P2P},${F2_IP}:${VAL4_P2P},${F2_IP}:${VAL6_P2P},${F1_IP}:${ARCH1_P2P},${F2_IP}:${ARCH2_P2P}"
export PEERS_VAL6="${F1_IP}:${VAL1_P2P},${F1_IP}:${VAL2_P2P},${F1_IP}:${VAL3_P2P},${F2_IP}:${VAL4_P2P},${F2_IP}:${VAL5_P2P},${F1_IP}:${ARCH1_P2P},${F2_IP}:${ARCH2_P2P}"
export PEERS_ARCH1="${F1_IP}:${VAL1_P2P},${F1_IP}:${VAL2_P2P},${F1_IP}:${VAL3_P2P},${F2_IP}:${VAL4_P2P},${F2_IP}:${VAL5_P2P},${F2_IP}:${VAL6_P2P},${F2_IP}:${ARCH2_P2P}"
export PEERS_ARCH2="${F1_IP}:${VAL1_P2P},${F1_IP}:${VAL2_P2P},${F1_IP}:${VAL3_P2P},${F2_IP}:${VAL4_P2P},${F2_IP}:${VAL5_P2P},${F2_IP}:${VAL6_P2P},${F1_IP}:${ARCH1_P2P}"

export MESH_ROOT_FALCON="${MESH_ROOT_FALCON:-$HOME/falcon-dag-mesh}"
export BIN_FALCON="${BIN_FALCON:-$MESH_ROOT_FALCON/bin/falcon-dagd}"

# --- Tuning pack (defaults = best measured for full-mesh agreement) ---
# v1 baseline: admit=2000 uncert=8 batch=64 open=0  → best ~22 exec TPS tight mesh
# v2: admit=512 uncert=4 batch=128 open=2           → worse (earlier skew)
# v3: admit=1000 uncert=8 batch=128 open=0          → worse agreement; more busy at extreme
export ADMIT_AHEAD_CAP="${ADMIT_AHEAD_CAP:-2000}"
export MAX_UNCERTIFIED_PER_CREATOR="${MAX_UNCERTIFIED_PER_CREATOR:-8}"
export MAX_BATCH_TXS="${MAX_BATCH_TXS:-64}"
export PACK_OPEN_CAP="${PACK_OPEN_CAP:-0}"
export BATCH_INTERVAL="${BATCH_INTERVAL:-0.05}"
export OPEN_BODY_REPOOL_AFTER="${OPEN_BODY_REPOOL_AFTER:-8}"
export SNAPSHOT_INTERVAL="${SNAPSHOT_INTERVAL:-15}"
export TUNING_TAG="${TUNING_TAG:-v1-best-measured}"
# Multipack / density experiment knobs
export SINGLE_PACKER="${SINGLE_PACKER:-false}"
export ADAPTIVE_BATCH="${ADAPTIVE_BATCH:-true}"
export PACK_CHECK_SLOT="${PACK_CHECK_SLOT:-0.25}"
# Load harness account count (genesis funds 100 by default)
export FAUCET_ACCOUNTS="${FAUCET_ACCOUNTS:-40}"
export ACCOUNTS="${ACCOUNTS:-$FAUCET_ACCOUNTS}"
