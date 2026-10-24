// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "../FalconRailLock.sol";

/**
 * @dev Foundry tests for claim codec parity with falcon-dag-rs fd_exec.
 * Golden vectors match claim_leaf_golden_matches_solidity_spec + claim_codec_vectors.py.
 *
 *   cd contracts/evm && forge test
 */
contract FalconRailLockTest is Test {
    FalconRailLock lock;

    bytes32 constant NOTE1 = bytes32(hex"1111111111111111111111111111111111111111111111111111111111111111");
    bytes32 constant NOTE2 = bytes32(hex"2222222222222222222222222222222222222222222222222222222222222222");
    bytes32 constant NOTE3 = bytes32(hex"3333333333333333333333333333333333333333333333333333333333333333");
    bytes32 constant NOTE4 = bytes32(hex"4444444444444444444444444444444444444444444444444444444444444444");

    bytes32 constant LEAF1 =
        bytes32(hex"9ae206ffe6f284b3c64883949436e6e21cb6da3ebac76fb09c7b588a8cff4e85");
    bytes32 constant LEAF2 =
        bytes32(hex"939f90ad03104b5c2a1381b4c814352f367f99bd4ab4868596b2101770058132");
    bytes32 constant LEAF3 =
        bytes32(hex"1a76906c79499f0a65c6b219910493a418e5adaa35041f2c135ae8fd7578d594");
    bytes32 constant ROOT3 =
        bytes32(hex"d83931a298ae420ff828525c844f810690464a2a1dae74dd6ddaa11d92be4a8a");
    bytes32 constant EMPTY =
        bytes32(hex"55eabc17e925fa0d01fbfcb220fecafce4dab37a12b5a165642a760c0167845c");

    function setUp() public {
        bytes32 lid = sha256(bytes("falcon-spv-lock-v1:ETH"));
        lock = new FalconRailLock(lid);
    }

    function test_emptyRootMatchesFalcon() public view {
        assertEq(lock.falconStateRoot(), EMPTY);
    }

    function test_claimLeafGolden() public view {
        assertEq(lock.claimLeaf(NOTE1, 7000, 9, "bc1q_restart_out", "BTC"), LEAF1);
        assertEq(lock.claimLeaf(NOTE2, 1500, 12, "0xeth_smoke_out", "ETH"), LEAF2);
        assertEq(lock.claimLeaf(NOTE3, 1000, 15, "0xbnb_smoke_out", "BNB"), LEAF3);
    }

    function test_merkleRootThreeNotes() public pure {
        // leaves sorted by note_id: NOTE1, NOTE2, NOTE3; pad odd with last
        bytes32 p01 = sha256(abi.encodePacked(LEAF1, LEAF2));
        bytes32 p23 = sha256(abi.encodePacked(LEAF3, LEAF3));
        bytes32 root = sha256(abi.encodePacked(p01, p23));
        assertEq(root, ROOT3);
    }

    function test_claimWithProof() public {
        vm.deal(address(this), 10 ether);
        lock.deposit{value: 5 ether}(bytes32(0));

        lock.submitFalconRoot(ROOT3, 100);

        // proof for NOTE2 at index 1 (external_to is not a 20-byte address → no bind)
        bytes32[] memory path = new bytes32[](2);
        path[0] = LEAF1;
        path[1] = sha256(abi.encodePacked(LEAF3, LEAF3));

        address payable to = payable(address(0xBEEF));
        uint256 before = to.balance;
        lock.claim(NOTE2, to, 1500, 12, "ETH", "0xeth_smoke_out", path, 1);
        assertEq(to.balance - before, 1500);
        assertTrue(lock.claimed(NOTE2));

        vm.expectRevert(FalconRailLock.AlreadyClaimed.selector);
        lock.claim(NOTE2, to, 1500, 12, "ETH", "0xeth_smoke_out", path, 1);
    }

    function test_badProofReverts() public {
        vm.deal(address(this), 1 ether);
        lock.deposit{value: 1 ether}(bytes32(0));
        lock.submitFalconRoot(ROOT3, 1);
        bytes32[] memory path = new bytes32[](1);
        path[0] = bytes32(uint256(1));
        vm.expectRevert(FalconRailLock.BadProof.selector);
        lock.claim(
            NOTE1,
            payable(address(0xBEEF)),
            7000,
            9,
            "BTC",
            "bc1q_restart_out",
            path,
            0
        );
    }

    function test_staleHeightReverts() public {
        lock.submitFalconRoot(ROOT3, 10);
        vm.expectRevert(FalconRailLock.StaleHeight.selector);
        lock.submitFalconRoot(ROOT3, 9);
    }

    function test_recipientBindForHexAddress() public {
        address payable recipient = payable(address(0xabCDeF0123456789AbcdEf0123456789aBCDEF01));
        string memory ext = "0xabcdef0123456789abcdef0123456789abcdef01"; // lowercase

        bytes32 leaf = lock.claimLeaf(NOTE4, 2500, 20, ext, "ETH");
        // single-note tree: root == leaf
        lock.submitFalconRoot(leaf, 20);

        vm.deal(address(this), 1 ether);
        lock.deposit{value: 1 ether}(bytes32(0));

        bytes32[] memory path = new bytes32[](0);

        // wrong to
        vm.expectRevert(FalconRailLock.RecipientMismatch.selector);
        lock.claim(NOTE4, payable(address(0xBEEF)), 2500, 20, "ETH", ext, path, 0);

        // correct to
        uint256 before = recipient.balance;
        lock.claim(NOTE4, recipient, 2500, 20, "ETH", ext, path, 0);
        assertEq(recipient.balance - before, 2500);
    }

    function test_insufficientLiquidityReverts() public {
        // root set, no ETH locked
        lock.submitFalconRoot(LEAF1, 1);
        bytes32[] memory path = new bytes32[](0);
        // single leaf tree would need root==leaf; use LEAF1 as root with empty path only if claim leaf matches
        // deposit tiny amount less than claim
        vm.deal(address(this), 1 ether);
        lock.deposit{value: 100}(bytes32(0));
        // set root to single-note leaf for NOTE1
        bytes32 leaf = lock.claimLeaf(NOTE1, 7000, 9, "bc1q_restart_out", "BTC");
        lock.submitFalconRoot(leaf, 2);
        vm.expectRevert(FalconRailLock.InsufficientLiquidity.selector);
        lock.claim(NOTE1, payable(address(0xBEEF)), 7000, 9, "BTC", "bc1q_restart_out", path, 0);
    }

    function test_parseAddressRoundTrip() public view {
        address a = address(0xabCDeF0123456789AbcdEf0123456789aBCDEF01);
        assertEq(lock.parseAddress("0xabcdef0123456789abcdef0123456789abcdef01"), a);
        assertTrue(lock.isHexAddress("0xabcdef0123456789abcdef0123456789abcdef01"));
        assertFalse(lock.isHexAddress("0xeth_smoke_out"));
        assertFalse(lock.isHexAddress("bc1q_restart_out"));
    }
}
