#!/usr/bin/env bash
# Build + install BitVM2 testnet cutover on host1.
# Run ON host1 only:  bash scripts/deploy-bitvm2-testnet.sh
set -euo pipefail
PL_ROOT="${PL_ROOT:-/home/falcon/falcon-pl-public-testnet-2300}"
REPO="${REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
export BTC_EXIT_MODE=bitvm2
export PATH="${HOME}/.cargo/bin:${PATH}"

cd "$REPO"
if [[ -d .git ]]; then
  echo "==> git pull (optional)"
  git pull --ff-only 2>/dev/null || true
fi

echo "==> build fd-pl + bitvm2 prove (release)"
cargo build --release -p fd-pl --bin falcon-pl-node --bin falcon-pl-ctl --bin falcon-pl-bitvm2-prove

install -d "$PL_ROOT/bin" "$PL_ROOT/scripts" "$PL_ROOT/systemd"
install -m755 target/release/falcon-pl-node target/release/falcon-pl-ctl target/release/falcon-pl-bitvm2-prove "$PL_ROOT/bin/"
install -m755 crates/fd-pl/scripts/pl-bitvm2-operator.py crates/fd-pl/scripts/pl-bitvm2-e2e.py "$PL_ROOT/scripts/"
install -m755 scripts/restart-v1-bitvm2.sh scripts/pl-bitvm2-resync-v1.sh "$PL_ROOT/scripts/"
install -m644 crates/fd-pl/systemd/falcon-pl-bitvm2-operator.service "$PL_ROOT/systemd/"

if command -v bitcoin-cli >/dev/null 2>&1; then
  if ! bitcoin-cli -testnet listwallets 2>/dev/null | grep -q bitvm2-operator; then
    echo "==> create operator wallet bitvm2-operator"
    bitcoin-cli -testnet createwallet bitvm2-operator 2>/dev/null || true
  fi
  OP_ADDR=$(bitcoin-cli -testnet -rpcwallet=bitvm2-operator getnewaddress "bitvm2-operator" bech32)
  echo ""
  echo "Operator testnet address (fund from faucet if empty): $OP_ADDR"
  echo ""
else
  OP_PRIV="${HOME}/falcon-pl-vault-test/btc-wallets/dave.privhex"
  if [[ -f "${OP_PRIV%.privhex}.json" ]]; then
    OP_ADDR=$(python3 -c "import json; print(json.load(open('${OP_PRIV%.privhex}.json'))['address'])")
    echo ""
    echo "No bitcoin-cli — operator uses privhex wallet:"
    echo "  BITVM2_PRIVHEX_FILE=$OP_PRIV"
    echo "  BITVM2_OPERATOR_ADDR=$OP_ADDR"
    echo "  Fund this address on testnet for pegouts."
    echo ""
  else
    echo "WARN: bitcoin-cli not in PATH and no dave.privhex wallet found"
  fi
fi

echo "==> patch validator systemd with BTC_EXIT_MODE=bitvm2"
for f in "$PL_ROOT/systemd/"*.service; do
  [[ -f "$f" ]] || continue
  grep -q BTC_EXIT_MODE "$f" 2>/dev/null && continue
  sed -i '/^\[Service\]/a Environment=BTC_EXIT_MODE=bitvm2' "$f" || true
done

cat <<EOF

=== Production checklist (host1) ===
1. Fund operator wallet (≥15000 sats testnet):
     BITVM2_OPERATOR_ADDR above / dave.privhex
2. Build + install (this script) on host1
3. If v1 shows state root mismatch / deferred-until-catchup:
     bash $PL_ROOT/scripts/pl-bitvm2-resync-v1.sh
4. Restart validators with BTC_EXIT_MODE=bitvm2:
     sudo systemctl restart 'falcon-pl-*'
5. Enable operator sidecar:
     sudo cp $PL_ROOT/systemd/falcon-pl-bitvm2-operator.service /etc/systemd/system/
     sudo systemctl daemon-reload
     sudo systemctl enable --now falcon-pl-bitvm2-operator
6. E2E (public path — run against synced RPC, e.g. v2:19302):
     FPL_PRODUCTION=1 FPL_RPC=100.64.0.2:19302 python3 $PL_ROOT/scripts/pl-bitvm2-e2e.py
7. Operator checkpoint: $PL_ROOT/run/bitvm2-operator.json

Do NOT use admin rail-deposit/rail-withdraw on the public hub for user flows.

EOF
