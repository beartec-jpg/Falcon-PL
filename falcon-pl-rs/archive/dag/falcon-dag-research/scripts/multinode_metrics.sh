#!/usr/bin/env bash
# Short honest multi-node baseline (not a soak).
# Reports: admit %, settle/apply TPS, soft/hard finality p50/p99 (client-observed),
# cert round-lag between nodes, bullshark_anchors, ordered, equivocations.
# Phase 49a: ordered_lag, pack_backpressured (retry-tolerant metrics RPC).
# Phase 49b: admit-failure histogram, pack_skip_reasons, mempool_accounts, pack_stall dump.
#
# Env:
#   NTX=200 FLOOD=1   — higher load flood mode
#   CRYPTO=hmac-sha256-dev (default) | falcon-512  — PQ cert/tx sigs
#   BIN=...           — path to falcon-dagd
#   LABEL=before|after — tag printed in report
#   PACK_SKIP_HOLE_AFTER=0.5 — optional sequence-hole abandon (seconds)
#   BULLSHARK_TIEBREAKER=ACCOUNT_ID_LEXICOGRAPHIC — multipack Option A (default in binary)
#     aliases: CREATOR_LEX | OPTION_A | ACCOUNT_ID_LEXICOGRAPHIC | LEGACY (timestamp)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
source "${HOME}/.cargo/env" 2>/dev/null || true
BIN="${BIN:-$ROOT/target/release/falcon-dagd}"
BASE_RPC=29800
BASE_P2P=29900
N=5
UNL="val1,val2,val3,val4,val5"
NTX="${NTX:-50}"
FLOOD="${FLOOD:-0}"
LABEL="${LABEL:-}"
DURATION_S="${DURATION_S:-8}"
PACK_SKIP_HOLE_AFTER="${PACK_SKIP_HOLE_AFTER:-0}"
PACK_AHEAD_CAP="${PACK_AHEAD_CAP:-0}"
DIRECT_SUBMIT="${DIRECT_SUBMIT:-1}"
CRYPTO="${CRYPTO:-hmac-sha256-dev}"
MAX_BATCH_TXS="${MAX_BATCH_TXS:-256}"
BATCH_INTERVAL="${BATCH_INTERVAL:-0.05}"
SEQUENCE_WINDOW="${SEQUENCE_WINDOW:-512}"
UNL_THRESHOLD="${UNL_THRESHOLD:-0.6}"
ADAPTIVE_BATCH="${ADAPTIVE_BATCH:-true}"
BATCH_INTERVAL_MIN="${BATCH_INTERVAL_MIN:-0.02}"
BATCH_INTERVAL_MAX="${BATCH_INTERVAL_MAX:-0.4}"
ADAPTIVE_MEMPOOL_LOW="${ADAPTIVE_MEMPOOL_LOW:-16}"
ADAPTIVE_MEMPOOL_HIGH="${ADAPTIVE_MEMPOOL_HIGH:-256}"
MAX_UNCERTIFIED_PER_CREATOR="${MAX_UNCERTIFIED_PER_CREATOR:-4}"
SINGLE_PACKER="${SINGLE_PACKER:-true}"
# Parallel accounts for account-sharded multi-packer (1 = classic single-acct flood)
ACCOUNTS="${ACCOUNTS:-1}"
# Multipack Bullshark expand tie-breaker (inherited by falcon-dagd via env)
BULLSHARK_TIEBREAKER="${BULLSHARK_TIEBREAKER:-ACCOUNT_ID_LEXICOGRAPHIC}"
export BULLSHARK_TIEBREAKER
LOGDIR=$(mktemp -d /tmp/fd-metrics.XXXXXX)
PIDS=()
KEYS=""
WALLETS=""

if [[ "$FLOOD" == "1" && "$NTX" == "50" ]]; then
  NTX=200
fi

cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  for i in $(seq 0 $((N-1))); do
    fuser -k $((BASE_RPC+i))/tcp >/dev/null 2>&1 || true
    fuser -k $((BASE_P2P+i))/tcp >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT

