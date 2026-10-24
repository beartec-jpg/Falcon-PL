#!/usr/bin/env python3
"""
Multi-N local chain: 5-minute ramp + settle load test.
Phases (default total ~300s):
  warm  — low steady rate
  ramp  — linear to peak
  hold  — stay at peak
  settle— no submits; watch exec drain / lag / freezes

Uses wire-fixed multipack dig knobs (falcon-512, conf-pool, open_cap=1).
"""
from __future__ import annotations
import json, os, signal, subprocess, sys, time, urllib.request
from pathlib import Path

BIN = os.environ.get("BIN", str(Path.home() / "falcon-dag-rs/target/release/falcon-dagd"))
N = int(os.environ.get("N", "3"))
BASE_RPC = int(os.environ.get("BASE_RPC", "29800"))
BASE_P2P = int(os.environ.get("BASE_P2P", "29900"))
WARM_S = float(os.environ.get("WARM_S", "30"))
RAMP_S = float(os.environ.get("RAMP_S", "60"))
HOLD_S = float(os.environ.get("HOLD_S", "150"))
SETTLE_S = float(os.environ.get("SETTLE_S", "60"))
WARM_TPS = float(os.environ.get("WARM_TPS", "10"))
PEAK_TPS = float(os.environ.get("PEAK_TPS", str(max(20, min(80, 10 * N)))))
REPOOL = os.environ.get("OPEN_BODY_REPOOL", "8")
OPEN_CAP = os.environ.get("OPEN_CAP", "1")
ACCOUNTS = int(os.environ.get("ACCOUNTS", "20"))
LABEL = os.environ.get("LABEL", f"N{N}")
OUT = Path(os.environ.get("OUT", f"/tmp/scale-{LABEL}-{int(time.time())}"))
OUT.mkdir(parents=True, exist_ok=True)
logdir = OUT / "nodes"
logdir.mkdir(exist_ok=True)
pids: list[int] = []
UNL = ",".join(f"val{i+1}" for i in range(N))


def log(m: str) -> None:
    line = f"[{time.strftime('%H:%M:%S')}] {m}"
    print(line, flush=True)
    (OUT / "run.log").open("a").write(line + "\n")


def kill() -> None:
    for p in list(pids):
        try:
            os.kill(p, 9)
        except Exception:
            pass
    pids.clear()
    for i in range(N):
        subprocess.run(["fuser", "-k", f"{BASE_RPC+i}/tcp"], capture_output=True)
        subprocess.run(["fuser", "-k", f"{BASE_P2P+i}/tcp"], capture_output=True)


def rpc(port: int, method: str, params=None, timeout: float = 4.0):
    pl = [{}] if params is None else ([params] if not isinstance(params, list) else params)
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": pl}).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        d = json.load(r)
    if "error" in d:
        raise RuntimeError(d["error"])
    return d


def sample_nodes() -> list[dict]:
    out = []
    for i in range(N):
        try:
            d = rpc(BASE_RPC + i, "server_info")
            info = ((d.get("result") or {}).get("info") or {})
            m = info.get("metrics") or {}
            ps = m.get("pack_stall") or {}
            oc = ps.get("open_conf") or {}
            mesh = m.get("mesh_net") or {}
            out.append({
                "id": f"val{i+1}",
                "exec": int(m.get("executed_txs") or 0),
                "lag": ps.get("admit_exec_lag"),
                "need_seed": ps.get("open_needs_body_seed"),
                "foreign": oc.get("foreign_sigs"),
                "open": oc.get("batch_hash12") or (oc.get("batch_hash") or "")[:12] or None,
                "uncert_me": ps.get("uncertified_by_me"),
                "body_enq": mesh.get("body_must_enqueued"),
                "drop_body": mesh.get("dropped_outbound_body"),
            })
        except Exception as e:
            out.append({"id": f"val{i+1}", "error": str(e)})
    return out


