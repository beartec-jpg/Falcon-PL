#!/usr/bin/env python3
"""Falcon PL public testnet 2300 — read-only soak / alert checklist (L3).

Polls public or Tailscale status endpoints. Never prints key material, private
hex, env secrets, or full account dumps.

Usage:
  # from falcon-pl-rs/ (ctl defaults to target/release/falcon-pl-ctl)
  python3 crates/fd-pl/scripts/pl-prod-soak-check.py
  python3 crates/fd-pl/scripts/pl-prod-soak-check.py --once
  python3 crates/fd-pl/scripts/pl-prod-soak-check.py --addr 100.64.0.1:19301   # v1 over the tailnet (or 127.0.0.1:19301 on host1)
  PL_CTL=/path/to/falcon-pl-ctl python3 ... --interval 60
  # userspace Tailscale (no direct 100.x TCP): hop via a seat host, read-only
  python3 ... --once --ssh-via falcon@host1 --addr 100.64.0.1:19301

Exit codes:
  0  healthy
  1  warning (lagging seat, elevated round, mempool backlog without stall yet)
  2  critical (tip stalled with work, committee < 6, consensus_stall set long)
  3  transport / parse failure

Config drift (2026-10-05): every reachable seat must report the expected
qc_header_version / qc_start_height / vote_rounds_from_height for 2300, and —
on binaries that expose `bridge_cfg` — a present, non-drifted keys/lc/bridge.json
whose qc_cfg_digest matches across seats. A mismatch is CRITICAL (a stale
bridge.json kept v13 from catching up on 2026-10-04).

Phase 2 pause window (2026-10-06, v2.9.48): seats report
vote_rounds_pause_from / vote_rounds_resume_at. Expected 2300 window is
371037..375000. Inside the window round_scoped_votes=false is expected; a seat
still in legacy (pre-2.9.48 binary, or the 999999999 override) when the tip is
within PL_SOAK_RESUME_MARGIN blocks of the resume height is CRITICAL — it
would split from the fleet at the resume height.

Disk (2026-10-07, v2.9.53): host2 hit 100% (173 GB of /tmp/pl-hub-join-*
dumps) and the chain stalled at 374384. Every check now reports free disk per
host — `df` over the same read-only SSH hop (PL_SOAK_DISK_HOSTS, default the
six 2300 hosts) plus the seat's own status `disk` field on >= 2.9.53 binaries.
Free < PL_SOAK_DISK_CRIT_PCT (5%) is CRITICAL, < PL_SOAK_DISK_WARN_PCT (10%)
is WARN. `--disk-hosts off` skips the SSH df.

Transport: with --ssh-via the remote side drops the ~1.9 MB `falcon_qc` / `rails`
blobs before the reply crosses the hop, waits up to PL_SOAK_PEER_TIMEOUT
seconds and retries once, and a slow seat is reported as a timeout (not as
unreachable).
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from typing import Any

DEFAULT_ADDR = os.environ.get("PL_SOAK_ADDR", "100.64.0.1:19301")
_HERE = os.path.dirname(os.path.realpath(__file__))
DEFAULT_CTL = os.environ.get(
    "PL_CTL",
    # scripts/ -> fd-pl -> crates -> falcon-pl-rs/target/release
    os.path.normpath(os.path.join(_HERE, "..", "..", "..", "target", "release", "falcon-pl-ctl")),
)
# Optional extra seats (Tailscale) — read-only, skipped if unreachable.
DEFAULT_PEERS = os.environ.get(
    "PL_SOAK_PEERS",
    # v1 v3 v6 v10 v11 v12 v13 (live 2300 seats, 2026-10-05)
    "100.64.0.1:19301,100.64.0.3:19303,100.64.0.6:19306,"
    "100.64.0.7:19307,100.64.0.3:19311,100.64.0.2:19312,"
    "100.64.0.4:19313",
)

# Expected 2300 network config (override via env/flags when it legitimately changes).
EXPECT_QC_HEADER_VERSION = int(os.environ.get("PL_SOAK_EXPECT_QC_HEADER_VERSION", "4"))
EXPECT_QC_START_HEIGHT = int(os.environ.get("PL_SOAK_EXPECT_QC_START_HEIGHT", "353091"))
EXPECT_VOTE_ROUNDS_FROM = int(os.environ.get("PL_SOAK_EXPECT_VOTE_ROUNDS_FROM", "369998"))
EXPECT_LC_START_HEIGHT = int(os.environ.get("PL_SOAK_EXPECT_LC_START_HEIGHT", "116957"))
# Phase 2 pause window FROM..RESUME (resume exclusive); "off" = expect none.
EXPECT_VOTE_ROUNDS_PAUSE = os.environ.get("PL_SOAK_EXPECT_VOTE_ROUNDS_PAUSE", "371037..375000")
RESUME_MARGIN = int(os.environ.get("PL_SOAK_RESUME_MARGIN", "1000"))
EXPECT_QC_CFG_DIGEST = os.environ.get("PL_SOAK_EXPECT_QC_CFG_DIGEST", "")

STALL_CRIT_MS = int(os.environ.get("PL_SOAK_STALL_CRIT_MS", "300000"))  # 5 min
LAG_WARN = int(os.environ.get("PL_SOAK_LAG_WARN", "20"))
LAG_CRIT = int(os.environ.get("PL_SOAK_LAG_CRIT", "200"))
COMMITTEE_NEED = 6

# Disk free per host (v2.9.53). Percent = avail / size (same as the node's
# statvfs f_bavail / f_blocks), so root-reserved blocks count as used.
DISK_WARN_PCT = float(os.environ.get("PL_SOAK_DISK_WARN_PCT", "10"))
DISK_CRIT_PCT = float(os.environ.get("PL_SOAK_DISK_CRIT_PCT", "5"))
DEFAULT_DISK_HOSTS = os.environ.get(
    "PL_SOAK_DISK_HOSTS",
    # host1 (v1) host2 (v3+v11) mhr (v6) hel1-6 (v10) hil (v12) fsn1 (v13)
    "falcon@host1,falcon@host2,root@seat-host-6,root@seat-host-7,"
    "root@seat-host-4,root@seat-host-5",
)
# Data dir + /tmp (separate tmpfs on some hosts). `~` expands remotely.
DISK_PATHS = os.environ.get("PL_SOAK_DISK_PATHS", "~/falcon-pl-public-testnet-2300 /tmp")
DISK_TIMEOUT = float(os.environ.get("PL_SOAK_DISK_TIMEOUT", "25"))


# Optional SSH hop for hosts whose Tailscale is userspace-only (no direct TCP
# to 100.x): run the remote seat's own ctl, read-only `status` only.
SSH_VIA = os.environ.get("PL_SOAK_SSH_VIA", "")  # e.g. falcon@host1
SSH_CMD = os.environ.get("PL_SOAK_SSH_CMD", "sudo -n tailscale ssh")
REMOTE_CTL = os.environ.get("PL_SOAK_REMOTE_CTL", "~/falcon-pl-public-testnet-2300/bin/falcon-pl-ctl")
# Status replies are ~2 MB (mostly `falcon_qc`); over userspace Tailscale a
# 20 s cap made healthy seats look unreachable (2026-10-06, v10/v12).
PEER_TIMEOUT = float(os.environ.get("PL_SOAK_PEER_TIMEOUT", "45"))
PEER_RETRIES = int(os.environ.get("PL_SOAK_PEER_RETRIES", "1"))
# Remote pre-filter (ssh-via only): drop the big blobs before the hop.
REMOTE_FILTER = os.environ.get("PL_SOAK_REMOTE_FILTER", "1") == "1"
_DROP_KEYS = ("falcon_qc", "rails", "accounts", "amm_lp_weights")
_REMOTE_PY = (
    "import sys,json;t=sys.stdin.read();i=t.find('{');"
    "m=json.loads(t[i:]) if i>=0 else {};b=m.get('body',m);"
    "[b.pop(k,None) for k in %r];print(json.dumps(m))" % (_DROP_KEYS,)
)


class StatusTimeout(RuntimeError):
    """Seat answered too slowly (distinct from unreachable)."""


def status_cmd(ctl: str, addr: str) -> list[str]:
    if SSH_VIA:
        import shlex

        remote = f"{REMOTE_CTL} --addr {shlex.quote(addr)} status"
        if REMOTE_FILTER:
            remote += f" | python3 -c {shlex.quote(_REMOTE_PY)}"
        return SSH_CMD.split() + [SSH_VIA, remote]
    return [ctl, "--addr", addr, "status"]


def ctl_status(ctl: str, addr: str, timeout: float = 20.0, retries: int = 0) -> dict[str, Any]:
    cmd = status_cmd(ctl, addr)
    last: Exception | None = None
    for _ in range(max(0, retries) + 1):
        try:
            out = subprocess.check_output(
                cmd,
                text=True,
                timeout=timeout,
                stderr=subprocess.DEVNULL,
            )
            break
        except subprocess.TimeoutExpired:
            last = StatusTimeout(f"status from {addr} timed out after {timeout:.0f}s")
        except subprocess.CalledProcessError as e:
            last = e
    else:
        assert last is not None
        raise last
    i = out.find("{")
    if i < 0:
        raise RuntimeError(f"no JSON from {addr}")
    msg, _ = json.JSONDecoder().raw_decode(out[i:])
    body = msg.get("body") or msg
    if not isinstance(body, dict):
        raise RuntimeError(f"bad body from {addr}")
    return body


def disk_cmd(host: str) -> list[str]:
    """Read-only `df` on `host` over the same SSH hop as status."""
    return SSH_CMD.split() + [host, f"df -P -B1 {DISK_PATHS}"]


def parse_df(out: str) -> list[dict[str, Any]]:
    """`df -P -B1` → [{fs, mount, total, avail, free_pct}], one row per mount."""
    rows: list[dict[str, Any]] = []
    seen: set[str] = set()
    for line in out.splitlines():
        parts = line.split()
        if len(parts) < 6 or not parts[1].isdigit():
            continue  # header / error lines
        fs, total, _used, avail, _cap = parts[0], int(parts[1]), parts[2], int(parts[3]), parts[4]
        mount = " ".join(parts[5:])
        if mount in seen:
            continue
        seen.add(mount)
        pct = (avail * 100.0 / total) if total > 0 else 100.0
        rows.append({"fs": fs, "mount": mount, "total": total, "avail": avail, "free_pct": round(pct, 2)})
    return rows


def host_disk(host: str, timeout: float = DISK_TIMEOUT) -> list[dict[str, Any]] | None:
    """df rows for `host`, or None if unreachable (reported as WARN)."""
    try:
        out = subprocess.check_output(
            disk_cmd(host), text=True, timeout=timeout, stderr=subprocess.DEVNULL
        )
    except Exception:
        return None
    rows = parse_df(out)
    return rows or None


def _gb(b: Any) -> str:
    try:
        return f"{int(b) / 1e9:.1f}GB"
    except (TypeError, ValueError):
        return "?"


def check_disk(
    disks: list[tuple[str, list[dict[str, Any]] | None]],
    seats: list[tuple[str, dict[str, Any]]],
) -> tuple[int, list[str]]:
    """Per-host df rows + per-seat status `disk` → (severity, messages)."""
    sev, msgs = 0, []

    def grade(label: str, pct: float, avail: Any, total: Any) -> None:
        nonlocal sev
        txt = f"{label} free={pct:.1f}% ({_gb(avail)} of {_gb(total)})"
        if pct < DISK_CRIT_PCT:
            sev = 2
            msgs.append(f"CRITICAL disk {txt} < {DISK_CRIT_PCT:g}% — free space now (snapshots/proposals will fail)")
        elif pct < DISK_WARN_PCT:
            sev = max(sev, 1)
            msgs.append(f"WARN disk {txt} < {DISK_WARN_PCT:g}%")
        else:
            msgs.append(f"OK disk {txt}")

    for host, rows in disks:
        if rows is None:
            sev = max(sev, 1)
            msgs.append(f"WARN disk {host}: df unavailable")
            continue
        for r in rows:
            grade(f"{host}:{r['mount']}", float(r["free_pct"]), r["avail"], r["total"])
    for name, body in seats:
        d = body.get("disk")
        if not isinstance(d, dict) or not d.get("known"):
            continue
        grade(f"seat {name}", float(d.get("free_pct") or 0.0), d.get("avail_bytes"), d.get("total_bytes"))
        if int(d.get("skipped_dumps") or 0) > 0:
            msgs.append(f"INFO disk seat {name}: node skipped {d.get('skipped_dumps')} optional dump(s): {d.get('last_skip')}")
    return sev, msgs


def slim(body: dict[str, Any]) -> dict[str, Any]:
    """Strip anything that could leak secrets or huge dumps."""
    vals = body.get("validators") or []
    bonded, jailed = [], []
    for v in vals:
        if not isinstance(v, dict):
            continue
        vid = v.get("id")
        bond = v.get("bond")
        entry = {"id": vid, "bond": bond}
        if v.get("jailed"):
            jailed.append(entry)
        elif bond and int(bond) > 0:
            bonded.append(entry)
    live = body.get("liveness") or {}
    bcfg = body.get("bridge_cfg")
    bridge_cfg = None
    if isinstance(bcfg, dict):
        bridge_cfg = {
            "present": bcfg.get("present"),
            "parsed": bcfg.get("parsed"),
            "sha256_prefix": (bcfg.get("sha256") or "")[:16],
            "qc_cfg_digest": bcfg.get("qc_cfg_digest"),
            "qc_header_version": bcfg.get("qc_header_version"),
            "qc_start_height": bcfg.get("qc_start_height"),
            "start_height": bcfg.get("start_height"),
            "network_id": bcfg.get("network_id"),
            "drift": bcfg.get("drift"),
            "drift_reasons": bcfg.get("drift_reasons"),
        }
    lc = body.get("lc") if isinstance(body.get("lc"), dict) else {}
    dk = body.get("disk")
    disk = None
    if isinstance(dk, dict):
        disk = {k: dk.get(k) for k in (
            "known", "path", "avail_bytes", "total_bytes", "free_pct", "level", "skipped_dumps", "last_skip",
        )}
    return {
        "id": body.get("id"),
        "network_id": body.get("network_id"),
        "product_version": body.get("product_version"),
        "tip_height": body.get("tip_height"),
        "tip_hash_prefix": (body.get("tip_hash") or "")[:16],
        "best_peer_tip": body.get("best_peer_tip"),
        "tip_lag": body.get("tip_lag"),
        "mempool": body.get("mempool"),
        "has_applicable_tx": body.get("has_applicable_tx"),
        "committee_size": body.get("committee_size"),
        "lottery_order": body.get("lottery_order"),
        "online_seats": body.get("online_seats"),
        "vote_rounds_from_height": body.get("vote_rounds_from_height"),
        "vote_rounds_network_default": body.get("vote_rounds_network_default"),
        "vote_rounds_pause_from": body.get("vote_rounds_pause_from"),
        "vote_rounds_resume_at": body.get("vote_rounds_resume_at"),
        "vote_rounds_paused_now": body.get("vote_rounds_paused_now"),
        "qc_header_version": body.get("qc_header_version"),
        "qc_start_height": body.get("qc_start_height"),
        "lc_start_height": lc.get("start_height"),
        "bridge_cfg": bridge_cfg,
        "round_scoped_votes": body.get("round_scoped_votes"),
        "height_view_round": body.get("height_view_round"),
        "consensus_stall": body.get("consensus_stall"),
        "last_catchup_conflict": body.get("last_catchup_conflict"),
        "liveness": {
            "stalled": live.get("stalled"),
            "stalled_for_ms": live.get("stalled_for_ms"),
            "has_work": live.get("has_work"),
            "mempool_len": live.get("mempool_len"),
            "view_round": live.get("view_round"),
            "alert_after_ms": live.get("alert_after_ms"),
        },
        "bonded": bonded,
        "jailed": jailed,
        "disk": disk,
    }


def check_cfg(
    name: str, body: dict[str, Any], exp: dict[str, Any]
) -> tuple[int, list[str], str | None]:
    """Per-seat config drift check. Returns (severity, messages, qc_cfg_digest)."""
    sev, msgs = 0, []
    if body.get("network_id") != 2300:
        return 0, [], None

    def bad(field: str, got: Any, want: Any) -> None:
        nonlocal sev
        sev = 2
        msgs.append(f"CRITICAL cfg {name}: {field}={got} expected {want}")

    if exp["qc_header_version"] and body.get("qc_header_version") != exp["qc_header_version"]:
        bad("qc_header_version", body.get("qc_header_version"), exp["qc_header_version"])
    if exp["qc_start_height"] and body.get("qc_start_height") != exp["qc_start_height"]:
        bad("qc_start_height", body.get("qc_start_height"), exp["qc_start_height"])
    if exp["vote_rounds_from"] and body.get("vote_rounds_from_height") != exp["vote_rounds_from"]:
        bad("vote_rounds_from_height", body.get("vote_rounds_from_height"), exp["vote_rounds_from"])
    want_pause = exp.get("vote_rounds_pause")
    if want_pause is not None and "vote_rounds_pause_from" in body and body.get("vote_rounds_pause_from") is not None:
        got = (int(body.get("vote_rounds_pause_from") or 0), int(body.get("vote_rounds_resume_at") or 0))
        if got != tuple(want_pause):
            bad("vote_rounds_pause", f"{got[0]}..{got[1]}", f"{want_pause[0]}..{want_pause[1]}")
    lcs = body.get("lc_start_height")
    if exp["lc_start_height"] and lcs is not None and lcs != exp["lc_start_height"]:
        bad("lc.start_height", lcs, exp["lc_start_height"])

    b = body.get("bridge_cfg")
    digest = None
    if b is None:
        msgs.append(f"INFO cfg {name}: bridge_cfg not in status (pre-upgrade binary) — runtime QC fields checked only")
    else:
        digest = b.get("qc_cfg_digest")
        if not b.get("present"):
            bad("bridge.json", "missing", "present")
        elif not b.get("parsed"):
            bad("bridge.json", "unparseable", "valid JSON")
        if b.get("drift"):
            bad(
                "bridge_cfg.drift",
                b.get("drift_reasons") or True,
                "false (file changed since start or differs from what the node runs; restart to apply)",
            )
        # LC / Falcon-QC attestations sign over this id: it must match the node
        # itself, not just agree across seats (all seats could share a wrong one).
        if b.get("network_id") != body.get("network_id"):
            bad("bridge.json network_id", b.get("network_id"), body.get("network_id"))
        if exp["qc_header_version"] and b.get("qc_header_version") != exp["qc_header_version"]:
            bad("bridge.json qc_header_version", b.get("qc_header_version"), exp["qc_header_version"])
        if exp["qc_start_height"] and b.get("qc_start_height") != exp["qc_start_height"]:
            bad("bridge.json qc_start_height", b.get("qc_start_height"), exp["qc_start_height"])
        if exp["qc_cfg_digest"] and digest != exp["qc_cfg_digest"]:
            bad("bridge.json qc_cfg_digest", (digest or "")[:16], exp["qc_cfg_digest"][:16])
    return sev, msgs, digest


def parse_pause(s: str | None) -> tuple[int, int] | None:
    """`FROM..RESUME` → (from, resume); `off`/`0` → (0, 0); None/'' → None (unchecked)."""
    if s is None:
        return None
    t = "".join(s.split())
    if not t:
        return None
    if t.lower() in ("off", "none", "0"):
        return (0, 0)
    a, _, b = t.partition("..")
    return (int(a), int(b))


def rounds_active(body: dict[str, Any], height: int) -> bool | None:
    """Whether `body`'s config makes `height` round-scoped (None if unknown)."""
    vrf = body.get("vote_rounds_from_height")
    if vrf is None:
        return None
    vrf = int(vrf)
    pf = int(body.get("vote_rounds_pause_from") or 0)
    ra = int(body.get("vote_rounds_resume_at") or 0)
    paused = pf > 0 and ra > pf and pf <= height < ra
    return vrf > 0 and height >= vrf and not paused