if [[ ! -x "$BIN" ]]; then
  (cd "$ROOT" && cargo build --release -p fd-node)
  BIN="$ROOT/target/release/falcon-dagd"
fi

if [[ "$CRYPTO" == falcon* ]]; then
  KEYS="$LOGDIR/keys"
  WALLETS="$LOGDIR/wallets"
  "$BIN" --gen-keys --keys-dir "$KEYS" --unl "$UNL"
  "$BIN" --gen-wallet-keys --wallet-dir "$WALLETS" --faucet-accounts 20
  echo "generated Falcon-512 UNL keys in $KEYS"
  echo "generated Falcon-512 faucet wallets in $WALLETS"
fi

for i in $(seq 0 $((N-1))); do
  fuser -k $((BASE_RPC+i))/tcp >/dev/null 2>&1 || true
  fuser -k $((BASE_P2P+i))/tcp >/dev/null 2>&1 || true
done
sleep 0.2

echo "multinode_metrics N=$N ntx=$NTX accounts=$ACCOUNTS flood=$FLOOD crypto=$CRYPTO label=${LABEL:-none} pack_skip_hole_after=$PACK_SKIP_HOLE_AFTER pack_ahead_cap=$PACK_AHEAD_CAP direct_submit=$DIRECT_SUBMIT max_batch_txs=$MAX_BATCH_TXS interval=$BATCH_INTERVAL single_packer=$SINGLE_PACKER bullshark_tiebreaker=$BULLSHARK_TIEBREAKER unl_th=$UNL_THRESHOLD bin=$BIN logdir=$LOGDIR"
for i in $(seq 0 $((N-1))); do
  id="val$((i+1))"
  peers=""
  for j in $(seq 0 $((N-1))); do
    [[ $j -eq $i ]] && continue
    peers+="127.0.0.1:$((BASE_P2P+j)),"
  done
  peers=${peers%,}
  EXTRA=()
  if [[ -n "${KEYS:-}" ]]; then
    EXTRA+=(--keys-dir "$KEYS")
  fi
  if [[ -n "${WALLETS:-}" ]]; then
    EXTRA+=(--wallet-dir "$WALLETS")
  fi
  ADAPT_FLAG=(--adaptive-batch="${ADAPTIVE_BATCH}")
  SP_FLAG=(--single-packer="${SINGLE_PACKER}")
  "$BIN" \
    --id "$id" \
    --role full \
    --rpc "127.0.0.1:$((BASE_RPC+i))" \
    --p2p "127.0.0.1:$((BASE_P2P+i))" \
    --peers "$peers" \
    --unl "$UNL" \
    --worker-index "$i" \
    --worker-count "$N" \
    --network-id 9003 \
    --crypto "$CRYPTO" \
    --require-tx-sigs \
    --auto-sign \
    --batch-interval "$BATCH_INTERVAL" \
    --max-batch-txs "$MAX_BATCH_TXS" \
    --sequence-window "$SEQUENCE_WINDOW" \
    --unl-threshold "$UNL_THRESHOLD" \
    --batch-interval-min "$BATCH_INTERVAL_MIN" \
    --batch-interval-max "$BATCH_INTERVAL_MAX" \
    --adaptive-mempool-low "$ADAPTIVE_MEMPOOL_LOW" \
    --adaptive-mempool-high "$ADAPTIVE_MEMPOOL_HIGH" \
    --max-uncertified-per-creator "$MAX_UNCERTIFIED_PER_CREATOR" \
    --pack-skip-hole-after "$PACK_SKIP_HOLE_AFTER" \
    --pack-ahead-cap "$PACK_AHEAD_CAP" \
    --data-dir "$LOGDIR/$id" \
    "${ADAPT_FLAG[@]}" \
    "${SP_FLAG[@]}" \
    "${EXTRA[@]}" \
    >"$LOGDIR/$id.log" 2>&1 &
  PIDS+=($!)
done

