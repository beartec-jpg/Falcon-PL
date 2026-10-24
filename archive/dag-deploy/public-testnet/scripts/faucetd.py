#!/usr/bin/env python3
"""
Public testnet faucet (R2).

Rate-limited drip of native FDAG from pre-funded genesis accounts via the GMP facade.
No private keys required here when validators run with --auto-sign + wallet-dir
(lab/testnet convenience). Clients never need faucet secrets.

  GET  /v1/info
  GET  /v1/health
  POST /v1/drip   {"destination":"acct7"} | {"destination":"acct7","amount":1000}
  GET  /v1/limits

Env:
  FACADE_URL=http://127.0.0.1:18499
  FAUCET_LISTEN=0.0.0.0:18550
  FAUCET_AMOUNT=1000
  FAUCET_FEE=10
  FAUCET_PER_IP_HOUR=20
  FAUCET_PER_DEST_HOUR=5
  FAUCET_SOURCES=acct1,acct2,acct3,acct4,acct5   # funded genesis sources on s0
"""
from __future__ import annotations

import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request
from collections import defaultdict, deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

FACADE = os.environ.get("FACADE_URL", "http://127.0.0.1:18499").rstrip("/")
LISTEN = os.environ.get("FAUCET_LISTEN", "0.0.0.0:18550")
AMOUNT = int(os.environ.get("FAUCET_AMOUNT", "1000"))
FEE = int(os.environ.get("FAUCET_FEE", "10"))
PER_IP = int(os.environ.get("FAUCET_PER_IP_HOUR", "20"))
PER_DEST = int(os.environ.get("FAUCET_PER_DEST_HOUR", "5"))
SOURCES = [
    s.strip()
    for s in os.environ.get("FAUCET_SOURCES", "acct1,acct2,acct3,acct4,acct5").split(",")
    if s.strip()
]
MESH = Path(os.environ.get("MESH_ROOT", Path.home() / "falcon-dag-mesh"))
GENESIS = Path(
    os.environ.get(
        "TESTNET_GENESIS",
        Path(__file__).resolve().parents[1] / "genesis" / "network.json",
    )
)
SEQ_FILE = MESH / "stack" / "faucet_seq.json"
STATE_LOCK = threading.Lock()
# ip/dest -> deque of timestamps
HITS_IP: dict[str, deque] = defaultdict(deque)
HITS_DEST: dict[str, deque] = defaultdict(deque)
SRC_I = 0
STATS = {
    "drip_ok": 0,
    "drip_err": 0,
    "rate_limited": 0,
    "started_at": time.time(),
}


def http_json(url: str, method: str = "GET", body=None, timeout: float = 12.0):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        url, data=data, method=method, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def load_seqs() -> dict:
    if SEQ_FILE.exists():
        try:
            return json.loads(SEQ_FILE.read_text())
        except Exception:
            pass
    return {}


def save_seqs(d: dict):
    SEQ_FILE.parent.mkdir(parents=True, exist_ok=True)
    tmp = SEQ_FILE.with_suffix(".tmp")
    tmp.write_text(json.dumps(d, indent=2) + "\n")
    tmp.replace(SEQ_FILE)


def next_seq(account: str) -> int:
    """Prefer chain sequence via any public RPC if known; else local tracker."""
    seqs = load_seqs()
    local = int(seqs.get(account) or 1)
    # Probe route for account's shard RPC through facade status is heavy; use facade
    # account path: submit will nack on bad seq. We track after success.
    # Try RPC from network env
    for port in (18001, 18004):
        try:
            body = json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "account_info",
                    "params": [{"account": account}],
                }
            ).encode()
            req = urllib.request.Request(
                f"http://127.0.0.1:{port}/",
                data=body,
                method="POST",
                headers={"Content-Type": "application/json"},
            )
            with urllib.request.urlopen(req, timeout=3) as r:
                d = json.loads(r.read().decode())
            res = d.get("result") or {}
            chain = int(res.get("sequence") or 1)
            return max(chain, local)
        except Exception:
            continue
    return local


def prune(dq: deque, window: float = 3600.0):
    now = time.time()
    while dq and now - dq[0] > window:
        dq.popleft()


def rate_ok(ip: str, dest: str) -> tuple[bool, str]:
    with STATE_LOCK:
        prune(HITS_IP[ip])
        prune(HITS_DEST[dest])
        if len(HITS_IP[ip]) >= PER_IP:
            return False, f"ip rate limit {PER_IP}/hour"
        if len(HITS_DEST[dest]) >= PER_DEST:
            return False, f"destination rate limit {PER_DEST}/hour"
        return True, ""


def record_hit(ip: str, dest: str):
    with STATE_LOCK:
        HITS_IP[ip].append(time.time())
        HITS_DEST[dest].append(time.time())


def pick_source() -> str:
    global SRC_I
    with STATE_LOCK:
        s = SOURCES[SRC_I % len(SOURCES)]
        SRC_I += 1
        return s


