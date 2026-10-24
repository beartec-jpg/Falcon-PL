#!/usr/bin/env python3
"""Sticky-open forensics: body on peers? signed? creator merged?"""
from __future__ import annotations
import json, os, subprocess, time, urllib.request
from pathlib import Path

BIN = os.environ.get(
    "BIN",
    str(Path.home() / "falcon-dag/falcon-dag-rs/target/release/falcon-dagd"),
)
if not Path(BIN).is_file():
    alt = Path.home() / "falcon-dag-rs/target/release/falcon-dagd"
    if alt.is_file():
        BIN = str(alt)
N = int(os.environ.get("N", "3"))
BASE_RPC = int(os.environ.get("BASE_RPC", "29800"))
BASE_P2P = int(os.environ.get("BASE_P2P", "29900"))
UNL = ",".join(f"val{i+1}" for i in range(N))
OUT = Path(f"/tmp/dig-sticky-n{N}-{int(time.time())}")
OUT.mkdir(parents=True)
logdir = OUT / "nodes"
logdir.mkdir()
pids = []
RATE = int(os.environ.get("RATE", "30"))
HOLD = float(os.environ.get("HOLD_S", "40"))
REPOOL = os.environ.get("OPEN_BODY_REPOOL", "8")
OPEN_CAP = os.environ.get("OPEN_CAP", "0")  # 0=auto(1 multipack); dig hypothesis: 2/3/4

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

def server_info(i):
    d = rpc(BASE_RPC + i, "server_info")
    info = ((d.get("result") or {}).get("info") or {})
    return info.get("metrics") or {}

def batch_status(i, h):
    try:
        d = rpc(BASE_RPC + i, "batch_status", {"batch_hash": h}, timeout=2.0)
        return (d.get("result") or d)
    except Exception as e:
        return {"error": str(e)}

