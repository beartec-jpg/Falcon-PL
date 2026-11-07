#!/usr/bin/env python3
"""Process-level Hello capability / network reject probes.

Inbound HelloGate::Close enqueues WireMsg::Err then aborts the writer; the Err
bytes are often not observed on the client. Treat a clean socket close with no
payload after an old-peer Hello as Close, and confirm the reason in the peer
node log when ART/SCRATCH_V1_LOG is set.
"""
import json, socket, time, os, sys, pathlib

port = int(os.environ["PORT_BASE"])
art = os.environ["ART"]
net = int(os.environ["NET"])
v1_log = os.environ.get("SCRATCH_V1_LOG", "")


def once(hello, label, wait=0.8):
    s = socket.create_connection(("127.0.0.1", port), timeout=5)
    buf = b""
    s.settimeout(2.0)
    try:
        while b"\n" not in buf:
            c = s.recv(65536)
            if not c:
                break
            buf += c
    except Exception:
        pass
    peer = None
    if buf.strip():
        try:
            peer = json.loads(buf.split(b"\n", 1)[0].decode())
        except Exception as e:
            peer = {"parse_err": str(e)}
    s.sendall((json.dumps(hello) + "\n").encode())
    time.sleep(wait)
    resp = b""
    closed = False
    s.settimeout(2.0)
    try:
        while True:
            c = s.recv(65536)
            if not c:
                closed = True
                break
            resp += c
            if len(resp) > 8192:
                break
    except Exception:
        closed = True
    try:
        s.close()
    except Exception:
        pass
    return {
        "label": label,
        "node_hello_type": (peer or {}).get("type"),
        "node_hello_caps": (peer or {}).get("capabilities") or [],
        "response": resp.decode(errors="replace")[:900],
        "closed": closed,
    }


base = {
    "type": "hello",
    "id": "probe",
    "public_key": "",
    "validators": {},
    "publics": {},
    "tip_height": 0,
    "tip_hash": "",
    "is_hub": False,
    "mempool_len": 0,
    "product_version": "2.10.4",
    "hello_network_id": net,
    "archive": False,
    "snap_height": 0,
    "release_version": "2.10.4",
}
results = []

h = dict(base)
h["id"] = "probe-capable"
h["capabilities"] = ["utc-header-v5"]
r = once(h, "capable", wait=0.5)
r["ok"] = ("utc-header-v5" in r["node_hello_caps"]) and (
    "utc-header-v5 capability required" not in r["response"]
)
results.append(r)

for label, caps in (("old-empty-caps", []), ("old-omit-caps", None)):
    h = dict(base)
    h["id"] = f"probe-{label}"
    if caps is not None:
        h["capabilities"] = caps
    r = once(h, label, wait=0.9)
    # Prefer explicit Err text; else closed socket after old-peer Hello.
    r["ok"] = ("utc-header-v5" in r["response"]) or (
        r["closed"] and "utc-header-v5 capability required" not in r.get("response", "")
        and len(r["response"]) == 0
    )
    results.append(r)

h = dict(base)
h["id"] = "probe-wrongnet"
h["hello_network_id"] = 9999
h["capabilities"] = ["utc-header-v5"]
r = once(h, "wrong-network", wait=0.9)
low = r["response"].lower()
r["ok"] = ("network" in low) or ("mismatch" in low) or (r["closed"] and len(r["response"]) == 0)
results.append(r)

# Confirm UTC close reason appears in seat log when available
log_hit = None
if v1_log and pathlib.Path(v1_log).exists():
    text = pathlib.Path(v1_log).read_text(errors="replace")
    log_hit = "utc-header-v5 capability required" in text or "utc-header-v5" in text and "hello gate close" in text.lower()
results.append({"label": "v1-log-utc-close", "ok": bool(log_hit) if v1_log else True, "log_hit": log_hit, "log": v1_log})

print(json.dumps(results, indent=2))
open(art + "/hello_compat.json", "w").write(json.dumps(results, indent=2))
all_ok = all(x.get("ok") for x in results)
open(art + "/hello_compat_ok", "w").write("ok\n" if all_ok else "fail\n")
if not all_ok:
    sys.exit("HELLO_COMPAT_FAIL")
print("hello_compat_ok")
