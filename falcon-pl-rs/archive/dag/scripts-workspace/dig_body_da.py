#!/usr/bin/env python3
"""Continuous rate dig with reject/body-DA forensics."""
from __future__ import annotations
import json, os, subprocess, time, urllib.request
from pathlib import Path

BIN = os.environ.get("BIN", str(Path.home() / "falcon-dag-rs/target/release/falcon-dagd"))
N, BASE_RPC, BASE_P2P = 3, 29800, 29900
OUT = Path(f"/tmp/dig-body-{int(time.time())}")
OUT.mkdir(parents=True)
logdir = OUT / "nodes"
logdir.mkdir()
pids = []
RATE = int(os.environ.get("RATE", "30"))
HOLD = float(os.environ.get("HOLD_S", "35"))
ADMIT = os.environ.get("ADMIT_AHEAD", "5000")
REPOOL = os.environ.get("OPEN_BODY_REPOOL", "8")

def log(m):
    print(f"[{time.strftime('%H:%M:%S')}] {m}", flush=True)
    (OUT / "run.log").open("a").write(m + "\n")

def kill():
    for p in pids:
        try: os.kill(p, 9)
        except Exception: pass
    pids.clear()

def rpc(port, method, params=None, timeout=4.0):
    pl = [{}] if params is None else ([params] if not isinstance(params, list) else params)
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": pl}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{port}/", data=body,
                                 headers={"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=timeout) as r:
        d = json.load(r)
    if "error" in d:
        raise RuntimeError(d["error"])
    return d

def packer(i):
    d = rpc(BASE_RPC + i, "server_info")
    info = ((d.get("result") or {}).get("info") or {})
    m = info.get("metrics") or {}
    ps = m.get("pack_stall") or {}
    return m, ps

def start():
    kill()
    for i in range(N):
        subprocess.run(["fuser", "-k", f"{BASE_RPC+i}/tcp"], capture_output=True)
        subprocess.run(["fuser", "-k", f"{BASE_P2P+i}/tcp"], capture_output=True)
    time.sleep(0.3)
    keys, wallets = logdir / "keys", logdir / "wallets"
    keys.mkdir(exist_ok=True); wallets.mkdir(exist_ok=True)
    subprocess.check_call([BIN, "--gen-keys", "--keys-dir", str(keys), "--unl", "val1,val2,val3"], stdout=subprocess.DEVNULL)
    subprocess.check_call([BIN, "--gen-wallet-keys", "--wallet-dir", str(wallets), "--faucet-accounts", "20"], stdout=subprocess.DEVNULL)
    for i in range(N):
        id_ = f"val{i+1}"
        peers = ",".join(f"127.0.0.1:{BASE_P2P+j}" for j in range(N) if j != i)
        data = logdir / id_
        data.mkdir(exist_ok=True)
        logf = open(logdir / f"{id_}.log", "ab")
        cmd = [BIN, "--id", id_, "--role", "full",
               "--rpc", f"127.0.0.1:{BASE_RPC+i}", "--p2p", f"127.0.0.1:{BASE_P2P+i}",
               "--peers", peers, "--unl", "val1,val2,val3",
               "--worker-index", str(i), "--worker-count", str(N),
               "--network-id", "9100", "--crypto", "falcon-512",
               "--require-tx-sigs", "--auto-sign",
               "--keys-dir", str(keys), "--wallet-dir", str(wallets),
               "--batch-interval", "0.05", "--max-batch-txs", "64",
               "--max-batch-parents", "3", "--unl-threshold", "0.6",
               "--max-uncertified-per-creator", "8",
               "--single-packer=false", "--adaptive-batch=true",
               "--uncertified-ttl", "0",
               "--open-body-repool-after", REPOOL,
               "--admit-ahead-cap", ADMIT,
               "--admit-busy-uncert-age", "0",
               "--conf-pool", "true", "--pack-check-slot", "0.25",
               "--data-dir", str(data)]
        env = os.environ.copy()
        env["BULLSHARK_TIEBREAKER"] = "ACCOUNT_ID_LEXICOGRAPHIC"
        env["DIRECT_SUBMIT"] = "1"
        p = subprocess.Popen(cmd, stdout=logf, stderr=subprocess.STDOUT, env=env)
        pids.append(p.pid)
        for _ in range(80):
            try:
                rpc(BASE_RPC + i, "ping"); break
            except Exception:
                time.sleep(0.1)
        log(f"started {id_} pid={p.pid}")

def submit(seq):
    key = min(seq.keys(), key=lambda a: (seq[a], a))
    s = seq[key]
    dest = "acct20" if key != "acct20" else "acct19"
    d = rpc(BASE_RPC, "submit", {"account": key, "sequence": s, "destination": dest, "amount": 1, "fee": 10})
    code = (d.get("result") or {}).get("engine_result")
    if code == "tesSUCCESS":
        seq[key] = s + 1
    return code

def main():
    log(f"OUT={OUT} RATE={RATE} HOLD={HOLD} REPOOL={REPOOL}")
    try:
        start()
        time.sleep(1.2)
        seq = {f"acct{a}": 1 for a in range(1, 13)}
        samples = []
        ok = busy = other = 0
        t0 = time.time()
        while time.time() - t0 < HOLD:
            sec0 = time.time()
            for _ in range(RATE):
                if time.time() - t0 >= HOLD:
                    break
                c = submit(seq)
                if c == "tesSUCCESS":
                    ok += 1
                elif c == "terBUSY":
                    busy += 1
                else:
                    other += 1
            # sample all 3 nodes
            nodes = []
            for i in range(3):
                try:
                    m, ps = packer(i)
                    mn = m.get("mesh_net") or {}
                    pull = m.get("pull") or mn.get("pull") or {}
                    nodes.append({
                        "id": f"val{i+1}",
                        "executed": int(m.get("executed_txs") or 0),
                        "mempool": int(m.get("mempool") or 0),
                        "uncert": int(m.get("uncertified") or 0),
                        "uncert_me": int(ps.get("uncertified_by_me") or 0),
                        "age": ps.get("oldest_uncertified_age_s"),
                        "lag": ps.get("admit_exec_lag"),
                        "foreign": (ps.get("open_conf") or {}).get("foreign_sigs"),
                        "need_seed": ps.get("open_needs_body_seed"),
                        "rej": m.get("rejected_batches") or ps.get("rejected_batches"),
                        "eq": m.get("equivocations") or ps.get("equivocations"),
                        "reasons": ps.get("reject_reasons") or m.get("reject_reasons"),
                        "ingest_new": ps.get("ingest_new_ok") or m.get("ingest_new_ok"),
                        "ingest_merge": ps.get("ingest_merge_ok") or m.get("ingest_merge_ok"),
                        "dropped_inbound": mn.get("dropped_inbound"),
                        "bodies_sent": mn.get("batch_bodies_sent"),
                        "reseed_dir": mn.get("open_reseed_directed"),
                        "reseed_mesh": mn.get("open_reseed_mesh"),
                        "sig_to_creator": mn.get("sig_to_creator_sent"),
                        "sig_to_creator_throttled": mn.get("sig_to_creator_throttled"),
                        "bodies_served": pull.get("bodies_served"),
                        "wants_sent": pull.get("wants_sent"),
                        "pack_skip": {k: v for k, v in (m.get("pack_skip_reasons") or {}).items()
                                      if k in ("packed", "uncertified_cap", "self_open_on_parents",
                                               "open_body_repool", "open_body_repool_txs", "open_body_repool_suppressed_quiet",
                                               "pack_cursor_reclaim", "remote_batch_signed", "remote_batch_deferred_sign",
                                               "conf_pool_signed", "conf_pool_sign_deferred_cap", "foreign_sig_merged_from_sig", "foreign_sig_merged_from_body", "foreign_sig_parked", "foreign_sig_applied_parked", "sig_reannounce_to_creator", "reseed_suppressed_progress")},
                    })
                except Exception as e:
                    nodes.append({"id": f"val{i+1}", "error": str(e)})
            row = {"t": round(time.time() - t0, 1), "ok": ok, "busy": busy, "nodes": nodes}
            samples.append(row)
            n1 = nodes[0] if nodes else {}
            log(
                f"t={row['t']} exec={n1.get('executed')} lag={n1.get('lag')} "
                f"uncert_me={[n.get('uncert_me') for n in nodes]} "
                f"foreign={[n.get('foreign') for n in nodes]} "
                f"need_seed={[n.get('need_seed') for n in nodes]} "
                f"rej={[n.get('rej') for n in nodes]} "
                f"eq={[n.get('eq') for n in nodes]} "
                f"reasons_v1={n1.get('reasons')} "
                f"ingest_new={[n.get('ingest_new') for n in nodes]} "
                f"drop={[n.get('dropped_inbound') for n in nodes]} "
                f"bsent={[n.get('bodies_sent') for n in nodes]} "
                f"rdir={[n.get('reseed_dir') for n in nodes]} "
                f"rmesh={[n.get('reseed_mesh') for n in nodes]} "
                f"sig2c={[n.get('sig_to_creator') for n in nodes]} "
                f"served={[n.get('bodies_served') for n in nodes]} "
                f"ok={ok} busy={busy}"
            )
            left = 1.0 - (time.time() - sec0)
            if left > 0 and time.time() - t0 < HOLD:
                time.sleep(left)
        (OUT / "samples.json").write_text(json.dumps(samples, indent=2))
        # summarize reject reasons across nodes at end
        final = samples[-1] if samples else {}
        summary = {
            "out": str(OUT), "rate": RATE, "ok": ok, "busy": busy, "other": other,
            "final": final,
        }
        (OUT / "summary.json").write_text(json.dumps(summary, indent=2))
        log("SUMMARY " + json.dumps({
            "ok": ok, "busy": busy,
            "exec": [n.get("executed") for n in final.get("nodes", [])],
            "rej": [n.get("rej") for n in final.get("nodes", [])],
            "reasons": [n.get("reasons") for n in final.get("nodes", [])],
            "ingest_new": [n.get("ingest_new") for n in final.get("nodes", [])],
            "foreign": [n.get("foreign") for n in final.get("nodes", [])],
            "dropped_inbound": [n.get("dropped_inbound") for n in final.get("nodes", [])],
            "bodies_sent": [n.get("bodies_sent") for n in final.get("nodes", [])],
            "reseed_dir": [n.get("reseed_dir") for n in final.get("nodes", [])],
            "reseed_mesh": [n.get("reseed_mesh") for n in final.get("nodes", [])],
            "bodies_served": [n.get("bodies_served") for n in final.get("nodes", [])],
            "pack_skip": [n.get("pack_skip") for n in final.get("nodes", [])],
        }))
    finally:
        kill()

if __name__ == "__main__":
    main()
