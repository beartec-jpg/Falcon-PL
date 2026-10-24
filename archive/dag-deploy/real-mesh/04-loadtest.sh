#!/usr/bin/env bash
# Owner-routed load across 6 validators + report.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=/dev/null
source "$ROOT/env.sh"

RATE="${RATE:-30}"
HOLD="${HOLD_S:-40}"
OUT="${OUT:-/tmp/real-mesh-load-$(date +%s)}"
mkdir -p "$OUT"

python3 - "$RATE" "$HOLD" "$OUT" "$F1_IP" "$F2_IP" \
  "$VAL1_RPC" "$VAL2_RPC" "$VAL3_RPC" "$VAL4_RPC" "$VAL5_RPC" "$VAL6_RPC" \
  "$ARCH1_RPC" "$ARCH2_RPC" <<'PY'
import json, sys, time, urllib.request
from pathlib import Path

rate = int(sys.argv[1]); hold = float(sys.argv[2]); out = Path(sys.argv[3])
f1, f2 = sys.argv[4:6]
ports = {
  "val1": (f1, int(sys.argv[6])),
  "val2": (f1, int(sys.argv[7])),
  "val3": (f1, int(sys.argv[8])),
  "val4": (f2, int(sys.argv[9])),
  "val5": (f2, int(sys.argv[10])),
  "val6": (f2, int(sys.argv[11])),
  "arch1": (f1, int(sys.argv[12])),
  "arch2": (f2, int(sys.argv[13])),
}
VALS = ["val1", "val2", "val3", "val4", "val5", "val6"]
N = len(VALS)

