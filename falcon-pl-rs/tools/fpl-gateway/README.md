# fpl-gateway: Falcon Cloud RPC gateway

BEARTEC Build Plan step 3. A keyed, rate-limited, metered HTTPS JSON-RPC front for a
Falcon PL node. **Falcon PL is a testnet.** FPL testnet tokens have no monetary
value; the paid tiers buy RPC capacity and support, not tokens.

```
client --HTTPS--> Caddy :443 --> fpl-gatewayd 127.0.0.1:8545 --wire JSON--> node 127.0.0.1:19312
                                   metrics 127.0.0.1:9187
```

## Hosting plan
Runs on the **Helsinki public seed host (5.78.142.246)**, next to the seed node.
Source of truth: `crates/fd-pl/scripts/network-2300.env` (Public door: Helsinki 8 GB relay,
v12 listens locally) and `scripts/pl-seat-relay.py` (v12 :19312 is that host's own node).
The gateway talks only to that local node over loopback; it never dials seats over
Tailscale and never touches the relay. Install steps: `deploy/INSTALL.md` (not yet run).

## Tiers (prices approved; limits are PROPOSALS for Scott)

| Tier | Price | Sustained | Burst | Monthly quota (units) | `fpl_submitTx` |
|---|---|---|---|---|---|
| Free | £0 | 5 req/s | 10 | 300,000 | 10 / min |
| Builder | £29 / month | 25 req/s | 50 | 5,000,000 | 120 / min |
| Scale | £199 / month | 100 req/s | 200 | 50,000,000 | 600 / min |

One read = 1 unit; `fpl_submitTx` = 5 units. Quotas reset on the 1st of each month,
00:00 UTC. Request body cap 64 KiB. All values live in `gateway.toml`.

## API
`POST /v1/rpc`, JSON-RPC 2.0, key in `Authorization: Bearer fplk_...` or `X-API-Key`.

| Method | Node message | Params |
|---|---|---|
| `fpl_status` | `status_req` (always `brief`, never accounts) | none |
| `fpl_health` | `health_req` | `warnings` (max 64) |
| `fpl_getAccount` | `account_query` | `account` |
| `fpl_orderBook` | `order_book_query` | `pair`, `levels` (max 200) |
| `fpl_openOrders` | `open_orders_query` | `account` |
| `fpl_fills` | `fills_query` | `pair?`, `account?`, `limit` (max 200) |
| `fpl_submitTx` | `submit_tx` | `tx` (signed tx object) |

Nothing else is reachable: no admin (`WireMsg::is_admin`), mesh, snapshot or ctl
messages. Params can't set `type`. Errors: -32001 auth, -32005 rate/quota (HTTP 429 +
`Retry-After`), -32601 method not allowed, -32602 params, -32002 node unavailable (502).

Other routes: `GET /v1/health` (gateway + node probe, no key), `GET /v1/tiers`,
`GET /v1/usage` (caller's own month-to-date usage), `GET /metrics` on the metrics
listener, `POST /v1/billing/stripe/webhook` (stub; 404 unless enabled).

## Keys and metering
Keys are `fplk_` + 64 hex, shown once by `fpl-gatewayd new-key`, stored only as
SHA-256 in `keys.json`, reloaded on SIGHUP. Usage per key/month is written atomically
to `/var/lib/fpl-gateway/usage.json` every 30 s and on shutdown.

## Tests
```bash
scripts/test.sh     # cargo test --locked --all-targets: unit + mock-node integration
```
Standalone cargo workspace like `tools/fpl-operator`. No CI.
