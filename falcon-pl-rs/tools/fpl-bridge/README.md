# fpl-bridge (M2 regtest harness)
Upstream BitVM pinned at 7d1ca3660cac08aab62e76f3aa4daec0d7403ecc (MIT, see patches/BitVM-LICENSE-MIT),
fetched by `scripts/fetch-bitvm.sh` into `vendor/BitVM` (gitignored), then `git apply patches/*.patch`.
- `esplora_shim.py`: Esplora REST subset over local bitcoind regtest (port 8094, /regtest/api).
- patch: test faucet funds via local `bitcoin-cli` when `FPL_BTC_DATADIR` is set.
