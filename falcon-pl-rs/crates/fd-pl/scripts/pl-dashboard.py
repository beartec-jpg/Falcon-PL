#!/usr/bin/env python3
"""Falcon PL mesh dashboard — network, lottery, pong/pin, validators, mem, packs.

  python3 crates/fd-pl/scripts/pl-dashboard.py
  # → http://0.0.0.0:8787/

Polls falcon-pl-ctl status on every configured node and process RSS via ps/ssh.
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

ROOT = Path(os.environ.get("PL_ROOT", Path.home() / "falcon-pl-private-testnet-v22"))
BIN = Path(
    os.environ.get(
        "PL_BIN",
        Path.home() / "falcon-pl" / "falcon-pl-rs" / "target" / "release",
    )
)
CTL = BIN / "falcon-pl-ctl"
HOST = os.environ.get("PL_DASH_HOST", "0.0.0.0")
PORT = int(os.environ.get("PL_DASH_PORT", "8787"))
POLL_SEC = float(os.environ.get("PL_DASH_POLL", "2.0"))

# Default private testnet v2.2 mesh (overridden by run/layout.txt + joiners.txt)
# `addr` is Tailscale host:port used for direct RPC from any fleet host.
DEFAULT_NODES = [
    {
        "id": "v1",
        "rpc_host": "100.64.0.1",
        "port": 19101,
        "role": "validator",
        "hist": True,
        "addr": "100.64.0.1:19101",
        "where": "f1",
    },
    {
        "id": "v2",
        "rpc_host": "100.64.0.1",
        "port": 19102,
        "role": "validator",
        "hist": False,
        "addr": "100.64.0.1:19102",
        "where": "f1",
    },
    {
        "id": "v3",
        "rpc_host": "100.64.0.3",
        "port": 19111,
        "role": "validator",
        "hist": False,
        "addr": "100.64.0.3:19111",
        "where": "f2",
    },
    {
        "id": "v4",
        "rpc_host": "100.64.0.5",
        "port": 19121,
        "role": "validator",
        "hist": False,
        "addr": "100.64.0.5:19121",
        "where": "f3",
    },
]

# This machine's Tailscale IPv4s (for local-vs-remote mem probes).
_LOCAL_IPS: set[str] | None = None


def local_ips() -> set[str]:
    global _LOCAL_IPS
    if _LOCAL_IPS is not None:
        return _LOCAL_IPS
    ips = {"127.0.0.1", "0.0.0.0", "localhost"}
    try:
        out = subprocess.check_output(
            ["tailscale", "ip", "-4"], text=True, timeout=3, stderr=subprocess.DEVNULL
        )
        for line in out.splitlines():
            line = line.strip()
            if line:
                ips.add(line)
    except Exception:
        pass
    try:
        out = subprocess.check_output(["hostname", "-I"], text=True, timeout=3)
        for tok in out.split():
            ips.add(tok)
    except Exception:
        pass
    _LOCAL_IPS = ips
    return ips


_lock = threading.Lock()
_cache: dict[str, Any] = {"ts": 0.0, "mesh": None, "error": None}


def load_nodes() -> list[dict]:
    nodes: list[dict] = []
    layout = ROOT / "run" / "layout.txt"
    if layout.is_file():
        for line in layout.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            # id host_or_ip port hist where
            if len(parts) >= 3:
                nid, host, port = parts[0], parts[1], int(parts[2])
                where = parts[4] if len(parts) >= 5 else "remote"
                nodes.append(
                    {
                        "id": nid,
                        "rpc_host": host,
                        "port": port,
                        "role": "validator",
                        "hist": parts[3] == "1" if len(parts) >= 4 else False,
                        "addr": f"{host}:{port}",
                        "where": where,
                    }
                )
    joiners = ROOT / "run" / "joiners.txt"
    if joiners.is_file():
        for line in joiners.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) >= 3:
                nid, host, port = parts[0], parts[1], int(parts[2])
                if not any(n["id"] == nid for n in nodes):
                    nodes.append(
                        {
                            "id": nid,
                            "rpc_host": host,
                            "port": port,
                            "role": "joiner",
                            "hist": False,
                            "addr": f"{host}:{port}",
                            "where": "join",
                        }
                    )
    return nodes or [dict(n) for n in DEFAULT_NODES]


# Full status dumps every account (1024+ wallets) → multi‑MB /api/mesh and browser hang.
# Keep only fields the dash UI actually renders.
_STATUS_DROP = frozenset(
    {
        "accounts",
        "claimable",
        "amm_lp_weights",
        "lend_lp_weights",
        "watcher_slots",
        "watcher_work",
        "watcher_weights_preview",
        "epoch_check_votes",
    }
)


def slim_status(st: dict[str, Any] | None) -> dict[str, Any] | None:
    """Strip bulk fields so /api/mesh stays small with large load-wallet genesis."""
    if not st or st.get("_error"):
        return st
    out = {k: v for k, v in st.items() if k not in _STATUS_DROP}
    # Keep a tiny sample for faucet health if present
    accts = st.get("accounts")
    if isinstance(accts, dict):
        sample = {}
        for k in ("faucet", "alice", "w00"):
            if k in accts:
                sample[k] = accts[k]
        out["accounts_sample"] = sample
        out["accounts_n"] = len(accts)
    return out


def rpc_status(addr: str) -> dict[str, Any] | None:
    """Direct RPC over Tailscale/mesh — no SSH hop (works from any fleet host)."""
    if not CTL.is_file():
        return {"_error": f"missing ctl binary {CTL}", "up": False}
    cmd = [str(CTL), "--addr", addr, "status"]
    try:
        out = subprocess.check_output(cmd, text=True, timeout=20, stderr=subprocess.DEVNULL)
        i = out.find("{")
        if i < 0:
            return {"_error": "no json in status", "up": False}
        msg = json.JSONDecoder().raw_decode(out[i:])[0]
        return slim_status(msg.get("body", msg))
    except Exception as e:
        return {"_error": str(e), "up": False}


def proc_mem(rpc_host: str, node_id: str) -> dict[str, Any]:
    """RSS/VSZ in MiB for falcon-pl-node --id <node_id>."""
    awk = (
        f"ps -eo pid,rss,vsz,args | awk '/falcon-pl-node/ && /--id {node_id} / "
        f"&& !/awk/ {{printf \"%s %s %s\\n\", $1, $2, $3; exit}}'"
    )
    try:
        if rpc_host in local_ips() or rpc_host in ("local", "host1", ""):
            out = subprocess.check_output(awk, shell=True, text=True, timeout=5)
        else:
            # Remote RSS is best-effort. Prefer root@ first (cloud TS boxes are root-only
            # and falcon@ fails slowly on tailnet policy). Short timeouts so dash never hangs.
            out = ""
            for user in ("root", "falcon"):
                try:
                    out = subprocess.check_output(
                        [
                            "ssh",
                            "-o",
                            "BatchMode=yes",
                            "-o",
                            "ConnectTimeout=2",
                            "-o",
                            "StrictHostKeyChecking=accept-new",
                            f"{user}@{rpc_host}",
                            awk,
                        ],
                        text=True,
                        timeout=3,
                        stderr=subprocess.DEVNULL,
                    )
                    if out.strip():
                        break
                except Exception:
                    continue
        parts = out.strip().split()
        if len(parts) >= 3:
            pid, rss_kb, vsz_kb = int(parts[0]), int(parts[1]), int(parts[2])
            return {
                "pid": pid,
                "rss_mib": round(rss_kb / 1024, 2),
                "vsz_mib": round(vsz_kb / 1024, 2),
            }
    except Exception:
        pass
    return {"pid": None, "rss_mib": None, "vsz_mib": None}


def poll_mesh() -> dict[str, Any]:
    nodes_cfg = load_nodes()
    results = []
    for n in nodes_cfg:
        addr = n.get("addr") or f"{n.get('rpc_host', '127.0.0.1')}:{n['port']}"
        st = rpc_status(addr)
        mem = proc_mem(n.get("rpc_host") or "127.0.0.1", n["id"])
        up = bool(st and not st.get("_error") and "tip_height" in st)
        entry = {
            "cfg": n,
            "up": up,
            "mem": mem,
            "status": st if up else {"error": (st or {}).get("_error", "down")},
        }
        results.append(entry)

    tips = [e["status"]["tip_height"] for e in results if e["up"]]
    hashes = {
        (e["status"].get("tip_hash") or "")[:16] for e in results if e["up"]
    }
    span = (max(tips) - min(tips)) if tips else 0
    sync_ok = bool(tips) and span <= 1 and len(hashes) <= 1

    # Merge validator views from primary (first up node) for registry table
    primary = next((e for e in results if e["up"]), None)
    registry = []
    if primary:
        registry = primary["status"].get("validators") or []

    # Lottery readiness summary across mesh
    lottery = {}
    if primary:
        st = primary["status"]
        vals = st.get("validators") or []
        ready = [v for v in vals if v.get("lottery_ready") or v.get("pong_fresh")]
        stale = [v for v in vals if not v.get("pong_fresh")]
        lottery = {
            "winner": st.get("lottery_winner_next"),
            "standby": st.get("lottery_standby_next"),
            "ranked": st.get("lottery_ranked_next") or [],
            "skips": st.get("skips_next") or [],
            "ready_ids": [v.get("id") for v in ready],
            "stale_ids": [v.get("id") for v in stale],
            "ready_n": len(ready),
            "stale_n": len(stale),
            "pong_window_ms": st.get("pong_window_ms"),
            "i_may_pack": st.get("i_may_pack"),
            "height_open_elapsed_ms": st.get("height_open_elapsed_ms"),
            "progress_timeout_ms": st.get("progress_timeout_ms"),
        }

    # Aggregate pack stats (prefer a node that exposes last_pack_* fields)
    pack = {}
    pack_src = primary
    for e in results:
        if e["up"] and e["status"].get("last_pack_height") is not None:
            pack_src = e
            break
    if pack_src and pack_src.get("up"):
        st = pack_src["status"]
        m = st.get("metrics") or {}
        # Prefer richest metrics (often full-history v1)
        met_src = primary["status"] if primary else st
        mm = met_src.get("metrics") or m
        pack = {
            "last_pack_height": st.get("last_pack_height"),
            "last_pack_packer": st.get("last_pack_packer"),
            "last_pack_txs": st.get("last_pack_txs"),
            "last_pack_fee_total": st.get("last_pack_fee_total"),
            "txs_sealed_total": mm.get("txs_sealed"),
            "ledgers_sealed": mm.get("ledgers_sealed"),
            "fees_sealed_total": mm.get("fees_sealed_total"),
            "fees_burned_total": mm.get("fees_burned_total"),
            "fees_reward_pool_total": mm.get("fees_reward_pool_total"),
            "validator_pack_counts": {
                v.get("id"): v.get("pack_count")
                for v in (met_src.get("validators") or st.get("validators") or [])
            },
            "epoch_pack_txs": met_src.get("epoch_pack_txs") or st.get("epoch_pack_txs") or {},
        }

    mem_total = sum(
        (e["mem"].get("rss_mib") or 0) for e in results if e["mem"].get("rss_mib")
    )

    return {
        "ts": time.time(),
        "ts_iso": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "product": (primary or {}).get("status", {}).get("product", "Falcon PL"),
        "network": (primary or {}).get("status", {}).get("network"),
        "network_id": (primary or {}).get("status", {}).get("network_id"),
        "consensus": (primary or {}).get("status", {}).get("consensus"),
        "sync": {
            "ok": sync_ok,
            "span": span,
            "tips": tips,
            "hashes": list(hashes),
            "nodes_up": sum(1 for e in results if e["up"]),
            "nodes_total": len(results),
        },
        "lottery": lottery,
        "pack": pack,
        "registry": registry,
        "mem_total_rss_mib": round(mem_total, 2),
        "nodes": results,
        "primary": primary["cfg"]["id"] if primary else None,
    }


def poller():
    while True:
        try:
            mesh = poll_mesh()
            with _lock:
                _cache["mesh"] = mesh
                _cache["ts"] = time.time()
                _cache["error"] = None
        except Exception as e:
            with _lock:
                _cache["error"] = str(e)
        time.sleep(POLL_SEC)


HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>Falcon PL · Mesh Dashboard</title>
<style>
:root {
  --bg: #0b0f14;
  --panel: #121820;
  --panel2: #18202b;
  --border: #243041;
  --text: #e7eef7;
  --muted: #8b9bb0;
  --ok: #3dd68c;
  --warn: #f5a524;
  --bad: #f31260;
  --accent: #6ea8fe;
  --accent2: #b197fc;
  --mono: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  --sans: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif;
}
* { box-sizing: border-box; }
body {
  margin: 0; background: var(--bg); color: var(--text);
  font: 14px/1.45 var(--sans);
}
header {
  display: flex; flex-wrap: wrap; gap: 12px; align-items: center;
  justify-content: space-between;
  padding: 14px 18px; border-bottom: 1px solid var(--border);
  background: linear-gradient(180deg, #101820, #0b0f14);
  position: sticky; top: 0; z-index: 10;
}
header h1 { margin: 0; font-size: 16px; letter-spacing: .02em; }
header h1 span { color: var(--accent); }
.badge {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 4px 10px; border-radius: 999px; font-size: 12px; font-weight: 600;
  border: 1px solid var(--border); background: var(--panel);
}
.badge.ok { color: var(--ok); border-color: #1f4d38; }
.badge.bad { color: var(--bad); border-color: #5c1a30; }
.badge.warn { color: var(--warn); border-color: #5c4312; }
.meta { color: var(--muted); font-size: 12px; font-family: var(--mono); }
main { padding: 16px; display: grid; gap: 14px; }
.grid { display: grid; gap: 12px; }
.grid.kpis { grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); }
.grid.two { grid-template-columns: repeat(auto-fit, minmax(340px, 1fr)); }
.card {
  background: var(--panel); border: 1px solid var(--border);
  border-radius: 12px; padding: 12px 14px;
}
.card h2 {
  margin: 0 0 10px; font-size: 12px; text-transform: uppercase;
  letter-spacing: .08em; color: var(--muted); font-weight: 700;
}
.kpi .v { font-size: 22px; font-weight: 700; font-family: var(--mono); }
.kpi .l { font-size: 11px; color: var(--muted); margin-top: 2px; }
table { width: 100%; border-collapse: collapse; font-size: 12.5px; }
th, td { text-align: left; padding: 7px 8px; border-bottom: 1px solid var(--border); vertical-align: top; }
th { color: var(--muted); font-weight: 600; font-size: 11px; text-transform: uppercase; letter-spacing: .04em; }
tr:hover td { background: rgba(255,255,255,.02); }
.mono { font-family: var(--mono); }
.dot { width: 8px; height: 8px; border-radius: 50%; display: inline-block; margin-right: 6px; }
.dot.on { background: var(--ok); box-shadow: 0 0 8px var(--ok); }
.dot.off { background: var(--bad); }
.dot.warn { background: var(--warn); }
.pill {
  display: inline-block; padding: 2px 7px; border-radius: 6px;
  background: var(--panel2); border: 1px solid var(--border); font-size: 11px;
}
.pill.ok { color: var(--ok); }
.pill.bad { color: var(--bad); }
.pill.acc { color: var(--accent); }
.pill.acc2 { color: var(--accent2); }
.hash { max-width: 120px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; display: inline-block; }
.bar {
  height: 6px; background: #1a2430; border-radius: 4px; overflow: hidden; min-width: 60px;
}
.bar > i { display: block; height: 100%; background: linear-gradient(90deg, var(--accent), var(--accent2)); }
.row { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; }
.muted { color: var(--muted); }
.small { font-size: 11px; }
footer { padding: 10px 18px 24px; color: var(--muted); font-size: 11px; }
</style>
</head>
<body>
<header>
  <div>
    <h1><span>Falcon PL</span> · Mesh Dashboard</h1>
    <div class="meta" id="hdr-meta">loading…</div>
  </div>
  <div class="row">
    <span class="badge" id="sync-badge">…</span>
    <span class="badge" id="nodes-badge">…</span>
    <span class="meta" id="clock"></span>
  </div>
</header>
<main>
  <section class="grid kpis" id="kpis"></section>
  <section class="grid two">
    <div class="card">
      <h2>Lottery · readiness pin/pong</h2>
      <div id="lottery"></div>
    </div>
    <div class="card">
      <h2>Last pack · totals</h2>
      <div id="pack"></div>
    </div>
  </section>
  <section class="card">
    <h2>Nodes</h2>
    <div style="overflow-x:auto"><table id="nodes-tbl">
      <thead><tr>
        <th>Node</th><th>Host</th><th>Up</th><th>Tip</th><th>Lag</th><th>Hash</th>
        <th>Winner</th><th>Mempool</th><th>Chain</th><th>Last pack</th>
        <th>RSS</th><th>Sealed txs</th><th>May pack</th>
      </tr></thead>
      <tbody></tbody>
    </table></div>
  </section>
  <section class="card">
    <h2>Validators · bond · pong pin · packs</h2>
    <div style="overflow-x:auto"><table id="vals-tbl">
      <thead><tr>
        <th>ID</th><th>Bond</th><th>Ready</th><th>Pong</th><th>Pong age</th>
        <th>Adv tip</th><th>Packs (total)</th><th>Epoch txs</th>
        <th>Perf</th><th>Jailed</th><th>Slash</th><th>Unbond</th>
      </tr></thead>
      <tbody></tbody>
    </table></div>
  </section>
  <section class="grid two">
    <div class="card">
      <h2>Fee market · intake metrics</h2>
      <div id="metrics"></div>
    </div>
    <div class="card">
      <h2>Epoch · treasury · defi · bridge</h2>
      <div id="econ"></div>
    </div>
  </section>
  <section class="card">
    <h2>Pong pin detail (per node view of registry)</h2>
    <div style="overflow-x:auto"><table id="pong-tbl">
      <thead><tr>
        <th>Viewer</th><th>Ready set</th><th>Stale set</th><th>Window ms</th><th>Open height ms</th>
      </tr></thead>
      <tbody></tbody>
    </table></div>
  </section>
</main>
<footer>Auto-refresh via <code>/api/mesh</code> · source: falcon-pl-ctl status + process RSS</footer>
<script>
const $ = (s) => document.querySelector(s);
function short(h, n=12) { return h ? String(h).slice(0,n) : '—'; }
function fmt(n) {
  if (n === null || n === undefined || Number.isNaN(n)) return '—';
  if (typeof n === 'number' && Math.abs(n) >= 1000) return n.toLocaleString();
  return String(n);
}
function age(ms) {
  if (ms === null || ms === undefined) return 'never';
  if (ms < 1000) return ms + 'ms';
  if (ms < 60000) return (ms/1000).toFixed(1) + 's';
  return (ms/60000).toFixed(1) + 'm';
}
function pill(cls, t) { return `<span class="pill ${cls||''}">${t}</span>`; }

async function tick() {
  let data;
  try {
    const r = await fetch('/api/mesh?_=' + Date.now());
    data = await r.json();
  } catch (e) {
    $('#hdr-meta').textContent = 'fetch error: ' + e;
    return;
  }
  if (data.error && !data.mesh) {
    $('#hdr-meta').textContent = data.error;
    return;
  }
  const m = data.mesh || data;
  $('#clock').textContent = m.ts_iso || '';
  $('#hdr-meta').textContent =
    `${m.network || ''} · id ${m.network_id ?? ''} · ${m.consensus || ''} · poll ${data.age_ms|0}ms old`;

  const sync = m.sync || {};
  const sb = $('#sync-badge');
  sb.className = 'badge ' + (sync.ok ? 'ok' : 'bad');
  sb.textContent = sync.ok ? 'SYNC OK' : `SYNC DRIFT span=${sync.span}`;
  $('#nodes-badge').textContent = `${sync.nodes_up||0}/${sync.nodes_total||0} up · RSS ${fmt(m.mem_total_rss_mib)} MiB`;

  const p = m.pack || {};
  const lot = m.lottery || {};
  const tip = (sync.tips && sync.tips.length) ? Math.max(...sync.tips) : '—';
  $('#kpis').innerHTML = [
    ['Tip', tip],
    ['Hash', short((sync.hashes||[])[0], 10)],
    ['Span', sync.span ?? '—'],
    ['Winner', lot.winner || '—'],
    ['Standby', lot.standby || '—'],
    ['Ready', `${lot.ready_n??'—'}/${(lot.ready_n||0)+(lot.stale_n||0)}`],
    ['Last pack txs', p.last_pack_txs ?? '—'],
    ['Txs sealed Σ', p.txs_sealed_total ?? '—'],
    ['Ledgers sealed', p.ledgers_sealed ?? '—'],
    ['Mem RSS', (m.mem_total_rss_mib ?? '—') + ' MiB'],
  ].map(([l,v]) => `<div class="card kpi"><div class="v">${v}</div><div class="l">${l}</div></div>`).join('');

  // Lottery panel
  const ranked = (lot.ranked || []).map((id,i) =>
    pill(i===0?'acc':(i===1?'acc2':''), `#${i+1} ${id}`)).join(' ');
  $('#lottery').innerHTML = `
    <div class="row" style="margin-bottom:8px">${ranked || pill('','no ready packers')}</div>
    <table>
      <tr><th>Winner</th><td class="mono">${lot.winner||'—'}</td>
          <th>Standby</th><td class="mono">${lot.standby||'—'}</td></tr>
      <tr><th>Skips</th><td class="mono" colspan="3">${JSON.stringify(lot.skips||[])}</td></tr>
      <tr><th>Ready</th><td class="mono" colspan="3">${(lot.ready_ids||[]).join(', ')||'—'}</td></tr>
      <tr><th>Stale / offline</th><td class="mono" colspan="3">${(lot.stale_ids||[]).map(x=>pill('bad',x)).join(' ')||'—'}</td></tr>
      <tr><th>Pong window</th><td class="mono">${fmt(lot.pong_window_ms)} ms</td>
          <th>Height open</th><td class="mono">${fmt(lot.height_open_elapsed_ms)} / ${fmt(lot.progress_timeout_ms)} ms</td></tr>
    </table>
    <p class="muted small" style="margin:8px 0 0">Lottery = bonded ∩ pong-fresh ∩ not skipped. Pin/pong = mesh-flooded <code>Pong</code>/Hello.</p>
  `;

  // Pack panel
  const vpc = p.validator_pack_counts || {};
  const bars = Object.entries(vpc).sort((a,b)=>b[1]-a[1]).map(([id,c]) => {
    const max = Math.max(...Object.values(vpc), 1);
    const pct = Math.round(100 * c / max);
    return `<tr><td class="mono">${id}</td><td class="mono">${fmt(c)}</td>
      <td><div class="bar"><i style="width:${pct}%"></i></div></td></tr>`;
  }).join('');
  $('#pack').innerHTML = `
    <table>
      <tr><th>Last pack h</th><td class="mono">${fmt(p.last_pack_height)}</td>
          <th>Packer</th><td class="mono">${p.last_pack_packer||'—'}</td></tr>
      <tr><th>Last pack #txs</th><td class="mono">${fmt(p.last_pack_txs)}</td>
          <th>Last fees</th><td class="mono">${fmt(p.last_pack_fee_total)}</td></tr>
      <tr><th>Σ txs sealed</th><td class="mono">${fmt(p.txs_sealed_total)}</td>
          <th>Σ ledgers</th><td class="mono">${fmt(p.ledgers_sealed)}</td></tr>
      <tr><th>Fees sealed</th><td class="mono">${fmt(p.fees_sealed_total)}</td>
          <th>Burned / reward</th><td class="mono">${fmt(p.fees_burned_total)} / ${fmt(p.fees_reward_pool_total)}</td></tr>
    </table>
    <h2 style="margin-top:12px">Pack counts by validator</h2>
    <table><thead><tr><th>ID</th><th>Packs</th><th></th></tr></thead><tbody>${bars}</tbody></table>
  `;

  // Nodes table
  const tb = $('#nodes-tbl tbody');
  tb.innerHTML = (m.nodes||[]).map(e => {
    const s = e.status || {};
    const cfg = e.cfg || {};
    const mem = e.mem || {};
    const met = s.metrics || {};
    const up = e.up;
    const lag = s.tip_lag;
    return `<tr>
      <td><span class="dot ${up?'on':'off'}"></span><b class="mono">${cfg.id}</b>
        ${cfg.role==='joiner'?pill('acc2','join'):''}</td>
      <td class="mono muted">${cfg.addr || ((cfg.rpc_host||cfg.host||'?')+':'+cfg.port)}</td>
      <td>${up?pill('ok','up'):pill('bad','down')}</td>
      <td class="mono">${up?fmt(s.tip_height):'—'}</td>
      <td class="mono">${up?fmt(lag):'—'}</td>
      <td class="mono"><span class="hash" title="${s.tip_hash||''}">${short(s.tip_hash)}</span></td>
      <td class="mono">${s.lottery_winner_next||'—'}</td>
      <td class="mono">${up?fmt(s.mempool):'—'}${s.mempool_full?' ⚠':''}</td>
      <td class="mono">${up?fmt(s.chain_len):'—'}</td>
      <td class="mono">${up?`${fmt(s.last_pack_txs)}tx @h${fmt(s.last_pack_height)}`:'—'}</td>
      <td class="mono">${mem.rss_mib!=null?mem.rss_mib+' MiB':'—'}</td>
      <td class="mono">${fmt(met.txs_sealed)}</td>
      <td>${s.i_may_pack?pill('ok','yes'):pill('','no')}</td>
    </tr>`;
  }).join('');

  // Validators
  const epochPack = p.epoch_pack_txs || {};
  const vtb = $('#vals-tbl tbody');
  vtb.innerHTML = (m.registry||[]).map(v => {
    const ready = v.lottery_ready ?? v.pong_fresh;
    const ageMs = v.pong_age_ms;
    return `<tr>
      <td class="mono"><b>${v.id}</b></td>
      <td class="mono">${fmt(v.bond)}</td>
      <td>${ready?pill('ok','ready'):pill('bad','out')}</td>
      <td>${v.pong_fresh?pill('ok','fresh'):pill('bad','stale')}</td>
      <td class="mono">${age(ageMs)}</td>
      <td class="mono">${v.last_advertised_tip ?? '—'}</td>
      <td class="mono"><b>${fmt(v.pack_count)}</b></td>
      <td class="mono">${fmt(epochPack[v.id] ?? 0)}</td>
      <td class="mono">${v.perf!=null?Number(v.perf).toFixed(3):'—'}</td>
      <td>${v.jailed?pill('bad','jail'):pill('ok','ok')}</td>
      <td class="mono">${fmt(v.slash_total)}</td>
      <td class="mono">${v.unbonding?fmt(v.unbonding):'—'}</td>
    </tr>`;
  }).join('');

  // Metrics from primary
  const prim = (m.nodes||[]).find(e => e.up);
  const met = (prim && prim.status && prim.status.metrics) || {};
  const fm = met.fee_market || {};
  $('#metrics').innerHTML = `
    <table>
      <tr><th>Tier</th><td>${fm.congestion_tier||'—'}</td>
          <th>util bps</th><td class="mono">${fmt(fm.mempool_util_bps)}</td></tr>
      <tr><th>min fee</th><td class="mono">${fmt(fm.suggested_min_fee)}</td>
          <th>mult</th><td class="mono">${fmt(fm.fee_multiplier)}×</td></tr>
      <tr><th>fee p50/p90</th><td class="mono">${fmt(met.fee_p50)} / ${fmt(met.fee_p90)}</td>
          <th>reject bps</th><td class="mono">${fmt(met.reject_rate_bps)}</td></tr>
      <tr><th>submit ok/dup</th><td class="mono">${fmt(met.submit_accepted)} / ${fmt(met.submit_duplicate)}</td>
          <th>rejects</th><td class="mono">fee ${fmt(met.submit_reject_fee_low)} · full ${fmt(met.submit_reject_mempool_full)} · crypto ${fmt(met.submit_reject_crypto)} · other ${fmt(met.submit_reject_other)}</td></tr>
    </table>`;

  const st = (prim && prim.status) || {};
  $('#econ').innerHTML = `
    <table>
      <tr><th>Epoch</th><td class="mono">${fmt(st.epoch)}</td>
          <th>Settled</th><td class="mono">${fmt(st.last_settled_epoch)}</td></tr>
      <tr><th>Treasury</th><td class="mono">${fmt(st.treasury)}</td>
          <th>Emission bps</th><td class="mono">${fmt(st.emission_bps)}</td></tr>
      <tr><th>AMM pools</th><td class="mono">${fmt(st.amm_pools)}</td>
          <th>Lend markets</th><td class="mono">${fmt(st.lend_markets)}</td></tr>
      <tr><th>State root</th><td class="mono" colspan="3"><span class="hash" title="${st.state_root||''}">${short(st.state_root, 20)}</span></td></tr>
      <tr><th>Rails</th><td colspan="3" class="mono small">${(st.rails||[]).map(r=>`${r.asset}:h${r.tip_height} mint ${r.total_minted}`).join(' · ')||'—'}</td></tr>
    </table>`;

  // Per-node pong view
  const ptb = $('#pong-tbl tbody');
  ptb.innerHTML = (m.nodes||[]).filter(e=>e.up).map(e => {
    const vals = e.status.validators || [];
    const ready = vals.filter(v=>v.pong_fresh).map(v=>v.id);
    const stale = vals.filter(v=>!v.pong_fresh).map(v=>v.id);
    return `<tr>
      <td class="mono"><b>${e.cfg.id}</b></td>
      <td class="mono">${ready.map(x=>pill('ok',x)).join(' ')||'—'}</td>
      <td class="mono">${stale.map(x=>pill('bad',x)).join(' ')||'—'}</td>
      <td class="mono">${fmt(e.status.pong_window_ms)}</td>
      <td class="mono">${fmt(e.status.height_open_elapsed_ms)}</td>
    </tr>`;
  }).join('');
}
tick();
setInterval(tick, 2000);
</script>
</body>
</html>
"""


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def _send(self, code: int, body: bytes, ctype: str):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = urlparse(self.path).path
        if path in ("/", "/index.html"):
            self._send(200, HTML.encode(), "text/html; charset=utf-8")
            return
        if path == "/api/mesh":
            with _lock:
                mesh = _cache.get("mesh")
                err = _cache.get("error")
                ts = _cache.get("ts") or 0
            age_ms = int((time.time() - ts) * 1000) if ts else None
            payload = {
                "mesh": mesh,
                "error": err,
                "age_ms": age_ms,
                "polled_at": ts,
            }
            if mesh is None and err is None:
                # first poll inline
                try:
                    mesh = poll_mesh()
                    with _lock:
                        _cache["mesh"] = mesh
                        _cache["ts"] = time.time()
                    payload["mesh"] = mesh
                    payload["age_ms"] = 0
                except Exception as e:
                    payload["error"] = str(e)
            self._send(
                200,
                json.dumps(payload, default=str).encode(),
                "application/json",
            )
            return
        if path == "/api/health":
            self._send(200, b'{"ok":true}', "application/json")
            return
        self._send(404, b"not found", "text/plain")


def main():
    if not CTL.is_file():
        print(f"WARN: ctl not found at {CTL}", flush=True)
    # Bind HTTP first so the UI is reachable while the first poll runs
    # (remote RSS SSH must never block listen).
    httpd = ThreadingHTTPServer((HOST, PORT), Handler)
    print(f"Falcon PL dashboard → http://{HOST}:{PORT}/", flush=True)
    print(f"  ROOT={ROOT} BIN={BIN}", flush=True)
    print(f"  nodes={json.dumps(load_nodes())}", flush=True)

    def warm():
        try:
            mesh = poll_mesh()
            with _lock:
                _cache["mesh"] = mesh
                _cache["ts"] = time.time()
                _cache["error"] = None
        except Exception as e:
            with _lock:
                _cache["error"] = str(e)
            print(f"initial poll: {e}", flush=True)

    threading.Thread(target=warm, daemon=True).start()
    threading.Thread(target=poller, daemon=True).start()
    httpd.serve_forever()


if __name__ == "__main__":
    main()
