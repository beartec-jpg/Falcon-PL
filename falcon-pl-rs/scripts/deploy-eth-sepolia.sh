#!/usr/bin/env bash
# Build + install ETH/USDC Sepolia bridge operators (testnet auditable).
# Run ON host1: bash scripts/deploy-eth-sepolia.sh
set -euo pipefail
PL_ROOT="${PL_ROOT:-/home/falcon/falcon-pl-public-testnet-2300}"
REPO="${REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
export PATH="${HOME}/.cargo/bin:${HOME}/.foundry/bin:${PATH}"

cd "$REPO"
echo "==> build fd-pl + falcon-pl-snark (release)"
cargo build --release -p fd-pl --bin falcon-pl-node --bin falcon-pl-ctl
cargo build --release -p fd-pl-snark --bin falcon-pl-snark

install -d "$PL_ROOT/bin" "$PL_ROOT/scripts" "$PL_ROOT/systemd" "$PL_ROOT/run/snark/falcon-qc-one" "$PL_ROOT/run/bridge" "$PL_ROOT/config"
install -m755 target/release/falcon-pl-node target/release/falcon-pl-ctl "$PL_ROOT/bin/"
install -m755 target/release/falcon-pl-snark "$PL_ROOT/bin/"

SCRIPTS="$REPO/crates/fd-pl/scripts"
install -m755 \
  "$SCRIPTS/pl-eth-lc-relayer.py" \
  "$SCRIPTS/pl-eth-auto-mint.py" \
  "$SCRIPTS/pl-eth-mint.py" \
  "$SCRIPTS/pl-eth-claim-proof.py" \
  "$SCRIPTS/pl-falcon-qc-prover.py" \
  "$SCRIPTS/pl-falcon-qc-catchup.py" \
  "$SCRIPTS/pl-eth-sepolia-e2e.py" \
  "$SCRIPTS/pl-bitvm2-e2e.py" \
  "$SCRIPTS/pl-btc-header-submitter.py" \
  "$SCRIPTS/eth_bridge_config.py" \
  "$SCRIPTS/pl-bridge-install-config.py" \
  "$SCRIPTS/pl-bridge-health.py" \
  "$SCRIPTS/pl-bridge-e2e-wait.py" \
  "$PL_ROOT/scripts/"
install -m755 "$REPO/scripts/falcon-qc-setup-one-ptau.sh" "$PL_ROOT/scripts/"

install -m644 "$REPO/crates/fd-pl/config/sepolia-qc.json" "$PL_ROOT/config/sepolia-qc.json"
cp -f "$PL_ROOT/config/sepolia-qc.json" "$PL_ROOT/run/bridge/sepolia-qc.json" 2>/dev/null || true
python3 "$PL_ROOT/scripts/pl-bridge-install-config.py"

install -m644 "$REPO/crates/fd-pl/systemd/"*.service "$PL_ROOT/systemd/" 2>/dev/null || true
install -m644 "$REPO/crates/fd-pl/systemd/"*.timer "$PL_ROOT/systemd/" 2>/dev/null || true
install -m644 \
  "$REPO/crates/fd-pl/systemd/falcon-pl-falcon-qc-prover.service" \
  "$REPO/crates/fd-pl/systemd/falcon-pl-falcon-qc-catchup.service" \
  "$REPO/crates/fd-pl/systemd/falcon-pl-falcon-qc-catchup.timer" \
  "$REPO/crates/fd-pl/systemd/falcon-pl-btc-header-submitter.service" \
  "$REPO/crates/fd-pl/systemd/falcon-pl-eth-lc-relayer.service" \
  "$REPO/crates/fd-pl/scripts/falcon-pl-eth-auto-mint.service" \
  "$PL_ROOT/systemd/"

BRIDGE=$(python3 -c "import json; print(json.load(open('$PL_ROOT/run/bridge/sepolia-qc.json'))['bridge'])")

echo "==> falcon-qc committee pub keys"
mkdir -p "$PL_ROOT/keys/falcon-qc/sigs"
python3 - "$PL_ROOT/keys" <<'PY'
import json, sys
from pathlib import Path
keys = Path(sys.argv[1])
out = keys / "falcon-qc"
out.mkdir(parents=True, exist_ok=True)
for seat in ("v1", "v2", "v3", "v4", "v5", "v6"):
    p = keys / f"{seat}.json"
    if not p.exists():
        continue
    raw = json.loads(p.read_text())
    pk = (raw.get("public_key") or "").strip().removeprefix("0x")
    sid = raw.get("id") or seat
    if not pk:
        continue
    (out / f"{seat}.pub.json").write_text(json.dumps({"id": sid, "pk_hex": pk}, indent=2) + "\n")
    print("exported", seat)
