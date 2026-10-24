#!/usr/bin/env bash
# Kit M1 attest end to end with stub falcon-pl-ctl / bitcoin-cli and a snarkjs v1 key
# (dry-run key from tools/fpl-bridge/vk_setup_dryrun_v1.sh or ceremony/dryrun_local.sh).
#   ZKEY=v1_final.zkey VK_JSON=v1_vk.json FPL_SETUP=.. FPL_ATTEST=.. SNARKJS=.. tests/attest_snarkjs_stub.sh
set -euo pipefail
D=$(cd "$(dirname "$0")/attest-stub" && pwd); rm -f "$D/submitted_body.json"
VKHEX=$("$FPL_SETUP" vk-import --vk "$VK_JSON" | python3 -c 'import json,sys;print(json.load(sys.stdin)["vk_hex"])')
j() { python3 -c "import json;print(json.load(open('$D/fx.json'))['$1'])"; }
FPL_ATTEST_NOTE=$(j note) FPL_ATTEST_PEGOUT_TXID=$(j pegout_txid) FPL_ATTEST_CLAIM_TXID=$(printf 'cc%.0s' {1..32}) \
FPL_OP_FALCON_CTL=$D/fake-ctl FPL_OP_FALCON_RPC=x FPL_OP_FALCON_ACCOUNT=op1 FPL_OP_BITCOIN_CLI=$D/fake-bitcoin-cli FPL_OP_BITCOIN_ARGS= \
FPL_ZKEY=$ZKEY FPL_VK_JSON=$VK_JSON FPL_VK_HEX=$VKHEX "$(dirname "$0")/../scripts/fpl-operator-attest.sh"
python3 -c "import json;b=json.load(open('$D/submitted_body.json'));assert b['kind']=='bitvm2_attest_v2';print('KIT ATTEST OK', b['attest']['note_id'], 'ckpt', b['attest']['checkpoint_height'])"
