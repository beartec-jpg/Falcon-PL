# Testnet 2300 verifying keys (setup of 2026-10-19)
- `v3_vk.bin` (sha256 c6d3509b1e64bc4e22fe73503901070e5b9717c4cd40869ac5fac80401787843): BitVM disprove
  circuit key. Put it in `FPL_PROOF_DIR/vk.bin` (graph building) and `FPL_OP_CHALLENGE_VK_DIR` (watchers)
  for NEW testnet4 graphs only, and only together with every party of those graphs.
  Prove with `v3_snarkjs_prove.sh` using `v3_final.zkey` (sha256 ca8a54d0…291e).
- `v1_vk.json`: ledger withdraw key in snarkjs form (= `BITVM2_WITHDRAW_VK_HEX`). The kit's
  `FPL_OP_ATTEST_VK_JSON`; `FPL_OP_ATTEST_ZKEY` = `v1_final.zkey` (sha256 67fd200f…9a46).
Transcript and re-verify steps: `docs/bridge/ceremony/testnet-vk-2026-10-19/`. Testnet only.
