#!/usr/bin/env python3
import json, socket, time, os, sys
port = int(os.environ["PORT_BASE"])
art = os.environ["ART"]
net = int(os.environ["NET"])

def once(hello, label):
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
    time.sleep(0.7)
    resp = b""
    s.settimeout(2.0)
    try:
        while True:
            c = s.recv(65536)
            if not c:
                break
            resp += c
            if len(resp) > 8192:
                break
    except Exception:
        pass
    try:
        s.close()
    except Exception:
        pass
    return {
        "label": label,
        "node_hello_type": (peer or {}).get("type"),
        "node_hello_caps": (peer or {}).get("capabilities") or [],
        "response": resp.decode(errors="replace")[:900],
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

h = dict(base); h["id"] = "probe-capable"; h["capabilities"] = ["utc-header-v5"]
r = once(h, "capable")
r["ok"] = ("utc-header-v5" in r["node_hello_caps"]) and ("utc-header-v5 capability required" not in r["response"])
results.append(r)

h = dict(base); h["id"] = "probe-old-empty"; h["capabilities"] = []
r = once(h, "old-empty-caps")
r["ok"] = "utc-header-v5" in r["response"]
results.append(r)

h = dict(base); h["id"] = "probe-old-omit"
r = once(h, "old-omit-caps")
r["ok"] = "utc-header-v5" in r["response"]
results.append(r)

h = dict(base); h["id"] = "probe-wrongnet"; h["hello_network_id"] = 9999; h["capabilities"] = ["utc-header-v5"]
r = once(h, "wrong-network")
low = r["response"].lower()
r["ok"] = ("network" in low) or ("mismatch" in low)
results.append(r)

print(json.dumps(results, indent=2))
open(art + "/hello_compat.json", "w").write(json.dumps(results, indent=2))
all_ok = all(x.get("ok") for x in results)
open(art + "/hello_compat_ok", "w").write("ok\n" if all_ok else "fail\n")
if not all_ok:
    sys.exit("HELLO_COMPAT_FAIL")
print("hello_compat_ok")
