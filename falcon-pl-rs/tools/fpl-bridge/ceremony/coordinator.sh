#!/usr/bin/env bash
# Ceremony COORDINATOR (holds no secret; can be any public machine, e.g. the Falcon box).
#   coordinator.sh init                 fetch+check phase-1 ptau, export r1cs, zkey new -> <c>_0000.zkey
#   coordinator.sh accept <c> <file>    verify an uploaded contribution, append to the transcript
#   coordinator.sh beacon <blockhash> [height]   final beacon from a pre-announced Bitcoin block
#   coordinator.sh publish              export VKs, vk-import (VK hex / vk.bin), manifest.json
# Work dir: $CER_DIR (default ./ceremony-work). Needs: node+snarkjs, falcon-pl-bitvm2-setup ($FPL_SETUP).
set -euo pipefail
H=$(cd "$(dirname "$0")" && pwd); . "$H/ceremony.env"; CIRCUITS=${CER_CIRCUITS:-$CIRCUITS}
W=${CER_DIR:-$PWD/ceremony-work}; mkdir -p "$W"; cd "$W"
S=${FPL_SETUP:-falcon-pl-bitvm2-setup}; X=${SNARKJS:-npx snarkjs}
export NODE_OPTIONS=${NODE_OPTIONS:---max-old-space-size=8192}
T=transcript.md
last() { ls "$1"_[0-9][0-9][0-9][0-9].zkey 2>/dev/null | sort | tail -1; }
sha() { sha256sum "$1" | cut -d' ' -f1; }
case "${1:-}" in
init)
  [ -f "$PTAU_FILE" ] || curl -sSfL -o "$PTAU_FILE" "$PTAU_URL"
  got=$(sha "$PTAU_FILE"); echo "ptau sha256 $got"
  [ "$PTAU_SHA256" = "__FILL__" ] || [ "$got" = "$PTAU_SHA256" ] || { echo "PTAU HASH MISMATCH"; exit 1; }
  echo "# Falcon PL BitVM2 Groth16 ceremony transcript" > $T
  echo "- phase1: $PTAU_URL sha256=$got" >> $T
  for c in $CIRCUITS; do
    $S r1cs --circuit $c --out $c.r1cs | tee $c.r1cs.json
    $X zkey new $c.r1cs "$PTAU_FILE" ${c}_0000.zkey >/dev/null
    echo "- $c: r1cs sha256=$(sha $c.r1cs) $(cat $c.r1cs.json) initial ${c}_0000.zkey sha256=$(sha ${c}_0000.zkey)" >> $T
  done
  echo "init done; publish ${CIRCUITS// /_0000.zkey, }_0000.zkey + $T";;
accept)
  c=$2; f=$3
  [ -f "$f" ] || { echo "missing $f"; exit 1; }
  prev=$(last $c); [ -n "$prev" ] || { echo "no prior zkey for $c (run init)"; exit 1; }
  base=${prev%.zkey}; n=$((10#${base#${c}_} + 1)); nn=$(printf %04d $n)
  $X zkey verify $c.r1cs "$PTAU_FILE" "$f" > verify_${c}_$nn.log 2>&1 || { echo "REJECT: zkey verify failed (see verify_${c}_$nn.log)"; exit 1; }
  grep -q "ZKey Ok" verify_${c}_$nn.log || { echo "REJECT: not ZKey Ok"; exit 1; }
  cp "$f" ${c}_$nn.zkey
  echo "- $c #$nn accepted sha256=$(sha ${c}_$nn.zkey) from $f; $(sed 's/\x1b\[[0-9;]*m//g' verify_${c}_$nn.log | grep -E 'contribution #' | head -1 | sed 's/.*snarkJS: //')" >> $T
  echo "accepted ${c}_$nn.zkey (next contributor starts from this file)";;
beacon)
  bh=$2; ht=${3:-?}
  for c in $CIRCUITS; do
    $X zkey beacon $(last $c) ${c}_final.zkey "$bh" 10 -n="bitcoin-block-$ht" >/dev/null
    $X zkey verify $c.r1cs "$PTAU_FILE" ${c}_final.zkey > verify_${c}_final.log 2>&1; grep -q "ZKey Ok" verify_${c}_final.log
    echo "- $c beacon block $ht $bh final sha256=$(sha ${c}_final.zkey)" >> $T
  done;;
publish)
  echo "{" > manifest.json
  for c in $CIRCUITS; do
    $X zkey export verificationkey ${c}_final.zkey ${c}_vk.json >/dev/null
    $S vk-import --vk ${c}_vk.json --out-bin ${c}_vk.bin > ${c}_vk_import.json
    echo "\"$c\": $(cat ${c}_vk_import.json), \"${c}_final_zkey_sha256\": \"$(sha ${c}_final.zkey)\"," >> manifest.json
  done
  echo "\"ptau_sha256\": \"$(sha "$PTAU_FILE")\"}" >> manifest.json
  python3 -m json.tool manifest.json >/dev/null && cat manifest.json
  echo "v1 vk_hex -> crates/fd-pl/src/bitvm2_exit.rs BITVM2_WITHDRAW_VK_HEX (reviewed PR); v3_vk.bin -> operators' FPL_PROOF_DIR/vk.bin";;
*) sed -n 2,8p "$0"; exit 2;;
esac
