#!/usr/bin/env bash
# Generate shared UNL + wallet keys on host1 mesh root.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=/dev/null
source "$ROOT/env.sh"

MESH="${MESH_ROOT_FALCON}"
BIN="${BIN_FALCON}"
mkdir -p "$MESH"/{keys,wallets,bin,logs,data}
if [[ ! -x "$BIN" ]]; then
  echo "missing binary $BIN — copy falcon-dagd first" >&2
  exit 1
fi

# Archivals are not on the UNL but still need local Falcon secrets (sign paths in order-exec).
ARCH_IDS="${ARCH_IDS:-arch1,arch2}"
echo "Generating UNL keys for $UNL + archival $ARCH_IDS ($CRYPTO)…"
rm -rf "$MESH/keys"/* "$MESH/wallets"/* 2>/dev/null || true
mkdir -p "$MESH/keys" "$MESH/wallets"
"$BIN" --gen-keys --keys-dir "$MESH/keys" --unl "$UNL"
# Merge archival node secrets without replacing UNL key material
TMP_ARCH="$(mktemp -d)"
"$BIN" --gen-keys --keys-dir "$TMP_ARCH" --unl "$ARCH_IDS"
for f in "$TMP_ARCH"/*.json; do
  base="$(basename "$f")"
  if [[ "$base" == "unl_public.json" ]]; then
    python3 - "$MESH/keys/unl_public.json" "$f" <<'PY'
import json, sys
from pathlib import Path
main, extra = Path(sys.argv[1]), Path(sys.argv[2])
m = json.loads(main.read_text())
e = json.loads(extra.read_text())
m.update(e)
main.write_text(json.dumps(m, indent=2) + "\n")
print("merged public keys:", sorted(m.keys()))
PY
  else
    cp -a "$f" "$MESH/keys/"
  fi
done
rm -rf "$TMP_ARCH"
"$BIN" --gen-wallet-keys --wallet-dir "$MESH/wallets" --faucet-accounts 40
ls -la "$MESH/keys" "$MESH/wallets" | head -40
echo "OK keys at $MESH/keys wallets at $MESH/wallets"
