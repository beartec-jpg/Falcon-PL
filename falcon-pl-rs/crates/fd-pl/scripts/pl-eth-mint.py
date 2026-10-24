#!/usr/bin/env python3
"""Mint FPL ETH/USDC from a FalconQc Deposit via the Sepolia light client.

Usage:
  pl-eth-mint.py --account sally --txid 0x... [--asset ETH|USDC]

Signs as watcher-eth (FPL_WATCHER_ACCOUNT), not watcher-eth-lc (the LC
relayer daemon) and not watcher-browser. dest20 must match
sha256(lowercase account)[:20] on the log. Does not hold vault keys.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

ROOT = os.environ.get("PL_ROOT", str(os.path.expanduser("~/falcon-pl-public-testnet-2300")))
RELAYER = os.environ.get(
    "FPL_ETH_LC_RELAYER",
    str(Path(ROOT) / "scripts" / "pl-eth-lc-relayer.py"),
)
if not Path(RELAYER).exists():
    RELAYER = str(
        Path.home() / "falcon-pl/falcon-pl-rs/crates/fd-pl/scripts/pl-eth-lc-relayer.py"
    )

os.execv(sys.executable, [sys.executable, RELAYER, "mint", *sys.argv[1:]])