DEFAULT_EXPECT = {
    "vote_rounds_pause": parse_pause(EXPECT_VOTE_ROUNDS_PAUSE),
    "qc_header_version": EXPECT_QC_HEADER_VERSION,
    "qc_start_height": EXPECT_QC_START_HEIGHT,
    "vote_rounds_from": EXPECT_VOTE_ROUNDS_FROM,
    "lc_start_height": EXPECT_LC_START_HEIGHT,
    "qc_cfg_digest": EXPECT_QC_CFG_DIGEST,
}


def evaluate(
    hub: dict[str, Any],
    peers: list[tuple[str, dict[str, Any] | None]],
    expect: dict[str, Any] | None = None,
    disks: list[tuple[str, list[dict[str, Any]] | None]] | None = None,
) -> tuple[int, list[str]]:
    """Return (severity 0/1/2, messages)."""
    msgs: list[str] = []
    sev = 0
    exp = dict(DEFAULT_EXPECT)
    if expect:
        exp.update(expect)

    # Config drift across hub + every reachable seat.
    digests: dict[str, str] = {}
    info_once = False
    for name, body in [("hub", hub)] + [(a, b) for a, b in peers if b is not None and not b.get("_timeout")]:
        csev, cmsgs, dg = check_cfg(f"{name}({body.get('id')})", body, exp)
        sev = max(sev, csev)
        for m in cmsgs:
            if m.startswith("INFO"):
                if not info_once:
                    msgs.append(m)
                    info_once = True
            else:
                msgs.append(m)
        if dg:
            digests[f"{name}({body.get('id')})"] = dg
    if len(set(digests.values())) > 1:
        sev = max(sev, 2)
        groups: dict[str, list[str]] = {}
        for n, d in digests.items():
            groups.setdefault(d[:16], []).append(n)
        msgs.append(f"CRITICAL bridge.json qc_cfg_digest differs across seats: {groups}")

    tip = int(hub.get("tip_height") or 0)
    mempool = int(hub.get("mempool") or 0)
    has_tx = bool(hub.get("has_applicable_tx"))
    live = hub.get("liveness") or {}
    stalled = bool(live.get("stalled"))
    stalled_for = int(live.get("stalled_for_ms") or 0)
    has_work = bool(live.get("has_work")) or has_tx or mempool > 0
    lottery = hub.get("lottery_order") or []
    online = hub.get("online_seats") or []
    bonded = hub.get("bonded") or []
    stall = hub.get("consensus_stall")

    if len(lottery) < COMMITTEE_NEED:
        sev = max(sev, 2)
        msgs.append(f"CRITICAL committee lottery_order size {len(lottery)} < {COMMITTEE_NEED}")
    if len(bonded) < COMMITTEE_NEED:
        sev = max(sev, 2)
        msgs.append(f"CRITICAL bonded live {len(bonded)} < {COMMITTEE_NEED}: {[b['id'] for b in bonded]}")
    if len(online) < COMMITTEE_NEED:
        sev = max(sev, 1)
        msgs.append(f"WARN online_seats {len(online)} < {COMMITTEE_NEED}: {online}")

    # Idle vs stalled: only alert when work exists.
    if has_work and (stalled or stalled_for >= STALL_CRIT_MS):
        sev = max(sev, 2)
        msgs.append(
            f"CRITICAL tip stalled with work tip={tip} stalled_for_ms={stalled_for} "
            f"mempool={mempool} has_applicable_tx={has_tx}"
        )
    elif has_work and stalled_for >= STALL_CRIT_MS // 2:
        sev = max(sev, 1)
        msgs.append(f"WARN tip slow with work stalled_for_ms={stalled_for} mempool={mempool}")
    elif not has_work and tip > 0:
        msgs.append(f"OK idle (no work) tip={tip} — not an incident")

    if stall is not None:
        sev = max(sev, 1)
        msgs.append(f"WARN consensus_stall={stall}")

    # Phase 2 sanity on 2300 (activation + v2.9.48 pause window).
    if hub.get("network_id") == 2300:
        vrf = hub.get("vote_rounds_from_height")
        want = rounds_active(hub, tip + 1)
        rs = hub.get("round_scoped_votes")
        if want and not rs:
            sev = max(sev, 2)
            msgs.append(f"CRITICAL tip {tip} >= activation {vrf} but round_scoped_votes=false")
        pause = exp.get("vote_rounds_pause") or (0, 0)
        if pause[1] > pause[0] > 0 and pause[0] <= tip + 1 < pause[1]:
            left = pause[1] - (tip + 1)
            msgs.append(f"INFO Phase 2 paused by window {pause[0]}..{pause[1]}: resumes in {left} blocks")
            if left <= RESUME_MARGIN:
                for name, body in [("hub", hub)] + [(a, b) for a, b in peers if b is not None and not b.get("_timeout")]:
                    if rounds_active(body, pause[1]) is not True:
                        sev = max(sev, 2)
                        msgs.append(
                            f"CRITICAL {name}({body.get('id')}) not round-scoped at resume {pause[1]} "
                            f"(vr={body.get('vote_rounds_from_height')} pause={body.get('vote_rounds_pause_from')}.."
                            f"{body.get('vote_rounds_resume_at')}) and tip is {left} blocks away — "
                            "upgrade / drop the legacy override, or bump the resume height"
                        )

    # Lag: measure every seat (hub included) against the highest reachable tip,
    # so a stale hub with peers ahead is caught too.
    seats: list[tuple[str, dict[str, Any]]] = [(f"hub {hub.get('id')}", hub)]
    for addr, body in peers:
        if body is None:
            sev = max(sev, 1)
            msgs.append(f"WARN peer unreachable {addr}")
            continue
        if body.get("_timeout"):
            sev = max(sev, 1)
            msgs.append(f"WARN peer status timeout {addr} ({body.get('_timeout')})")
            continue
        seats.append((f"{addr} {body.get('id')}", body))
    ref = max(int(b.get("tip_height") or 0) for _, b in seats)
    ref_name = next(n for n, b in seats if int(b.get("tip_height") or 0) == ref)
    for name, body in seats:
        pt = int(body.get("tip_height") or 0)
        lag = ref - pt
        if lag >= LAG_CRIT:
            sev = max(sev, 2)
            msgs.append(f"CRITICAL lag {name} tip={pt} best={ref} ({ref_name}) lag={lag}")
        elif lag >= LAG_WARN:
            sev = max(sev, 1)
            msgs.append(f"WARN lag {name} tip={pt} best={ref} ({ref_name}) lag={lag}")
    # Divergence: any two seats at the same height with different tip hashes.
    by_height: dict[int, dict[str, list[str]]] = {}
    for name, body in seats:
        h = body.get("tip_hash_prefix")
        if not h:
            continue
        by_height.setdefault(int(body.get("tip_height") or 0), {}).setdefault(h, []).append(name)
    for h, groups in sorted(by_height.items()):
        if len(groups) > 1:
            sev = max(sev, 2)
            msgs.append(f"CRITICAL tip hash diverge at h={h}: {groups}")

    # Disk free per host (df over SSH) and per seat (status `disk`, >= 2.9.53).
    dsev, dmsgs = check_disk(disks or [], seats)
    sev = max(sev, dsev)
    msgs.extend(m for m in dmsgs if not m.startswith("OK"))
    disk_ok = [m for m in dmsgs if m.startswith("OK")]

    if sev == 0:
        msgs.append(
            f"OK tip={tip} mp={mempool} committee={lottery} jailed={[j['id'] for j in hub.get('jailed') or []]}"
        )
    msgs.extend(disk_ok)
    return sev, msgs


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--addr", default=DEFAULT_ADDR, help="hub / public proxy status addr")
    ap.add_argument("--ctl", default=DEFAULT_CTL)
    ap.add_argument("--peers", default=DEFAULT_PEERS, help="comma-separated extra addrs (optional)")
    ap.add_argument("--interval", type=int, default=60, help="seconds between checks")
    ap.add_argument("--once", action="store_true")
    ap.add_argument("--json", action="store_true", help="emit one JSON object per check")
    ap.add_argument("--expect-qc-header-version", type=int, default=EXPECT_QC_HEADER_VERSION)
    ap.add_argument("--expect-qc-start-height", type=int, default=EXPECT_QC_START_HEIGHT)
    ap.add_argument("--expect-vote-rounds-from", type=int, default=EXPECT_VOTE_ROUNDS_FROM)
    ap.add_argument("--expect-lc-start-height", type=int, default=EXPECT_LC_START_HEIGHT)
    ap.add_argument("--expect-vote-rounds-pause", default=EXPECT_VOTE_ROUNDS_PAUSE,
                    help="FROM..RESUME Phase 2 pause window, or 'off' (default %(default)s)")
    ap.add_argument("--peer-timeout", type=float, default=PEER_TIMEOUT,
                    help="seconds per status call (default %(default)s)")
    ap.add_argument("--expect-qc-cfg-digest", default=EXPECT_QC_CFG_DIGEST,
                    help="optional canonical bridge.json digest every seat must report")
    ap.add_argument("--disk-hosts", default=DEFAULT_DISK_HOSTS,
                    help="comma-separated SSH targets for read-only `df` ('' or 'off' = skip; default %(default)s)")
    ap.add_argument("--ssh-via", default=SSH_VIA,
                    help="run the remote ctl via `$PL_SOAK_SSH_CMD HOST` (read-only status), e.g. falcon@host1")
    args = ap.parse_args()
    if args.ssh_via:
        globals()["SSH_VIA"] = args.ssh_via
    expect = {
        "qc_header_version": args.expect_qc_header_version,
        "qc_start_height": args.expect_qc_start_height,
        "vote_rounds_from": args.expect_vote_rounds_from,
        "lc_start_height": args.expect_lc_start_height,
        "qc_cfg_digest": args.expect_qc_cfg_digest,
        "vote_rounds_pause": parse_pause(args.expect_vote_rounds_pause),
    }

    if not args.ssh_via and not os.path.isfile(args.ctl):
        print(f"missing ctl binary: {args.ctl}", file=sys.stderr)
        return 3

    worst = 0
    while True:
        ts = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        try:
            hub_raw = ctl_status(args.ctl, args.addr, timeout=args.peer_timeout, retries=PEER_RETRIES)
            hub = slim(hub_raw)
        except Exception as e:
            print(f"[{ts}] TRANSPORT hub {args.addr}: {e}", file=sys.stderr)
            if args.once:
                return 3
            time.sleep(args.interval)
            continue

        peer_results: list[tuple[str, dict[str, Any] | None]] = []
        for addr in [a.strip() for a in args.peers.split(",") if a.strip()]:
            if addr == args.addr:
                continue
            try:
                peer_results.append(
                    (addr, slim(ctl_status(args.ctl, addr, timeout=args.peer_timeout, retries=PEER_RETRIES)))
                )
            except StatusTimeout as e:
                peer_results.append((addr, {"_timeout": str(e)}))
            except Exception:
                peer_results.append((addr, None))

        disk_hosts = [h.strip() for h in (args.disk_hosts or "").split(",") if h.strip()]
        if [h.lower() for h in disk_hosts] in (["off"], ["none"]):
            disk_hosts = []
        disks = [(h, host_disk(h)) for h in disk_hosts]

        sev, msgs = evaluate(hub, peer_results, expect, disks)
        worst = max(worst, sev)
        label = {0: "OK", 1: "WARN", 2: "CRIT"}[sev]

        if args.json:
            print(
                json.dumps(
                    {
                        "ts": ts,
                        "severity": label,
                        "hub": hub,
                        "peers": [
                            {
                                "addr": a,
                                "id": (b or {}).get("id"),
                                "tip": (b or {}).get("tip_height"),
                                "ok": b is not None and not b.get("_timeout"),
                                "qc_header_version": (b or {}).get("qc_header_version"),
                                "qc_start_height": (b or {}).get("qc_start_height"),
                                "vote_rounds_from_height": (b or {}).get("vote_rounds_from_height"),
                                "qc_cfg_digest": ((b or {}).get("bridge_cfg") or {}).get("qc_cfg_digest"),
                            }
                            for a, b in peer_results
                        ],
                        "disk": [
                            {"host": h, "ok": rows is not None, "mounts": rows or []}
                            for h, rows in disks
                        ],
                        "messages": msgs,
                    },
                    separators=(",", ":"),
                )
            )
        else:
            print(f"[{ts}] {label} hub={args.addr} tip={hub.get('tip_height')} mp={hub.get('mempool')}")
            print(f"  committee={hub.get('lottery_order')} online={hub.get('online_seats')}")
            print(f"  bonded={[b['id'] for b in hub.get('bonded') or []]} jailed={[j['id'] for j in hub.get('jailed') or []]}")
            print(
                f"  rounds_from={hub.get('vote_rounds_from_height')} "
                f"pause={hub.get('vote_rounds_pause_from')}..{hub.get('vote_rounds_resume_at')} "
                f"round_scoped={hub.get('round_scoped_votes')} view_round={hub.get('height_view_round')}"
            )
            print(f"  liveness={hub.get('liveness')} stall={hub.get('consensus_stall')}")
            print(
                f"  qc_header_version={hub.get('qc_header_version')} qc_start_height={hub.get('qc_start_height')} "
                f"lc_start={hub.get('lc_start_height')} bridge_cfg={hub.get('bridge_cfg')}"
            )
            for m in msgs:
                print(f"  · {m}")
            for addr, b in peer_results:
                if b is None:
                    print(f"  · peer {addr}: unreachable")
                elif b.get("_timeout"):
                    print(f"  · peer {addr}: timeout ({b.get('_timeout')})")
                else:
                    print(
                        f"  · peer {addr}: tip={b.get('tip_height')} "
                        f"behind_hub={int(hub.get('tip_height') or 0) - int(b.get('tip_height') or 0)} "
                        f"id={b.get('id')} qcv={b.get('qc_header_version')} "
                        f"qcs={b.get('qc_start_height')} vr={b.get('vote_rounds_from_height')} "
                        f"digest={((b.get('bridge_cfg') or {}).get('qc_cfg_digest') or '-')[:12]}"
                    )

        if args.once:
            return sev
        time.sleep(args.interval)


if __name__ == "__main__":
    sys.exit(main())
