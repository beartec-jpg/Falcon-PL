#!/bin/sh
# zig cc wrapper for build-release.sh: hides AVX-512 asm in aws-lc (guarded by this define) so the
# portable kit has an AVX-512 instruction count of 0. Delegates to cargo-zigbuild's zigcc.
exec "${FPL_ZIGCC:?}" -DMY_ASSEMBLER_IS_TOO_OLD_FOR_512AVX "$@"
