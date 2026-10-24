#!/usr/bin/env python3
"""Falcon PL v2.2 — epoch payday soak harness (private fleet).

Exercises pack/check/watcher/AMM/lend sector scoring + claim path by advancing
wall-clock via ctl tick (short --epoch-ms on nodes recommended).

Watcher soak:
  - dust path: work + 1 wall slot → weight 0 (integer)
  - full path: --presence-slots 168 / --full-presence → non-dust claim

  PL_EPOCH_MS=10000 bash crates/fd-pl/scripts/pl-private-fleet-v22.sh start
  python3 crates/fd-pl/scripts/pl-epoch-payday.py --keys-dir ~/falcon-pl-private-v22/keys

Exit 0 if pack/check claim AND full-presence watcher claim land.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from typing import Any


def ctl(bin_ctl: str, addr: str, *args: str) -> dict[str, Any]:
    cmd = [bin_ctl, "--addr", addr, *args]
    p = subprocess.run(cmd, capture_output=True, text=True)
    out = (p.stdout or "") + (p.stderr or "")
    start = out.find("{")
    if start < 0:
        raise RuntimeError(f"ctl failed rc={p.returncode}: {out[-500:]}")
    try:
        obj, _ = json.JSONDecoder().raw_decode(out[start:])
        return obj
    except json.JSONDecodeError as e:
        raise RuntimeError(f"ctl JSON parse: {e}; tail={out[-400:]}") from e


def status(bin_ctl: str, addr: str) -> dict[str, Any]:
    r = ctl(bin_ctl, addr, "status")
    if r.get("type") == "status":
        return r.get("body") or {}
    return r


def account(bin_ctl: str, addr: str, who: str) -> dict[str, Any]:
    r = ctl(bin_ctl, addr, "account", "--account", who)
    if r.get("type") == "account":
        return r.get("body") or {}
    return r


def seal_mesh(bin_ctl: str, addrs: list[str], rounds: int = 6) -> None:
    for _ in range(rounds):
        for a in addrs:
            try:
                ctl(bin_ctl, a, "seal")
            except Exception:
                pass
        time.sleep(0.15)


def wait_tip_advance(
    bin_ctl: str,
    addr: str,
    tip0: int,
    seal_addrs: list[str],
    timeout_s: float,
    scheme: str,
    keys_dir: str,
) -> int:
    """Drive seals + pay until tip > tip0 or timeout. Returns final tip."""
    deadline = time.time() + timeout_s
    tip = tip0
    while time.time() < deadline:
        seal_mesh(bin_ctl, seal_addrs, rounds=2)
        try:
            # Keep mempool warm so lottery has work.
            ctl(
                bin_ctl,
                addr,
                "pay",
                "--from",
                "alice",
                "--to",
                "bob",
                "--amount",
                "1",
                "--fee",
                "2",
                "--sequence",
                "0",
                "--scheme",
                scheme,
                "--keys-dir",
                keys_dir,
            )
        except Exception:
            pass
        st = status(bin_ctl, addr)
        tip = int(st.get("tip_height") or 0)
        if tip > tip0:
            return tip
        time.sleep(0.25)
    return tip


def wait_status_pred(
    bin_ctl: str,
    addr: str,
    seal_addrs: list[str],
    pred,
    timeout_s: float,
    label: str,
) -> dict[str, Any]:
    deadline = time.time() + timeout_s
    last: dict[str, Any] = {}
    while time.time() < deadline:
        seal_mesh(bin_ctl, seal_addrs, rounds=2)
        last = status(bin_ctl, addr)
        if pred(last):
            return last
        time.sleep(0.3)
    print(f"[payday] WARN: timeout waiting for {label}")
    return last


def main() -> int:
    import os

    ap = argparse.ArgumentParser(description="Epoch payday soak (FPL private)")
    ap.add_argument("--addr", default="127.0.0.1:19101")
    ap.add_argument(
        "--seal-addrs",
        default="",
        help="comma-separated RPC addrs to SealNow (local validators only)",
    )
    ap.add_argument("--ctl", default="", help="path to falcon-pl-ctl")
    ap.add_argument("--keys-dir", required=True)
    ap.add_argument("--scheme", default="hmac")
    ap.add_argument("--payments", type=int, default=8, help="txs to create pack work")
    ap.add_argument("--wait-seal-s", type=float, default=3.0)
    ap.add_argument(
        "--pack-wait-s",
        type=float,
        default=15.0,
        help="max seconds to wait for tip / watcher ledger inclusion",
    )
    ap.add_argument("--watcher-work", type=int, default=100)
    ap.add_argument(
        "--watcher-presence-slots",
        type=int,
        default=168,
        help="bulk presence slots for full watcher (0=skip full path)",
    )
    ap.add_argument(
        "--require-watcher",
        action="store_true",
        default=True,
        help="fail if full-presence watcher earns 0 (default on)",
    )
    ap.add_argument("--no-require-watcher", action="store_true")
    args = ap.parse_args()
    require_watcher = args.require_watcher and not args.no_require_watcher
    os.environ["PL_KEYS_DIR"] = args.keys_dir

    rs = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../.."))
    bin_ctl = args.ctl or os.path.join(rs, "target/release/falcon-pl-ctl")
    if not os.path.isfile(bin_ctl):
        bin_ctl = os.path.join(rs, "target/debug/falcon-pl-ctl")

    st0 = status(bin_ctl, args.addr)
    epoch_ms = int(st0.get("epoch_ms") or 604_800_000)
    tip0 = int(st0.get("tip_height") or 0)
    print(
        f"[payday] tip={tip0} epoch={st0.get('epoch')} epoch_ms={epoch_ms} "
        f"params_version={st0.get('params_version')} treasury={st0.get('treasury')} "
        f"watcher_slots_per_epoch={st0.get('watcher_slots_per_epoch')}"
    )

    # Seal targets: explicit list, else primary only (never invent remote :19103).
    if args.seal_addrs.strip():
        seal_addrs = [a.strip() for a in args.seal_addrs.split(",") if a.strip()]
    else:
        seal_addrs = [args.addr]
        if args.addr.endswith(":19101"):
            host = args.addr.rsplit(":", 1)[0]
            seal_addrs.append(f"{host}:19102")

    # 1) Advance past bootstrap quiet epochs 1–7.
    cur = int(st0.get("epoch") or 1)
    need = max(0, 8 - cur)
    if need > 0:
        print(f"[payday] bootstrap advance: {need} epoch(s) to claimable window…")
        for _ in range(need):
            ctl(bin_ctl, args.addr, "tick", "--dt-ms", str(max(epoch_ms, 1)))
        st_b = status(bin_ctl, args.addr)
        print(
            f"  … now epoch={st_b.get('epoch')} last_settled={st_b.get('last_settled_epoch')}"
        )

    # 2) Pack/check work inside claimable epoch (finish before epoch rolls).
    print(f"[payday] submit {args.payments} payments alice→bob")
    for _ in range(args.payments):
        ctl(
            bin_ctl,
            args.addr,
            "pay",
            "--from",
            "alice",
            "--to",
            "bob",
            "--amount",
            "1",
            "--fee",
            "2",
            "--sequence",
            "0",
            "--scheme",
            args.scheme,
            "--keys-dir",
            args.keys_dir,
        )
    tip1 = wait_tip_advance(
        bin_ctl,
        args.addr,
        tip0,
        seal_addrs,
        max(args.pack_wait_s, args.wait_seal_s),
        args.scheme,
        args.keys_dir,
    )
    st1 = status(bin_ctl, args.addr)
    pack = st1.get("epoch_pack_txs") or {}
    check = st1.get("epoch_check_votes") or {}
    print(f"[payday] tip {tip0}→{tip1} pack_txs={pack} check_votes={check}")
    if tip1 <= tip0 and not pack:
        print("[payday] WARN: tip/pack did not advance")

    # 3) Watcher soak: dust (bob) vs full presence (alice) — ledger txs must pack.
    kd = ["--scheme", args.scheme, "--keys-dir", args.keys_dir]
    print("[payday] watcher dust: bob work=100 (ledger tx)")
    ctl(
        bin_ctl,
        args.addr,
        "watcher-credit",
        "--account",
        "bob",
        "--work",
        "100",
        *kd,
    )
    print(
        f"[payday] watcher full: alice work={args.watcher_work} "
        f"presence_slots={args.watcher_presence_slots}"
    )
    if args.watcher_presence_slots > 0:
        ctl(
            bin_ctl,
            args.addr,
            "watcher-credit",
            "--account",
            "alice",
            "--work",
            str(args.watcher_work),
            "--presence-slots",
            str(args.watcher_presence_slots),
            *kd,
        )
    # Short pack window — epoch_ms may be 10s; do not wait across settle clears.
    pack_s = min(args.pack_wait_s, max(6.0, epoch_ms / 2000.0))
    st_w = wait_status_pred(
        bin_ctl,
        args.addr,
        seal_addrs,
        lambda s: int((s.get("watcher_work") or {}).get("alice") or 0) > 0
        or int((s.get("watcher_slots") or {}).get("alice") or 0) > 0,
        pack_s,
        "alice watcher ledger inclusion",
    )
    print(
        f"[payday] watcher after pack: work={st_w.get('watcher_work')} "
        f"slots={st_w.get('watcher_slots')}"
    )
    # If wall-clock settled the epoch (cleared work), re-submit watcher once.
    if int((st_w.get("watcher_work") or {}).get("alice") or 0) <= 0:
        print("[payday] re-submit alice watcher after empty work (epoch may have settled)")
        if args.watcher_presence_slots > 0:
            ctl(
                bin_ctl,
                args.addr,
                "watcher-credit",
                "--account",
                "alice",
                "--work",
                str(args.watcher_work),
                "--presence-slots",
                str(args.watcher_presence_slots),
                *kd,
            )
        ctl(
            bin_ctl,
            args.addr,
            "watcher-credit",
            "--account",
            "bob",
            "--work",
            "100",
            *kd,
        )
        st_w = wait_status_pred(
            bin_ctl,
            args.addr,
            seal_addrs,
            lambda s: int((s.get("watcher_work") or {}).get("alice") or 0) > 0,
            pack_s,
            "alice watcher retry",
        )
        print(
            f"[payday] watcher retry: work={st_w.get('watcher_work')} "
            f"slots={st_w.get('watcher_slots')}"
        )

    # 4) Real AMM + lend LP (protocol txs → seal)
    ts = int(time.time())
    pool_id = f"pay-{ts}"
    mkt_id = f"m-{ts}"
    print(f"[payday] AMM pool {pool_id} + lend market {mkt_id}")
    ctl(
        bin_ctl,
        args.addr,
        "create-pool",
        "--pool-id",
        pool_id,
        "--asset-a",
        "FPL",
        "--asset-b",
        "USD",
        "--account",
        "alice",
        *kd,
    )
    ctl(
        bin_ctl,
        args.addr,
        "credit-asset",
        "--account",
        "alice",
        "--asset",
        "USD",
        "--amount",
        "50000",
        *kd,
    )
    seal_mesh(bin_ctl, seal_addrs)
    ctl(
        bin_ctl,
        args.addr,
        "add-liq",
        "--pool-id",
        pool_id,
        "--provider",
        "alice",
        "--amt-a",
        "10000",
        "--amt-b",
        "10000",
        *kd,
    )
    ctl(
        bin_ctl,
        args.addr,
        "create-market",
        "--market-id",
        mkt_id,
        "--asset",
        "USD",
        "--account",
        "carol",
        *kd,
    )
    ctl(
        bin_ctl,
        args.addr,
        "credit-asset",
        "--account",
        "carol",
        "--asset",
        "USD",
        "--amount",
        "20000",
        *kd,
    )
    seal_mesh(bin_ctl, seal_addrs, rounds=4)
    ctl(
        bin_ctl,
        args.addr,
        "lend-supply",
        "--market-id",
        mkt_id,
        "--who",
        "carol",
        "--amount",
        "10000",
        *kd,
    )
    st_work = wait_status_pred(
        bin_ctl,
        args.addr,
        seal_addrs,
        lambda s: bool(s.get("amm_lp_weights")) or bool(s.get("lend_lp_weights")),
        pack_s,
        "AMM/lend LP ledger inclusion",
    )
    # Merge latest watcher view (may have been re-submitted).
    if not (st_work.get("watcher_work") or {}).get("alice"):
        st_work = status(bin_ctl, args.addr)
    print(
        f"[payday] pre-settle pack={st_work.get('epoch_pack_txs')} "
        f"check={st_work.get('epoch_check_votes')} "
        f"watcher_work={st_work.get('watcher_work')} "
        f"watcher_slots={st_work.get('watcher_slots')} "
        f"weights={st_work.get('watcher_weights_preview')} "
        f"amm_lp={st_work.get('amm_lp_weights')} lend_lp={st_work.get('lend_lp_weights')}"
    )
    ww = st_work.get("watcher_weights_preview") or {}
    if args.watcher_presence_slots >= 168 and int(ww.get("alice") or 0) <= 0:
        # One more pack window before failing hard.
        st_work = wait_status_pred(
            bin_ctl,
            args.addr,
            seal_addrs,
            lambda s: int((s.get("watcher_weights_preview") or {}).get("alice") or 0) > 0,
            max(8.0, args.pack_wait_s / 2),
            "alice watcher weight",
        )
        ww = st_work.get("watcher_weights_preview") or {}
    if args.watcher_presence_slots >= 168 and int(ww.get("alice") or 0) <= 0:
        print(
            f"[payday] FAIL: alice watcher weight still 0 with full presence "
            f"(slots={st_work.get('watcher_slots')} work={st_work.get('watcher_work')})",
            file=sys.stderr,
        )
        return 1
    if int(ww.get("bob") or 0) != 0:
        print(f"[payday] WARN: bob dust weight expected 0 got {ww.get('bob')}")

    print("[payday] force-settle current epoch…")
    ctl(bin_ctl, args.addr, "force-settle")
    stf = status(bin_ctl, args.addr)
    claimable = stf.get("claimable") or {}
    print(
        f"[payday] after settle: last_settled={stf.get('last_settled_epoch')} "
        f"claimable={claimable}"
    )

    # alice has watcher + AMM LP combined in claimable
    alice_claim = int(claimable.get("alice") or 0)
    bob_claim = int(claimable.get("bob") or 0)
    carol_claim = int(claimable.get("carol") or 0)
    val_claim = sum(
        int(v or 0)
        for k, v in claimable.items()
        if str(k).startswith("v")
    )

    claimed_total = 0
    for acct, amt in sorted(claimable.items(), key=lambda x: -int(x[1] or 0))[:16]:
        r = ctl(
            bin_ctl,
            args.addr,
            "claim",
            "--account",
            str(acct),
            "--scheme",
            args.scheme,
            "--keys-dir",
            args.keys_dir,
        )
        print(f"  claim {acct}: {r}")
        seal_mesh(bin_ctl, seal_addrs, rounds=3)
        body = account(bin_ctl, args.addr, str(acct))
        print(f"    balance={body.get('balance')} claimable_left={body.get('claimable')}")
        claimed_total += int(amt or 0)

    st_end = status(bin_ctl, args.addr)
    summary = {
        "tip_height": st_end.get("tip_height"),
        "epoch": st_end.get("epoch"),
        "last_settled_epoch": st_end.get("last_settled_epoch"),
        "epoch_claimable": st_end.get("epoch_claimable"),
        "treasury": st_end.get("treasury"),
        "alice_claim_watcher_plus_amm": alice_claim,
        "bob_dust_claim": bob_claim,
        "carol_lend_claim": carol_claim,
        "validator_claim_sum": val_claim,
        "claimed_pool_seen": claimed_total,
        "metrics_fee": (st_end.get("metrics") or {}).get("fees_burned_total"),
    }
    print("[payday] summary")
    print(json.dumps(summary, indent=2))

    ok_val = val_claim > 0 or claimed_total > 0
    ok_watcher = (not require_watcher) or (alice_claim > 0)
    ok_dust = bob_claim == 0  # dust must not farm
    ok_lend = carol_claim > 0
    # alice should exceed pure watcher cap if AMM also paid (watcher cap 294k alone)
    # at minimum alice > 0 and carol > 0

    if not ok_val:
        print("[payday] FAIL: no pack/check claimable", file=sys.stderr)
        return 1
    if not ok_watcher:
        print(
            f"[payday] FAIL: full-presence watcher/AMM alice claim={alice_claim}",
            file=sys.stderr,
        )
        return 1
    if not ok_dust:
        print(
            f"[payday] FAIL: dust bob should earn 0 got {bob_claim}",
            file=sys.stderr,
        )
        return 1
    if not ok_lend:
        print(
            f"[payday] FAIL: lend LP carol claim={carol_claim}",
            file=sys.stderr,
        )
        return 1

    print(
        f"[payday] OK pack/check + watcher/AMM alice={alice_claim} "
        f"lend carol={carol_claim} dust bob=0"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
