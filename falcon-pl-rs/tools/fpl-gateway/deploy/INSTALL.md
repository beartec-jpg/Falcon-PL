# Installing fpl-gateway on the seed host (NOT done yet)

Target: the Helsinki public seed host **5.78.142.246** (the 8 GB relay that runs
`scripts/pl-seat-relay.py` and the local v12 node on `:19312`). Nothing below has
been run; it needs Scott's go-ahead. Do not restart or reconfigure the node.

## 0. Pre-flight (read-only)
```bash
ss -ltnp | grep -E ':19312|:8545|:9187'     # node on 19312; 8545/9187 must be free
free -m                                      # gateway is capped at 512 MB
```
Confirm the local node answers a brief status on `127.0.0.1:19312` with
`falcon-pl-ctl` (read-only). If the node uses `--lock-public-wire`/`--admin-sock`,
that is fine: the gateway only sends read queries + `submit_tx`.

## 1. Build (on beartec-brain, not on the seed host)
```bash
cd falcon-pl-rs/tools/fpl-gateway
cargo build --release --locked
# copy target/release/fpl-gatewayd to the host as /usr/local/bin/fpl-gatewayd (0755)
```

## 2. User, config, keys
```bash
useradd --system --no-create-home --shell /usr/sbin/nologin fpl-gateway
install -d -m 0750 -o root -g fpl-gateway /etc/fpl-gateway
install -m 0640 -o root -g fpl-gateway deploy/gateway.example.toml /etc/fpl-gateway/gateway.toml
install -m 0640 -o root -g fpl-gateway deploy/keys.example.json   /etc/fpl-gateway/keys.json
```
Edit `gateway.toml`: set `cors_origins` (e.g. `["https://falcon-fpl.com"]`),
keep `listen = "127.0.0.1:8545"` and `metrics_listen = "127.0.0.1:9187"`.

Issue a key (plaintext printed once to stdout, hashed record to stderr):
```bash
fpl-gatewayd new-key --id k_scott_test --tier builder --label "Scott test" 2>>record.json
```
Append the record to `keys.json` and `systemctl reload fpl-gateway`.
keys.json holds only SHA-256 hashes; never commit it or paste keys in chat.

`fpl-gatewayd check-config --config /etc/fpl-gateway/gateway.toml` validates both files.

## 3. TLS front (Caddy example)
```
rpc.falcon-fpl.com {
    reverse_proxy /v1/* 127.0.0.1:8545
    request_body { max_size 64KB }
}
```
Open only 443 publicly. `/metrics` stays on loopback (scrape over Tailscale).

## 4. Service
```bash
install -m 0644 deploy/fpl-gateway.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now fpl-gateway
curl -s 127.0.0.1:8545/v1/health
```

## 5. Billing (stays off)
`[billing] enabled = false` by default. When Scott approves Stripe, create the GBP
prices in Stripe, map their ids in `[billing.price_tiers]`, put the webhook signing
secret in `/etc/fpl-gateway/billing.env` (0600) and enable. The stub only verifies
and queues events; tier changes are applied by an operator.

## Rollback
`systemctl disable --now fpl-gateway` and remove the Caddy site. The node is untouched.