def rpc(host, port, method, params=None, timeout=5.0):
    pl = [{}] if params is None else ([params] if not isinstance(params, list) else params)
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": pl}).encode()
    req = urllib.request.Request(
        f"http://{host}:{port}/", data=body,
        headers={"Content-Type": "application/json"}, method="POST",
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        d = json.load(r)
    if "error" in d and "result" not in d:
        raise RuntimeError(d["error"])
    return d

def metrics(vid):
    host, port = ports[vid]
    d = rpc(host, port, "server_info")
    info = ((d.get("result") or {}).get("info") or {})
    return info.get("metrics") or {}

def account_shard(account: str) -> int:
    h = 0x811C9DC5
    for b in account.encode():
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h

def owner_idx(account: str) -> int:
    return account_shard(account) % N

def submit(account, seq):
    oi = owner_idx(account)
    vid = VALS[oi]
    host, port = ports[vid]
    dest = "acct40" if account != "acct40" else "acct39"
    d = rpc(host, port, "submit", {
        "account": account, "sequence": seq, "destination": dest,
        "amount": 1, "fee": 10,
    })
    return (d.get("result") or {}).get("engine_result"), vid

for vid in VALS:
    for _ in range(40):
        try:
            metrics(vid)
            break
        except Exception:
            time.sleep(0.25)
    else:
        print(f"WARN: {vid} not ready")
for vid in ("arch1", "arch2"):
    try:
        metrics(vid)
    except Exception as e:
        print(f"WARN: {vid} not ready ({e})")

def next_seq(account: str) -> int:
    for vid in VALS:
        host, port = ports[vid]
        try:
            d = rpc(host, port, "account_info", {"account": account})
            r = d.get("result") or {}
            if isinstance(r, dict) and "sequence" in r:
                return int(r["sequence"])
        except Exception:
            continue
    return 1

seq = {f"acct{a}": next_seq(f"acct{a}") for a in range(1, 41)}
print("seq_seed sample", {k: seq[k] for k in ("acct1", "acct2", "acct10", "acct40")}, flush=True)
ok = busy = err = 0
err_codes = {}
by_node = {v: 0 for v in VALS}
t0 = time.time()
samples = []
print(f"LOAD start rate={rate} hold={hold}s owner-route n_vals={N}", flush=True)
while time.time() - t0 < hold:
    sec0 = time.time()
    for _ in range(rate):
        if time.time() - t0 >= hold:
            break
        acct = min(seq.keys(), key=lambda a: (seq[a], a))
        s = seq[acct]
        try:
            code, vid = submit(acct, s)
            if code == "tesSUCCESS":
                ok += 1
                by_node[vid] += 1
                seq[acct] = s + 1
            elif code == "terBUSY":
                busy += 1
            elif code in ("tecSEQ_PAST", "tefPAST_SEQ", "tecSEQ_FUTURE", "tefALREADY"):
                err += 1
                err_codes[code] = err_codes.get(code, 0) + 1
                seq[acct] = next_seq(acct)
            else:
                err += 1
                err_codes[str(code)] = err_codes.get(str(code), 0) + 1
        except Exception as e:
            err += 1
            err_codes[type(e).__name__] = err_codes.get(type(e).__name__, 0) + 1
    snap = {}
    for vid in list(ports.keys()):
        try:
            m = metrics(vid)
            ps = m.get("pack_stall") or {}
            snap[vid] = {
                "exec": m.get("executed_txs"),
                "lag": ps.get("admit_exec_lag", m.get("admit_exec_lag")),
                "admits": ps.get("local_admits_ok", m.get("local_admits_ok")),
                "profile": m.get("node_profile"),
                "role": m.get("role"),
                "need_seed": ps.get("open_needs_body_seed"),
            }
        except Exception as e:
            snap[vid] = {"error": str(e)}
    samples.append({"t": round(time.time() - t0, 1), "ok": ok, "busy": busy, "err": err, "nodes": snap})
    ex = [snap.get(v, {}).get("exec") for v in VALS]
    lag = [snap.get(v, {}).get("lag") for v in VALS]
    print(f"t={time.time()-t0:.1f} ok={ok} busy={busy} err={err} exec={ex} lag={lag}", flush=True)
    left = 1.0 - (time.time() - sec0)
    if left > 0:
        time.sleep(left)

final = samples[-1]["nodes"] if samples else {}
first = samples[0]["nodes"] if samples else {}
execs = [final.get(v, {}).get("exec") or 0 for v in VALS]
execs0 = [first.get(v, {}).get("exec") or 0 for v in VALS]
deltas = [max(0, (e or 0) - (e0 or 0)) for e, e0 in zip(execs, execs0)]
arch = {a: final.get(a, {}) for a in ("arch1", "arch2")}
arch_exec = [arch.get(a, {}).get("exec") for a in ("arch1", "arch2")]
report = {
    "suite": "real_mesh_6v2a",
    "n_validators": 6,
    "archivals": 2,
    "hosts": {
        "host1": ["val1", "val2", "val3", "arch1"],
        "host2": ["val4", "val5", "val6", "arch2"],
    },
    "transport": "LAN",
    "rate": rate,
    "hold_s": hold,
    "ok": ok,
    "busy": busy,
    "err": err,
    "submits_by_owner": by_node,
    "exec_vals": dict(zip(VALS, execs)),
    "exec_min": min(execs) if execs else 0,
    "exec_max": max(execs) if execs else 0,
    "exec_spread": (max(execs) - min(execs)) if execs else 0,
    "exec_delta_min": min(deltas) if deltas else 0,
    "exec_delta_max": max(deltas) if deltas else 0,
    "hold_tps": (min(deltas) / hold) if hold > 0 and deltas else 0,
    "lag_final": {v: final.get(v, {}).get("lag") for v in VALS},
    "archival": arch,
    "archival_exec": {a: arch.get(a, {}).get("exec") for a in ("arch1", "arch2")},
    "need_seed_ticks": sum(
        1 for s in samples for v in VALS if (s["nodes"].get(v) or {}).get("need_seed")
    ),
    "err_codes": err_codes,
    "seq_final_sample": {k: seq[k] for k in ("acct1", "acct2", "acct10", "acct40")},
}
lag_ok = all((final.get(v, {}).get("lag") in (0, None)) for v in VALS)
spread_ok = (max(execs) - min(execs)) <= max(50, int(0.15 * max(1, min(execs)))) if execs else False
delta_ok = (min(deltas) if deltas else 0) >= max(1, int(ok * 0.4))
arch_ok = all(isinstance(x, int) and x >= (min(execs) - 50) for x in arch_exec) if execs and all(isinstance(x, int) for x in arch_exec) else False
report["pass_detail"] = {
    "err_ok": err == 0,
    "busy_ok": busy < max(1, ok) * 0.05 if ok else busy == 0,
    "lag_ok": lag_ok,
    "delta_ok": delta_ok,
    "spread_ok": spread_ok,
    "arch_ok": arch_ok,
}
report["pass"] = all(report["pass_detail"].values())
(out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
(out / "samples.json").write_text(json.dumps(samples[-30:], indent=2) + "\n")
print("REPORT", json.dumps(report, indent=2))
print("wrote", out / "report.json")
sys.exit(0 if report["pass"] else 1)
PY
