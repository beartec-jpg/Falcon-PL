# NOTICE — Falcon PL node

Copyright (c) 2025-2026 BEARTEC. All rights reserved. See `LICENSE`.

Original code in this repository was written by or for BEARTEC (including
AI-assisted commits made under the BEARTEC GitHub account `beartec-jpg`).

## Third-party software

### Linked Rust dependencies (crates.io, not vendored)
Resolved from `falcon-pl-rs/Cargo.lock` (`cargo metadata`), 203 crates. All
are permissive; none is copyleft-only.

| Licence (SPDX) | Crates |
|---|---|
| MIT and/or Apache-2.0 (incl. dual / LLVM-exception) | ~180 (e.g. tokio, serde, `ark-*` arkworks, `pqcrypto-falcon`) |
| CC0-1.0 | `bitcoin`, `bitcoin_hashes`, `secp256k1`, `secp256k1-sys`, `base58ck`, `bitcoin-*`, `hex-conservative` |
| BSD-3-Clause | `subtle` |
| ISC / Apache-2.0 AND ISC | `untrusted`, `ring` |
| Unicode-3.0 (with MIT OR Apache-2.0) | `unicode-ident` |
| MITNFA | `hex_lit` |
| BSD-2-Clause / Zlib / Unlicense / MIT-0 options | various (dual-licensed, permissive option used) |
| MIT OR Apache-2.0 OR LGPL-2.1-or-later | `r-efi` (used under MIT / Apache-2.0) |

`pqcrypto-falcon` bundles the PQClean Falcon reference implementation
(MIT).

### Fetched / patched third-party code (not compiled into the node)
- **BitVM** (https://github.com/BitVM/BitVM), MIT. Fetched into
  `falcon-pl-rs/vendor/BitVM` by `scripts/fetch-bitvm.sh` (not committed).
  Patch and licence copy: `falcon-pl-rs/tools/fpl-bridge/patches/`
  (`BitVM-LICENSE-MIT`). These files keep their original licence.

### Solidity contracts
`falcon-pl-rs/crates/fd-pl/scripts/*.sol` and
`archive/dag-python/contracts/evm/*.sol` carry `SPDX-License-Identifier: MIT`
headers. They have been left unchanged pending a decision by BEARTEC.

### Submodule
`falcon-dag-rs` is a git submodule (gitlink) and is not covered by this notice.
