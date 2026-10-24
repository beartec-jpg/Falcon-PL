#!/usr/bin/env python3
"""Roll LC binary onto 2300 seats. Does NOT rsync keys/secrets."""
from __future__ import annotations

import os
import subprocess
import sys
import time

SSH = [
    "ssh",
    "-o",
    "ConnectTimeout=15",
    "-o",
    "BatchMode=yes",
    "-o",
    "StrictHostKeyChecking=accept-new",
]
RSYNC_E = "ssh -o ConnectTimeout=15 -o BatchMode=yes -o StrictHostKeyChecking=accept-new"
NEW_NODE = "/home/falcon/falcon-pl/falcon-pl-rs/target/release/falcon-pl-node"
NEW_CTL = "/home/falcon/falcon-pl/falcon-pl-rs/target/release/falcon-pl-ctl"
CTL = "/home/falcon/falcon-pl-public-testnet-2300/bin/falcon-pl-ctl"
WANT_SHA = os.environ.get("ROLL_WANT_SHA", "")

# id, ssh, rroot, rpc, exe, extra flags after --id, listen port
SEATS = [
    (
        "v2",
        "root@100.64.0.2",
        "/root/falcon-pl-public-testnet-2300",
        "100.64.0.2:19302",
        "falcon-pl-archive",
        "--hub --archive",
        19302,
        False,
    ),
    (
        "v3",
        "host2",
        "/home/falcon/falcon-pl-public-testnet-2300",
        "100.64.0.3:19303",
        "falcon-pl-hub",
        "--hub",
        19303,
        False,
    ),
    (
        "v4",
        "root@100.64.0.4",
        "/root/falcon-pl-public-testnet-2300",
        "100.64.0.4:19304",
        "falcon-pl-archive",
        "--hub --archive",
        19304,
        False,
    ),
    (
        "v6",
        "root@100.64.0.6",
        "/root/falcon-pl-public-testnet-2300",
        "100.64.0.6:19306",
        "falcon-pl-val",
        "--join",
        19306,
        False,
    ),
    (
        "v7",
        "root@100.64.0.7",
        "/root/falcon-pl-public-testnet-2300",
        "100.64.0.7:19307",
        "falcon-pl-val",
        "--join",
        19307,
        False,
    ),
    (
        "v5",
        "root@100.64.0.5",
        "/home/falcon/falcon-pl-public-testnet-2300",
        "100.64.0.5:19305",
        "falcon-pl-val",
        "",
        19305,
        True,
    ),
]

PEERS = {
    "v2": "100.64.0.1:19301,100.64.0.3:19303,100.64.0.4:19304,100.64.0.5:19305,100.64.0.6:19306,100.64.0.7:19307",
    "v3": "100.64.0.1:19301,100.64.0.2:19302,100.64.0.4:19304,100.64.0.5:19305,100.64.0.6:19306,100.64.0.7:19307",
    "v4": "100.64.0.1:19301,100.64.0.2:19302,100.64.0.3:19303,100.64.0.5:19305,100.64.0.6:19306,100.64.0.7:19307",
    "v5": "100.64.0.1:19301,100.64.0.2:19302,100.64.0.3:19303,100.64.0.4:19304,100.64.0.6:19306,100.64.0.7:19307",
    "v6": "100.64.0.1:19301,100.64.0.2:19302,100.64.0.3:19303,100.64.0.4:19304,100.64.0.5:19305,100.64.0.7:19307",
    "v7": "100.64.0.1:19301,100.64.0.2:19302,100.64.0.3:19303,100.64.0.4:19304,100.64.0.5:19305,100.64.0.6:19306",
}
HUBS = "100.64.0.4:19304,100.64.0.2:19302,100.64.0.3:19303,100.64.0.1:19301"
COMMON = (
    "--validators v1,v2,v3,v4,v5 --scheme falcon-512 --load-wallets 0 --bond 1000 "
    "--network-id 2300 --seal-interval-ms 200 --pack-budget-ms 1000 --progress-timeout-ms 3000 "
    "--max-txs 128 --max-mempool-txs 50000 --epoch-ms 604800000 --first-claim-epoch 1 "
    "--unbond-cooldown-ms 1209600000 --snapshot-interval-ms 60000 --online-epoch-heights 10 --lock-public-wire"
)


def sh(cmd, **kw):
    kw.setdefault("check", True)
    return subprocess.run(cmd, **kw)


def ssh(host, cmd, check=True):
    return subprocess.run(SSH + [host, cmd], text=True, capture_output=True, check=check)


def tip(addr):
    try:
        raw = subprocess.check_output([CTL, "--addr", addr, "status"], text=True, timeout=12)
    except Exception:
        return -1, ""
    i = raw.find("{")
    if i < 0:
        return -1, ""
    import json

    b = json.JSONDecoder().raw_decode(raw[i:])[0]
    body = b.get("body") or b
    return int(body.get("tip_height") or -1), str(body.get("tip_hash") or "")


