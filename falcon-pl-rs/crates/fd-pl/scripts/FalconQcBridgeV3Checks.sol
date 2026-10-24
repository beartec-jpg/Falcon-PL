// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

/// @title FalconQcBridgeV3Checks
/// @notice Stateless helper for FalconQcBridgeV3, deployed as its own transaction so each
///         deployment stays well under the EIP-7825 per-transaction gas cap (2**24).
///         No storage, no immutables, no owner, no constructor arguments, no upgrade path.
///         Every address it reads is passed in by FalconQcBridgeV3 from its own immutables.
/// @dev FalconQcBridgeV3's constructor requires
///      `checks.codehash == keccak256(type(FalconQcBridgeV3Checks).runtimeCode)`, so V3 can only
///      be bound to this exact code (same source and compiler settings as V3's own build).
///      Anyone may call these view functions; they hold nothing and change nothing.

interface IERC20 {
    function balanceOf(address account) external view returns (uint256);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
    function transfer(address to, uint256 amount) external returns (bool);
}

interface IGroth16Verifier {
    function verifyProof(
        uint256[2] calldata a,
        uint256[2][2] calldata b,
        uint256[2] calldata c,
        uint256[] calldata input
    ) external view returns (bool);
}

/// V2 `headers` / `claims` getters. Claim layout matches FalconQcBridgeV2.Claim:
/// dest, amount, usdc, readyBlock, fplHeight, leaf, open, taken.
interface IFalconQcBridgeV2View {
    function fplTip() external view returns (uint64);
    function currentCommitteeRoot() external view returns (bytes32);
    function currentLeaves(uint256 index) external view returns (bytes32);
    function fplNetworkId() external view returns (uint32);
    function usdc() external view returns (address);
    function verifier() external view returns (address);

    function headers(uint64 height)
        external
        view
        returns (
            bytes32 hash,
            bytes32 parent,
            bytes32 innerHash,
            bytes32 claimRoot,
            bytes32 nextCommitteeRoot,
            bool finalized
        );

    function claims(bytes32 noteId)
        external
        view
        returns (
            address dest,
            uint256 amount,
            bool usdc,
            uint64 readyBlock,
            uint64 fplHeight,
            bytes32 leaf,
            bool open,
            bool taken
        );
}

/// V1 `claims` getter. Claim layout matches FalconQcBridge.Claim:
/// dest, amount, usdc, readyBlock, fplHeight, leaf, open, challenged, taken.
interface IFalconQcBridgeV1View {
    function claims(bytes32 noteId)
        external
        view
        returns (
            address dest,
            uint256 amount,
            bool usdc,
            uint64 readyBlock,
            uint64 fplHeight,
            bytes32 leaf,
            bool open,
            bool challenged,
            bool taken
        );
}

/// DestLock `claims` getter. Claim layout matches FalconDestLock.Claim:
/// dest, amount, usdc, readyBlock, open, taken.
interface IDestLockView {
    function claims(bytes32 noteId)
        external
        view
        returns (
            address dest,
            uint256 amount,
            bool usdc,
            uint64 readyBlock,
            bool open,
            bool taken
        );
}