PY

QC_DIR="$PL_ROOT/run/snark/falcon-qc-one"
if [[ -f "$QC_DIR/pk.bin" && ! -f "$QC_DIR/ptau.json" ]]; then
  if [[ "${FPL_PRODUCTION:-0}" == "1" && "${FPL_ALLOW_TEST_SEED:-0}" != "1" ]]; then
    echo "FATAL: $QC_DIR/pk.bin has no ptau.json (test-seed / OsRng). Move it aside, set FPL_PTAU, re-run." >&2
    exit 2
  fi
  echo "WARN: existing OPEN=1 pk looks like test-seed (no ptau.json)" >&2
fi
if [[ ! -f "$QC_DIR/pk.bin" ]]; then
  if [[ -n "${FPL_PTAU:-}" ]]; then
    echo "==> Groth16 OPEN=1 from ptau + contribute + beacon"
    bash "$REPO/scripts/falcon-qc-setup-one-ptau.sh"
  elif [[ "${FPL_PRODUCTION:-0}" == "1" && "${FPL_ALLOW_TEST_SEED:-0}" != "1" ]]; then
    echo "FATAL: FPL_PRODUCTION=1 refuses --test-seed. Set FPL_PTAU to powersOfTau28_hez_final_23.ptau (or _24)." >&2
    exit 2
  else
    echo "==> Groth16 setup OPEN=1 Falcon QC (test-seed, auditable testnet — not protocol-controlled)"
    "$PL_ROOT/bin/falcon-pl-snark" falcon-setup-one \
      --out-dir "$QC_DIR" --test-seed
  fi
fi

cat <<EOF

=== ETH/USDC testnet checklist (host1) ===
Bridge config: $PL_ROOT/run/bridge/sepolia-qc.json
Bridge env:    $PL_ROOT/run/bridge/falcon-pl-bridge.env
Bridge:        FalconQcBridge $BRIDGE (Sepolia)

0. Install bridge config (done by this script):
     cat $PL_ROOT/run/bridge/falcon-pl-bridge.env

1. LC relayer (peg-in):
     sudo cp $PL_ROOT/systemd/falcon-pl-eth-lc-relayer.service /etc/systemd/system/
     sudo systemctl daemon-reload
     sudo systemctl enable --now falcon-pl-eth-lc-relayer

2. Auto-mint (optional):
     sudo cp $PL_ROOT/systemd/falcon-pl-eth-auto-mint.service /etc/systemd/system/
     sudo systemctl enable --now falcon-pl-eth-auto-mint

3. Falcon QC prover (peg-out headers):
     sudo cp $PL_ROOT/systemd/falcon-pl-falcon-qc-prover.service /etc/systemd/system/
     sudo systemctl daemon-reload
     sudo systemctl enable --now falcon-pl-falcon-qc-prover

3a. Falcon QC sig collection (timer, every 10 min):
     sudo cp $PL_ROOT/systemd/falcon-pl-falcon-qc-catchup.service /etc/systemd/system/
     sudo cp $PL_ROOT/systemd/falcon-pl-falcon-qc-catchup.timer /etc/systemd/system/
     sudo systemctl daemon-reload
     sudo systemctl enable --now falcon-pl-falcon-qc-catchup.timer

3b. BTC header submitter (peg-in SPV):
     sudo cp $PL_ROOT/systemd/falcon-pl-btc-header-submitter.service /etc/systemd/system/
     sudo systemctl daemon-reload
     sudo systemctl enable --now falcon-pl-btc-header-submitter

4. Live Sepolia e2e:
     FPL_RPC=127.0.0.1:19301 python3 $PL_ROOT/scripts/pl-eth-sepolia-e2e.py

5. Prove-only (no Sepolia submit):
     FALCON_QC_PROVE_ONLY=1 FALCON_QC_ONCE=1 python3 $PL_ROOT/scripts/pl-falcon-qc-prover.py

Legacy BabyJub pl-lc-prover.py is DEPRECATED — do not use for new tests.

EOF
