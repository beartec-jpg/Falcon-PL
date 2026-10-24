// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "./FalconQcBridgeV2.sol";

contract AlwaysTrueVerifier is IGroth16Verifier {
    function verifyProof(
        uint256[2] calldata,
        uint256[2][2] calldata,
        uint256[2] calldata,
        uint256[] calldata input
    ) external pure returns (bool) {
        return input.length == 16;
    }
}

contract AlwaysFalseVerifier is IGroth16Verifier {
    function verifyProof(
        uint256[2] calldata,
        uint256[2][2] calldata,
        uint256[2] calldata,
        uint256[] calldata
    ) external pure returns (bool) {
        return false;
    }
}

contract FalconQcBridgeV2Test is Test {
    AlwaysTrueVerifier ver;
    FalconQcBridgeV2 br;
    bytes32 inner;
    bytes32 claim;
    bytes32 nextRoot;
    bytes32[6] leaves;
    bytes32 genesis;

    function setUp() public {
        ver = new AlwaysTrueVerifier();
        inner = keccak256("inner");
        claim = keccak256("claim");
        nextRoot = keccak256("next");
        for (uint256 i = 0; i < 6; i++) {
            leaves[i] = keccak256(abi.encodePacked("leaf", i));
        }
        genesis = sha256(abi.encodePacked(bytes("pl-ledger:v4-seal"), inner, nextRoot, claim));
        br = new FalconQcBridgeV2(
            address(0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238),
            keccak256("ethLock"),
            keccak256("usdcLock"),
            genesis,
            address(ver),
            2300,
            100,
            inner,
            claim,
            nextRoot,
            leaves
        );
    }

    function _dummyProofs()
        internal
        pure
        returns (uint256[2][4] memory a, uint256[2][2][4] memory b, uint256[2][4] memory c)
    {}

    function testConstructorTip() public view {
        assertEq(br.fplTip(), 100);
        assertEq(br.currentCommitteeRoot(), nextRoot);
        assertEq(br.proofsNeeded(), 4);
    }

    function testOpeningRejectsMutatedClaim() public {
        bytes32 inner2 = keccak256("inner2");
        bytes32 next2 = keccak256("next2");
        bytes32 claim2 = keccak256("claim2");
        bytes32 hash = sha256(abi.encodePacked(bytes("pl-ledger:v4-seal"), inner2, next2, claim2));
        bytes32 badClaim = keccak256("bad");
        uint64[4] memory idx = [uint64(0), 1, 2, 3];
        bytes32[6] memory nl = leaves;
        (uint256[2][4] memory a, uint256[2][2][4] memory b, uint256[2][4] memory c) = _dummyProofs();
        vm.expectRevert(bytes("open"));
        br.submitHeaderWithProofs(101, hash, genesis, inner2, badClaim, next2, nl, idx, a, b, c);
    }

    function testSeqRejectsJump() public {
        bytes32 inner2 = keccak256("inner2");
        bytes32 next2 = keccak256("next2");
        bytes32 claim2 = keccak256("claim2");
        bytes32 hash = sha256(abi.encodePacked(bytes("pl-ledger:v4-seal"), inner2, next2, claim2));
        uint64[4] memory idx = [uint64(0), 1, 2, 3];
        (uint256[2][4] memory a, uint256[2][2][4] memory b, uint256[2][4] memory c) = _dummyProofs();
        vm.expectRevert(bytes("seq"));
        br.submitHeaderWithProofs(102, hash, genesis, inner2, claim2, next2, leaves, idx, a, b, c);
    }

    function testIdxMustBeLt6() public {
        bytes32 inner2 = keccak256("inner2");
        bytes32 next2 = keccak256("next2");
        bytes32 claim2 = keccak256("claim2");
        bytes32 hash = sha256(abi.encodePacked(bytes("pl-ledger:v4-seal"), inner2, next2, claim2));
        uint64[4] memory idx = [uint64(0), 1, 2, 6];
        (uint256[2][4] memory a, uint256[2][2][4] memory b, uint256[2][4] memory c) = _dummyProofs();
        vm.expectRevert(bytes("idx"));
        br.submitHeaderWithProofs(101, hash, genesis, inner2, claim2, next2, leaves, idx, a, b, c);
    }

    function testSequentialSubmitRotates() public {
        bytes32 inner2 = keccak256("inner2");
        bytes32 next2 = keccak256("next2");
        bytes32 claim2 = keccak256("claim2");
        bytes32 hash = sha256(abi.encodePacked(bytes("pl-ledger:v4-seal"), inner2, next2, claim2));
        uint64[4] memory idx = [uint64(0), 1, 2, 3];
        bytes32[6] memory nl;
        for (uint256 i = 0; i < 6; i++) {
            nl[i] = keccak256(abi.encodePacked("n", i));
        }
        (uint256[2][4] memory a, uint256[2][2][4] memory b, uint256[2][4] memory c) = _dummyProofs();
        br.submitHeaderWithProofs(101, hash, genesis, inner2, claim2, next2, nl, idx, a, b, c);
        assertEq(br.fplTip(), 101);
        assertEq(br.currentCommitteeRoot(), next2);
    }

    function testDupIdxReverts() public {
        bytes32 inner2 = keccak256("inner2");
        bytes32 next2 = keccak256("next2");
        bytes32 claim2 = keccak256("claim2");
        bytes32 hash = sha256(abi.encodePacked(bytes("pl-ledger:v4-seal"), inner2, next2, claim2));
        uint64[4] memory idx = [uint64(0), 1, 2, 0];
        (uint256[2][4] memory a, uint256[2][2][4] memory b, uint256[2][4] memory c) = _dummyProofs();
        vm.expectRevert(bytes("dup"));
        br.submitHeaderWithProofs(101, hash, genesis, inner2, claim2, next2, leaves, idx, a, b, c);
    }

    function testConstructorClaimRootUsable() public view {
        (,,, bytes32 storedClaim,, bool finalized) = br.headers(100);
        assertEq(storedClaim, claim);
        assertTrue(finalized);
    }

    function testMutatedNextLeavesFailsVerify() public {
        CheckLeaf0Verifier chk = new CheckLeaf0Verifier(uint256(leaves[0]));
        FalconQcBridgeV2 br2 = new FalconQcBridgeV2(
            address(0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238),
            keccak256("ethLock"),
            keccak256("usdcLock"),
            genesis,
            address(chk),
            2300,
            100,
            inner,
            claim,
            nextRoot,
            leaves
        );
        bytes32 inner2 = keccak256("inner2");
        bytes32 next2 = keccak256("next2");
        bytes32 claim2 = keccak256("claim2");
        bytes32 hash = sha256(abi.encodePacked(bytes("pl-ledger:v4-seal"), inner2, next2, claim2));
        uint64[4] memory idx = [uint64(0), 1, 2, 3];
        bytes32[6] memory bad = leaves;
        bad[0] = keccak256("mutated");
        (uint256[2][4] memory a, uint256[2][2][4] memory b, uint256[2][4] memory c) = _dummyProofs();
        vm.expectRevert(bytes("proof"));
        br2.submitHeaderWithProofs(101, hash, genesis, inner2, claim2, next2, bad, idx, a, b, c);
    }

    function testTakeOnlyDest() public {
        bytes32 note = keccak256("note");
        address payable dest = payable(address(0xBEEF));
        uint256 amount = 1 ether;
        bytes32 leaf = br.withdrawLeaf(note, dest, amount, false);
        bytes32[6] memory startLeaves = leaves;
        bytes32 gen = sha256(abi.encodePacked(bytes("pl-ledger:v4-seal"), inner, nextRoot, leaf));
        FalconQcBridgeV2 br2 = new FalconQcBridgeV2(
            address(0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238),
            keccak256("ethLock"),
            keccak256("usdcLock"),
            gen,
            address(ver),
            2300,
            100,
            inner,
            leaf,
            nextRoot,
            startLeaves
        );
        vm.deal(address(this), amount);
        br2.depositEth{value: amount}(bytes20(uint160(address(dest))));
        bytes32[] memory proof;
        br2.openClaim(note, dest, amount, false, proof, 0, 100);
        vm.expectRevert(bytes("dest"));
        br2.take(note);
        uint256 before = dest.balance;
        vm.prank(dest);
        br2.take(note);
        assertEq(dest.balance, before + amount);
    }

    function testOpenClaimWrongHeight() public {
        bytes32[] memory proof;
        vm.expectRevert(bytes("header"));
        br.openClaim(keccak256("note"), payable(address(this)), 1, false, proof, 0, 99);
    }
}

contract CheckLeaf0Verifier is IGroth16Verifier {
    uint256 public immutable want;

    constructor(uint256 w) {
        want = w;
    }

    function verifyProof(
        uint256[2] calldata,
        uint256[2][2] calldata,
        uint256[2] calldata,
        uint256[] calldata input
    ) external view returns (bool) {
        return input.length == 16 && input[10] == want;
    }
}
