#!/usr/bin/env bash
# Portable operator kit build (x86_64-linux, glibc <= 2.34, NO target-cpu=native).
#   tools/fpl-operator/scripts/build-release.sh [--with-bitvm]
# Output: dist/fpl-operator-<ver>-x86_64-linux.tar.gz (+ .sha256). Sign SHA256SUMS with the release
# minisign key on the release machine (not here), then upload to beartec-jpg/falcon-pl-releases.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd); VER=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
export RUSTFLAGS="${RUSTFLAGS:-} -C target-cpu=x86-64-v2"
case "$RUSTFLAGS" in *native*) echo "refusing target-cpu=native" >&2; exit 1;; esac
OUT="$ROOT/dist/fpl-operator-$VER-x86_64-linux"; rm -rf "$OUT"; mkdir -p "$OUT/bin"
# glibc 2.34 floor: cargo-zigbuild (pip install ziglang; cargo install cargo-zigbuild --version 0.21.8)
# or build on a glibc-2.34 host (Ubuntu 22.04 / RHEL 9) with plain cargo.
if command -v cargo-zigbuild >/dev/null || [[ -x "$HOME/.cargo/bin/cargo-zigbuild" ]]; then
  cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.34
  REL=target/x86_64-unknown-linux-gnu/release
else
  cargo build --release; REL=target/release
fi
cp $REL/fpl-operatord "$OUT/bin/"
cp scripts/fpl-operator-graph.sh scripts/fpl-operator-challenge.sh scripts/fpl-operator-slash.sh scripts/fpl-operator-attest.sh scripts/fpl-operator-pegin.sh ../fpl-bridge/slash_evidence.py ../fpl-bridge/check_graph_bindings.py ../fpl-bridge/intake/fpl_bridge_intake.py ../fpl-bridge/intake/seat_sync.py "$OUT/bin/"
mkdir -p "$OUT/systemd" && cp ../fpl-bridge/intake/*.service "$OUT/systemd/"
if [[ "${1:-}" == "--with-bitvm" ]]; then
  # vendored BitVM (pinned 7d1ca36 + tools/fpl-bridge/patches). BITVM_FEATURES default fpl-tn4-lowfee
  # (testnet4 presigned fee rate 0.125 sat/vB); set BITVM_FEATURES= for default-policy networks.
  # openssl-sys (native-tls via esplora-client/reqwest/suppaftp) under zig needs a static OpenSSL built
  # for glibc 2.34: OPENSSL_DIR=<prefix> (e.g. openssl-3.0.x `./Configure linux-x86_64 no-shared no-asm`
  # with CC=zigcc-x86_64-unknown-linux-gnu.2.34). Host libssl.a would pull newer glibc symbols.
  [[ -n "${OPENSSL_DIR:-}" ]] && export OPENSSL_STATIC=1
  # AVX-512 count 0: aws-lc (rustls) AVX-512 asm is guarded by MY_ASSEMBLER_IS_TOO_OLD_FOR_512AVX, which
  # aws-lc-sys' cc builder does not pass to .S files -> zig cc wrapper adds it; blake3 via feature "pure".
  ZCC=$(ls "$HOME"/.cache/cargo-zigbuild/*/zigcc-x86_64-unknown-linux-gnu.2.34-*.sh 2>/dev/null | tail -1)
  [[ -n "$ZCC" ]] && export FPL_ZIGCC="$ZCC" CC_x86_64_unknown_linux_gnu="$ROOT/scripts/zigcc-noavx512.sh"
  # Space-separated. fpl-tn4-timelocks (testnet4 timelocks = mainnet/28, Disprove window 72 blocks) changes
  # graph scripts: every party of a graph must use the same setting -> enable only with Scott's OK, for all kits at once.
  FEAT=${BITVM_FEATURES-fpl-tn4-lowfee}; FA=(); [[ -n "$FEAT" ]] && FA=(--features "$(for f in $FEAT; do printf 'bridge/%s,' "$f"; done | sed 's/,$//')")
  ( cd "${BITVM_SRC:-../../vendor/BitVM}" && { [[ -n "${BITVM_TARGET_DIR:-}" ]] && export CARGO_TARGET_DIR="$BITVM_TARGET_DIR"; true; } && if command -v cargo-zigbuild >/dev/null || [[ -x "$HOME/.cargo/bin/cargo-zigbuild" ]]; then Z=(zigbuild --target x86_64-unknown-linux-gnu.2.34); else Z=(build); fi && cargo "${Z[@]}" --release -j"${JOBS:-2}" -p bridge "${FA[@]}" \
      --example fpl_pregen --example fpl_ceremony_coordinator --example fpl_ceremony_participant --example fpl_challenge --example fpl_depositor_ext )
  for b in fpl_pregen fpl_ceremony_coordinator fpl_ceremony_participant fpl_challenge fpl_depositor_ext; do
    BT=${BITVM_TARGET_DIR:-${BITVM_SRC:-../../vendor/BitVM}/target}
    f=$BT/x86_64-unknown-linux-gnu/release/examples/$b; [[ -f $f ]] || f=$BT/release/examples/$b
    cp "$f" "$OUT/bin/"
  done
  echo "bitvm features: ${FEAT:-none}" > "$OUT/BUILD_INFO"
fi
# glibc ceiling check
max=$(for f in "$OUT"/bin/*; do [[ "$(head -c4 "$f" | tail -c3)" == ELF ]] && objdump -T "$f" | grep -o "GLIBC_[0-9.]*" || true; done | sort -uV | tail -1)
echo "max glibc symbol: ${max:-none}"
[[ -z "$max" ]] || [[ "$(printf '%s\nGLIBC_2.34\n' "$max" | sort -V | tail -1)" == "GLIBC_2.34" ]] || { echo "glibc too new: $max" >&2; exit 1; }
# AVX-512 must be absent (portable x86-64-v2)
z=0; for f in "$OUT"/bin/*; do [[ "$(head -c4 "$f" | tail -c3)" == ELF ]] && z=$((z + $(objdump -d "$f" | grep -c '%zmm' || true))); done
echo "AVX-512 (zmm) instructions: $z"; [[ $z == 0 ]] || { echo "AVX-512 found" >&2; exit 1; }
cp ../../../docs/bridge/OPERATOR_GUIDE.md "$OUT/" 2>/dev/null || true
cp scripts/install-operator.sh "$OUT/"
tar -C "$ROOT/dist" -czf "$OUT.tar.gz" "$(basename "$OUT")"
( cd "$ROOT/dist" && sha256sum "$(basename "$OUT").tar.gz" > "$(basename "$OUT").tar.gz.sha256" )
echo "built $OUT.tar.gz"
