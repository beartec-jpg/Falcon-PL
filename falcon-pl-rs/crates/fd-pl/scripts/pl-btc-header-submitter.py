#!/usr/bin/env python3
"""FPL Bitcoin header submitter — watcher wallet, not validator keys.

Pulls Bitcoin testnet headers from an Esplora explorer and posts them as
signed TxBody::RailHeader (submit-btc-header). One node is not enough:
the tx must be packed into the ledger so all seven see it.

Requires Falcon PL 2.9.35+ (BTC rail rejects fixture headers).
Do not start this against 2.9.34 — height/parent will not reanchor.

Reorgs: Bitcoin testnet regularly forks more than one block. The rail can
replace any height H ≤ tip whose parent matches the stored H-1 header. This
process walks back to the common ancestor (via status `header_tail` when the
node exposes it, otherwise by probing from the stale tip) and then catch-up
bursts. It never retries the same failing (height, hash) forever.

Env:
  FPL_CTL              default: /home/falcon/falcon-pl-public-testnet-2300/bin/falcon-pl-ctl
  FPL_RPC              comma-separated hubs; first healthy (tip_lag=0) wins
                       default: 127.0.0.1:19301. The choice is cached for
                       RPC_REPICK_SEC and re-picked early on any RPC error.
  FPL_KEYS             default: /home/falcon/falcon-pl-public-testnet-2300/keys
  FPL_WATCHER_ACCOUNT  required — funded named/account that signs headers
  FPL_SCHEME           default: falcon-512
  FPL_NETWORK_ID       default: 2300
  BTC_EXPLORER         default: https://mempool.space/testnet/api
  STATE_PATH           default: /home/falcon/falcon-pl-public-testnet-2300/run/btc-headers.json
  POLL_SEC             idle poll when caught up (default 20)
  MAX_REORG            max walk-back from rail tip (default 200, < HEADER_HISTORY)
  ADOPT_SEC            pack wait after a submit (default 40)
  CATCHUP_SLEEP_SEC    pause between catch-up submits (default 0.05)
  CATCHUP_BURST        sequential headers per pack round (default 32)
  HEADER_FEE           fee passed to ctl (default 2)
  RPC_REPICK_SEC       re-select the RPC this often (default 300)
  TIP_POLL_SEC         poll interval while a header is in flight (default 2).
                       Uses the tiny `account_query` (watcher sequence, ~400 B)
                       and only reads the 30 KB `brief` status once the
                       sequence moves, never the 2 MB full status.
  RAIL_MAX_AGE_SEC     reuse a rail view this young at round start (default 10)
  BTC_TIP_TTL_SEC      cache the explorer tip while far behind (default 30)
  BURST_PROBE_SEC      after a burst is refused past the first header (node
                       admits only on the committed tip), send one header per
                       round and re-probe a burst this often (default 600)
  STALL_BACKOFF_MAX    max sleep while the ledger tip is not moving (default 60)

Load (2.9.53): the old loop called full `status` ~8x per round to pick an
RPC and every 0.4 s while waiting for a pack. Now: one brief status per RPC
every RPC_REPICK_SEC, one cheap account query every TIP_POLL_SEC while a
header is pending, one brief status per landed header. ctl submit itself
still does one full status (suggested fee) per submit.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from typing import Callable

CTL = os.environ.get(
    "FPL_CTL", "/home/falcon/falcon-pl-public-testnet-2300/bin/falcon-pl-ctl"
)
RPCS = [
    a.strip()
    for a in os.environ.get("FPL_RPC", "127.0.0.1:19301").split(",")
    if a.strip()
]
KEYS = os.environ.get("FPL_KEYS", "/home/falcon/falcon-pl-public-testnet-2300/keys")
ACCOUNT = os.environ.get("FPL_WATCHER_ACCOUNT", "").strip()
SCHEME = os.environ.get("FPL_SCHEME", "falcon-512")
NETWORK_ID = os.environ.get("FPL_NETWORK_ID", "2300")
EXPLORER = os.environ.get("BTC_EXPLORER", "https://mempool.space/testnet/api").rstrip("/")
STATE_PATH = os.environ.get(
    "STATE_PATH", "/home/falcon/falcon-pl-public-testnet-2300/run/btc-headers.json"
)
POLL_SEC = int(os.environ.get("POLL_SEC", "20"))
MAX_REORG = int(os.environ.get("MAX_REORG", "200"))
ADOPT_SEC = float(os.environ.get("ADOPT_SEC", "25"))
CATCHUP_SLEEP_SEC = float(os.environ.get("CATCHUP_SLEEP_SEC", "0.05"))
CATCHUP_BURST = int(os.environ.get("CATCHUP_BURST", "32"))
HEADER_FEE = os.environ.get("HEADER_FEE", "2")
# A late Claim FBTC writes this height. We pull those blocks before new tips
# so a forgotten claim is not pruned again while we catch up.
WANT_PATH = os.environ.get(
    "BTC_HEADER_WANT",
    "/home/falcon/falcon-pl-public-testnet-2300/run/btc-header-want",
)
BACKFILL_CHUNK = int(os.environ.get("BTC_BACKFILL_CHUNK", "40"))
RPC_DEAD_SEC = float(os.environ.get("RPC_DEAD_SEC", "120"))
EXPLORER_RETRIES = int(os.environ.get("EXPLORER_RETRIES", "4"))
RPC_REPICK_SEC = float(os.environ.get("RPC_REPICK_SEC", "300"))
TIP_POLL_SEC = float(os.environ.get("TIP_POLL_SEC", "2"))
RAIL_MAX_AGE_SEC = float(os.environ.get("RAIL_MAX_AGE_SEC", "10"))
BTC_TIP_TTL_SEC = float(os.environ.get("BTC_TIP_TTL_SEC", "30"))
BURST_PROBE_SEC = float(os.environ.get("BURST_PROBE_SEC", "600"))
STALL_BACKOFF_MAX = float(os.environ.get("STALL_BACKOFF_MAX", "60"))
WIRE_TIMEOUT_SEC = float(os.environ.get("WIRE_TIMEOUT_SEC", "20"))
# Within this many blocks of the Bitcoin tip, always re-read explorer hashes
# (reorgs). Deeper heights are final for our purposes and served from cache.
FRESH_DEPTH = int(os.environ.get("FRESH_DEPTH", "12"))

USER_AGENT = "fpl-btc-headers/2.9.54"


def log(msg: str) -> None:
    ts = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    print(f"{ts} {msg}", flush=True)


class RpcError(RuntimeError):
    """The chosen node did not answer; caller re-picks the RPC."""


def norm_hash(h: str) -> str:
    return (h or "").strip().lower().removeprefix("0x")


def header_prev_hash(raw_hex: str) -> str:
    raw = bytes.fromhex(raw_hex.strip())
    if len(raw) != 80:
        return ""
    return raw[4:36][::-1].hex()


def parse_header_tail(rail: dict) -> list[tuple[int, str]]:
    tail = rail.get("header_tail") or []
    out: list[tuple[int, str]] = []
    if not isinstance(tail, list):
        return out
    for item in tail:
        if not isinstance(item, dict):
            continue
        try:
            h = int(item.get("height") or 0)
        except (TypeError, ValueError):
            continue
        hx = norm_hash(str(item.get("hash") or ""))
        if h > 0 and hx:
            out.append((h, hx))
    out.sort()
    return out


def choose_submit(
    *,
    spv: str,
    rail_tip: int,
    rail_hash: str,
    btc_tip: int,
    next_parent: str,
    btc_hash_at: Callable[[int], str],
    header_tail: list[tuple[int, str]],
    reorg_cursor: int | None,
    max_reorg: int,
) -> tuple[str, int | None]:
    """Pick the next header height to submit.

    Returns (action, height) where action is:
      reanchor | caught_up | extend | reorg
    """
    if spv != "bitcoin":
        return ("reanchor", btc_tip)
    if btc_tip <= 0:
        return ("caught_up", None)
    rail_hash = norm_hash(rail_hash)
    next_parent = norm_hash(next_parent)
    if rail_tip > btc_tip:
        return ("caught_up", None)
    if rail_tip == btc_tip:
        try:
            live_tip = norm_hash(btc_hash_at(rail_tip)) if rail_tip > 0 else ""
        except Exception:
            live_tip = ""
        if live_tip and live_tip == rail_hash:
            return ("caught_up", None)
        # Same height, different hash: Bitcoin replaced our tip (or a deep fork).
    elif next_parent and rail_hash and next_parent == rail_hash:
        return ("extend", rail_tip + 1)

    floor = max(1, rail_tip - max_reorg)
    if header_tail:
        common = None
        for h, hx in reversed(header_tail):
            if h < floor or h > min(rail_tip, btc_tip):
                continue
            try:
                btc = norm_hash(btc_hash_at(h))
            except Exception:
                continue
            if btc and btc == hx:
                common = h
                break
        if common is not None:
            nxt = common + 1
            if nxt > btc_tip:
                return ("caught_up", None)
            return ("reorg", nxt)
        # Tail did not overlap Bitcoin — walk from the oldest tail entry.
        oldest = min(h for h, _ in header_tail)
        nxt = max(floor, min(oldest, rail_tip))
        return ("reorg", nxt)

    cursor = reorg_cursor if reorg_cursor is not None else rail_tip
    cursor = min(cursor, rail_tip)
    if cursor < floor:
        return ("reorg", floor)
    return ("reorg", cursor)


def load_state() -> dict:
    try:
        with open(STATE_PATH, encoding="utf-8") as f:
            st = json.load(f)
        if not isinstance(st, dict):
            return {"last_height": 0, "last_hash": ""}
        return st
    except (OSError, json.JSONDecodeError):
        return {"last_height": 0, "last_hash": ""}


def save_state(st: dict) -> None:
    os.makedirs(os.path.dirname(STATE_PATH) or ".", exist_ok=True)
    tmp = STATE_PATH + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(st, f, indent=2)
        f.write("\n")
    os.replace(tmp, STATE_PATH)


class Explorer:
    def __init__(self, base: str) -> None:
        self.base = base.rstrip("/")
        self._hash: dict[int, str] = {}
        self._header: dict[str, str] = {}
        self._tip: tuple[float, int] = (0.0, 0)

    def _get(self, url: str, timeout: int = 20) -> str:
        last: Exception | None = None
        for i in range(EXPLORER_RETRIES):
            try:
                req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
                with urllib.request.urlopen(req, timeout=timeout) as r:
                    return r.read().decode("utf-8").strip()
            except (urllib.error.URLError, TimeoutError, OSError) as e:
                last = e
                time.sleep(min(8.0, 0.4 * (2**i)))
        raise RuntimeError(f"explorer {url}: {last}")

    def tip_height(self, max_age: float = 0.0) -> int:
        t, h = self._tip
        if max_age > 0 and h > 0 and time.time() - t < max_age:
            return h
        h = int(self._get(f"{self.base}/blocks/tip/height"))
        self._tip = (time.time(), h)
        return h

    def hash_at(self, height: int, *, fresh: bool = False) -> str:
        if fresh:
            self.drop_height(height)
        elif height in self._hash:
            return self._hash[height]
        hx = norm_hash(self._get(f"{self.base}/block-height/{height}"))
        if not hx:
            raise RuntimeError(f"empty bitcoin hash at {height}")
        self._hash[height] = hx
        return hx

    def header_hex(self, block_hash: str, *, fresh: bool = False) -> str:
        key = norm_hash(block_hash)
        if fresh:
            self._header.pop(key, None)
        elif key in self._header:
            return self._header[key]
        raw = self._get(f"{self.base}/block/{key}/header")
        if len(raw) != 160:
            raise RuntimeError(f"bad header hex for {key}: len={len(raw)}")
        self._header[key] = raw
        return raw

    def drop_height(self, height: int) -> None:
        hx = self._hash.pop(height, None)
        if hx:
            self._header.pop(norm_hash(hx), None)

    def drop_from(self, height: int, span: int = 64) -> None:
        """Invalidate cached tip-region hashes after a Bitcoin reorg."""
        for h in range(max(0, height - span), height + span + 1):
            self.drop_height(h)


def wire_query(addr: str, msg: dict, timeout: float = WIRE_TIMEOUT_SEC) -> dict:
    """One request on the node's line-JSON TCP wire (same as ctl uses).

    The node greets with `hello`, then answers the request. No keys involved:
    only read-only queries (status_req brief, account_query) go through here.
    """
    host, port = addr.rsplit(":", 1)
    try:
        with socket.create_connection((host, int(port)), timeout=timeout) as s:
            s.settimeout(timeout)
            f = s.makefile("rwb", buffering=1 << 16)
            f.readline()  # hello
            f.write((json.dumps(msg) + "\n").encode())
            f.flush()
            for _ in range(64):
                line = f.readline()
                if not line:
                    raise RpcError(f"{addr}: connection closed")
                m = json.loads(line)
                if m.get("type") not in ("hello", "pong"):
                    return m
    except (OSError, ValueError) as e:
        raise RpcError(f"{addr}: {e}") from e
    raise RpcError(f"{addr}: no reply")


def btc_rail_of(body: dict) -> dict:
    for r in body.get("rails") or []:
        if isinstance(r, dict) and r.get("asset") == "BTC":
            return r
    return {}


def score_rpcs(rows: list[tuple[str, int, int]]) -> list[tuple[str, int, int]]:
    """rows=(addr, tip_lag, btc_tip). Prefer tip_lag==0, then lowest BTC tip,
    then lowest ledger lag (same order as the pre-2.9.54 picker)."""
    return sorted(rows, key=lambda t: (0 if t[1] == 0 else 1, t[2], t[1]))


class RpcPool:
    """Pick an RPC rarely, then reuse it.

    Picking reads one *brief* status per candidate (~30 KB instead of the
    ~2.2 MB full status, which cost 0.45 s on v1 and up to 9+ s on v10).
    The pick is cached for RPC_REPICK_SEC; any RpcError drops it at once.
    """

    def __init__(self, addrs: list[str]) -> None:
        self.addrs = addrs
        self.dead_until: dict[str, float] = {}
        self.cur: str | None = None
        self.picked_at = 0.0
        # Last brief status seen: (addr, time, btc rail dict, ledger tip_height)
        self._view: tuple[str, float, dict, int] | None = None

    def mark_dead(self, addr: str, why: str) -> None:
        self.dead_until[addr] = time.time() + RPC_DEAD_SEC
        log(f"rpc dead {addr} ({why}); skip {int(RPC_DEAD_SEC)}s")

    def live(self) -> list[str]:
        now = time.time()
        out = [a for a in self.addrs if self.dead_until.get(a, 0) <= now]
        return out or list(self.addrs)

    def invalidate(self, why: str) -> None:
        if self.cur:
            self.mark_dead(self.cur, why)
        self.cur = None
        self._view = None

    def status_body(self, addr: str) -> dict:
        m = wire_query(addr, {"type": "status_req", "include_accounts": False, "brief": True})
        body = m.get("body")
        if m.get("type") != "status" or not isinstance(body, dict):
            raise RpcError(f"{addr}: unexpected status reply {str(m)[:120]}")
        return body

    def get(self) -> str:
        now = time.time()
        if self.cur is None or now - self.picked_at >= RPC_REPICK_SEC:
            self.cur = self.pick()
            self.picked_at = time.time()
        return self.cur

    def pick(self) -> str:
        """Prefer a ledger-synced node that still has the *lowest* BTC tip.

        Otherwise we only talk to a caught-up hub, declare lag=0, and leave a
        wedged archive (host1) permanently behind on SPV headers.
        """
        t0 = time.time()
        res: dict[str, tuple[dict, float] | None] = {}

        def one(addr: str) -> None:
            try:
                res[addr] = (self.status_body(addr), time.time())
            except RpcError as e:
                self.mark_dead(addr, str(e).split("\n")[0][:120])
                res[addr] = None

        ths = [threading.Thread(target=one, args=(a,), daemon=True) for a in self.live()]
        for t in ths:
            t.start()
        for t in ths:
            t.join(WIRE_TIMEOUT_SEC + 5)
        rows: list[tuple[str, int, int]] = []
        bodies: dict[str, tuple[dict, float]] = {}
        for addr, got in list(res.items()):
            if not got:
                continue
            body, t = got
            rail = btc_rail_of(body)
            bt = int(rail.get("tip_height") or 0)
            if bt <= 0:
                continue
            rows.append((addr, int(body.get("tip_lag") or 0), bt))
            bodies[addr] = got
        if not rows:
            addr = self.addrs[0]
            log(f"rpc pick: none healthy, fall back to {addr}")
            return addr
        best = score_rpcs(rows)[0][0]
        body, t = bodies[best]
        self._view = (best, t, btc_rail_of(body), int(body.get("tip_height") or 0))
        log(
            "rpc pick " + best + " in %.1fs from " % (time.time() - t0)
            + " ".join(f"{a}(lag={lg},btc={bt})" for a, lg, bt in rows)
        )
        return best

    def view(self, addr: str, max_age: float = 0.0) -> tuple[dict, int]:
        """(BTC rail, ledger tip) from a brief status, reusing a young one."""
        v = self._view
        if v and v[0] == addr and max_age > 0 and time.time() - v[1] <= max_age:
            return v[2], v[3]
        body = self.status_body(addr)
        rail = btc_rail_of(body)
        tip = int(body.get("tip_height") or 0)
        self._view = (addr, time.time(), rail, tip)
        return rail, tip

    def rail(self, addr: str, max_age: float = 0.0) -> dict:
        return self.view(addr, max_age)[0]

    def drop_view(self) -> None:
        self._view = None

    def account_seq(self, addr: str) -> int:
        """Watcher sequence via `account_query` (a few hundred bytes)."""
        m = wire_query(addr, {"type": "account_query", "account": ACCOUNT})
        body = m.get("body")
        if m.get("type") != "account" or not isinstance(body, dict):
            raise RpcError(f"{addr}: unexpected account reply {str(m)[:120]}")
        return int(body.get("sequence") or 0)


def submit_header(rpc: str, raw: str, height: int, sequence: int = 0) -> str:
    cmd = [
        CTL,
        "--addr",
        rpc,
        "submit-btc-header",
        "--account",
        ACCOUNT,
        "--raw",
        raw,
        "--height",
        str(height),
        "--scheme",
        SCHEME,
        "--keys-dir",
        KEYS,
        "--network-id",
        NETWORK_ID,
        "--fee",
        HEADER_FEE,
    ]
    if sequence > 0:
        cmd.extend(["--sequence", str(sequence)])
    return subprocess.check_output(cmd, text=True, timeout=60, stderr=subprocess.STDOUT)


def ctl_err(out: str) -> str | None:
    low = out.lower()
    if '"err"' in low or "deferred" in low:
        return out.strip()
    if "parent mismatch" in low or "bits do not follow" in low:
        return out.strip()
    return None


def rail_has(rail: dict, height: int, want_hash: str, before_tip: int) -> bool:
    adopted = int(rail.get("tip_height") or 0)
    adopted_hash = norm_hash(str(rail.get("tip_hash") or ""))
    want = norm_hash(want_hash)
    if adopted_hash == want and adopted == height:
        return True
    # Sequential extend: tip moved to or past the submitted height.
    return height > before_tip and adopted >= height and (
        adopted_hash == want or adopted > height
    )


def wait_adopted(
    pool: RpcPool,
    rpc: str,
    height: int,
    want_hash: str,
    before_tip: int,
    before_hash: str,
    timeout: float,
    seq0: int | None = None,
    n_txs: int = 1,
) -> dict | None:
    """Wait until the submitted header(s) are committed, cheaply.

    With `seq0` (watcher sequence used for the first submitted tx) we poll the
    tiny account query every TIP_POLL_SEC and read the brief status only once
    the sequence has moved (a header tx committed). Each partial advance of a
    burst re-arms the timeout. Without `seq0`, poll the brief status at a
    slower rate. Returns the last rail view seen (or None).
    """
    t0 = time.time()
    last: dict = {}
    last_seq = seq0
    slow = max(TIP_POLL_SEC * 3, 6.0)
    next_view = 0.0 if seq0 is None else t0 + max(15.0, timeout / 2)
    while time.time() - t0 < timeout:
        time.sleep(TIP_POLL_SEC)
        now = time.time()
        moved = False
        if seq0 is not None:
            try:
                sq = pool.account_seq(rpc)
            except RpcError:
                sq = None
            if sq is not None and last_seq is not None and sq > last_seq:
                last_seq = sq
                moved = True
                t0 = now  # progress: keep waiting for the rest of a burst
        if not moved and now < next_view:
            continue
        try:
            rail = pool.rail(rpc)
        except RpcError:
            continue
        next_view = now + (slow if seq0 is None else max(15.0, timeout / 2))
        last = rail
        if rail_has(rail, height, want_hash, before_tip):
            return rail
        if seq0 is not None and last_seq is not None and last_seq >= seq0 + n_txs:
            # All our txs left the pool but the rail lacks the last header
            # (applied short / replaced). Caller re-reads and decides.
            return rail
    try:
        last = pool.rail(rpc)
    except RpcError:
        pass
    return last or None


def wait_seq(pool: RpcPool, rpc: str, seq0: int, timeout: float) -> int | None:
    """Poll the watcher sequence until it passes seq0 (one tx committed)."""
    t0 = time.time()
    while time.time() - t0 < timeout:
        time.sleep(TIP_POLL_SEC)
        try:
            sq = pool.account_seq(rpc)
        except RpcError:
            continue
        if sq > seq0:
            return sq
    return None


def read_want() -> int:
    try:
        raw = open(WANT_PATH, encoding="utf-8").read().strip()
    except OSError:
        return 0
    try:
        return int(raw or "0")
    except ValueError:
        return 0


def clear_want() -> None:
    try:
        os.remove(WANT_PATH)
    except OSError:
        pass


def serve_late_claim(rpc: str, explorer: Explorer, rail: dict) -> bool:
    """Pull headers below the kept window when a claim asks for them.

    Returns True when this round was spent on that pull (caller should not
    also extend the tip — both use the same watcher sequence).
    """
    want = read_want()
    if want <= 0:
        return False
    floor = int(rail.get("header_floor") or 0)
    if floor <= 0:
        return False
    if want >= floor:
        clear_want()
        log(f"late-claim headers present down to {floor} (want {want})")
        return False
    end = floor - 1
    start = max(want, end - BACKFILL_CHUNK + 1)
    batch = []
    for h in range(start, end + 1):
        hx = explorer.hash_at(h, fresh=True)
        raw = explorer.header_hex(hx, fresh=True)
        batch.append(
            {
                "height": h,
                "hash": "",
                "parent_hash": "",
                "merkle_root": "",
                "raw": raw,
            }
        )
    path = WANT_PATH + ".chunk.json"
    with open(path, "w", encoding="utf-8") as f:
        json.dump(batch, f)
    log(f"late-claim backfill {start}..{end} n={len(batch)} floor={floor} want={want}")
    subprocess.check_output(
        [
            CTL,
            "--addr",
            rpc,
            "submit-btc-backfill",
            "--account",
            ACCOUNT,
            "--file",
            path,
            "--fee",
            HEADER_FEE,
            "--scheme",
            SCHEME,
            "--keys-dir",
            KEYS,
            "--network-id",
            NETWORK_ID,
        ],
        text=True,
        timeout=60,
        stderr=subprocess.STDOUT,
    )
    return True


def is_parent_err(out: str) -> bool:
    low = (out or "").lower()
    return "parent mismatch" in low or "bits do not follow" in low


def loop() -> None:
    if not ACCOUNT:
        sys.exit("FPL_WATCHER_ACCOUNT is required (funded watcher, not a val key)")
    if not RPCS:
        sys.exit("FPL_RPC is empty")
    st = load_state()
    explorer = Explorer(EXPLORER)
    pool = RpcPool(RPCS)
    log(
        f"btc-header-submitter account={ACCOUNT} rpcs={','.join(RPCS)} "
        f"explorer={EXPLORER} max_reorg={MAX_REORG} repick={int(RPC_REPICK_SEC)}s "
        f"poll={TIP_POLL_SEC}s burst={CATCHUP_BURST} burst_probe={int(BURST_PROBE_SEC)}s"
    )
    backoff = 1.0
    stall_sleep = 0.0
    burst_retry_at = 0.0  # 0 = bursts allowed; else time to re-probe a burst
    last_land = time.time()
    last_lag = 0
    stat_landed = 0
    stat_t0 = time.time()
    while True:
        try:
            rpc = pool.get()
            rail, ledger_tip = pool.view(rpc, max_age=RAIL_MAX_AGE_SEC)
            if read_want() > 0:
                seq_before = pool.account_seq(rpc)
                if serve_late_claim(rpc, explorer, rail):
                    # Same watcher sequence as the tip extend: let it commit.
                    if wait_seq(pool, rpc, seq_before, max(ADOPT_SEC, 30.0)) is None:
                        log("WARN late-claim backfill not committed yet")
                    pool.drop_view()
                    time.sleep(CATCHUP_SLEEP_SEC)
                    continue
            btc_tip = explorer.tip_height(max_age=BTC_TIP_TTL_SEC if last_lag > FRESH_DEPTH else 0)
            rail_tip = int(rail.get("tip_height") or 0)
            rail_hash = norm_hash(str(rail.get("tip_hash") or ""))
            spv = str(rail.get("spv") or "protocol")
            lag = max(0, btc_tip - rail_tip) if spv == "bitcoin" else btc_tip
            last_lag = lag
            deep = lag > FRESH_DEPTH

            # Tip-hash mismatch ⇒ Bitcoin replaced our tip. Never trust a stale
            # explorer cache for that height or we "reorg" to the same dead hash
            # forever and CATCHUP_BURST never runs. (Far behind the Bitcoin tip
            # the cached hash is final; near it, always re-read.)
            if spv == "bitcoin" and rail_tip > 0:
                try:
                    live_tip = explorer.hash_at(rail_tip, fresh=not deep)
                except Exception:
                    live_tip = ""
                if live_tip and live_tip != rail_hash:
                    explorer.drop_from(rail_tip, span=max(8, min(MAX_REORG, 128)))
                    if st.get("reorg_cursor") is None:
                        st["reorg_cursor"] = rail_tip
                        save_state(st)
                    log(
                        f"tip fork rail={rail_tip} "
                        f"rail_hash={rail_hash[:16]}… "
                        f"btc_hash={live_tip[:16]}… → reorg walk"
                    )

            next_parent = ""
            if spv == "bitcoin" and rail_tip < btc_tip:
                nxt_h = rail_tip + 1
                nxt_raw = explorer.header_hex(
                    explorer.hash_at(nxt_h, fresh=not deep), fresh=not deep
                )
                next_parent = header_prev_hash(nxt_raw)

            action, height = choose_submit(
                spv=spv,
                rail_tip=rail_tip,
                rail_hash=rail_hash,
                btc_tip=btc_tip,
                next_parent=next_parent,
                btc_hash_at=lambda h: explorer.hash_at(h, fresh=True),
                header_tail=parse_header_tail(rail),
                reorg_cursor=st.get("reorg_cursor"),
                max_reorg=MAX_REORG,
            )
            if action == "caught_up" or height is None:
                log(f"caught up rail={rail_tip} btc={btc_tip}")
                st.pop("reorg_cursor", None)
                save_state(st)
                backoff = 1.0
                stall_sleep = 0.0
                time.sleep(POLL_SEC)
                pool.drop_view()
                continue

            if action == "extend" and lag > 1:
                # Catch-up. One header per round while the node only admits a
                # header on the committed rail tip; a CATCHUP_BURST probe every
                # BURST_PROBE_SEC switches back to bursts once admission allows.
                want_burst = CATCHUP_BURST > 1 and time.time() >= burst_retry_at
                n = min(CATCHUP_BURST, max(1, btc_tip - rail_tip)) if want_burst else 1
                seq = pool.account_seq(rpc)
                last_h = height
                last_bh = ""
                submitted = 0
                stop_why = ""
                t_round = time.time()
                for i in range(n):
                    h = rail_tip + 1 + i
                    try:
                        hb = explorer.hash_at(h, fresh=(btc_tip - h) <= FRESH_DEPTH)
                        raw = explorer.header_hex(hb, fresh=(btc_tip - h) <= FRESH_DEPTH)
                    except Exception as e:
                        log(f"WARN explorer {h}: {e}")
                        break
                    try:
                        out = submit_header(rpc, raw, h, sequence=seq + i)
                    except subprocess.CalledProcessError as e:
                        out = (e.output or str(e)).strip()
                        stop_why = out.splitlines()[-1][:240] if out else str(e)
                        if not (i >= 1 and is_parent_err(out)):
                            log(f"WARN submit {h}: {stop_why}")
                        break
                    err = ctl_err(out)
                    if err:
                        stop_why = err[:200]
                        log(f"WARN burst stop {h}: {stop_why}")
                        break
                    last_h, last_bh = h, hb
                    submitted += 1
                if n > 1:
                    if submitted == 1 and is_parent_err(stop_why):
                        burst_retry_at = time.time() + BURST_PROBE_SEC
                        log(
                            f"burst refused past first header (node admits on "
                            f"committed tip only); single-header mode, re-probe in "
                            f"{int(BURST_PROBE_SEC)}s"
                        )
                    elif submitted > 1:
                        burst_retry_at = 0.0
                if submitted == 0:
                    if is_parent_err(stop_why):
                        # Our view of the rail or explorer cache is stale.
                        explorer.drop_from(rail_tip + 1, span=FRESH_DEPTH)
                        pool.drop_view()
                    time.sleep(min(POLL_SEC, backoff))
                    backoff = min(60.0, backoff * 1.5)
                    continue
                wait_s = min(120.0, max(ADOPT_SEC, submitted * 1.5))
                rail2 = wait_adopted(
                    pool, rpc, last_h, last_bh, rail_tip, rail_hash, wait_s,
                    seq0=seq, n_txs=submitted,
                ) or {}
                adopted_h = int(rail2.get("tip_height") or 0)
                _, tip_now = pool.view(rpc, max_age=RAIL_MAX_AGE_SEC)
                if adopted_h > rail_tip:
                    st["last_height"] = adopted_h
                    st["last_hash"] = norm_hash(str(rail2.get("tip_hash") or last_bh))
                    st.pop("reorg_cursor", None)
                    save_state(st)
                    backoff = 1.0
                    stall_sleep = 0.0
                    now = time.time()
                    stat_landed += adopted_h - rail_tip
                    log(
                        f"landed {rail_tip}->{adopted_h} (submitted {submitted}) "
                        f"round={now - t_round:.1f}s since_last={now - last_land:.1f}s "
                        f"lag={max(0, btc_tip - adopted_h)} ledger={tip_now} rpc={rpc}"
                    )
                    last_land = now
                    if now - stat_t0 >= 600:
                        log(
                            f"rate {stat_landed} headers in {now - stat_t0:.0f}s "
                            f"= {stat_landed * 3600 / (now - stat_t0):.0f}/h"
                        )
                        stat_landed, stat_t0 = 0, now
                elif tip_now <= ledger_tip:
                    # Ledger not moving: back off instead of hammering the node.
                    stall_sleep = min(STALL_BACKOFF_MAX, max(5.0, stall_sleep * 2))
                    log(
                        f"WARN ledger stalled at {tip_now} (rail={adopted_h}); "
                        f"backoff {stall_sleep:.0f}s"
                    )
                    time.sleep(stall_sleep)
                else:
                    log(
                        f"WARN no rail move submitted={submitted} rail={adopted_h} "
                        f"ledger {ledger_tip}->{tip_now}; resubmit"
                    )
                time.sleep(CATCHUP_SLEEP_SEC)
                continue

            fresh = not deep
            bh = explorer.hash_at(height, fresh=fresh)
            raw = explorer.header_hex(bh, fresh=fresh)
            # Replacing tip with the same hash we already have is a no-op loop.
            if (
                action == "reorg"
                and height == rail_tip
                and norm_hash(bh) == rail_hash
            ):
                nxt_cursor = height - 1
                floor = max(1, rail_tip - MAX_REORG)
                explorer.drop_from(height, span=8)
                if nxt_cursor < floor:
                    raise RuntimeError(
                        f"reorg no-op at tip {height} and walk exhausted (floor {floor})"
                    )
                st["reorg_cursor"] = nxt_cursor
                save_state(st)
                log(f"WARN reorg no-op tip {height} still {bh[:16]}…; walk to {nxt_cursor}")
                time.sleep(CATCHUP_SLEEP_SEC)
                continue
            log(
                f"{action} h={height} hash={bh} rpc={rpc} "
                f"(btc_tip={btc_tip} rail={rail_tip} lag={lag})"
            )
            try:
                seq = pool.account_seq(rpc)
            except RpcError:
                seq = None
            try:
                out = submit_header(rpc, raw, height, sequence=seq or 0)
            except subprocess.CalledProcessError as e:
                out = (e.output or str(e)).strip()
                err = ctl_err(out) or out
                log(f"WARN submit {height}: {err.splitlines()[-1][:300]}")
                pool.drop_view()
                if is_parent_err(out):
                    if action == "reorg":
                        nxt_cursor = height - 1
                        floor = max(1, rail_tip - MAX_REORG)
                        if nxt_cursor < floor:
                            raise RuntimeError(
                                f"reorg walk exhausted at {height} (floor {floor})"
                            )
                        st["reorg_cursor"] = nxt_cursor
                        save_state(st)
                        log(f"reorg walk {height} rejected; next cursor {nxt_cursor}")
                        backoff = 1.0
                        time.sleep(CATCHUP_SLEEP_SEC)
                        continue
                    explorer.drop_from(height, span=8)
                time.sleep(min(POLL_SEC, backoff))
                backoff = min(60.0, backoff * 1.5)
                continue

            log(out.strip().splitlines()[0] if out.strip() else out.strip())
            err = ctl_err(out)
            if err:
                raise RuntimeError(f"header {height} rejected via {rpc}: {err[:240]}")

            adopted = wait_adopted(
                pool, rpc, height, bh, rail_tip, rail_hash, ADOPT_SEC, seq0=seq, n_txs=1
            )
            adopted_hash = norm_hash(str((adopted or {}).get("tip_hash") or ""))
            adopted_h = int((adopted or {}).get("tip_height") or 0)
            ok = adopted is not None and (
                (adopted_hash == norm_hash(bh) and adopted_h == height)
                or (height > rail_tip and adopted_h >= height)
            )
            # Tip replace must actually change tip_hash; same hash is not progress.
            if ok and action == "reorg" and height == rail_tip and adopted_hash == rail_hash:
                ok = False
            if not ok:
                if action == "reorg":
                    nxt_cursor = height - 1
                    floor = max(1, rail_tip - MAX_REORG)
                    if nxt_cursor < floor:
                        raise RuntimeError(
                            f"rail did not adopt reorg {height} and walk exhausted "
                            f"(floor {floor})"
                        )
                    st["reorg_cursor"] = nxt_cursor
                    save_state(st)
                    log(
                        f"WARN rail did not adopt reorg {height} "
                        f"(tip {adopted_h} {adopted_hash[:16]}…); "
                        f"walk to {nxt_cursor}"
                    )
                    time.sleep(CATCHUP_SLEEP_SEC)
                    continue
                # Sequential pack can land after ADOPT_SEC. Follow the rail;
                # back off only while the ledger itself is not moving.
                _, tip_now = pool.view(rpc, max_age=RAIL_MAX_AGE_SEC)
                if tip_now <= ledger_tip:
                    stall_sleep = min(STALL_BACKOFF_MAX, max(5.0, stall_sleep * 2))
                    log(
                        f"WARN rail did not adopt header {height}; ledger stalled at "
                        f"{tip_now}; backoff {stall_sleep:.0f}s"
                    )
                    time.sleep(stall_sleep)
                else:
                    log(
                        f"WARN rail did not adopt header {height} "
                        f"(tip {adopted_h} {adopted_hash[:16]}…); retry"
                    )
                backoff = 1.0
                time.sleep(CATCHUP_SLEEP_SEC)
                continue

            st["last_height"] = height
            st["last_hash"] = bh
            st.pop("reorg_cursor", None)
            save_state(st)
            backoff = 1.0
            stall_sleep = 0.0
            last_land = time.time()
            # Catch-up burst: do not sleep the idle poll while lagging.
            if lag > 1 and action == "extend":
                time.sleep(CATCHUP_SLEEP_SEC)
            elif action == "reorg":
                time.sleep(CATCHUP_SLEEP_SEC)
            else:
                time.sleep(POLL_SEC)
                pool.drop_view()
        except RpcError as e:
            log(f"WARN rpc: {e}; re-pick")
            pool.invalidate(str(e)[:120])
            time.sleep(min(POLL_SEC, max(2.0, backoff)))
            backoff = min(60.0, backoff * 1.5)
        except Exception as e:
            log(f"WARN {e}")
            pool.drop_view()
            time.sleep(min(POLL_SEC, max(2.0, backoff)))
            backoff = min(60.0, backoff * 1.5)


def _self_test() -> None:
    hashes = {
        10: "aa" * 32,
        11: "bb" * 32,
        12: "cc" * 32,
        13: "dd" * 32,
        14: "ee" * 32,
    }

    def btc_at(h: int) -> str:
        return hashes[h]

    # Sequential extend when parent matches tip.
    act, h = choose_submit(
        spv="bitcoin",
        rail_tip=12,
        rail_hash=hashes[12],
        btc_tip=14,
        next_parent=hashes[12],
        btc_hash_at=btc_at,
        header_tail=[],
        reorg_cursor=None,
        max_reorg=200,
    )
    assert (act, h) == ("extend", 13), (act, h)

    # Caught up.
    act, h = choose_submit(
        spv="bitcoin",
        rail_tip=14,
        rail_hash=hashes[14],
        btc_tip=14,
        next_parent="",
        btc_hash_at=btc_at,
        header_tail=[],
        reorg_cursor=None,
        max_reorg=200,
    )
    assert (act, h) == ("caught_up", None), (act, h)

    # Reanchor off fixture chain.
    act, h = choose_submit(
        spv="protocol",
        rail_tip=1,
        rail_hash="00" * 32,
        btc_tip=14,
        next_parent="",
        btc_hash_at=btc_at,
        header_tail=[],
        reorg_cursor=None,
        max_reorg=200,
    )
    assert (act, h) == ("reanchor", 14), (act, h)

    # Deep reorg with header_tail: common ancestor 11, submit 12.
    stale = {
        10: hashes[10],
        11: hashes[11],
        12: "c1" * 32,
        13: "d1" * 32,
        14: "e1" * 32,
    }
    act, h = choose_submit(
        spv="bitcoin",
        rail_tip=14,
        rail_hash=stale[14],
        btc_tip=14,
        next_parent=hashes[13],  # bitcoin 14 parent is live 13, not stale tip
        btc_hash_at=btc_at,
        header_tail=list(stale.items()),
        reorg_cursor=None,
        max_reorg=200,
    )
    assert (act, h) == ("reorg", 12), (act, h)

    # No tail: start at the stale tip, then honor a walk-back cursor.
    act, h = choose_submit(
        spv="bitcoin",
        rail_tip=14,
        rail_hash=stale[14],
        btc_tip=20,
        next_parent="ff" * 32,
        btc_hash_at=btc_at,
        header_tail=[],
        reorg_cursor=None,
        max_reorg=200,
    )
    assert (act, h) == ("reorg", 14), (act, h)
    act, h = choose_submit(
        spv="bitcoin",
        rail_tip=14,
        rail_hash=stale[14],
        btc_tip=20,
        next_parent="ff" * 32,
        btc_hash_at=btc_at,
        header_tail=[],
        reorg_cursor=12,
        max_reorg=200,
    )
    assert (act, h) == ("reorg", 12), (act, h)

    # Cursor must not walk past MAX_REORG.
    act, h = choose_submit(
        spv="bitcoin",
        rail_tip=14,
        rail_hash=stale[14],
        btc_tip=20,
        next_parent="ff" * 32,
        btc_hash_at=btc_at,
        header_tail=[],
        reorg_cursor=1,
        max_reorg=5,
    )
    assert (act, h) == ("reorg", 9), (act, h)  # floor = 14-5 = 9

    # RPC scoring keeps the old preference order.
    rows = [("a", 3, 100), ("b", 0, 120), ("c", 0, 110), ("d", 0, 110)]
    assert [r[0] for r in score_rpcs(rows)] == ["c", "d", "b", "a"]

    # rail_has: exact tip, sequential extend past the submitted height.
    assert rail_has({"tip_height": 13, "tip_hash": hashes[13]}, 13, hashes[13], 12)
    assert rail_has({"tip_height": 14, "tip_hash": hashes[14]}, 13, hashes[13], 12)
    assert not rail_has({"tip_height": 12, "tip_hash": hashes[12]}, 13, hashes[13], 12)
    assert is_parent_err("Error: BTC header parent mismatch")
    assert not is_parent_err("Error: fee too low")

    _self_test_wire()
    print("self-test ok")


def _self_test_wire() -> None:
    """Fake node on localhost: brief status + account_query; the header
    "commits" (sequence and rail tip move) after a few account polls."""
    import socketserver

    state = {"seq": 7, "rail": 100, "polls": 0, "status_calls": 0, "acct_calls": 0}

    class H(socketserver.StreamRequestHandler):
        def handle(self) -> None:
            self.wfile.write(b'{"type":"hello","id":"fake"}\n')
            req = json.loads(self.rfile.readline())
            if req.get("type") == "status_req":
                assert req.get("brief") is True, req
                state["status_calls"] += 1
                body = {
                    "tip_height": 5000 + state["rail"],
                    "tip_lag": 0,
                    "rails": [{"asset": "BTC", "tip_height": state["rail"],
                               "tip_hash": "%064x" % state["rail"], "spv": "bitcoin"}],
                }
                out = {"type": "status", "body": body}
            elif req.get("type") == "account_query":
                state["acct_calls"] += 1
                state["polls"] += 1
                if state["polls"] == 3:
                    state["seq"] += 1
                    state["rail"] += 1
                out = {"type": "account", "body": {"sequence": state["seq"]}}
            else:
                out = {"type": "err", "msg": "unexpected"}
            self.wfile.write((json.dumps(out) + "\n").encode())

    socketserver.TCPServer.allow_reuse_address = True
    srv = socketserver.ThreadingTCPServer(("127.0.0.1", 0), H)
    th = threading.Thread(target=srv.serve_forever, daemon=True)
    th.start()
    addr = "127.0.0.1:%d" % srv.server_address[1]
    global TIP_POLL_SEC
    saved = TIP_POLL_SEC
    TIP_POLL_SEC = 0.01
    try:
        pool = RpcPool([addr, "127.0.0.1:1"])
        assert pool.get() == addr
        rail, tip = pool.view(addr, max_age=60)
        assert (rail["tip_height"], tip) == (100, 5100), (rail, tip)
        calls = state["status_calls"]
        pool.view(addr, max_age=60)
        assert state["status_calls"] == calls, "young view must be reused"
        seq0 = pool.account_seq(addr)
        assert seq0 == 7
        got = wait_adopted(pool, addr, 101, "%064x" % 101, 100, "%064x" % 100, 5.0,
                           seq0=seq0, n_txs=1)
        assert got and int(got["tip_height"]) == 101, got
        # Exactly one brief status after the sequence moved (no busy polling).
        assert state["status_calls"] == calls + 1, state
        assert pool.dead_until.get("127.0.0.1:1"), "dead RPC is marked"
    finally:
        TIP_POLL_SEC = saved
        srv.shutdown()
        srv.server_close()


if __name__ == "__main__":
    if os.environ.get("BTC_HEADER_SELFTEST") == "1" or (
        len(sys.argv) > 1 and sys.argv[1] == "--self-test"
    ):
        _self_test()
    else:
        loop()