contract FalconQcBridgeV3Checks {
    bytes public constant SEAL_PREFIX = "pl-ledger:v4-seal";

    /// @notice The seal opening and the four Groth16 checks of `FalconQcBridgeV3.submitHeaderWithProofs`.
    /// @dev `hash` must open as `sha256(SEAL_PREFIX || innerHash || nextCommitteeRoot || claimRoot)`
    ///      (`"open"`). Four distinct indices `< 6`. Verifier inputs are byte-for-byte V2, with
    ///      `input[5] = currentCommitteeRoot` (the committee V3 already trusts, not the new root).
    ///      Reverts `"open"`, `"idx"`, `"dup"` or `"proof"` (or whatever the verifier reverts with);
    ///      returns true only when the seal opens and all four proofs verify.
    function checkHeaderProofs(
        IGroth16Verifier verifier,
        uint32 fplNetworkId,
        bytes32 currentCommitteeRoot,
        uint64 height,
        bytes32 hash,
        bytes32 parent,
        bytes32 innerHash,
        bytes32 claimRoot,
        bytes32 nextCommitteeRoot,
        bytes32[6] calldata nextLeaves,
        uint64[4] calldata idx,
        uint256[2][4] calldata a,
        uint256[2][2][4] calldata b,
        uint256[2][4] calldata c
    ) external view returns (bool) {
        require(
            sha256(abi.encodePacked(SEAL_PREFIX, innerHash, nextCommitteeRoot, claimRoot)) == hash,
            "open"
        );
        uint256[] memory input = new uint256[](16);
        input[0] = height;
        input[1] = uint256(parent) >> 128;
        input[2] = uint256(parent) & ((uint256(1) << 128) - 1);
        input[3] = uint256(hash) >> 128;
        input[4] = uint256(hash) & ((uint256(1) << 128) - 1);
        input[5] = uint256(currentCommitteeRoot);
        input[6] = uint256(nextCommitteeRoot);
        input[7] = 1;
        input[8] = fplNetworkId;
        for (uint256 j = 0; j < 6; j++) {
            input[10 + j] = uint256(nextLeaves[j]);
        }
        uint256 seen;
        for (uint256 i = 0; i < 4; i++) {
            require(idx[i] < 6, "idx");
            uint256 bit = uint256(1) << idx[i];
            require(seen & bit == 0, "dup");
            seen |= bit;
            input[9] = idx[i];
            require(verifier.verifyProof(a[i], b[i], c[i], input), "proof");
        }
        return true;
    }

    /// @notice Claim checks of `FalconQcBridgeV3.openClaim`, in V3's order: the leaf
    ///         `withdrawLeaf(noteId, dest, amount, isUsdc)` must sit under `claimRoot` (`"merkle"`),
    ///         the note must not be one of V2's anchor notes (`"anchor"`, see `inAnchorSet`), and it
    ///         must not be recorded on V2, V1 or DestLock (`dest != 0`):
    ///         `"paid-v2"`, `"paid-v1"`, `"paid-dl"`. Returns the leaf.
    function checkClaim(
        IFalconQcBridgeV2View v2,
        IFalconQcBridgeV1View v1,
        IDestLockView destLock,
        bytes32 noteId,
        address dest,
        uint256 amount,
        bool isUsdc,
        bytes32[] calldata proof,
        uint32 index,
        bytes32 claimRoot,
        address anchorSet
    ) external view returns (bytes32 leaf) {
        leaf = withdrawLeaf(noteId, dest, amount, isUsdc);
        require(merkleRoot(leaf, proof, index) == claimRoot, "merkle");
        require(!inAnchorSet(anchorSet, noteId), "anchor");
        (address destV2, , , , , , , ) = v2.claims(noteId);
        require(destV2 == address(0), "paid-v2");
        (address destV1, , , , , , , , ) = v1.claims(noteId);
        require(destV1 == address(0), "paid-v1");
        (address destDl, , , , , ) = destLock.claims(noteId);
        require(destDl == address(0), "paid-dl");
    }

    /// @notice Bytes per `FalconQcBridgeV3AnchorSet` entry: noteId (32) || dest (20) ||
    ///         amount (uint64 big-endian, 8) || isUsdc (0x00 / 0x01, 1).
    uint256 public constant ANCHOR_ENTRY_BYTES = 61;

    /// @dev The set's code (`0x00 || entries`) and its entry count. `address(0)` is the empty set.
    ///      Reverts `"set"` on any other code shape or a flag byte other than 0x00 / 0x01.
    function _anchorCode(address set) internal view returns (bytes memory code, uint256 n) {
        if (set == address(0)) return (code, 0);
        code = set.code;
        require(code.length > 1 && (code.length - 1) % ANCHOR_ENTRY_BYTES == 0 && code[0] == 0x00, "set");
        n = (code.length - 1) / ANCHOR_ENTRY_BYTES;
        require(n <= 4096, "set");
    }

    function _anchorEntry(bytes memory code, uint256 i)
        internal
        pure
        returns (bytes32 noteId, address dest, uint64 amount, bool isUsdc)
    {
        uint256 off = 1 + i * ANCHOR_ENTRY_BYTES;
        uint256 flag;
        assembly {
            let ptr := add(add(code, 32), off)
            noteId := mload(ptr)
            dest := shr(96, mload(add(ptr, 32)))
            amount := shr(192, mload(add(ptr, 52)))
            flag := shr(248, mload(add(ptr, 60)))
        }
        require(flag <= 1, "set");
        isUsdc = flag == 1;
    }

    /// @notice Tree root of an anchor set and its entry count. Each entry is a withdrawal
    ///         preimage, and its leaf is computed here with `withdrawLeaf` (71-byte preimage), so
    ///         an entry can never stand for an internal node: a set of internal hashes (e.g. the
    ///         two children of the root) cannot reproduce the root without a sha256 collision.
    ///         Tree code is FPL's (`evm_spv::build_tree`: sha256 pairs, an odd layer repeats its
    ///         last node, one leaf is its own root, empty is zero). V3's constructor requires the
    ///         result to equal V2's anchor `claimRoot`.
    function anchorSetRoot(address set) external view returns (bytes32 root, uint256 n) {
        bytes memory code;
        (code, n) = _anchorCode(set);
        if (n == 0) return (bytes32(0), 0);
        bytes32[] memory layer = new bytes32[](n);
        for (uint256 i = 0; i < n; i++) {
            (bytes32 noteId, address dest, uint64 amount, bool isUsdc) = _anchorEntry(code, i);
            layer[i] = withdrawLeaf(noteId, dest, amount, isUsdc);
        }
        uint256 len = n;
        while (len > 1) {
            uint256 m = (len + 1) / 2;
            for (uint256 i = 0; i < m; i++) {
                bytes32 l = layer[2 * i];
                bytes32 r = 2 * i + 1 < len ? layer[2 * i + 1] : l;
                layer[i] = sha256(abi.encodePacked(l, r));
            }
            len = m;
        }
        root = layer[0];
    }

    /// @notice True when `noteId` is one of the anchor set's notes (any position, any dest/amount).
    function inAnchorSet(address set, bytes32 noteId) public view returns (bool) {
        (bytes memory code, uint256 n) = _anchorCode(set);
        for (uint256 i = 0; i < n; i++) {
            bytes32 x;
            assembly {
                x := mload(add(add(code, 33), mul(i, 61)))
            }
            if (x == noteId) return true;
        }
        return false;
    }

    /// @dev Same bytes as V1/V2 and `evm_spv::withdraw_leaf`:
    ///      sha256("evm-wd-v1|" || note32 || dest20 || uint64_be || 0x01/0x00).
    ///      `amount` must fit in uint64 (V3 checks this before calling).
    function withdrawLeaf(bytes32 noteId, address dest, uint256 amount, bool isUsdc) public pure returns (bytes32) {
        return sha256(
            abi.encodePacked(
                "evm-wd-v1|",
                noteId,
                dest,
                uint64(amount),
                isUsdc ? bytes1(0x01) : bytes1(0x00)
            )
        );
    }

    /// @notice Root of a sha256 Merkle path (same fold as V1/V2 and `pl-eth-claim-proof.py`).
    /// @dev Bounds (Review Office nit 4): at most 32 levels (`index` is uint32), and `index` must
    ///      fit in `proof.length` bits. Canonical index only (`"pad"`): FPL pads an odd layer by
    ///      repeating its last node, so a padded copy is a right child equal to its left sibling.
    ///      Refusing that makes a (leaf, root) pair have exactly one accepted index. (V3's anchor rule
    ///      does not depend on indices: it checks the leaf against `anchorSet`.) Real nodes never
    ///      repeat (notes are unique).
    ///      No extra domain separation: the leaf is always computed on-chain by `withdrawLeaf`
    ///      from a 71-byte `evm-wd-v1|` preimage, which cannot equal a 64-byte inner-node preimage
    ///      without a sha256 collision, and FPL's claim tree format stays unchanged.
    function merkleRoot(bytes32 leaf, bytes32[] calldata proof, uint32 index) public pure returns (bytes32 h) {
        require(proof.length <= 32 && (uint256(index) >> proof.length) == 0, "depth");
        h = leaf;
        uint32 idx = index;
        for (uint256 i = 0; i < proof.length; i++) {
            bytes32 p = proof[i];
            if (idx % 2 == 0) {
                h = sha256(abi.encodePacked(h, p));
            } else {
                require(p != h, "pad");
                h = sha256(abi.encodePacked(p, h));
            }
            idx /= 2;
        }
    }
}

/// @title FalconQcBridgeV3AnchorSet
/// @notice Data only: its runtime code is 0x00 (STOP, so calls do nothing) followed by one 61-byte
///         entry per note V2's anchor header commits, in FPL leaf order: noteId || dest ||
///         uint64 amount (big-endian) || isUsdc. No storage, no functions, no owner.
///         `FalconQcBridgeV3Checks.anchorSetRoot` rebuilds the leaves and their root from it;
///         V3's constructor binds it to V2's anchor root.
contract FalconQcBridgeV3AnchorSet {
    constructor(bytes32[] memory noteIds, address[] memory dests, uint64[] memory amounts, bool[] memory isUsdc) {
        uint256 n = noteIds.length;
        require(n > 0 && dests.length == n && amounts.length == n && isUsdc.length == n, "len");
        bytes memory code = abi.encodePacked(bytes1(0x00));
        for (uint256 i = 0; i < n; i++) {
            code = abi.encodePacked(code, noteIds[i], dests[i], amounts[i], isUsdc[i] ? bytes1(0x01) : bytes1(0x00));
        }
        assembly {
            return(add(code, 32), mload(code))
        }
    }
}