def network_info() -> dict:
    info = {
        "service": "falcon-testnet-faucet",
        "network_name": "falcon-dag-testnet-1",
        "facade": FACADE,
        "amount_default": AMOUNT,
        "fee": FEE,
        "sources": SOURCES,
        "limits": {"per_ip_hour": PER_IP, "per_dest_hour": PER_DEST},
        "stats": dict(STATS),
        "uptime_s": round(time.time() - STATS["started_at"], 1),
    }
    if GENESIS.exists():
        try:
            g = json.loads(GENESIS.read_text())
            info["network"] = {
                "name": g.get("network_name"),
                "version": g.get("version"),
                "crypto": g.get("crypto"),
                "shards": len(g.get("shards") or []),
                "unl_public_sha256": g.get("unl_public_sha256"),
            }
        except Exception:
            pass
    return info


def do_drip(destination: str, amount: int, ip: str) -> dict:
    if not destination or not destination.startswith("acct"):
        return {"ok": False, "error": "destination must look like acctN"}
    ok, why = rate_ok(ip, destination)
    if not ok:
        STATS["rate_limited"] += 1
        return {"ok": False, "error": why, "rate_limited": True}

    src = pick_source()
    if destination == src:
        return {"ok": False, "error": "destination equals faucet source"}

    seq = next_seq(src)
    body = {
        "account": src,
        "sequence": seq,
        "destination": destination,
        "amount": amount,
        "fee": FEE,
        "client_id": f"faucet-{src}-{seq}-{destination}-{int(time.time())}",
    }
    try:
        r = http_json(f"{FACADE}/v1/submit", "POST", body, timeout=15)
    except Exception as e:
        STATS["drip_err"] += 1
        return {"ok": False, "error": f"facade submit: {e}", "source": src, "seq": seq}

    if r.get("error"):
        STATS["drip_err"] += 1
        return {"ok": False, "error": r.get("error"), "detail": r, "source": src, "seq": seq}

    # Advance local seq on accept
    seqs = load_seqs()
    seqs[src] = seq + 1
    save_seqs(seqs)
    record_hit(ip, destination)
    STATS["drip_ok"] += 1
    return {
        "ok": True,
        "source": src,
        "destination": destination,
        "amount": amount,
        "fee": FEE,
        "sequence": seq,
        "route": r.get("route"),
        "tx_id": r.get("tx_id"),
        "xfer_id": r.get("xfer_id"),
        "status": r.get("status"),
        "client_id": body["client_id"],
        "note": "funds pending until shard executes; poll account_info on public RPC",
    }


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))

    def _read_json(self):
        n = int(self.headers.get("Content-Length") or 0)
        if n <= 0:
            return {}
        raw = self.rfile.read(n)
        try:
            return json.loads(raw.decode())
        except Exception:
            return {}

    def _send(self, code: int, obj: dict):
        body = json.dumps(obj, indent=2).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(body)

    def do_OPTIONS(self):
        self.send_response(204)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET,POST,OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.end_headers()

    def do_GET(self):
        path = self.path.split("?")[0]
        if path in ("/", "/v1/info"):
            self._send(200, network_info())
        elif path == "/v1/health":
            try:
                st = http_json(f"{FACADE}/v1/status", timeout=4)
                ok = st.get("service") == "falcon-gmp-facade"
                self._send(200 if ok else 503, {"ok": ok, "facade": FACADE})
            except Exception as e:
                self._send(503, {"ok": False, "error": str(e)})
        elif path == "/v1/limits":
            self._send(
                200,
                {
                    "per_ip_hour": PER_IP,
                    "per_dest_hour": PER_DEST,
                    "amount_default": AMOUNT,
                    "fee": FEE,
                },
            )
        else:
            self._send(404, {"error": "not found"})

    def do_POST(self):
        path = self.path.split("?")[0]
        if path != "/v1/drip":
            self._send(404, {"error": "not found"})
            return
        body = self._read_json()
        dest = str(body.get("destination") or body.get("account") or "").strip()
        amount = int(body.get("amount") or AMOUNT)
        if amount <= 0 or amount > 1_000_000:
            self._send(400, {"ok": False, "error": "amount out of range (1..1000000)"})
            return
        ip = self.headers.get("X-Forwarded-For", self.client_address[0]).split(",")[0].strip()
        result = do_drip(dest, amount, ip)
        code = 200 if result.get("ok") else (429 if result.get("rate_limited") else 400)
        self._send(code, result)


def main():
    host, _, port_s = LISTEN.partition(":")
    if not port_s:
        host, port_s = "0.0.0.0", host
    port = int(port_s)
    httpd = ThreadingHTTPServer((host, port), Handler)
    print(
        f"faucetd listen={host}:{port} facade={FACADE} sources={SOURCES} amount={AMOUNT}",
        flush=True,
    )
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
