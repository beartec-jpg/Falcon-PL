# Boring reliable lab stack

Default: **2 shards**, facade **:18499**, clients only talk to the facade.

## Day-to-day

```bash
export SSH_F2=100.64.0.3

# bring up + ensure watchdog
bash deploy/gmp/start-stack.sh --watchdog

# one glance
bash deploy/gmp/status.sh

# health (exit 0 ok / 1 degraded / 2 down)
python3 deploy/gmp/health.py

# smoke (same-shard + cross-shard + idempotent replay)
python3 deploy/gmp/smoke.py

# watchdog only (if stack already up)
bash deploy/gmp/watchdog.sh --daemon

# stop mesh+gmp+facade (leave watchdog — it will re-boot if DOWN)
bash deploy/gmp/stop-stack.sh

# stop everything including watchdog
bash deploy/gmp/stop-stack.sh --all
```

## Default config

| Item | Value |
|------|--------|
| Shards | 2 |
| Accounts | s0: acct1–20 · s1: acct21–40 |
| Bridges | s0: acct20 · s1: acct40 |
| GMP | :18500 / :18510 |
| Facade | :18499 |
| Local vals | val1,val2 @ F1 · val4,val5 @ F1 |
| Remote vals | val3,val6 @ F2 (`F2_IP` / `SSH_F2`) |
| Env defaults | `deploy/gmp/stack.env.default` |
| Runtime state | `~/falcon-dag-mesh/stack/state.json` |

## What watchdog does

Every ~60s (`WATCHDOG_INTERVAL_S`):

1. Heartbeat → `stack/watchdog.heartbeat` + pidfile  
2. Rotate large logs (cap `LOG_MAX_MB`)  
3. Run `health.py`  
4. If **DOWN** (exit 2): restart via `start-stack.sh` with **cooldown** (`RESTART_COOLDOWN_S`, default 120s) and **max 3 restarts/hour**  
5. If **DEGRADED** (exit 1): log only — no restart  
6. Every N loops (`SMOKE_EVERY_N`, default 5): run `smoke.py`  

**Single instance** via `flock` on `stack/watchdog.lock`.  
Second `watchdog.sh --daemon` is a no-op if one is already alive.

## Health levels

| Exit | Meaning | Watchdog action |
|------|---------|-----------------|
| 0 | All checks OK | log OK |
| 1 | Degraded (low bridge, remote RPC, stale adverts) | log only |
| 2 | Down (facade/GMP gone, zero local RPCs) | restart (rate-limited) |

Checks include:

- Facade `/v1/status` + bridge balances  
- GMP via facade **and** direct `:18500`/`:18510`  
- Local packer RPCs + remote UNL thirds (`val3`/`val6` on F2)  
- Advisory: pidfiles, watchdog heartbeat age  

## Smoke expectations

- Route acct3→s0, acct23→s1  
- Same-shard submit succeeds  
- Cross-shard settles to `credited` / `credited_rpc` (refund also accepted)  
- Idempotent replay of same `client_id`  

Sequences in `~/falcon-dag-mesh/stack/smoke_seq.json` (cleared on every stack start/wipe).

## Bridge monitoring

```bash
curl -sS http://127.0.0.1:18499/v1/bridges | jq .
```

Health goes **degraded** if any bridge balance &lt; `BRIDGE_MIN_BALANCE` (default 1_000_000).

## Cross-shard (lab)

1. Debit user → source bridge  
2. Wait source exec  
3. Credit dest bridge → recipient  
4. Credit fail after debit → **refund** from source bridge  
5. `client_id` → idempotency  

## Logs & state

| Path | Content |
|------|---------|
| `~/falcon-dag-mesh/logs/facade.log` | Facade |
| `~/falcon-dag-mesh/logs/gmp0.log` | GMP shard 0 |
| `~/falcon-dag-mesh/logs/val*.log` | Validators |
| `~/falcon-dag-mesh/logs/watchdog.log` | Watchdog |
| `~/falcon-dag-mesh/logs/*.pid` | Process pids |
| `~/falcon-dag-mesh/stack/state.json` | Topology for health |
| `~/falcon-dag-mesh/stack/last_health.json` | Last health |
| `~/falcon-dag-mesh/stack/last_smoke.json` | Last smoke |
| `~/falcon-dag-mesh/stack/watchdog.heartbeat` | Watchdog liveness (unix ts) |
| `~/falcon-dag-mesh/stack/watchdog_restarts.json` | Restart rate limit |

## Failure playbook

| Symptom | Action |
|---------|--------|
| `status.sh` DOWN | Check `logs/facade.log`, `gmp0.log`; if watchdog up it should restart within ~2–3 min |
| Smoke FAIL | Read `stack/last_smoke.json`; often transient xshard — re-run smoke |
| DEGRADED remote RPC | Check F2: `ssh $SSH_F2 'pgrep -a falcon-dagd'` |
| DEGRADED low bridge | Fund bridge account or lower `BRIDGE_MIN_BALANCE` for lab |
| Watchdog not running | `bash deploy/gmp/watchdog.sh --daemon` |
| Restart thrash | Check `watchdog_restarts.json`; cooldown/max/hour protect the mesh |

## Digs (optional, not for uptime)

```bash
python3 deploy/gmp/18-facade-xshard-dig.py
python3 deploy/gmp/17-shard-v0-dig.py
python3 deploy/gmp/19-shard3-dig.py
```