for i in $(seq 0 $((N-1))); do
  for _ in $(seq 1 80); do
    if curl -sf -m1 -X POST "http://127.0.0.1:$((BASE_RPC+i))" \
      -H 'Content-Type: application/json' \
      -d '{"method":"ping","params":[{}]}' 2>/dev/null | grep -q pong; then
      break
    fi
    sleep 0.1
  done
done

# Flood submit + client-observed soft/hard finality samples (primary node).
# Soft = appears in executed_txs (soft-final path). Hard = hard_final batch count advanced
# enough that executed depth covers the sample (proxy via hard_final count growth).
python3 - <<PY
import json, urllib.request, time, statistics, os, sys

base = $BASE_RPC
n = $N
ntx = $NTX
n_accts = max(1, int(os.environ.get("ACCOUNTS", "${ACCOUNTS}") or "1"))
label = os.environ.get("LABEL", "") or "${LABEL}"

def rpc(port, method, params=None, timeout=5.0):
    """JSON-RPC POST with Content-Length. Retries transient empty/truncated bodies."""
    body = json.dumps({"method": method, "params": [params or {}]}).encode()
    last_err = None
    for attempt in range(8):
        try:
            req = urllib.request.Request(
                f"http://127.0.0.1:{port}",
                data=body,
                headers={
                    "Content-Type": "application/json",
                    "Content-Length": str(len(body)),
                    "Connection": "close",
                },
                method="POST",
            )
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                raw = resp.read()
            if not raw or not raw.strip():
                raise RuntimeError("empty response body")
            return json.loads(raw)
        except Exception as e:
            last_err = e
            time.sleep(0.05 * (attempt + 1))
    raise RuntimeError(f"rpc {method} :{port} failed after retries: {last_err}")

def metrics(i, retries=12):
    """Fetch node metrics; tolerate transient server_info errors under load.

    Under flood, falcon-dagd sometimes returns
      {"result":{"error":"bad json: EOF while parsing a value..."}}
    instead of result.info — never KeyError; retry until valid metrics.
    """
    last = None
    for attempt in range(retries):
        try:
            d = rpc(base + i, "server_info")
            result = d.get("result") if isinstance(d, dict) else None
            if not isinstance(result, dict):
                last = f"no result: {d!r}"
            elif "error" in result and "info" not in result:
                last = f"rpc error: {result.get('error')}"
            else:
                info = result.get("info")
                if isinstance(info, dict) and isinstance(info.get("metrics"), dict):
                    return info["metrics"]
                last = f"missing info.metrics: {list(result.keys())}"
        except Exception as e:
            last = str(e)
        time.sleep(0.08 * (attempt + 1))
    raise RuntimeError(f"metrics(val{i+1}) failed after {retries} tries: {last}")

def metrics_all(retries=12):
    """Collect metrics from every validator; partial failures raise after retries."""
    out = []
    for i in range(n):
        out.append(metrics(i, retries=retries))
    return out

def percentile(xs, p):
    if not xs:
        return 0.0
    xs = sorted(xs)
    if len(xs) == 1:
        return float(xs[0])
    k = (len(xs) - 1) * (p / 100.0)
    f = int(k)
    c = min(f + 1, len(xs) - 1)
    if f == c:
        return float(xs[f])
    return float(xs[f] + (xs[c] - xs[f]) * (k - f))

# Submit flood; sample latency for up to 40 txs by polling executed depth.
sample_cap = min(40, ntx)
submit_ts = {}
soft_ms = []
hard_ms = []
admit_ok = 0
admit_fail = 0
admit_hist = {}  # engine_result / engine_detail histogram
t0 = time.time()
try:
    hard0 = int(metrics(0).get("hard_final") or 0)
except Exception as e:
    print(f"WARN: initial metrics(0) failed ({e}); hard0=0", file=sys.stderr)
    hard0 = 0

