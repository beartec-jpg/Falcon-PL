#!/usr/bin/env python3
"""Minimal Esplora REST shim over a local bitcoind (regtest / private signet).
Serves the subset of endpoints the vendored BitVM bridge uses:
  GET /tx/:txid/status  GET /tx/:txid/raw  GET /tx/:txid/hex
  GET /blocks/tip/height  GET /blocks/tip/hash  GET /address/:addr/utxo  POST /tx
Usage: esplora_shim.py --datadir DIR [--port 8094]   (base path /regtest/api)"""
import argparse, json, subprocess
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ap = argparse.ArgumentParser(); ap.add_argument("--datadir", required=True); ap.add_argument("--port", type=int, default=8094); ap.add_argument("--also-push-datadir", default=None, help="also submit accepted txs directly to this node (e.g. the miner) to avoid P2P relay lag")
A = ap.parse_args()

def rpc(*args):
    # Pass arguments on stdin: large tx hex exceeds the per-argument exec limit.
    a = list(map(str, args))
    if len(a) > 1:
        p = subprocess.run(["bitcoin-cli", f"-datadir={A.datadir}", "-stdin", a[0]], input="\n".join(a[1:]) + "\n", capture_output=True, text=True)
    else:
        p = subprocess.run(["bitcoin-cli", f"-datadir={A.datadir}", a[0]], capture_output=True, text=True)
    if p.returncode: raise RuntimeError(p.stderr.strip())
    return p.stdout.strip()

def jrpc(*args): return json.loads(rpc(*args))

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, body, ctype="text/plain"):
        b = body if isinstance(body, bytes) else body.encode()
        self.send_response(code); self.send_header("Content-Type", ctype); self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def path_parts(self):
        p = self.path.split("?")[0]
        for pre in ("/regtest/api", "/signet/api", "/api"):
            if p.startswith(pre): p = p[len(pre):]
        return [x for x in p.split("/") if x]
    def do_GET(self):
        q = self.path_parts()
        try:
            if q == ["blocks", "tip", "height"]: return self.send(200, rpc("getblockcount"))
            if q == ["blocks", "tip", "hash"]: return self.send(200, rpc("getbestblockhash"))
            if len(q) == 3 and q[0] == "tx" and q[2] == "status":
                try: t = jrpc("getrawtransaction", q[1], "true")
                except RuntimeError: return self.send(200, json.dumps({"confirmed": False}), "application/json")
                if t.get("blockhash"):
                    b = jrpc("getblockheader", t["blockhash"])
                    s = {"confirmed": True, "block_height": b["height"], "block_hash": t["blockhash"], "block_time": b["time"]}
                else: s = {"confirmed": False}
                return self.send(200, json.dumps(s), "application/json")
            if len(q) == 3 and q[0] == "tx" and q[2] in ("raw", "hex"):
                try: h = rpc("getrawtransaction", q[1])
                except RuntimeError: return self.send(404, "Transaction not found")
                return self.send(200, bytes.fromhex(h) if q[2] == "raw" else h, "application/octet-stream")
            if len(q) == 3 and q[0] == "address" and q[2] == "utxo":
                r = jrpc("scantxoutset", "start", json.dumps([f"addr({q[1]})"]))
                tip = int(rpc("getblockcount")); out = []
                for u in r.get("unspents", []):
                    bh = rpc("getblockhash", u["height"])
                    out.append({"txid": u["txid"], "vout": u["vout"], "value": round(u["amount"] * 1e8),
                                "status": {"confirmed": True, "block_height": u["height"], "block_hash": bh, "block_time": 0}})
                # Esplora also lists unconfirmed outputs from the mempool.
                spent = set()
                for txid in jrpc("getrawmempool"):
                    try: t = jrpc("getrawtransaction", txid, "true")
                    except RuntimeError: continue
                    for i in t["vin"]: spent.add((i.get("txid"), i.get("vout")))
                    for o in t["vout"]:
                        if o["scriptPubKey"].get("address") == q[1]:
                            out.append({"txid": txid, "vout": o["n"], "value": round(o["value"] * 1e8), "status": {"confirmed": False}})
                out = [u for u in out if (u["txid"], u["vout"]) not in spent]
                return self.send(200, json.dumps(out), "application/json")
            return self.send(404, "not found")
        except Exception as e:
            return self.send(400, str(e))
    def do_POST(self):
        q = self.path_parts(); n = int(self.headers.get("Content-Length", 0)); body = self.rfile.read(n).decode().strip()
        if q == ["tx"]:
            try:
                r = rpc("sendrawtransaction", body, 0)
                if A.also_push_datadir:
                    subprocess.run(["bitcoin-cli", f"-datadir={A.also_push_datadir}", "-stdin", "sendrawtransaction"], input=body + "\n0\n", capture_output=True, text=True)
                return self.send(200, r)
            except RuntimeError as e: return self.send(400, str(e))
        self.send(404, "not found")

ThreadingHTTPServer(("127.0.0.1", A.port), H).serve_forever()
