#!/usr/bin/env bash
# Isolation dig: multipack vs single-packer at N=3 and N=6.
# Answers: is the scale-out fail from "too many packers" or "larger UNL"?
set +e
set -uo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
export SSH_F2="${SSH_F2:-100.64.0.3}"
unset F1_IP F2_IP || true
export SCALE_NS=3,6
export LADDER=warm,light,med

BASE_OUT="${OUT:-/tmp/packer-vs-unl-$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$BASE_OUT"
echo "BASE_OUT=$BASE_OUT"

echo "===== CELL A: multipack (single_packer=false) N=3,6 ====="
export SINGLE_PACKER=false
export OUT="$BASE_OUT/multipack"
python3 "$ROOT/07-scale-n-curve.py" | tee "$BASE_OUT/multipack.log"
MP_EC=${PIPESTATUS[0]}

echo "===== CELL B: single-packer=true N=3,6 ====="
export SINGLE_PACKER=true
export OUT="$BASE_OUT/single"
python3 "$ROOT/07-scale-n-curve.py" | tee "$BASE_OUT/single.log"
SP_EC=${PIPESTATUS[0]}

python3 - "$BASE_OUT" <<'PY'
import json, sys
from pathlib import Path
base = Path(sys.argv[1])

def load(cell):
    p = base / cell / "SCALE_N_REPORT.json"
    if not p.exists():
        return None
    return json.loads(p.read_text())

mp, sp = load("multipack"), load("single")
rows = []
for n in (3, 6):
    def pick(rep, n):
        if not rep:
            return {}
        for r in rep.get("results") or []:
            if r.get("n") == n and "error" not in r:
                return r
        return {}
    m, s = pick(mp, n), pick(sp, n)
    rows.append({
        "n": n,
        "multipack_best_min": m.get("best_exec_tps_min"),
        "multipack_tight": m.get("best_tight_tps_min"),
        "single_best_min": s.get("best_exec_tps_min"),
        "single_tight": s.get("best_tight_tps_min"),
        "multipack_phases": {p["label"]: p["exec_tps_min"] for p in m.get("phases") or []},
        "single_phases": {p["label"]: p["exec_tps_min"] for p in s.get("phases") or []},
    })

# Interpretation
notes = []
r3 = next(x for x in rows if x["n"] == 3)
r6 = next(x for x in rows if x["n"] == 6)

def ratio(a, b):
    if a is None or b is None or b == 0:
        return None
    return round(a / b, 2)

notes.append({
    "question": "At fixed N=6, does single-packer beat multipack?",
    "multipack": r6["multipack_best_min"],
    "single": r6["single_best_min"],
    "single_over_multi": ratio(r6["single_best_min"], r6["multipack_best_min"]),
})
notes.append({
    "question": "Under single-packer, does N=6 beat N=3? (UNL-size-only scale-out)",
    "n3": r3["single_best_min"],
    "n6": r6["single_best_min"],
    "n6_over_n3": ratio(r6["single_best_min"], r3["single_best_min"]),
})
notes.append({
    "question": "Under multipack, does N=6 beat N=3?",
    "n3": r3["multipack_best_min"],
    "n6": r6["multipack_best_min"],
    "n6_over_n3": ratio(r6["multipack_best_min"], r3["multipack_best_min"]),
})

# Auto verdict text
def num(x):
    return x if isinstance(x, (int, float)) else None

s6, m6 = num(r6["single_best_min"]), num(r6["multipack_best_min"])
s3, m3 = num(r3["single_best_min"]), num(r3["multipack_best_min"])

verdict_bits = []
if s6 is not None and m6 is not None:
    if s6 > m6 * 1.15:
        verdict_bits.append("PRIMARY: multipack contention hurts at N=6 (single-packer recovers throughput).")
    elif m6 > s6 * 1.15:
        verdict_bits.append("PRIMARY: multipack still helps at N=6 vs single-packer.")
    else:
        verdict_bits.append("PRIMARY: packer mode similar at N=6 — bottleneck elsewhere.")
if s3 is not None and s6 is not None:
    if s6 > s3 * 1.05:
        verdict_bits.append("UNL-only: larger UNL under single-packer still scales up (unexpected win).")
    elif s6 < s3 * 0.85:
        verdict_bits.append("UNL-only: larger UNL under single-packer still loses TPS (cert/quorum tax).")
    else:
        verdict_bits.append("UNL-only: N=3≈N=6 under single-packer (UNL size not the main cliff).")

report = {
    "suite": "packer_vs_unl_isolation",
    "rows": rows,
    "notes": notes,
    "verdict": " ".join(verdict_bits) if verdict_bits else "incomplete data",
}
(base / "PACKER_VS_UNL.json").write_text(json.dumps(report, indent=2) + "\n")

md = [
    "# Packer vs UNL isolation dig",
    "",
    f"**Verdict:** {report['verdict']}",
    "",
    "| N | multipack best min | multipack tight | single-packer best min | single tight |",
    "|---|--------------------|-----------------|------------------------|--------------|",
]
for r in rows:
    md.append(
        f"| {r['n']} | {r['multipack_best_min']} | {r['multipack_tight']} | "
        f"{r['single_best_min']} | {r['single_tight']} |"
    )
md += ["", "## Phase detail", ""]
for r in rows:
    md.append(f"### N={r['n']}")
    md.append(f"- multipack phases: `{r['multipack_phases']}`")
    md.append(f"- single phases: `{r['single_phases']}`")
    md.append("")
md += ["## Interpretation questions", ""]
for n in notes:
    md.append(f"- **{n['question']}** → `{json.dumps({k:v for k,v in n.items() if k!='question'})}`")
(base / "PACKER_VS_UNL.md").write_text("\n".join(md) + "\n")
print("\n".join(md))
print(f"\nWrote {base}/PACKER_VS_UNL.md")
PY

echo "done base=$BASE_OUT"
# also copy summary into deploy
cp -f "$BASE_OUT/PACKER_VS_UNL.md" "$ROOT/PACKER_VS_UNL.md" 2>/dev/null || true
cp -f "$BASE_OUT/PACKER_VS_UNL.json" "$ROOT/PACKER_VS_UNL.json" 2>/dev/null || true
