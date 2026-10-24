#!/usr/bin/env bash
# ANYONE: verify the published ceremony from scratch.
#   verify_ceremony.sh <dir with ppot ptau, *_final.zkey, manifest.json>   (falcon-pl-bitvm2-setup built from the tagged commit)
set -euo pipefail
H=$(cd "$(dirname "$0")" && pwd); . "$H/ceremony.env"; CIRCUITS=${CER_CIRCUITS:-$CIRCUITS}; cd "$1"
S=${FPL_SETUP:-falcon-pl-bitvm2-setup}; X=${SNARKJS:-npx snarkjs}; export NODE_OPTIONS=${NODE_OPTIONS:---max-old-space-size=8192}
[ "$PTAU_SHA256" = "__FILL__" ] || [ "$(sha256sum "$PTAU_FILE" | cut -d' ' -f1)" = "$PTAU_SHA256" ]
for c in $CIRCUITS; do
  $S r1cs --circuit $c --out $c.check.r1cs >/dev/null
  $X zkey verify $c.check.r1cs "$PTAU_FILE" ${c}_final.zkey | grep -E "contribution #|Beacon|ZKey Ok|ERROR"
  $X zkey export verificationkey ${c}_final.zkey $c.check_vk.json >/dev/null
  got=$($S vk-import --vk $c.check_vk.json | python3 -c 'import json,sys;print(json.load(sys.stdin)["vk_hex"])')
  want=$(python3 -c "import json;print(json.load(open('manifest.json'))['$c']['vk_hex'])")
  [ "$got" = "$want" ] && echo "$c: VK matches manifest" || { echo "$c: VK MISMATCH"; exit 1; }
done
