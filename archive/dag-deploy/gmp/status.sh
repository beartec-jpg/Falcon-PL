#!/usr/bin/env bash
# One-glance status for the boring lab stack.
#   bash deploy/gmp/status.sh
#   bash deploy/gmp/status.sh --json
set -uo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1091
if [[ -f "$DIR/stack.env.default" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$DIR/stack.env.default" || true
  set +a
fi
MESH="${MESH_ROOT:-$HOME/falcon-dag-mesh}"
JSON=0
[[ "${1:-}" == "--json" ]] && JSON=1

set +e
python3 "$DIR/health.py" >"$MESH/stack/last_health.json" 2>"$MESH/stack/last_health.err"
HC=$?
set -e

export MESH HC JSON
python3 - <<'PY'
import json, os, time
from pathlib import Path

mesh = Path(os.environ["MESH"])
hc = int(os.environ["HC"])
want_json = os.environ.get("JSON") == "1"

health_s = {0: "OK", 1: "DEGRADED", 2: "DOWN"}.get(hc, f"UNKNOWN({hc})")

wd_pid = None
wd_alive = False
pidf = mesh / "logs" / "watchdog.pid"
if pidf.exists():
    try:
        wd_pid = int(pidf.read_text().strip())
        wd_alive = Path(f"/proc/{wd_pid}").exists()
    except Exception:
        pass

hb_age = None
hbf = mesh / "stack" / "watchdog.heartbeat"
if hbf.exists():
    try:
        hb_age = int(time.time() - float(hbf.read_text().strip()))
    except Exception:
        pass

smoke_pass = "?"
sf = mesh / "stack" / "last_smoke.json"
if sf.exists():
    try:
        smoke_pass = "PASS" if json.loads(sf.read_text()).get("pass") else "FAIL"
    except Exception:
        smoke_pass = "?"

uptime = None
stf = mesh / "stack" / "state.json"
if stf.exists():
    try:
        uptime = int(time.time() - float(json.loads(stf.read_text()).get("started_at", time.time())))
    except Exception:
        pass

h = {}
try:
    h = json.loads((mesh / "stack" / "last_health.json").read_text())
except Exception:
    pass

payload = {
    "health": health_s,
    "health_code": hc,
    "watchdog_pid": wd_pid,
    "watchdog_alive": wd_alive,
    "watchdog_heartbeat_age_s": hb_age,
    "last_smoke": smoke_pass,
    "uptime_s": uptime,
    "rpc_up": h.get("rpc_up"),
    "rpc_total": h.get("rpc_total"),
    "facade": h.get("facade") or os.environ.get("FACADE_URL"),
}

if want_json:
    print(json.dumps(payload, indent=2))
else:
    print("=== Falcon lab stack ===")
    print(f"health:     {health_s} (exit {hc})")
    if wd_alive:
        print(f"watchdog:   alive pid={wd_pid} hb_age={hb_age}s")
    else:
        print("watchdog:   not running")
    print(f"last smoke: {smoke_pass}")
    print(f"uptime:     {uptime}s since start-stack" if uptime is not None else "uptime:     ?")
    print(f"facade:     {payload['facade']}")
    print()
    if h.get("rpc_total") is not None:
        print(f"rpc:        {h.get('rpc_up')}/{h.get('rpc_total')} up")
    failed = [c["name"] for c in h.get("checks", []) if not c.get("ok")]
    warns = [f"{c['name']}({c['warn']})" for c in h.get("checks", []) if c.get("warn")]
    if failed:
        print("failed:    ", ", ".join(failed))
    if warns:
        print("warns:     ", ", ".join(warns))
    for c in h.get("checks", []):
        if str(c.get("name", "")).startswith("bridge_"):
            print(f"  {c['name']}: bal={c.get('balance')} bridge={c.get('bridge')}")
    print()
    print("cmds: health.py | smoke.py | start-stack.sh [--watchdog] | stop-stack.sh [--all]")
    print("      watchdog.sh --daemon | status.sh [--json]")

raise SystemExit(hc)
PY
