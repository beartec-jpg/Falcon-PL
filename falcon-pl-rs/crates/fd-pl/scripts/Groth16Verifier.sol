// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

/// Generic Groth16 verifier (BN254 precompiles 6/7/8).
/// VK is constructor-injected so test-seed and ceremony keys can share this binary.
/// Proof encoding matches ark-groth16 0.4: G2 Fq2 as (c1, c0).
contract Groth16Verifier {
    uint256 constant PRIME_Q =
        21888242871839275222246405745257275088696311157297823662689037894645226208583;
    uint256 constant SNARK_SCALAR =
        21888242871839275222246405745257275088548364400416034343698204186575808495617;

    uint256[2] public alpha;
    uint256[2][2] public beta;
    uint256[2][2] public gamma;
    uint256[2][2] public delta;
    uint256[2][] public ic;

    constructor(
        uint256[2] memory alpha_,
        uint256[2][2] memory beta_,
        uint256[2][2] memory gamma_,
        uint256[2][2] memory delta_,
        uint256[2][] memory ic_
    ) {
        require(ic_.length >= 2, "ic");
        alpha = alpha_;
        beta = beta_;
        gamma = gamma_;
        delta = delta_;
        for (uint256 i = 0; i < ic_.length; i++) {
            ic.push(ic_[i]);
        }
    }

    function icLength() external view returns (uint256) {
        return ic.length;
    }

    function verifyProof(
        uint256[2] calldata a,
        uint256[2][2] calldata b,
        uint256[2] calldata c,
        uint256[] calldata input
    ) external view returns (bool) {
        require(input.length + 1 == ic.length, "n");
        uint256[2] memory vkx = ic[0];
        for (uint256 i = 0; i < input.length; i++) {
            require(input[i] < SNARK_SCALAR, "fr");
            vkx = _ecAdd(vkx, _ecMul(ic[i + 1], input[i]));
        }
        // e(-A, B) * e(alpha, beta) * e(vk_x, gamma) * e(C, delta) == 1
        return _pairing4(
            _negate(a),
            b,
            alpha,
            beta,
            vkx,
            gamma,
            c,
            delta
        );
    }

    function _negate(uint256[2] memory p) internal pure returns (uint256[2] memory r) {
        if (p[0] == 0 && p[1] == 0) {
            return p;
        }
        r[0] = p[0];
        r[1] = PRIME_Q - (p[1] % PRIME_Q);
    }

    function _ecAdd(uint256[2] memory p1, uint256[2] memory p2) internal view returns (uint256[2] memory r) {
        uint256[4] memory input;
        input[0] = p1[0];
        input[1] = p1[1];
        input[2] = p2[0];
        input[3] = p2[1];
        bool ok;
        assembly {
            ok := staticcall(gas(), 6, input, 0x80, r, 0x40)
        }
        require(ok, "add");
    }

    function _ecMul(uint256[2] memory p, uint256 s) internal view returns (uint256[2] memory r) {
        uint256[3] memory input;
        input[0] = p[0];
        input[1] = p[1];
        input[2] = s;
        bool ok;
        assembly {
            ok := staticcall(gas(), 7, input, 0x60, r, 0x40)
        }
        require(ok, "mul");
    }

    function _pairing4(
        uint256[2] memory a1,
        uint256[2][2] memory a2,
        uint256[2] memory b1,
        uint256[2][2] memory b2,
        uint256[2] memory c1,
        uint256[2][2] memory c2,
        uint256[2] memory d1,
        uint256[2][2] memory d2
    ) internal view returns (bool) {
        uint256[24] memory input;
        input[0] = a1[0];
        input[1] = a1[1];
        input[2] = a2[0][0];
        input[3] = a2[0][1];
        input[4] = a2[1][0];
        input[5] = a2[1][1];
        input[6] = b1[0];
        input[7] = b1[1];
        input[8] = b2[0][0];
        input[9] = b2[0][1];
        input[10] = b2[1][0];
        input[11] = b2[1][1];
        input[12] = c1[0];
        input[13] = c1[1];
        input[14] = c2[0][0];
        input[15] = c2[0][1];
        input[16] = c2[1][0];
        input[17] = c2[1][1];
        input[18] = d1[0];
        input[19] = d1[1];
        input[20] = d2[0][0];
        input[21] = d2[0][1];
        input[22] = d2[1][0];
        input[23] = d2[1][1];
        uint256[1] memory out;
        bool ok;
        assembly {
            ok := staticcall(gas(), 8, input, 768, out, 0x20)
        }
        require(ok, "pair");
        return out[0] != 0;
    }
}
