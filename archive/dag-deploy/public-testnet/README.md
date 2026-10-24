# Public play testnet package

Productizes the dual-shard GMP lab into **`falcon-dag-testnet-1`** (R2).

```
deploy/public-testnet/
  genesis/           # frozen network.json + unl_public + bootstrap template
  scripts/
    start.sh         # mesh + facade + faucet + watchdog
    stop.sh
    readiness.sh     # READY / NOT READY gate
    faucetd.py       # rate-limited faucet :18550
    client-smoke.sh  # user-facing curl checks
  PUBLIC_TESTNET.md  # user + operator guide
  README.md          # this file
```

## One-shot (operators)

```bash
export SSH_F2=100.64.0.3
bash deploy/public-testnet/scripts/start.sh
bash deploy/public-testnet/scripts/readiness.sh
```

Expect: **`READY: falcon-dag-testnet-1 is good enough for public play testnet (R2)`**

## Relation to other profiles

| Profile | network_id | Role |
|---------|------------|------|
| Python regtest | 9002 | Legacy local |
| Docker/bare `testnet-9003` | 9003 single chain | R1 private multi-node |
| **This package** | **9003 + 9004** dual shard | **R2 public play testnet** |
| Lab digs (ad-hoc) | 9930+ | Experiments |

Full user doc: [PUBLIC_TESTNET.md](./PUBLIC_TESTNET.md)
