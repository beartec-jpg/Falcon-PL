#!/usr/bin/env bash
# BitVM v3 proof through the PUBLIC-setup path (snarkjs + final v3 zkey); drop-in for
# falcon-pl-bitvm2-v3-proof (same flags, same DIR/{vk.bin,proof.bin,public.bin,binding_commit.hex}).
#   v3_snarkjs_prove.sh --out DIR --amount SATS --dest-h160 HEX --pegout-txid INTERNALHEX --height H --header-hex HEX [--note ID]
#   or: v3_snarkjs_prove.sh --out DIR --input attest.json      (real statement, e.g. from falcon-pl-bitvm2-attest input)
# Env: FPL_V3_ZKEY (final v3 zkey), FPL_V3_VK_JSON (its verification_key.json), FPL_SETUP, SNARKJS.
set -euo pipefail
S=${FPL_SETUP:-falcon-pl-bitvm2-setup}; X=${SNARKJS:-npx snarkjs}
declare -A a; while [ $# -gt 0 ]; do a[$1]=$2; shift 2; done
out=${a[--out]}; mkdir -p "$out"; W=$(mktemp -d); trap 'rm -rf "$W"' EXIT
if [ -n "${a[--input]:-}" ]; then cp "${a[--input]}" "$W/in.json"; else
  $S v3-input --amount "${a[--amount]}" --dest-h160 "${a[--dest-h160]}" --pegout-txid "${a[--pegout-txid]}" \
     --height "${a[--height]}" --header-hex "${a[--header-hex]}" --note "${a[--note]:-fpl-withdraw}" --out "$W/in.json" >/dev/null
fi
$S v3-wtns --input "$W/in.json" --out "$W/w.wtns" --binding-out "$out/binding_commit.hex" >/dev/null
$X groth16 prove "$FPL_V3_ZKEY" "$W/w.wtns" "$W/proof.json" "$W/public.json"
$S vk-import --vk "$FPL_V3_VK_JSON" --out-bin "$out/vk.bin" >/dev/null
$S proof-import --vk "$FPL_V3_VK_JSON" --proof "$W/proof.json" --public "$W/public.json" --out-bin "$out/proof.bin" --public-bin "$out/public.bin" > "$W/imp.json"
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); assert d["verifies"]; print(json.dumps({"x_be": d["public_be"][0], "binding_commit": open(sys.argv[2]).read().strip(), "prover": "snarkjs"}))' "$W/imp.json" "$out/binding_commit.hex"
