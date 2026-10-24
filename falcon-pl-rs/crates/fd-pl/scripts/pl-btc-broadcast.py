#!/usr/bin/env python3
"""Broadcast ledger-signed BTC withdraw txs. Holds no vault key."""
from __future__ import annotations

import json
import os
import time
import urllib.request

RPC = os.environ.get("FPL_RPC", "127.0.0.1:19301")
CTL = os.environ.get(
    "FPL_CTL", "/home/falcon/falcon-pl-public-testnet-2300/bin/falcon-pl-ctl"
)
POLL = int(os.environ.get("POLL_SEC", "20"))
BROADCAST = os.environ.get("BTC_BROADCAST_URL", "")  # esplora /tx POST, empty = print only


def main() -> None:
    import subprocess

    print("pl-btc-broadcast: no vault key; waits for signed notes in status", flush=True)
    seen: set[str] = set()
    while True:
        try:
            out = subprocess.check_output(
                [CTL, "--addr", RPC, "status"], text=True, timeout=20
            )
            body = json.loads(out)
            if isinstance(body, dict) and "rails" not in body:
                body = body.get("body", body)
            for r in body.get("rails") or []:
                if r.get("asset") != "BTC":
                    continue
                for w in r.get("withdrawals") or []:
                    nid = str(w.get("note_id") or "")
                    signed = str(w.get("signed_btc_tx") or "")
                    if not nid or nid in seen or not signed:
                        continue
                    print(f"SIGNED_NOTE {nid} (broadcast={'yes' if BROADCAST else 'dry'})", flush=True)
                    if BROADCAST:
                        req = urllib.request.Request(
                            BROADCAST,
                            data=signed.encode(),
                            method="POST",
                            headers={"Content-Type": "text/plain"},
                        )
                        urllib.request.urlopen(req, timeout=20)
                    seen.add(nid)
        except Exception as e:
            print(f"WARN {e}", flush=True)
        time.sleep(POLL)


if __name__ == "__main__":
    main()
