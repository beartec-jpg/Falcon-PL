#!/usr/bin/env bash
# Push binary + keys + wallets to host2 (6v+2a topology — no droplet).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=/dev/null
source "$ROOT/env.sh"

MESH_F="$MESH_ROOT_FALCON"
BIN_SRC="$HOME/falcon-dag/falcon-dag-rs/target/release/falcon-dagd"
mkdir -p "$MESH_F/bin"
if [[ -x "$BIN_SRC" ]]; then
  cp -f "$BIN_SRC" "$MESH_F/bin/falcon-dagd" || true
elif [[ -x "$HOME/falcon-dag-mesh/bin/falcon-dagd" ]]; then
  : # already in place
elif [[ ! -x "$MESH_F/bin/falcon-dagd" ]]; then
  echo "missing falcon-dagd binary" >&2
  exit 1
fi
chmod +x "$MESH_F/bin/falcon-dagd"
[[ -d "$MESH_F/keys" && -f "$MESH_F/keys/unl_public.json" ]] || {
  echo "missing keys — run 00-gen-keys.sh first" >&2
  exit 1
}

echo "→ host2 ($F2_IP)"
ssh host2 "mkdir -p $MESH_ROOT_FALCON/{bin,keys,wallets,data,logs}"
rsync -az "$MESH_F/bin/falcon-dagd" "host2:$MESH_ROOT_FALCON/bin/"
rsync -az --delete "$MESH_F/keys/" "host2:$MESH_ROOT_FALCON/keys/"
rsync -az --delete "$MESH_F/wallets/" "host2:$MESH_ROOT_FALCON/wallets/"

echo "distribute OK (host1 local + host2)"
