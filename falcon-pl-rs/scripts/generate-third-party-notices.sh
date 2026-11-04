#!/usr/bin/env bash
# Regenerates THIRD-PARTY-NOTICES (repo root) from the Cargo.lock files.
# Requires cargo-about (`cargo install cargo-about --locked`; 0.8.4 works on rustc 1.85).
# Ship the generated file inside every Falcon PL release tarball / operator kit.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RS="$ROOT/falcon-pl-rs"
CFG="$RS/third-party/about.toml"
TPL="$RS/third-party/about.hbs"
OUT="$ROOT/THIRD-PARTY-NOTICES"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

gen() { (cd "$1" && cargo about generate --locked -c "$CFG" "$TPL" -o "$2"); }
gen "$RS" "$TMP/node.txt"
gen "$RS/tools/fpl-gateway" "$TMP/gateway.txt"
gen "$RS/tools/fpl-operator" "$TMP/operator.txt"

{
  cat "$RS/third-party/HEADER.txt"
  printf '\n\n################################################################################\n'
  printf '# PART 1: falcon-pl-rs workspace (falcon-pl-node, falcon-pl-ctl)\n'
  printf '################################################################################\n\n'
  cat "$TMP/node.txt"
  printf '\n\n################################################################################\n'
  printf '# PART 2: tools/fpl-gateway (Falcon Cloud RPC gateway)\n'
  printf '################################################################################\n\n'
  cat "$TMP/gateway.txt"
  printf '\n\n################################################################################\n'
  printf '# PART 3: tools/fpl-operator (bridge operator)\n'
  printf '################################################################################\n\n'
  cat "$TMP/operator.txt"
} > "$OUT"
echo "wrote $OUT"
