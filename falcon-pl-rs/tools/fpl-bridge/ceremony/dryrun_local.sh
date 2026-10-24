#!/usr/bin/env bash
# DRY RUN of the whole ceremony on ONE machine with throwaway entropy. The keys it makes are
# NOT usable for anything real (every contribution ran here). Checks the scripts end to end.
#   CER_DIR=/tmp/cer FPL_SETUP=... CER_CIRCUITS="v3" ./dryrun_local.sh
set -euo pipefail
H=$(cd "$(dirname "$0")" && pwd)
export CER_DIR=${CER_DIR:-$PWD/ceremony-dryrun}; mkdir -p "$CER_DIR"
[ -n "${PTAU_LOCAL:-}" ] && ln -sf "$PTAU_LOCAL" "$CER_DIR/ppot_0080_18.ptau"
"$H/coordinator.sh" init
. "$H/ceremony.env"; CIRCUITS=${CER_CIRCUITS:-$CIRCUITS}
for who in dryrun-A dryrun-B; do
  for c in $CIRCUITS; do
    cur=$(ls "$CER_DIR/${c}"_[0-9][0-9][0-9][0-9].zkey | sort | tail -1)
    CER_TYPED="$(python3 -c 'import secrets;print(secrets.token_hex(16))')" "$H/contribute.sh" $c "$cur" "$CER_DIR/in_${who}_$c.zkey" "$who" </dev/null | grep -iE "contribution hash" -A4 | head -5
    "$H/coordinator.sh" accept $c "$CER_DIR/in_${who}_$c.zkey"
  done
done
"$H/coordinator.sh" beacon 0000000000000000000000000000000000000000000000000000000000000001 dryrun
"$H/coordinator.sh" publish
"$H/verify_ceremony.sh" "$CER_DIR"
echo CEREMONY_DRYRUN_DONE
