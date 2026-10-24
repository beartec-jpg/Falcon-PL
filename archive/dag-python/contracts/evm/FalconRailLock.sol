// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

/**
 * @title FalconRailLock
 * @notice Protocol-controlled lock for Falcon ETH/BNB rails — NOT a multisig custody wallet.
 *
 * Claim codec MUST match falcon-dag-rs fd_exec::bridge withdrawal_claim_*:
 *   leaf = sha256( noteId[32] || amount_be8 || height_be8 || sha256(external_to) || sha256(rail) )
 *   parent = sha256( left[32] || right[32] )
 *   empty root = sha256("falcon-claim-root-empty-v1")
 *
 * Flow:
 *   1. deposit{value} — lock ETH; agents observe Deposit for Falcon mint path.
 *   2. Falcon RailWithdraw auto-commits claim_root on the rail (public, no committee).
 *   3. Anyone submitFalconRoot(committed claim_root, claim_height) then claim(...).
 *   4. Production: replace open submitFalconRoot with on-chain Falcon cert verification.
 */
contract FalconRailLock {
    bytes32 public immutable lockId;
    /// @dev Must equal Falcon rail.claim_root (hex decoded) when claims are valid.
    bytes32 public falconStateRoot;
    uint64 public falconHeight;

    mapping(bytes32 => bool) public claimed;
    mapping(address => uint256) public locked;

    event Deposit(
        bytes32 indexed lockId,
        address indexed from,
        uint256 amount,
        bytes32 falconAccountHash
    );
    event FalconRootUpdated(bytes32 root, uint64 height, address poster);
    event Claimed(bytes32 indexed noteId, address indexed to, uint256 amount);

    error ZeroAmount();
    error AlreadyClaimed();
    error BadProof();
    error RecipientMismatch();
    error InsufficientLiquidity();
    error StaleHeight();

    constructor(bytes32 lockId_) {
        lockId = lockId_;
        falconStateRoot = sha256(bytes("falcon-claim-root-empty-v1"));
    }

    function deposit(bytes32 falconAccountHash) external payable {
        if (msg.value == 0) revert ZeroAmount();
        locked[msg.sender] += msg.value;
        emit Deposit(lockId, msg.sender, msg.value, falconAccountHash);
    }

    /**
     * @notice Post Falcon's auto-committed claim_root (from rail_info).
     * Anyone may call; incorrect roots simply fail claim proofs.
     * Production: gate on Falcon certificate / header batch.
     */
    function submitFalconRoot(bytes32 root, uint64 height) external {
        if (height < falconHeight) revert StaleHeight();
        falconStateRoot = root;
        falconHeight = height;
        emit FalconRootUpdated(root, height, msg.sender);
    }

    /**
     * @param noteId         32-byte Falcon note id
     * @param to             EVM recipient (must match external_to when that is a 0x address)
     * @param amount         Burn amount
     * @param noteHeight     Height stored on the note (applied_user_txs at burn)
     * @param rail           Asset code e.g. "ETH" / "BNB" / "BTC" (utf-8)
     * @param externalTo     Exact external_to string from Falcon note
     * @param proofPath      Sibling hashes (32-byte) from leaf to root
     * @param pathIndex      Leaf index in sorted-by-note_id tree
     */
    function claim(
        bytes32 noteId,
        address payable to,
        uint256 amount,
        uint64 noteHeight,
        string calldata rail,
        string calldata externalTo,
        bytes32[] calldata proofPath,
        uint32 pathIndex
    ) external {
        if (claimed[noteId]) revert AlreadyClaimed();
        if (amount == 0) revert ZeroAmount();

        bytes32 leaf = claimLeaf(noteId, amount, noteHeight, externalTo, rail);
        bytes32 root = merkleRoot(leaf, proofPath, pathIndex);
        if (root != falconStateRoot) revert BadProof();

        // Bind payout when Falcon note external_to is a standard 0x + 40 hex address.
        if (isHexAddress(externalTo)) {
            if (to != parseAddress(externalTo)) revert RecipientMismatch();
        }

        if (address(this).balance < amount) revert InsufficientLiquidity();

        claimed[noteId] = true;
        (bool ok, ) = to.call{value: amount}("");
        require(ok, "xfer");
        emit Claimed(noteId, to, amount);
    }

    function claimLeaf(
        bytes32 noteId,
        uint256 amount,
        uint64 height,
        string memory externalTo,
        string memory rail
    ) public pure returns (bytes32) {
        // amount and height as big-endian 8 bytes (uint64), matching Rust to_be_bytes
        bytes8 amt8 = bytes8(uint64(amount));
        bytes8 h8 = bytes8(height);
        return
            sha256(
                abi.encodePacked(
                    noteId,
                    amt8,
                    h8,
                    sha256(bytes(externalTo)),
                    sha256(bytes(rail))
                )
            );
    }

    function merkleRoot(
        bytes32 leaf,
        bytes32[] calldata path,
        uint32 index
    ) public pure returns (bytes32) {
        bytes32 h = leaf;
        uint32 idx = index;
        for (uint256 i = 0; i < path.length; i++) {
            bytes32 sib = path[i];
            if (idx % 2 == 0) {
                h = sha256(abi.encodePacked(h, sib));
            } else {
                h = sha256(abi.encodePacked(sib, h));
            }
            idx /= 2;
        }
        return h;
    }

    /// @dev true when s is "0x" / "0X" + exactly 40 hex chars (EVM address encoding).
    function isHexAddress(string memory s) public pure returns (bool) {
        bytes memory b = bytes(s);
        if (b.length != 42) return false;
        if (b[0] != "0" || (b[1] != "x" && b[1] != "X")) return false;
        for (uint256 i = 2; i < 42; i++) {
            bytes1 c = b[i];
            bool ok = (c >= "0" && c <= "9") || (c >= "a" && c <= "f") || (c >= "A" && c <= "F");
            if (!ok) return false;
        }
        return true;
    }

    function parseAddress(string memory s) public pure returns (address) {
        bytes memory b = bytes(s);
        require(b.length == 42, "len");
        uint160 result = 0;
        for (uint256 i = 2; i < 42; i++) {
            result <<= 4;
            uint8 c = uint8(b[i]);
            if (c >= 48 && c <= 57) {
                result |= uint160(c - 48); // 0-9
            } else if (c >= 97 && c <= 102) {
                result |= uint160(c - 87); // a-f
            } else if (c >= 65 && c <= 70) {
                result |= uint160(c - 55); // A-F
            } else {
                revert("hex");
            }
        }
        return address(result);
    }
}