def start() -> None:
    kill()
    time.sleep(0.4)
    keys, wallets = logdir / "keys", logdir / "wallets"
    keys.mkdir(exist_ok=True)
    wallets.mkdir(exist_ok=True)
    subprocess.check_call(
        [BIN, "--gen-keys", "--keys-dir", str(keys), "--unl", UNL],
        stdout=subprocess.DEVNULL,
    )
    subprocess.check_call(
        [BIN, "--gen-wallet-keys", "--wallet-dir", str(wallets), "--faucet-accounts", str(ACCOUNTS)],
        stdout=subprocess.DEVNULL,
    )
    for i in range(N):
        id_ = f"val{i+1}"
        peers = ",".join(f"127.0.0.1:{BASE_P2P+j}" for j in range(N) if j != i)
        data = logdir / id_
        data.mkdir(exist_ok=True)
        logf = open(logdir / f"{id_}.log", "ab")
        cmd = [
            BIN, "--id", id_, "--role", "full",
            "--rpc", f"127.0.0.1:{BASE_RPC+i}",
            "--p2p", f"127.0.0.1:{BASE_P2P+i}",
            "--peers", peers, "--unl", UNL,
            "--worker-index", str(i), "--worker-count", str(N),
            "--network-id", "9200", "--crypto", "falcon-512",
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
            "--data-dir", str(data),
        ]
        env = os.environ.copy()
        env["BULLSHARK_TIEBREAKER"] = "ACCOUNT_ID_LEXICOGRAPHIC"
        env["DIRECT_SUBMIT"] = "1"
        p = subprocess.Popen(cmd, stdout=logf, stderr=subprocess.STDOUT, env=env)
        pids.append(p.pid)
        for _ in range(100):
            try:
                rpc(BASE_RPC + i, "ping")
                break
            except Exception:
                time.sleep(0.1)
        else:
            raise RuntimeError(f"node {id_} failed to start")
        log(f"started {id_} pid={p.pid}")


def submit_one(seq: dict, k: int) -> str:
    src = f"acct{(k % ACCOUNTS) + 1}"
    dst = f"acct{((k + 7) % ACCOUNTS) + 1}"
    s = seq[src]
    port = BASE_RPC + (k % N)
    try:
        d = rpc(port, "submit", {
            "account": src, "sequence": s,
            "destination": dst, "amount": 1, "fee": 10,
        }, timeout=3.0)
        code = (d.get("result") or {}).get("engine_result") or "unknown"
        if code == "tesSUCCESS":
            seq[src] = s + 1
        return str(code)
    except Exception as e:
        return f"err:{type(e).__name__}"


def phase_at(elapsed: float) -> tuple[str, float]:
    t = 0.0
    for name, dur, a, b in [
        ("warm", WARM_S, WARM_TPS, WARM_TPS),
        ("ramp", RAMP_S, WARM_TPS, PEAK_TPS),
        ("hold", HOLD_S, PEAK_TPS, PEAK_TPS),
        ("settle", SETTLE_S, 0.0, 0.0),
    ]:
        if elapsed < t + dur:
            if dur <= 0:
                return name, b
            u = (elapsed - t) / dur
            return name, a + (b - a) * u
        t += dur
    return "done", 0.0