# Multi-account flood: spread load across n_accts faucet accounts (acct1..acctN).
# Per-account sequences stay contiguous 1..per_acct so packing is hole-free.
# Critical for account-sharded multi-packer (each UNL owns a subset of accounts).
per_acct = max(1, (ntx + n_accts - 1) // n_accts)
submit_plan = []  # list of (account, sequence, destination)
for ai in range(n_accts):
    acct = f"acct{ai + 1}"
    dest = f"acct{((ai + 1) % n_accts) + 1}"
    for seq in range(1, per_acct + 1):
        submit_plan.append((acct, seq, dest))
# trim to ntx
submit_plan = submit_plan[:ntx]

for plan_i, (acct, seq, dest) in enumerate(submit_plan, 1):
    t_sub = time.time()
    code = ""
    detail = ""
    # Retry transient RPC empty-body / incomplete reads (Phase 49c).
    for attempt in range(6):
        try:
            d = rpc(base, "submit", {
                "account": acct,
                "sequence": seq,
                "destination": dest,
                "amount": 1,
                "fee": 10,
            })
            res = d.get("result") or d
            code = res.get("engine_result", "") or ""
            detail = res.get("engine_detail", "") or ""
            err = res.get("error") if isinstance(res, dict) else None
            retryable = bool(res.get("retryable")) if isinstance(res, dict) else False
            if not code and err:
                code = f"rpc_error:{err}"
                detail = "rpc_error"
                err_l = str(err).lower()
                if res.get("retryable") or "incomplete_body" in err_l or "empty_body" in err_l or "eof" in err_l or "bad json" in err_l:
                    retryable = True
            if code == "tesSUCCESS" or not retryable:
                break
        except Exception as e:
            code = "fail"
            detail = type(e).__name__
            # network blip — retry
        time.sleep(0.02 * (attempt + 1))
    key = f"{code}|{detail}" if detail else (code or "empty")
    admit_hist[key] = admit_hist.get(key, 0) + 1
    if code == "tesSUCCESS":
        admit_ok += 1
        if plan_i <= sample_cap:
            submit_ts[plan_i] = t_sub
    else:
        admit_fail += 1

t_submit = time.time()

# Wait for multi-node progress + collect soft finality for samples
target = max(1, (ntx * 95 + 99) // 100)  # 95% full-apply wait
pending_soft = set(submit_ts.keys())
pending_hard = set(submit_ts.keys())
deadline = time.time() + 30.0
while time.time() < deadline:
    try:
        ms = metrics_all(retries=4)
        ex = [int(x.get("executed_txs") or 0) for x in ms]
        an = [int(x.get("bullshark_anchors") or 0) for x in ms]
        od = [int(x.get("ordered") or 0) for x in ms]
        hf = [int(x.get("hard_final") or 0) for x in ms]
        # Soft: primary executed count covers seq (seq <= executed)
        now = time.time()
        for seq in list(pending_soft):
            if ex[0] >= seq:
                soft_ms.append((now - submit_ts[seq]) * 1000.0)
                pending_soft.discard(seq)
        # Hard proxy: hard_final advanced past genesis and executed covers seq
        # after hard_final count has grown from start
        for seq in list(pending_hard):
            if hf[0] > hard0 and ex[0] >= seq:
                hard_ms.append((now - submit_ts[seq]) * 1000.0)
                pending_hard.discard(seq)
        if (
            min(ex) == max(ex)
            and min(an) == max(an)
            and min(od) == max(od)
            and min(ex) >= target
            and min(an) >= 1
            and not pending_soft
        ):
            break
    except Exception:
        pass
    time.sleep(0.1)

t1 = time.time()
try:
    ms = metrics_all(retries=16)
except Exception as e:
    print(f"FATAL: final metrics collection failed: {e}", file=sys.stderr)
    sys.exit(2)
execs = [int(m.get("executed_txs") or 0) for m in ms]
ordered = [int(m.get("ordered") or 0) for m in ms]
ordered_lag = [int(m.get("ordered_lag") or 0) for m in ms]
pack_bp = [bool(m.get("pack_backpressured")) for m in ms]
anchors = [int(m.get("bullshark_anchors") or 0) for m in ms]
equiv = [int(m.get("equivocations") or 0) for m in ms]
certs = [int(m.get("certified") or 0) for m in ms]
batches = [int(m.get("batches_created") or 0) for m in ms]
mempool = [int(m.get("mempool") or 0) for m in ms]
soft_n = [int(m.get("soft_final_txs") or 0) for m in ms]
hard_n = [int(m.get("hard_final") or 0) for m in ms]
rounds = [int(m.get("max_cert_round") or m.get("cert_round_max") or 0) for m in ms]
lag_avg = [float(m.get("cert_round_lag_avg") or 0) for m in ms]

elapsed = max(t1 - t0, 1e-6)
submit_s = max(t_submit - t0, 1e-6)
# Settle TPS: min executed across cluster / wall from first submit to settle observation
settle_tps = min(execs) / elapsed
submit_tps = admit_ok / submit_s
admit_pct = 100.0 * admit_ok / max(admit_ok + admit_fail, 1)
apply_pct = 100.0 * min(execs) / max(ntx, 1)
agree = min(execs) == max(execs) and min(ordered) == max(ordered) and min(anchors) == max(anchors)
round_lag = max(rounds) - min(rounds) if rounds else 0

print("=== multinode_metrics (flood baseline) ===")
if label:
    print(f"label={label}")
print(f"validators={n} submitted={ntx} admit_ok={admit_ok} admit_fail={admit_fail}")
print(f"admit_pct={admit_pct:.1f}%")
print(f"executed={execs}  (min={min(execs)} max={max(execs)})")
print(f"apply_pct={apply_pct:.1f}%  settle_tps={settle_tps:.2f}  apply_tps={settle_tps:.2f}  submit_tps={submit_tps:.2f}  wall_s={elapsed:.2f}")
print(f"ordered={ordered}")
print(f"ordered_lag={ordered_lag}  pack_backpressured={pack_bp}")
print(f"bullshark_anchors={anchors}")
print(f"equivocations={equiv}")
print(f"certified={certs}  batches_created(sum)={sum(batches)}  mempool={mempool}")
print(f"soft_final_txs={soft_n}  hard_final={hard_n}")
print(f"max_cert_round={rounds}  cert_round_lag_between_nodes={round_lag}  cert_round_lag_avg_reported={lag_avg}")
print(
    f"soft_finality_ms_p50={percentile(soft_ms,50):.1f}  soft_finality_ms_p99={percentile(soft_ms,99):.1f}  soft_samples={len(soft_ms)}"
)
print(
    f"hard_finality_ms_p50={percentile(hard_ms,50):.1f}  hard_finality_ms_p99={percentile(hard_ms,99):.1f}  hard_samples={len(hard_ms)}"
)
print(f"agreement={'YES' if agree else 'NO'}")
print(f"lock_split={ms[0].get('lock_split')}")
print(f"architecture={ms[0].get('architecture')}")
print(f"build={ms[0].get('build_version')}")
print(f"crypto_scheme={ms[0].get('crypto_scheme')}  wallet_scheme={ms[0].get('wallet_scheme')}")

# Stall signature dump (per-node pack + pull) — always print when partial apply.
stall_band = settle_tps < 10.0 or min(execs) < max(1, ntx // 2)
if stall_band or not agree:
    print("--- stall signature (per node) ---")
    for i, m in enumerate(ms):
        ps = m.get("pack_stall") or {}
        pull = m.get("pull") or {}
        print(
            f"  val{i+1}: exec={execs[i]} mempool={mempool[i]} cert_round={rounds[i]} "
            f"uncert={m.get('uncertified')} primary={m.get('primary_packer')} "
            f"assigned={m.get('assigned_packer')} open_creators={ps.get('open_batch_creators')} "
            f"ordered_lag={ordered_lag[i]} pack_bp={pack_bp[i]} "
            f"oldest_unc_age={ps.get('oldest_uncertified_age_s')} tip_fp={str(ps.get('tip_fp'))[:24]} "
            f"pull_pending={pull.get('pending_pulls')} pull_inflight={pull.get('inflight_total')} "
            f"pull_retries={pull.get('retries')} hard_final={hard_n[i]} anchors={anchors[i]}"
        )

# --- Phase 49b: packing stall instrumentation dump ---
print("--- admit failure histogram (client-observed engine_result|detail) ---")
for k, v in sorted(admit_hist.items(), key=lambda kv: (-kv[1], kv[0])):
    print(f"  {k}: {v}")

print("--- pack_stall_snapshot + pack_skip / mempool_accounts (per node) ---")
for i, m in enumerate(ms):
    ps = m.get("pack_stall") or {}
    print(
        f"  val{i+1}: assigned={ps.get('assigned_packer')} primary={ps.get('primary_packer')} "
        f"mempool={ps.get('mempool')} last_batch_age_s={ps.get('last_batch_age_s')} "
        f"uncert={ps.get('uncertified')} uncert_by_me={ps.get('uncertified_by_me')} "
        f"oldest_unc_age_s={ps.get('oldest_uncertified_age_s')} tip_fp={str(ps.get('tip_fp') or '')[:20]} "
        f"open_creators={ps.get('open_batch_creators')}"
    )
    skips = m.get("pack_skip_reasons") or ps.get("pack_skip_reasons") or {}
    admits = m.get("admit_reasons") or ps.get("admit_reasons") or {}
    print(f"    pack_skip_reasons={skips}")

    sf = m.get("shard_flow") or {}
    if sf:
        print("    shard_flow=%s" % (sf,))
    md = m.get("mesh") or m.get("mesh_diag") or {}
    if isinstance(md, dict):
        print("    parent_stall_total=%s parent_stall_max=%s" % (
            md.get("parent_stall_total"), md.get("parent_stall_max_per_batch")))
        tl = md.get("cert_timeline") or []
        if tl:
            print("    cert_timeline_n=%s" % (len(tl),))
            for row in tl[:8]:
                print("      cert_tl %s" % (row,))

    print(f"    admit_reasons={admits}")
    accts = m.get("mempool_accounts") or ps.get("mempool_accounts") or []
    if accts:
        for a in accts[:8]:
            print(
                f"    acct {a.get('account')}: count={a.get('count')} "
                f"min={a.get('min_seq')} max={a.get('max_seq')} "
                f"pack_cursor={a.get('pack_cursor')} exec_next={a.get('exec_next')} "
                f"gaps={a.get('gaps')} missing_at_pack_cursor={a.get('missing_at_pack_cursor')}"
            )
    else:
        print("    mempool_accounts=[]")

# Quiet-pack diagnosis one-liner
primary_skips = (ms[0].get("pack_skip_reasons") or {}) if ms else {}
batches0 = int((ms[0].get("batches_created") if ms else 0) or 0)
quiet = min(execs) < max(1, ntx // 2) or settle_tps < 10.0 or batches0 < 3
if quiet or stall_band:
    print("--- pack quiet diagnosis (val1 pack_skip top reasons) ---")
    top = sorted(primary_skips.items(), key=lambda kv: -int(kv[1] or 0))[:12]
    for k, v in top:
        print(f"  {k}: {v}")
    if primary_skips.get("not_assigned_packer"):
        print("  HINT: not_assigned_packer dominant → packer assignment / single_packer")
    if primary_skips.get("interval_throttle"):
        print("  HINT: interval_throttle → batch interval / adaptive throttle")
    if primary_skips.get("filter_seq_gap") or primary_skips.get("no_ready_txs"):
        print("  HINT: no_ready_txs/filter_seq_gap → sequence hole or pack_cursor ahead of mempool")
    if primary_skips.get("uncertified_cap") or primary_skips.get("self_open_on_parents"):
        print("  HINT: uncertified_cap/self_open → cert pipeline backlog on primary")
    if primary_skips.get("rival_on_tips"):
        print("  HINT: rival_on_tips → another creator open on same tips")
    if primary_skips.get("skip_hole"):
        print("  HINT: skip_hole fired → aged sequence holes abandoned (pack_skip_hole_after)")

# Optional machine-readable result for tps_knob_sweep
results_path = os.environ.get("RESULTS_JSON", "")
if results_path:
    import pathlib
    out = {
        "label": label,
        "ntx": ntx,
        "admit_ok": admit_ok,
        "admit_fail": admit_fail,
        "admit_pct": admit_pct,
        "executed_min": min(execs),
        "executed_max": max(execs),
        "executed": execs,
        "settle_tps": settle_tps,
        "submit_tps": submit_tps,
        "wall_s": elapsed,
        "ordered_lag": ordered_lag,
        "agreement": agree,
        "anchors_min": min(anchors),
        "certified": certs,
        "batches_created_sum": sum(batches),
        "crypto_scheme": ms[0].get("crypto_scheme"),
        "wallet_scheme": ms[0].get("wallet_scheme"),
        "mesh": [m.get("mesh") or {} for m in ms],
        "cert_apply_lag_ms_max": max(
            float((m.get("mesh") or {}).get("cert_apply_lag_ms_max") or 0) for m in ms
        ),
        "cert_from_create_lag_ms_max": max(
            float((m.get("mesh") or {}).get("cert_from_create_lag_ms_max") or 0) for m in ms
        ),
        "soft_finality_ms_p50": percentile(soft_ms, 50),
        "soft_finality_ms_p99": percentile(soft_ms, 99),
        "knobs": {
            "crypto": os.environ.get("CRYPTO", ""),
            "max_batch_txs": os.environ.get("MAX_BATCH_TXS", ""),
            "batch_interval": os.environ.get("BATCH_INTERVAL", ""),
            "sequence_window": os.environ.get("SEQUENCE_WINDOW", ""),
            "unl_threshold": os.environ.get("UNL_THRESHOLD", ""),
            "adaptive_batch": os.environ.get("ADAPTIVE_BATCH", ""),
            "batch_interval_min": os.environ.get("BATCH_INTERVAL_MIN", ""),
            "batch_interval_max": os.environ.get("BATCH_INTERVAL_MAX", ""),
            "adaptive_mempool_low": os.environ.get("ADAPTIVE_MEMPOOL_LOW", ""),
            "adaptive_mempool_high": os.environ.get("ADAPTIVE_MEMPOOL_HIGH", ""),
            "max_uncertified_per_creator": os.environ.get("MAX_UNCERTIFIED_PER_CREATOR", ""),
            "single_packer": os.environ.get("SINGLE_PACKER", ""),
            "pack_skip_hole_after": os.environ.get("PACK_SKIP_HOLE_AFTER", ""),
            "pack_ahead_cap": os.environ.get("PACK_AHEAD_CAP", ""),
            "direct_submit": os.environ.get("DIRECT_SUBMIT", "1"),
            "accounts": os.environ.get("ACCOUNTS", "1"),
            "bullshark_tiebreaker": os.environ.get("BULLSHARK_TIEBREAKER", ""),
        },
    }
    pathlib.Path(results_path).write_text(json.dumps(out, indent=2) + "\n")

ok = True
if admit_pct < 90:
    print("WARN: admit_pct < 90")
    ok = False
if min(execs) < max(1, ntx // 2):
    print(f"WARN: executed min {min(execs)} < half of submitted")
    ok = False
if min(anchors) < 1:
    print("WARN: no bullshark anchors")
    ok = False
if not agree:
    print("WARN: multi-node disagreement")
    ok = False
if round_lag > 32:
    print(f"WARN: cert_round_lag_between_nodes={round_lag} unbounded-ish")
    ok = False
if min(equiv) > 0:
    print(f"NOTE: equivocations present {equiv}")

if ok and agree and min(anchors) >= 1 and admit_pct >= 90 and min(execs) >= max(1, ntx // 2):
    print("multinode_metrics OK")
    sys.exit(0)
print("multinode_metrics PARTIAL (see WARNs) — still useful baseline")
if min(execs) >= 1 and min(anchors) >= 1:
    sys.exit(0)
sys.exit(1)
PY
