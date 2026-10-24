#!/usr/bin/env python3
"""
Health check for the boring lab stack (facade + shards + bridges + RPCs).

Exit 0 = healthy, 1 = degraded, 2 = down.
Prints JSON summary to stdout; human lines to stderr.
"""
from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

FACADE = os.environ.get("FACADE_URL", "http://127.0.0.1:18499")
BRIDGE_MIN = int(os.environ.get("BRIDGE_MIN_BALANCE", "1000000"))
MESH = Path(os.environ.get("MESH_ROOT", Path.home() / "falcon-dag-mesh"))
STATE = MESH / "stack" / "state.json"
ADVERT_STALE_S = float(os.environ.get("ADVERT_STALE_S", "30"))

# Default dual-shard: local packers + remote UNL third (F2).
DEFAULT_RPCS = [
    ("s0", "127.0.0.1", 18001),
    ("s0", "127.0.0.1", 18002),
    ("s0_remote", os.environ.get("F2_IP", "192.0.2.2"), 18003),
    ("s1", "127.0.0.1", 18004),
    ("s1", "127.0.0.1", 18005),
    ("s1_remote", os.environ.get("F2_IP", "192.0.2.2"), 18006),
]

DEFAULT_GMPS = [
    ("gmp0", "http://127.0.0.1:18500"),
    ("gmp1", "http://127.0.0.1:18510"),
]


