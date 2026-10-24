#!/usr/bin/env bash
# Portable, reproducible release build of falcon-pl-node + falcon-pl-ctl (linux x86_64).
#
#   bash falcon-pl-rs/crates/fd-pl/scripts/release/build-portable.sh [OUT_DIR]
#
# Rules (docs/ops/HANDOVER.md §3 "Builds"):
#   * RUSTFLAGS / CARGO_ENCODED_RUSTFLAGS are UNSET. Never `-C target-cpu=native`.
#   * After the build the AVX-512 instruction count must be 0 (v10 Skylake SIGILL lesson).
#   * Same source + same container image + same path => same sha256 (CI builds twice and compares).
#
# Output (OUT_DIR, default ./dist):
#   falcon-pl-node, falcon-pl-ctl                         raw binaries
#   falcon-pl-<ver>-linux-x86_64.tar.gz                   deterministic tarball (sorted, mtime=SOURCE_DATE_EPOCH, uid 0)
#   install-falcon-node.sh, network-2300.env             installer + network profile (copied from scripts/)
#   BUILDINFO.txt                                         commit, toolchain, image, glibc floor, avx512 count
#   SHA256SUMS                                            sha256 of every file above
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TOOLS_FDPL="$(cd "$SCRIPT_DIR/../.." && pwd)"      # crates/fd-pl of the TOOLING checkout (installer, profile)
TOOLS_REPO="$(cd "$TOOLS_FDPL/../../.." && pwd)"
# Source tree to build: this checkout, or FALCON_SRC (e.g. the exact commit a live release runs).
REPO="$(cd "${FALCON_SRC:-$TOOLS_REPO}" && pwd)"
RS="$REPO/falcon-pl-rs"
FDPL="$RS/crates/fd-pl"
OUT="${1:-$PWD/dist}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

die() { echo "build-portable: $*" >&2; exit 1; }
[[ -f "$FDPL/src/lib.rs" ]] || die "no fd-pl source at $FDPL"

[[ "$(uname -m)" == "x86_64" ]] || die "x86_64 build host required (got $(uname -m))"
command -v cargo >/dev/null || die "cargo not found"
command -v objdump >/dev/null || die "objdump (binutils) not found"

# 1) Portable: no RUSTFLAGS of any kind, no target-cpu in any cargo config.
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS
for cfg in "$RS/.cargo/config.toml" "$RS/.cargo/config" "$REPO/.cargo/config.toml" "${CARGO_HOME:-$HOME/.cargo}/config.toml"; do
  if [[ -f "$cfg" ]] && grep -q 'target-cpu' "$cfg"; then
    die "$cfg sets target-cpu; refuse (portable builds only)"
  fi
done

COMMIT="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo unknown)"
DIRTY="$({ git -C "$REPO" status --porcelain --untracked-files=no 2>/dev/null || true; } | wc -l | tr -d ' ')"
if [[ "$COMMIT" == unknown ]]; then
  echo "warning: git metadata unavailable ($(git -C "$REPO" rev-parse HEAD 2>&1 | head -1))" >&2