def wait_tip(addr, want, tries=40):
    last = -1
    ha = ""
    for i in range(tries):
        last, ha = tip(addr)
        print(f"  wait {addr} tip={last} want>={want} ({i+1}/{tries})", flush=True)
        if last >= want and last > 0:
            return last, ha
        time.sleep(3)
    raise SystemExit(f"timeout waiting {addr} >={want} last={last}")


def install(host, rroot):
    sh(
        [
            "rsync",
            "-az",
            "-e",
            RSYNC_E,
            NEW_NODE,
            f"{host}:{rroot}/bin/falcon-pl-node.new",
        ]
    )
    sh(
        [
            "rsync",
            "-az",
            "-e",
            RSYNC_E,
            NEW_CTL,
            f"{host}:{rroot}/bin/falcon-pl-ctl.new",
        ]
    )
    r = ssh(
        host,
        f"mv -f {rroot}/bin/falcon-pl-node.new {rroot}/bin/falcon-pl-node; "
        f"mv -f {rroot}/bin/falcon-pl-ctl.new {rroot}/bin/falcon-pl-ctl; "
        f"chmod +x {rroot}/bin/falcon-pl-node {rroot}/bin/falcon-pl-ctl; "
        f"ln -sfn falcon-pl-node {rroot}/bin/falcon-pl-archive; "
        f"ln -sfn falcon-pl-node {rroot}/bin/falcon-pl-hub; "
        f"ln -sfn falcon-pl-node {rroot}/bin/falcon-pl-val; "
        f"sha256sum {rroot}/bin/falcon-pl-node",
    )
    sha = (r.stdout or "").split()[0]
    print(f"  installed sha={sha}", flush=True)
    if WANT_SHA and sha != WANT_SHA:
        raise SystemExit(f"sha mismatch {sha}")


def stop(host, rroot, vid):
    ssh(
        host,
        f"pf={rroot}/run/{vid}.pid; "
        f"if [[ -f $pf ]]; then pid=$(cat $pf); kill $pid 2>/dev/null || true; "
        f"sleep 1; kill -0 $pid 2>/dev/null && kill -9 $pid 2>/dev/null || true; rm -f $pf; fi; "
        f"pkids=$(ps -eo pid,args | awk '/falcon-pl-(archive|hub|val|node)/ && /--id {vid} / && !/awk/ {{print $1}}'); "
        f"for p in $pkids; do kill $p 2>/dev/null || true; done; sleep 1; "
        f"for p in $pkids; do kill -9 $p 2>/dev/null || true; done; "
        f"rm -f {rroot}/run/{vid}.admin.sock",
        check=False,
    )


def start(host, rroot, vid, exe, extra, port, as_falcon):
    chain = 128 if exe == "falcon-pl-val" else 20000
    flags = (
        f"--id {vid} {extra} --listen 0.0.0.0:{port} --peers {PEERS[vid]} "
        f"--keys-dir {rroot}/keys --max-chain-ledgers {chain} "
        f"--admin-sock {rroot}/run/{vid}.admin.sock "
        f"--snapshot {rroot}/snapshots/{vid}.json --snapshot-out {rroot}/snapshots/{vid}.json "
        f"--hubs {HUBS} {COMMON}"
    )
    inner = (
        f"nohup {rroot}/bin/{exe} {flags} >> {rroot}/logs/{vid}.log 2>&1 & "
        f"echo $! > {rroot}/run/{vid}.pid; echo started {vid} pid=$(cat {rroot}/run/{vid}.pid)"
    )
    if as_falcon:
        cmd = (
            f"sudo -u falcon bash -lc {repr(inner)}"
        )
        # sudo -u falcon: write pid of the node, not sudo. Use start-stop from falcon home.
        cmd = (
            f"rm -f {rroot}/run/{vid}.admin.sock; "
            f"sudo -u falcon env -C {rroot} bash -c "
            f"'nohup {rroot}/bin/{exe} {flags} >> {rroot}/logs/{vid}.log 2>&1 & echo $! > {rroot}/run/{vid}.pid'; "
            f"echo started {vid} pid=$(cat {rroot}/run/{vid}.pid)"
        )
    else:
        cmd = f"rm -f {rroot}/run/{vid}.admin.sock; {inner}"
    r = ssh(host, cmd)
    print(" ", (r.stdout or "").strip(), (r.stderr or "").strip(), flush=True)


def main():
    only = sys.argv[1:] or [s[0] for s in SEATS]
    head, _ = tip("127.0.0.1:19301")
    print(f"hub tip {head}", flush=True)
    by_id = {s[0]: s for s in SEATS}
    for vid in only:
        spec = by_id[vid]
        _, host, rroot, rpc, exe, extra, port, as_falcon = spec
        print(f"\n=== roll {vid} {host} ===", flush=True)
        h0, ha0 = tip("127.0.0.1:19301")
        install(host, rroot)
        stop(host, rroot, vid)
        time.sleep(1)
        start(host, rroot, vid, exe, extra, port, as_falcon)
        time.sleep(4)
        wait_tip(rpc, max(1, h0 - 32), 50)
        h1, ha1 = tip(rpc)
        hh, hah = tip("127.0.0.1:19301")
        print(f"  {vid} tip={h1} hash={ha1[:16]} hub={hh} {hah[:16]}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
