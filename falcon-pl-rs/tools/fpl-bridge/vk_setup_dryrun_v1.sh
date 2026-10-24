#!/usr/bin/env bash
# DRY RUN of the public setup for circuit v1 (local ptau + 2 local contributions + fixed beacon).
# NOT a usable setup: every contribution here ran on this box. Output is for format/flow checks only.
set -euo pipefail
S=/workspace/wt-bint-target/release/falcon-pl-bitvm2-setup; X="npx snarkjs"
R(){ python3 -c 'import secrets;print(secrets.token_hex(32))'; }
$X powersoftau new bn128 13 pot0.ptau >/dev/null
$X powersoftau contribute pot0.ptau pot1.ptau --name="dryrun-p1" -e="$(R)" >/dev/null
$X powersoftau prepare phase2 pot1.ptau pot_final.ptau >/dev/null
$X zkey new v1.r1cs pot_final.ptau v1_0.zkey >/dev/null
$X zkey contribute v1_0.zkey v1_1.zkey --name="contrib-A" -e="$(R)" >/dev/null
$X zkey contribute v1_1.zkey v1_2.zkey --name="contrib-B" -e="$(R)" >/dev/null
$X zkey beacon v1_2.zkey v1_final.zkey 0000000000000000000000000000000000000000000000000000000000000001 10 -n="beacon-dryrun" >/dev/null
$X zkey verify v1.r1cs pot_final.ptau v1_final.zkey 2>&1 | grep -E "ZKey Ok|ERROR" | head -3
$X zkey export verificationkey v1_final.zkey v1_vk.json >/dev/null
$S demo-wtns --circuit v1 --out v1_demo.wtns >/dev/null
$X wtns check v1.r1cs v1_demo.wtns 2>&1 | grep -E "WITNESS|ERROR|OK" | tail -2
$X groth16 prove v1_final.zkey v1_demo.wtns v1_proof.json v1_public.json
$X groth16 verify v1_vk.json v1_public.json v1_proof.json 2>&1 | tail -1
$S vk-import --vk v1_vk.json | cut -c1-120
$S proof-import --vk v1_vk.json --proof v1_proof.json --public v1_public.json | cut -c1-120
echo DRYRUN_DONE