fi
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$REPO" log -1 --format=%ct 2>/dev/null || echo 0)}"
export CARGO_INCREMENTAL=0
VERSION="$(sed -nE 's/^pub const PRODUCT_VERSION: &str = "([^"]+)";/\1/p' "$FDPL/src/lib.rs" | head -1)"
[[ -n "$VERSION" ]] || die "cannot read PRODUCT_VERSION from src/lib.rs"

echo "build-portable: version=$VERSION commit=$COMMIT dirty_files=$DIRTY out=$OUT"
(
  cd "$RS"
  cargo build --release --locked -p fd-pl --bin falcon-pl-node --bin falcon-pl-ctl
)

TGT="${CARGO_TARGET_DIR:-$RS/target}/release"
for b in falcon-pl-node falcon-pl-ctl; do
  install -m 0755 "$TGT/$b" "$OUT/$b"
done

# 2) AVX-512 scan (zmm, opmask k0-k7, xmm/ymm16-31). Must be 0.
avx_total=0
declare -A AVX
for b in falcon-pl-node falcon-pl-ctl; do
  n="$(objdump -d --no-show-raw-insn "$OUT/$b" | grep -cE '%zmm|%k[0-7]|%[xy]mm(1[6-9]|2[0-9]|3[01])' || true)"
  AVX[$b]="$n"
  avx_total=$((avx_total + n))
  echo "build-portable: $b avx512_insns=$n"
done
[[ "$avx_total" -eq 0 ]] || die "AVX-512 instructions found ($avx_total); portable build broken"

# 3) Version check (binaries must report the product version). Sources older than the --version flag
#    (e.g. a live release commit) get a --help smoke test instead; recorded in BUILDINFO.
VERSION_FLAG=present
for b in falcon-pl-node falcon-pl-ctl; do
  if "$OUT/$b" --version >/dev/null 2>&1; then
    got="$("$OUT/$b" --version 2>/dev/null | awk '{print $NF}')"
    [[ "$got" == "$VERSION" ]] || die "$b --version = '$got', want $VERSION"
  else
    "$OUT/$b" --help >/dev/null 2>&1 || die "$b does not run (--help failed)"
    VERSION_FLAG=absent
  fi
done

# 4) glibc floor (highest GLIBC_x.y symbol version referenced).
glibc_floor() { objdump -T "$1" | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sed 's/GLIBC_//' | sort -Vu | tail -1; }
GLIBC_NODE="$(glibc_floor "$OUT/falcon-pl-node")"
GLIBC_CTL="$(glibc_floor "$OUT/falcon-pl-ctl")"
GLIBC_MIN="$(printf '%s\n%s\n' "$GLIBC_NODE" "$GLIBC_CTL" | sort -V | tail -1)"

# 5) Installer + network profile travel with the release.
install -m 0755 "$TOOLS_FDPL/scripts/install-falcon-node.sh" "$OUT/install-falcon-node.sh"
install -m 0644 "$TOOLS_FDPL/scripts/network-2300.env" "$OUT/network-2300.env"

# 6) Deterministic tarball.
TAR="falcon-pl-${VERSION}-linux-x86_64.tar.gz"
STAGE="$(mktemp -d)"
mkdir -p "$STAGE/falcon-pl-${VERSION}"
cp "$OUT/falcon-pl-node" "$OUT/falcon-pl-ctl" "$STAGE/falcon-pl-${VERSION}/"
tar --sort=name --mtime="@${SOURCE_DATE_EPOCH}" --owner=0 --group=0 --numeric-owner \
    -C "$STAGE" -cf - "falcon-pl-${VERSION}" | gzip -n -9 >"$OUT/$TAR"
rm -rf "$STAGE"

RUSTC_V="$(rustc -V)"
CARGO_V="$(cargo -V)"
cat >"$OUT/BUILDINFO.txt" <<INFO
product_version=$VERSION
commit=$COMMIT
tooling_commit=$(git -C "$TOOLS_REPO" rev-parse HEAD 2>/dev/null || echo unknown)
version_flag=$VERSION_FLAG
dirty_tracked_files=$DIRTY
source_date_epoch=$SOURCE_DATE_EPOCH
rustc=$RUSTC_V
cargo=$CARGO_V
build_image=${BUILD_IMAGE:-host}
rustflags=unset
target=x86_64-unknown-linux-gnu
avx512_insns_node=${AVX[falcon-pl-node]}
avx512_insns_ctl=${AVX[falcon-pl-ctl]}
glibc_min=$GLIBC_MIN
cargo_lock_sha256=$(sha256sum "$RS/Cargo.lock" | awk '{print $1}')
INFO

(
  cd "$OUT"
  sha256sum falcon-pl-node falcon-pl-ctl "$TAR" install-falcon-node.sh network-2300.env BUILDINFO.txt >SHA256SUMS
  cat SHA256SUMS
)
echo "build-portable: OK ($OUT)"
