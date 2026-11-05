# falcon-pl-mm (scaffold)

Testnet4 order-book volume engine for BTC4-FPL. It quotes a 10-level ladder at 0.25% spacing around a mid that moves slowly between 900k and 1.1M FPL/BTC4 on a 6-hour sine wave. Orders go through `falcon-pl-ctl place-order` / `cancel-order`.

* `DRY_RUN=1` (default) only prints the ctl commands it would run.
* Kill switches: touch `MM_PAUSE` to pause; touch `MM_CANCEL_ALL` to cancel everything and stop.
* Before going live: fund the mm wallets (BTC4 graph-path peg-in plus FPL from treasury), set the wallet-reported `FPL_RAW_PER_FPL`/`BASE_RAW_PER_COIN`, and implement cancel-all (open orders come from `fpl_openOrders`).
* Matching is AMM-first and there is no post-only option, so a quote that crosses will take. The MIN_ORDER_QUOTE is 1000 FPL, and an account can have at most 32 open orders (20 per wallet here).