def http_json(url: str, method: str = "GET", body=None, timeout: float = 4.0):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        url, data=data, method=method, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def rpc(host: str, port: int, method: str, params=None, timeout: float = 3.0):
    body = json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": method, "params": [params or {}]}
    ).encode()
    req = urllib.request.Request(
        f"http://{host}:{port}/",
        data=body,
        method="POST",
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def main() -> int:
    checks = []
    ok = True
    down = False
    now = time.time()

    # Facade + bridges + GMP-via-facade
    try:
        st = http_json(f"{FACADE}/v1/status")
        checks.append(
            {
                "name": "facade",
                "ok": st.get("service") == "falcon-gmp-facade",
                "detail": "up",
            }
        )
        if st.get("service") != "falcon-gmp-facade":
            ok = False
            down = True
        bridges = st.get("bridges") or []
        for b in bridges:
            bal = b.get("balance")
            low = isinstance(bal, int) and bal < BRIDGE_MIN
            checks.append(
                {
                    "name": f"bridge_s{b.get('shard')}",
                    "ok": not low and bal is not None,
                    "balance": bal,
                    "bridge": b.get("bridge"),
                    "warn": "low_balance" if low else None,
                }
            )
            if low or bal is None:
                ok = False  # degraded, not necessarily down
        for sh in st.get("shards") or []:
            gs = sh.get("gmp_status") or {}
            gok = "error" not in gs
            # Stale adverts → degraded (validators not leasing)
            stale_vals = []
            vals = gs.get("validators") or {}
            if isinstance(vals, dict):
                for vid, vinfo in vals.items():
                    if not isinstance(vinfo, dict):
                        continue
                    age = vinfo.get("advert_age_s")
                    if isinstance(age, (int, float)) and age > ADVERT_STALE_S:
                        stale_vals.append(vid)
            checks.append(
                {
                    "name": f"gmp_s{sh.get('id')}",
                    "ok": gok,
                    "detail": gs if not gok else "up",
                    "warn": f"stale_adverts:{','.join(stale_vals)}" if stale_vals else None,
                }
            )
            if not gok:
                ok = False
                down = True
            if stale_vals:
                ok = False
    except Exception as e:
        checks.append({"name": "facade", "ok": False, "error": str(e)})
        ok = False
        down = True

    # Direct GMP probes (belt + suspenders if facade is half-up)
    gmps = list(DEFAULT_GMPS)
    if STATE.exists():
        try:
            state = json.loads(STATE.read_text())
            if state.get("gmps"):
                gmps = [tuple(x) for x in state["gmps"]]
            nshards = int(state.get("shards") or 2)
            if nshards >= 3 and not any(g[0] == "gmp2" for g in gmps):
                gmps.append(("gmp2", "http://127.0.0.1:18520"))
        except Exception:
            pass
    for name, url in gmps:
        try:
            gs = http_json(f"{url.rstrip('/')}/v1/status", timeout=3.0)
            checks.append(
                {
                    "name": f"direct_{name}",
                    "ok": True,
                    "pending": gs.get("pending"),
                    "active_leases": gs.get("active_leases"),
                }
            )
        except Exception as e:
            checks.append({"name": f"direct_{name}", "ok": False, "error": str(e)})
            ok = False
            # If facade already marked down, keep down; else direct GMP fail is degraded
            # unless all GMPs fail — handled below via facade gmp_s*

    # Validator RPCs (local + remote from state)
    rpcs = list(DEFAULT_RPCS)
    if STATE.exists():
        try:
            state = json.loads(STATE.read_text())
            if state.get("rpcs"):
                rpcs = [tuple(x) for x in state["rpcs"]]
        except Exception:
            pass

    up = 0
    for item in rpcs:
        if len(item) == 3:
            label, host, port = item
        else:
            continue
        try:
            r = rpc(str(host), int(port), "server_info")
            res = r.get("result") or {}
            m = (res.get("info") or {}).get("metrics") or res.get("metrics") or {}
            exec_n = m.get("executed_txs")
            checks.append(
                {
                    "name": f"rpc_{label}_{port}",
                    "ok": True,
                    "host": host,
                    "executed_txs": exec_n,
                }
            )
            up += 1
        except Exception as e:
            # Remote RPC fail is degraded (mesh still useful if locals up)
            is_remote = "remote" in str(label) or (
                str(host) not in ("127.0.0.1", "localhost", "0.0.0.0")
            )
            checks.append(
                {
                    "name": f"rpc_{label}_{port}",
                    "ok": False,
                    "host": host,
                    "error": str(e),
                    "warn": "remote_unreachable" if is_remote else None,
                }
            )
            ok = False
            if not is_remote:
                # local packer down is serious
                pass

    if up == 0:
        down = True

    # PID files advisory only
    for name in ("facade", "gmp0", "gmp1", "gmp2"):
        pidf = MESH / "logs" / f"{name}.pid"
        if not pidf.exists():
            continue
        try:
            pid = int(pidf.read_text().strip())
            running = Path(f"/proc/{pid}").exists()
            checks.append(
                {
                    "name": f"pid_{name}",
                    "ok": True,
                    "pid": pid,
                    "running": running,
                    "warn": None if running else "stale_pidfile",
                }
            )
        except Exception as e:
            checks.append({"name": f"pid_{name}", "ok": True, "warn": str(e)})

    # Watchdog heartbeat (advisory)
    hb = MESH / "stack" / "watchdog.heartbeat"
    if hb.exists():
        try:
            age = now - float(hb.read_text().strip())
            checks.append(
                {
                    "name": "watchdog_heartbeat",
                    "ok": True,
                    "age_s": round(age, 1),
                    "warn": "stale" if age > 180 else None,
                }
            )
        except Exception:
            pass

    report = {
        "healthy": ok and not down,
        "degraded": (not ok) and not down,
        "down": down,
        "facade": FACADE,
        "rpc_up": up,
        "rpc_total": len(rpcs),
        "ts": now,
        "checks": checks,
    }
    print(json.dumps(report, indent=2))
    failed = [c["name"] for c in checks if not c.get("ok")]
    warns = [c["name"] for c in checks if c.get("warn")]
    if failed:
        print(f"FAILED: {', '.join(failed)}", file=sys.stderr)
    elif warns:
        print(f"OK (warns: {', '.join(warns)})", file=sys.stderr)
    else:
        print("OK: all checks passed", file=sys.stderr)

    # Persist for status
    try:
        out = MESH / "stack" / "last_health.json"
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(report, indent=2) + "\n")
    except Exception:
        pass

    if down:
        return 2
    if not ok:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
