#!/usr/bin/env python3
"""falcon-pl-mm: testnet order-book volume engine (scaffold). DRY_RUN=1 is the default.

It quotes a ladder of limit orders around a reference price that oscillates
slowly (sine wave between BAND_LO and BAND_HI over PERIOD_S). Orders go out
through `falcon-pl-ctl place-order/cancel-order`.

Kill switches (checked every tick): a file at $MM_PAUSE pauses the engine;
a file at $MM_CANCEL_ALL makes it cancel all orders and exit.
Testnet4 only. It never prints keys; KEYS_DIR is passed through to ctl as-is.
"""
import math, os, shlex, subprocess, sys, time

def load_env(path):
    cfg = {}
    for line in open(path):
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1); cfg[k.strip()] = v.strip()
    return cfg

def ref_price(cfg, t):
    lo, hi, per = float(cfg["BAND_LO"]), float(cfg["BAND_HI"]), float(cfg["PERIOD_S"])
    return lo + (hi - lo) * (0.5 + 0.5 * math.sin(2 * math.pi * t / per))

def ladder(cfg, mid):
    lv, sp = int(cfg["LEVELS"]), float(cfg["SPACING_BPS"]) / 1e4
    qmin, qmax = float(cfg["QUOTE_MIN_FPL"]), float(cfg["QUOTE_MAX_FPL"])
    fpl_raw, base_raw = int(cfg["FPL_RAW_PER_FPL"]), int(cfg["BASE_RAW_PER_COIN"])
    out = []
    for i in range(1, lv + 1):
        q = qmin + (qmax - qmin) * (i - 1) / max(lv - 1, 1)
        for side, px in (("buy", mid * (1 - sp * i)), ("sell", mid * (1 + sp * i))):
            amount = int(q / px * base_raw)
            out.append((side, int(px * fpl_raw), amount))
    return out

def ctl(cfg, *a):
    cmd = [cfg["CTL"], "--addr", cfg["RPC_ADDR"], *a, "--scheme", cfg["SCHEME"],
           "--keys-dir", cfg["KEYS_DIR"], "--network-id", cfg["NETWORK_ID"]]
    if cfg.get("DRY_RUN", "1") != "0":
        print("DRY", " ".join(shlex.quote(x) for x in cmd)); return ""
    return subprocess.run(cmd, capture_output=True, text=True, timeout=60).stdout

def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "/etc/falcon-pl-mm/mm.env"
    wallets_i = 0
    while True:
        cfg = load_env(path)  # re-read each tick
        if os.path.exists(cfg.get("MM_CANCEL_ALL", "/nonexistent")):
            print("MM_CANCEL_ALL set: cancel-all (uses list from the node; TODO) then exit"); return
        if os.path.exists(cfg.get("MM_PAUSE", "/nonexistent")):
            time.sleep(10); continue
        wallets = cfg["WALLETS"].split(",")
        mid = ref_price(cfg, time.time())
        print(f"tick mid={mid:.0f} FPL/{cfg['PAIR'].split('-')[0]}")
        for side, px, amt in ladder(cfg, mid):
            w = wallets[wallets_i % len(wallets)]; wallets_i += 1
            ctl(cfg, "place-order", "--who", w, "--pair", cfg["PAIR"], "--side", side,
                "--price", str(px), "--amount", str(amt))
        if cfg.get("ONCE") == "1": return
        time.sleep(float(cfg["TICK_S"]))

if __name__ == "__main__":
    main()
