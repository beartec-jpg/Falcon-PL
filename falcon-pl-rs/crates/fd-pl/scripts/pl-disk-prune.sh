#!/usr/bin/env bash
# Falcon PL disk hygiene helper (v2.9.53). DRY RUN unless --apply.
#
# Lists (and with --apply deletes) the files that filled host2 on
# 2026-10-07 and other unbounded leftovers:
#   1. legacy hub dumps  $TMPDIR/pl-hub-join-<seat>-<tip>.json[.tmp]
#      (keeps the newest per seat — a pre-2.9.53 hub may be serving it)
#   2. legacy joiner pulls $TMPDIR/pl-snap-pull-<tip>.json older than 10 min
#   3. snapshot backups  <data>/snapshots/<seat>.json.<ts>.bak / .json.bak-<ts>
#      beyond the newest $KEEP per seat (same rule 2.9.53 applies at start)
#   4. zero-byte / stale  <data>/snapshots/*.json.tmp older than 10 min
#
# Never touches live snapshots, keys, bodies, logs or backups/.
#
# Usage:
#   pl-disk-prune.sh [--apply] [--keep N] [DATA_DIR]
#   DATA_DIR default: ~/falcon-pl-public-testnet-2300
set -euo pipefail

APPLY=0
KEEP=3
DATA="${HOME}/falcon-pl-public-testnet-2300"
while [ $# -gt 0 ]; do
  case "$1" in
    --apply) APPLY=1 ;;
    --keep) KEEP="$2"; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) DATA="$1" ;;
  esac
  shift
done
TMP="${TMPDIR:-/tmp}"
SNAPS="$DATA/snapshots"
total=0
victims=()

add() { # path
  local f="$1" sz
  [ -f "$f" ] || return 0
  sz=$(stat -c %s "$f" 2>/dev/null || echo 0)
  total=$((total + sz))
  victims+=("$f")
  printf '%12s  %s\n' "$sz" "$f"
}

echo "== df before"
df -h "$DATA" "$TMP" 2>/dev/null | awk 'NR==1 || !seen[$0]++'

echo "== 1. legacy hub dumps in $TMP (newest per seat kept)"
for seat in $(ls "$TMP" 2>/dev/null | sed -n 's/^pl-hub-join-\([^-]*\)-[0-9]*\.json\(\.tmp\)\{0,1\}$/\1/p' | sort -u); do
  # newest first by mtime; skip the first complete .json
  kept=0
  while IFS= read -r f; do
    if [ $kept -eq 0 ] && [[ "$f" == *.json ]] && [ -s "$f" ]; then kept=1; echo "        keep  $f"; continue; fi
    add "$f"
  done < <(ls -1t "$TMP"/pl-hub-join-"$seat"-*.json "$TMP"/pl-hub-join-"$seat"-*.json.tmp 2>/dev/null)
done

echo "== 2. legacy joiner pulls in $TMP (older than 10 min)"
while IFS= read -r f; do add "$f"; done < <(find "$TMP" -maxdepth 1 -type f -regex '.*/pl-snap-pull-[0-9]+\.json\(\.tmp\)?' -mmin +10 2>/dev/null)

echo "== 3. snapshot backups in $SNAPS beyond newest $KEEP per seat"
if [ -d "$SNAPS" ]; then
  for base in $(ls "$SNAPS" 2>/dev/null | sed -n 's/^\(.*\.json\)\.\(.*\.bak\|bak-.*\)$/\1/p' | sort -u); do
    while IFS= read -r f; do add "$f"; done < <(
      ls -1t "$SNAPS/$base".*.bak "$SNAPS/$base".bak-* 2>/dev/null | tail -n +"$((KEEP + 1))"
    )
  done
fi

echo "== 4. stale snapshot .json.tmp in $SNAPS (older than 10 min)"
[ -d "$SNAPS" ] && while IFS= read -r f; do add "$f"; done < <(find "$SNAPS" -maxdepth 1 -type f -name '*.json.tmp' -mmin +10 2>/dev/null)

echo "== total: ${#victims[@]} files, $((total / 1024 / 1024)) MB"
if [ "$APPLY" -ne 1 ]; then
  echo "DRY RUN — re-run with --apply to delete."
  exit 0
fi
for f in "${victims[@]}"; do rm -f -- "$f"; done
echo "== deleted ${#victims[@]} files"
df -h "$DATA" "$TMP" 2>/dev/null | awk 'NR==1 || !seen[$0]++'
