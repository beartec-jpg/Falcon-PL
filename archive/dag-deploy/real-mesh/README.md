# Real-mesh lab — 6 validators + 2 archivals (host1 + host2)

| Host | LAN IP | Processes |
|------|--------|-----------|
| **host1** | 192.0.2.1 | `val1` `val2` `val3` + `arch1` |
| **host2** | 192.0.2.2 | `val4` `val5` `val6` + `arch2` |

- **UNL:** `val1..val6` (threshold 0.6 ≈ 4/6)
- **Transport:** LAN (`192.0.2.x`), not Tailscale
- **Crypto:** falcon-512
- **Network id:** 9210

```bash
cd ~/falcon-dag/deploy/real-mesh
./00-gen-keys.sh
./01-distribute.sh
./02-start-all.sh
./03-status.sh
RATE=30 HOLD_S=40 ./04-loadtest.sh
./05-stop-all.sh
```
