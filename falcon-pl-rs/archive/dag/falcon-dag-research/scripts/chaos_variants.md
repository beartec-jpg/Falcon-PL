# Chaos / reliability variants (PR10)

Run on demand or nightly; not every commit.

| Variant | How | Expect |
|---------|-----|--------|
| Silent peer | kill -STOP one validator mid-hold | others continue; freeze classifiers visible |
| Delayed delivery | tc netem delay 200ms on p2p | cert lag bounded; no Mode B stickiness |
| Restart | SIGTERM one node, restart from snapshot | catch-up without manual repair |
| RPC pressure | flood submit while dig holds | terBUSY reasons intelligible; owner route works |

## Cadence

| When | Suite |
|------|--------|
| Per commit | unit tests + cargo check |
| Nightly | `reliability_gate_n3.sh` + optional dig_sticky |
| Pre-release | `reliability_gate_n5.sh` + chaos restart + role-separated topology |

## Summary schema

All suites should write `dig_summary.json` using `dig_summary_schema.py` so results are comparable.
