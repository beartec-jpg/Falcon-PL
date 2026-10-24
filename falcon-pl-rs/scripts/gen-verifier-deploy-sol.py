#!/usr/bin/env python3
"""Emit Groth16VerifierDeploy.sol with vk constants from vk.sol.json."""
from __future__ import annotations

import json
import sys
from pathlib import Path


def main() -> int:
    vk_path = Path(sys.argv[1])
    out_path = Path(sys.argv[2])
    vk = json.loads(vk_path.read_text())
    ic_rows = ",\n            ".join(f"[{a}, {b}]" for a, b in vk["ic"])
    beta = vk["beta"]
    gamma = vk["gamma"]
    delta = vk["delta"]
    src = f"""// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import {{Groth16Verifier}} from "./Groth16Verifier.sol";

/// Generated from vk.sol.json — single-tx deploy helper.
contract Groth16VerifierDeploy {{
    Groth16Verifier public immutable verifier;

    constructor() {{
        uint256[2] memory alpha = [{vk["alpha"][0]}, {vk["alpha"][1]}];
        uint256[2][2] memory beta = [
            [{beta[0][0]}, {beta[0][1]}],
            [{beta[1][0]}, {beta[1][1]}]
        ];
        uint256[2][2] memory gamma = [
            [{gamma[0][0]}, {gamma[0][1]}],
            [{gamma[1][0]}, {gamma[1][1]}]
        ];
        uint256[2][2] memory delta = [
            [{delta[0][0]}, {delta[0][1]}],
            [{delta[1][0]}, {delta[1][1]}]
        ];
        uint256[2][{len(vk["ic"])}] memory icFixed = [
            {ic_rows}
        ];
        uint256[2][] memory ic = new uint256[2][](icFixed.length);
        for (uint256 i = 0; i < icFixed.length; i++) {{
            ic[i] = icFixed[i];
        }}
        verifier = new Groth16Verifier(alpha, beta, gamma, delta, ic);
    }}
}}
"""
    out_path.write_text(src)
    print(out_path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