def start():
    kill()
    for i in range(N):
        subprocess.run(["fuser", "-k", f"{BASE_RPC+i}/tcp"], capture_output=True)
        subprocess.run(["fuser", "-k", f"{BASE_P2P+i}/tcp"], capture_output=True)
    time.sleep(0.3)
    keys, wallets = logdir / "keys", logdir / "wallets"
    keys.mkdir(exist_ok=True); wallets.mkdir(exist_ok=True)
    subprocess.check_call([BIN, "--gen-keys", "--keys-dir", str(keys), "--unl", UNL],
                          stdout=subprocess.DEVNULL)
    # More accounts under multipack N=5 so lane owners stay fed.
    faucet_n = max(20, N * 8)
    subprocess.check_call([BIN, "--gen-wallet-keys", "--wallet-dir", str(wallets),
                           "--faucet-accounts", str(faucet_n)], stdout=subprocess.DEVNULL)
    for i in range(N):
        id_ = f"val{i+1}"
        peers = ",".join(f"127.0.0.1:{BASE_P2P+j}" for j in range(N) if j != i)
        data = logdir / id_
        data.mkdir(exist_ok=True)
        logf = open(logdir / f"{id_}.log", "ab")
        cmd = [BIN, "--id", id_, "--role", "full",
               "--rpc", f"127.0.0.1:{BASE_RPC+i}", "--p2p", f"127.0.0.1:{BASE_P2P+i}",
               "--peers", peers, "--unl", UNL,
               "--worker-index", str(i), "--worker-count", str(N),
               "--network-id", "9100", "--crypto", "falcon-512",
               "--require-tx-sigs", "--auto-sign",
               "--keys-dir", str(keys), "--wallet-dir", str(wallets),
               "--batch-interval", "0.05", "--max-batch-txs", "64",
               "--max-batch-parents", "3", "--unl-threshold", "0.6",
               "--max-uncertified-per-creator", "8",
               "--pack-open-cap", OPEN_CAP,
               "--single-packer=false", "--adaptive-batch=true",
               "--uncertified-ttl", "0",
               "--open-body-repool-after", REPOOL,
               "--admit-ahead-cap", "5000",
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

def account_shard(account: str) -> int:
    """Match fd-node FNV-1a 32-bit account → lane shard."""
    h = 0x811C9DC5
    for b in account.encode("utf-8"):
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h


def owner_rpc_index(account: str) -> int:
    """UNL rank = FNV(account) % N (same as multipack owns_tx)."""
    return account_shard(account) % N


def submit(seq):
    key = min(seq.keys(), key=lambda a: (seq[a], a))
    s = seq[key]
    dest = "acct20" if key != "acct20" else "acct19"
    # Owner-route submits: each node only Local-admits its own lane.
    # Avoids sole-front-door lag artifact (all admits on val1).
    route = os.environ.get("SUBMIT_ROUTE", "owner").lower()
    if route in ("owner", "lane", "owned"):
        rpc_i = owner_rpc_index(key)
    elif route in ("rr", "roundrobin", "round-robin"):
        rpc_i = (s + hash(key)) % N
    else:
        rpc_i = 0  # legacy: all to val1
    d = rpc(
        BASE_RPC + rpc_i,
        "submit",
        {"account": key, "sequence": s, "destination": dest, "amount": 1, "fee": 10},
    )
    code = (d.get("result") or {}).get("engine_result")
    if code == "tesSUCCESS":
        seq[key] = s + 1
    return code

def sample():
    nodes = []
    for i in range(N):
        try:
            m = server_info(i)
            ps = m.get("pack_stall") or {}
            oc = ps.get("open_conf") or {}
            skip = m.get("pack_skip_reasons") or ps.get("pack_skip_reasons") or {}
            mesh = m.get("mesh_net") or {}
            # lag: prefer pack_stall, fall back to top-level metrics
            lag = ps.get("admit_exec_lag")
            if lag is None:
                lag = m.get("admit_exec_lag")
            nodes.append({
                "id": f"val{i+1}",
                "exec": int(m.get("executed_txs") or 0),
                "lag": lag,
                "admits": ps.get("local_admits_ok", m.get("local_admits_ok")),
                "admits_owned": ps.get("local_admits_owned", m.get("local_admits_owned")),
                "admits_foreign": ps.get(
                    "local_admits_not_owned", m.get("local_admits_not_owned")
                ),
                "uncert_me": ps.get("uncertified_by_me"),
                "need_seed": ps.get("open_needs_body_seed"),
                "open_bh": oc.get("batch_hash"),
                "open_bh12": oc.get("batch_hash12"),
                "foreign": oc.get("foreign_sigs"),
                "nsigs": oc.get("nsigs"),
                "signers": oc.get("signers"),
                "my_sig": oc.get("my_sig"),
                "parents_ok": oc.get("parents_certified"),
                "age": oc.get("age_s"),
                "support": oc.get("support_weight"),
                "fsig_merge": ps.get("foreign_sig_merged_from_sig"),
                "fsig_body": ps.get("foreign_sig_merged_from_body"),
                "on_rsig": ps.get("on_remote_sig_calls") or skip.get("on_remote_sig_calls"),
                "signed1st": skip.get("remote_batch_signed_first"),
                "signed_rs": skip.get("remote_batch_signed_reseed"),
                "sig2c": mesh.get("sig_to_creator_sent"),
                "body_enq": mesh.get("body_must_enqueued"),
                "drop_body": mesh.get("dropped_outbound_body"),
                "drop_in": mesh.get("dropped_inbound"),
                "drop_in_c": mesh.get("dropped_inbound_critical"),
                "wire_ok": mesh.get("wire_write_ok"),
                "wire_err": mesh.get("wire_write_err"),
                "peer_n": mesh.get("peer_count") or mesh.get("writers"),
                "rej_batch": ps.get("rejected_batches"),
                "rej": ps.get("reject_reasons"),
                "ingest_new": ps.get("ingest_new_ok"),
                "ingest_merge": ps.get("ingest_merge_ok"),
                "reseed_ok": ps.get("reseed_allowed") or skip.get("reseed_reliable"),
                "reseed_sup": ps.get("reseed_suppressed") or skip.get("reseed_suppressed_rate"),
                "conf_signed": skip.get("conf_pool_signed"),
                "deferred": skip.get("remote_batch_deferred_sign"),
                "packed": skip.get("packed"),
                "uncert_cap": skip.get("uncertified_cap"),
            })
        except Exception as e:
            nodes.append({"id": f"val{i+1}", "error": str(e)})
    return nodes

def cross_check(nodes):
    """For each sticky open hash, ask every peer batch_status."""
    lines = []
    hashes = []
    for n in nodes:
        h = n.get("open_bh")
        if h and n.get("foreign") == 0:
            hashes.append((n["id"], h))
    # unique hashes
    seen = set()
    for creator, h in hashes:
        if h in seen:
            continue
        seen.add(h)
        row = {"creator_open": creator, "hash12": h[:12], "peers": {}}
        for i in range(N):
            st = batch_status(i, h)
            row["peers"][f"val{i+1}"] = {
                "has_body": st.get("has_body"),
                "my_sig": st.get("my_sig"),
                "nsigs": st.get("nsigs"),
                "signers": st.get("signers"),
                "certified": st.get("certified"),
                "conf_pool": st.get("conf_pool"),
                "creator": st.get("creator"),
                "error": st.get("error"),
            }
        # classify gap (peer body, not creator body)
        peer_bodies = [bool(v.get("has_body")) for k, v in row["peers"].items() if k != creator]
        peer_sigs = [bool(v.get("my_sig")) for k, v in row["peers"].items() if k != creator]
        creator_st = row["peers"].get(creator) or {}
        if not any(peer_bodies):
            row["gap"] = "NO_BODY_ON_PEERS"
        elif not any(peer_sigs):
            row["gap"] = "PEER_HAS_BODY_NO_SIG"
        else:
            if (creator_st.get("nsigs") or 0) <= 1 and not creator_st.get("certified"):
                row["gap"] = "PEER_SIGNED_CREATOR_MISSING_FOREIGN"
            else:
                row["gap"] = "HAS_FOREIGN_OR_MULTI_SIG"
        lines.append(row)
    return lines

def main():
    route = os.environ.get("SUBMIT_ROUTE", "owner")
    log(
        f"OUT={OUT} N={N} UNL={UNL} RATE={RATE} HOLD={HOLD} REPOOL={REPOOL} "
        f"SUBMIT_ROUTE={route} BIN={BIN}"
    )
    try:
        start()
        time.sleep(1.5 if N >= 5 else 1.2)
        n_accts = max(12, N * 6)
        seq = {f"acct{a}": 1 for a in range(1, n_accts + 1)}
        ok = busy = 0
        t0 = time.time()
        last_exec = 0
        stall_s = 0
        freeze_rows = []
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
            nodes = sample()
            exec0 = nodes[0].get("exec") or 0
            if exec0 <= last_exec:
                stall_s += 1
            else:
                stall_s = 0
            last_exec = exec0
            log(
                f"t={time.time()-t0:.1f} exec={[n.get('exec') for n in nodes]} "
                f"lag={[n.get('lag') for n in nodes]} "
                f"admits={[n.get('admits') for n in nodes]} "
                f"owned={[n.get('admits_owned') for n in nodes]} "
                f"foreign_adm={[n.get('admits_foreign') for n in nodes]} "
                f"foreign={[n.get('foreign') for n in nodes]} "
                f"need={[n.get('need_seed') for n in nodes]} "
                f"open={[n.get('open_bh12') for n in nodes]} "
                f"nsigs={[n.get('nsigs') for n in nodes]} "
                f"parents={[n.get('parents_ok') for n in nodes]} "
                f"fsig_m={[n.get('fsig_merge') for n in nodes]} "
                f"on_rsig={[n.get('on_rsig') for n in nodes]} "
                f"s1={[n.get('signed1st') for n in nodes]} "
                f"srs={[n.get('signed_rs') for n in nodes]} "
                f"sig2c={[n.get('sig2c') for n in nodes]} "
                f"ok={ok}"
            )
            # freeze: 3s no exec progress and any need_seed
            if stall_s >= 3 and any(n.get("need_seed") for n in nodes):
                cross = cross_check(nodes)
                freeze_rows.append({
                    "t": round(time.time() - t0, 1),
                    "nodes": nodes,
                    "cross": cross,
                })
                for n in nodes:
                    log(
                        f"FREEZE_NET t={time.time()-t0:.1f} {n.get('id')} "
                        f"body_enq={n.get('body_enq')} drop_body={n.get('drop_body')} "
                        f"drop_in={n.get('drop_in')} wire_ok={n.get('wire_ok')} wire_err={n.get('wire_err')} "
                        f"peer_n={n.get('peer_n')} rej={n.get('rej')} rej_batch={n.get('rej_batch')} "
                        f"ingest_new={n.get('ingest_new')} ingest_merge={n.get('ingest_merge')} "
                        f"reseed_ok={n.get('reseed_ok')} reseed_sup={n.get('reseed_sup')} "
                        f"conf_signed={n.get('conf_signed')} deferred={n.get('deferred')} "
                        f"fsig_m={n.get('fsig_merge')} fsig_body={n.get('fsig_body')}"
                    )
                for c in cross:
                    log(f"FREEZE_GAP t={time.time()-t0:.1f} creator={c['creator_open']} "
                        f"h={c['hash12']} gap={c['gap']} peers={json.dumps(c['peers'])}")
                stall_s = 0  # log every 3s while frozen
            left = 1.0 - (time.time() - sec0)
            if left > 0:
                time.sleep(left)
        (OUT / "freeze.json").write_text(json.dumps(freeze_rows, indent=2))
        # gap histogram
        gaps = {}
        for fr in freeze_rows:
            for c in fr.get("cross") or []:
                g = c.get("gap") or "?"
                gaps[g] = gaps.get(g, 0) + 1
        log("GAP_HIST " + json.dumps(gaps))
        # Reliability score: seconds without hard freeze (exec stalled + need_seed)
        hard_freeze = sum(1 for fr in freeze_rows if fr.get("cross"))
        log("RELIABILITY " + json.dumps({
            "freeze_events": len(freeze_rows),
            "gap_hist": gaps,
            "goal": "steady continuous on small N; grow TPS via more nodes",
        }))
        log("SUMMARY " + json.dumps({
            "ok": ok, "busy": busy,
            "exec": [n.get("exec") for n in sample()],
            "gaps": gaps,
            "freeze_events": len(freeze_rows),
            "out": str(OUT),
        }))
    finally:
        kill()

if __name__ == "__main__":
    main()