def main() -> int:
    total = WARM_S + RAMP_S + HOLD_S + SETTLE_S
    log(f"OUT={OUT} N={N} total={total}s warm={WARM_S} ramp={RAMP_S} hold={HOLD_S} settle={SETTLE_S}")
    log(f"WARM_TPS={WARM_TPS} PEAK_TPS={PEAK_TPS} OPEN_CAP={OPEN_CAP} REPOOL={REPOOL} BIN={BIN}")
    signal.signal(signal.SIGTERM, lambda *_: (kill(), sys.exit(1)))
    signal.signal(signal.SIGINT, lambda *_: (kill(), sys.exit(1)))
    try:
        start()
        time.sleep(1.0)
        seq = {f"acct{i}": 1 for i in range(1, ACCOUNTS + 1)}
        t0 = time.time()
        ok = busy = fail = 0
        codes: dict[str, int] = {}
        samples: list[dict] = []
        freeze_events = 0
        last_exec_max = 0
        stall_s = 0
        k = 0
        next_tick = time.time()
        while True:
            elapsed = time.time() - t0
            phase, tps = phase_at(elapsed)
            if phase == "done":
                break
            # submit budget for this 1s window
            sec0 = time.time()
            nsub = int(round(tps))
            for _ in range(max(0, nsub)):
                code = submit_one(seq, k)
                k += 1
                codes[code] = codes.get(code, 0) + 1
                if code == "tesSUCCESS":
                    ok += 1
                elif code == "terBUSY":
                    busy += 1
                else:
                    fail += 1
            nodes = sample_nodes()
            execs = [n.get("exec") or 0 for n in nodes if "error" not in n]
            exec_max = max(execs) if execs else 0
            exec_min = min(execs) if execs else 0
            if exec_max <= last_exec_max and tps > 0:
                stall_s += 1
            elif exec_max > last_exec_max:
                stall_s = 0
            last_exec_max = max(last_exec_max, exec_max)
            need_any = any(n.get("need_seed") for n in nodes)
            frozen = stall_s >= 3 and need_any
            if frozen:
                freeze_events += 1
                stall_s = 0
            row = {
                "t": round(elapsed, 1),
                "phase": phase,
                "target_tps": round(tps, 1),
                "ok": ok,
                "busy": busy,
                "fail": fail,
                "exec_min": exec_min,
                "exec_max": exec_max,
                "exec_spread": exec_max - exec_min,
                "lags": [n.get("lag") for n in nodes],
                "need": [n.get("need_seed") for n in nodes],
                "freeze": frozen,
            }
            samples.append(row)
            log(
                f"t={elapsed:.0f} {phase} tgt={tps:.0f} ok={ok} busy={busy} fail={fail} "
                f"exec=[{exec_min}..{exec_max}] spread={exec_max-exec_min} "
                f"lag={row['lags'][:min(5,N)]} need={row['need'][:min(5,N)]} freeze_ev={freeze_events}"
            )
            # pace to ~1s ticks
            next_tick += 1.0
            sleep = next_tick - time.time()
            if sleep > 0:
                time.sleep(sleep)
            else:
                next_tick = time.time()

        nodes = sample_nodes()
        execs = [n.get("exec") or 0 for n in nodes if "error" not in n]
        elapsed = time.time() - t0
        # phase apply rates
        phase_apply = {}
        by_ph: dict[str, list] = {}
        for s in samples:
            by_ph.setdefault(s["phase"], []).append(s)
        for ph, rows in by_ph.items():
            if len(rows) < 2:
                continue
            de = rows[-1]["exec_max"] - rows[0]["exec_max"]
            dt = max(0.001, rows[-1]["t"] - rows[0]["t"])
            phase_apply[ph] = {
                "dt_s": round(dt, 1),
                "d_exec": de,
                "apply_tps": round(de / dt, 2),
                "avg_spread": round(sum(r["exec_spread"] for r in rows) / len(rows), 2),
                "freeze_ticks": sum(1 for r in rows if r["freeze"]),
            }

        summary = {
            "label": LABEL,
            "N": N,
            "duration_s": round(elapsed, 1),
            "phases": {
                "warm_s": WARM_S, "ramp_s": RAMP_S, "hold_s": HOLD_S, "settle_s": SETTLE_S,
                "warm_tps": WARM_TPS, "peak_tps": PEAK_TPS,
            },
            "submitted_ok": ok,
            "busy": busy,
            "fail": fail,
            "codes": codes,
            "freeze_events": freeze_events,
            "exec_min": min(execs) if execs else 0,
            "exec_max": max(execs) if execs else 0,
            "exec_spread": (max(execs) - min(execs)) if execs else 0,
            "submit_tps_avg": round(ok / max(elapsed, 0.001), 2),
            "apply_tps_avg": round((max(execs) if execs else 0) / max(elapsed, 0.001), 2),
            "phase_apply": phase_apply,
            "final_nodes": nodes,
            "out": str(OUT),
        }
        (OUT / "summary.json").write_text(json.dumps(summary, indent=2))
        (OUT / "samples.json").write_text(json.dumps(samples))
        log("SUMMARY " + json.dumps(summary))
        print("SUMMARY_JSON " + json.dumps(summary), flush=True)
        return 0
    finally:
        kill()


if __name__ == "__main__":
    raise SystemExit(main())
