// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

/// @title FalconQcBridgeV3
/// @notice FPL→Sepolia peg-out without sequential catch-up, plus V3 peg-in deposits
///         and FPL-issued refunds. No owner, no guardian, no pause, no rescue, no re-anchor.
///
/// @dev Deploy note (Sepolia addresses are constructor arguments, not branches in the code):
///      - usdc:     0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238
///      - groth16:  0x2Cb70e9f082F2DF91E9A5e6E7C9DF6b8b11B4F80  (v2 circuit, reused)
///      - v2:       0x811854827627024B38926Ea9DCc0f88ACd5fB23e  (anchor + double-pay)
///      - v1:       0xf8F1471643792eb1cD5d0C31061629777E55bc48  (double-pay)
///      - destLock: 0xdBF6855b00B78c047A729A21E13bfE5f4C991C05  (double-pay)
///      - maxJump:  50000
///      - checks:   FalconQcBridgeV3Checks, deployed first in its own transaction
///      - anchorHeight: 353130 (must equal V2 `fplTip()` at deploy, else the deploy reverts "tip")
///      - anchorSet:    FalconQcBridgeV3AnchorSet holding the 9 notes (withdrawal preimages) of V2's
///                      `headers(353130).claimRoot` (checked on-chain: the tree root of their leaves
///                      must equal that claimRoot, else "set")
///      `fplNetworkId` is read from V2 during construction (same value the v2 proofs use).
///      Three deployments, in order: (1) FalconQcBridgeV3Checks (no constructor arguments),
///      (2) FalconQcBridgeV3AnchorSet(notes) (data only), (3) FalconQcBridgeV3 with both addresses. Each stays well under the EIP-7825
///      per-transaction cap (2**24) at Sepolia's state-creation gas price. The checks address is
///      immutable, and the constructor accepts only code whose hash equals
///      `keccak256(type(FalconQcBridgeV3Checks).runtimeCode)`.
///      This file is not a deployment. No transaction is sent from this repo.

import "./FalconQcBridgeV3Checks.sol";

contract FalconQcBridgeV3 {
    /// @notice Peg-in recorded at deposit time. `sender` is the refund address.
    /// @dev `token` is `address(0)` for ETH and `usdc` for USDC.
    struct PegIn {
        address sender;
        bytes20 dest20;
        address token;
        uint256 amount;
        uint64 timestamp;
        bool exists;
        bool resolved;
    }

    /// @dev Same shape as FalconQcBridgeV2.Claim (no `challenged` field).
    struct Claim {
        address payable dest;
        uint256 amount;
        bool usdc;
        uint64 readyBlock;
        uint64 fplHeight;
        bytes32 leaf;
        bool open;
        bool taken;
    }

    struct FplHeader {
        bytes32 hash;
        bytes32 parent;
        bytes32 innerHash;
        bytes32 claimRoot;
        bytes32 nextCommitteeRoot;
        bool finalized;
    }

    address public immutable usdc;
    IGroth16Verifier public immutable verifier;
    IFalconQcBridgeV2View public immutable v2;
    IFalconQcBridgeV1View public immutable v1;
    IDestLockView public immutable destLock;
    uint64 public immutable proofsNeeded;
    uint32 public immutable fplNetworkId;
    /// @notice Max height gap accepted in one submit. Production deploy passes 50_000.
    uint64 public immutable maxJump;
    /// @notice Stateless proof / double-pay / Merkle helper. Fixed at construction, code-hash pinned.
    FalconQcBridgeV3Checks public immutable checks;
    /// @notice V2 header V3 starts from. Fixed at construction; must equal V2 `fplTip()` then.
    uint64 public immutable anchorHeight;
    /// @notice Data contract with every note V2's anchor header commits, as withdrawal preimages
    ///         (the tree root of their leaves equals V2 `headers(anchorHeight).claimRoot`, checked in
    ///         the constructor). `openClaim` refuses any of those noteIds at any index, and any
    ///         header at or below `anchorHeight`:
    ///         V2 can pay those notes, so V3 never does. Zero only when the anchor root is empty.
    address public immutable anchorSet;
    /// @notice Number of notes in `anchorSet` (9 for V2 header 353130).
    uint32 public immutable anchorLeafCount;

    uint64 public fplTip;
    bytes32 public currentCommitteeRoot;
    bytes32[6] public currentLeaves;
    mapping(uint64 => FplHeader) public headers;
    mapping(bytes32 => Claim) public claims;

    uint256 public ethPool;
    uint256 public usdcPool;

    /// @notice Monotonic nonce. The next deposit uses this value, then increments it.
    uint256 public depositNonce;
    mapping(bytes32 => PegIn) public pegIns;
    /// @notice refund noteId or mint noteId → depositId. Zero means the note is not deposit-linked.
    mapping(bytes32 => bytes32) public noteDeposit;

    /// @notice V3 peg-in. Not the V1/V2 `Deposit(bytes32,address,uint256,bytes32)` event.
    /// @dev topic0 = keccak256("Deposit(bytes32,address,bytes20,address,uint256,uint64)").
    ///      topic1 = depositId, topic2 = sender. `token` is address(0) for ETH.
    ///      `sender` is the refund address. `dest20` is the FPL destination.
    event Deposit(
        bytes32 indexed depositId,
        address indexed sender,
        bytes20 dest20,
        address token,
        uint256 amount,
        uint64 timestamp
    );
    event PoolFunded(address indexed sender, address indexed token, uint256 amount);
    event HeaderProved(uint64 indexed height, bytes32 hash, bytes32 claimRoot, bytes32 nextCommitteeRoot);
    event CommitteeRotated(uint64 indexed height, bytes32 newRoot);
    event ClaimOpened(bytes32 indexed noteId, address dest, uint256 amount, bool usdc, uint64 readyBlock);
    event ClaimTaken(bytes32 indexed noteId, address dest, uint256 amount);

    constructor(
        address usdcToken,
        address groth16,
        address v2Bridge,
        address v1Bridge,
        address destLockBridge,
        uint64 maxJump_,
        address checks_,
        uint64 anchorHeight_,
        address anchorSet_
    ) {
        require(v2Bridge != address(0) && v1Bridge != address(0) && destLockBridge != address(0), "ctor");
        require(maxJump_ > 0, "maxJump");
        require(
            checks_.code.length > 0 && checks_.codehash == keccak256(type(FalconQcBridgeV3Checks).runtimeCode),
            "checks"
        );
        checks = FalconQcBridgeV3Checks(checks_);
        v2 = IFalconQcBridgeV2View(v2Bridge);
        v1 = IFalconQcBridgeV1View(v1Bridge);
        destLock = IDestLockView(destLockBridge);
        maxJump = maxJump_;
        proofsNeeded = 4;

        require(groth16 != address(0) && groth16 == v2.verifier(), "verifier");
        verifier = IGroth16Verifier(groth16);
        require(usdcToken != address(0) && usdcToken == v2.usdc(), "usdc");
        usdc = usdcToken;

        uint32 net = v2.fplNetworkId();
        require(net != 0, "net");
        fplNetworkId = net;

        uint64 t = anchorHeight_;
        require(t > 0 && t == v2.fplTip(), "tip");
        (
            bytes32 hash,
            ,
            bytes32 innerHash,
            bytes32 claimRoot,
            bytes32 nextRoot,
            bool fin
        ) = v2.headers(t);
        require(fin, "fin");
        require(nextRoot != bytes32(0) && nextRoot == v2.currentCommitteeRoot(), "root");
        require(hash != bytes32(0) && innerHash != bytes32(0), "anchor");
        // The anchor set must be exactly the notes V2's anchor root commits (leaves rebuilt from
        // their preimages, same tree code as FPL), so it cannot be a set of internal nodes.
        (bytes32 setRoot, uint256 n) = FalconQcBridgeV3Checks(checks_).anchorSetRoot(anchorSet_);
        require(setRoot == claimRoot, "set");
        anchorHeight = t;
        anchorSet = anchorSet_;
        anchorLeafCount = uint32(n);

        fplTip = t;
        currentCommitteeRoot = nextRoot;
        for (uint256 i = 0; i < 6; i++) {
            currentLeaves[i] = v2.currentLeaves(i);
        }
        // Store only what V3 uses: `hash` (parent link for `anchorHeight + 1`) and `finalized`.
        // Nothing opens against the anchor (V2's header), so its parent, innerHash, claimRoot and
        // nextCommitteeRoot read zero on V3; V2 `headers(anchorHeight)` keeps them. Saves 4 slots.
        headers[t].hash = hash;
        headers[t].finalized = true;
    }

    /// @notice depositId = keccak256(abi.encode(block.chainid, address(this), nonce)).
    /// @dev ABI encoding, not packed: uint256 chainid || address (12-byte pad || 20 bytes) || uint256 nonce.
    ///      `nonce` is the pre-increment `depositNonce` (0 for the first deposit).
    function deriveDepositId(uint256 nonce) public view returns (bytes32) {
        return keccak256(abi.encode(block.chainid, address(this), nonce));
    }

    /// @notice Refund note id in a namespace that cannot collide with a raw depositId.
    /// @dev noteId = sha256("refund|" || depositId) — 7 ASCII bytes then the 32-byte id.
    ///      Solidity: `sha256(abi.encodePacked("refund|", depositId))`.
    ///      The leaf is still `withdrawLeaf` (`evm-wd-v1|...`). Paid only via `openClaim` / `take`.
    function refundNoteId(bytes32 depositId) public pure returns (bytes32) {
        return sha256(abi.encodePacked("refund|", depositId));
    }

    /// @notice Sibling namespace for a mint-outcome leaf of the same deposit.
    /// @dev noteId = sha256("mint|" || depositId). Release B mints on FPL and should not emit
    ///      this leaf. If a claim root contains it anyway, opening either this note or the
    ///      refund note resolves the deposit and the other cannot be paid.
    function mintNoteId(bytes32 depositId) public pure returns (bytes32) {
        return sha256(abi.encodePacked("mint|", depositId));
    }

    /// @notice Peg-in ETH. `msg.sender` is the refund address. `token` in the event is address(0).
    /// @dev The refund (and any claim) is paid only to the recorded dest, and only dest may call `take`.
    ///      `openClaim` reserves the pool up front. A contract sender must be able to call `take` and
    ///      accept ETH (no `receive()` means the ETH refund reverts). A USDC-blacklisted sender can
    ///      have `transfer` revert and strand the USDC refund. Same as V2, and intentional.
    function depositEth(bytes20 dest20) external payable returns (bytes32 depositId) {
        require(msg.value > 0, "zero");
        require(msg.value <= type(uint64).max, "amount");
        require(dest20 != bytes20(0), "dest");
        depositId = _pullDeposit(dest20, address(0), msg.value);
        ethPool += msg.value;
    }

    /// @notice Peg-in USDC. `msg.sender` is the refund address.
    /// @dev The refund (and any claim) is paid only to the recorded dest, and only dest may call `take`.
    ///      `openClaim` reserves the pool up front. A contract sender must be able to call `take` and
    ///      accept ETH (no `receive()` means the ETH refund reverts). A USDC-blacklisted sender can
    ///      have `transfer` revert and strand the USDC refund. Same as V2, and intentional.
    function depositUsdc(bytes20 dest20, uint256 amount) external returns (bytes32 depositId) {
        require(amount > 0, "zero");
        require(amount <= type(uint64).max, "amount");
        require(dest20 != bytes20(0), "dest");
        _pullExactUsdc(amount);
        depositId = _pullDeposit(dest20, usdc, amount);
        usdcPool += amount;
    }

    /// @notice Add peg-out liquidity. Does not emit `Deposit` and must not be minted on FPL.
    function fundPool(uint256 usdcAmount) external payable {
        require(msg.value > 0 || usdcAmount > 0, "zero");
        if (msg.value > 0) {
            ethPool += msg.value;
            emit PoolFunded(msg.sender, address(0), msg.value);
        }
        if (usdcAmount > 0) {
            _pullExactUsdc(usdcAmount);
            usdcPool += usdcAmount;
            emit PoolFunded(msg.sender, usdc, usdcAmount);
        }
    }

    /// @dev Credit `amount` only if this contract's USDC balance rose by exactly that much.
    ///      Sepolia FiatToken USDC has no transfer fee; reject fee-on-transfer or short-transfer tokens.
    ///      `transferFrom` returning false still reverts `"xfer"`. A short balance increase reverts `"fee"`.
    function _pullExactUsdc(uint256 amount) internal {
        IERC20 token = IERC20(usdc);
        uint256 beforeBal = token.balanceOf(address(this));
        require(token.transferFrom(msg.sender, address(this), amount), "xfer");
        require(token.balanceOf(address(this)) == beforeBal + amount, "fee");
    }

    /// @notice Accept any later height in `(fplTip, fplTip + maxJump]`.
    /// @dev `+1` still requires `parent == headers[fplTip].hash`. A jump does not link the parent
    ///      on-chain; the parent is inside the signed statement. Four distinct indices `< 6`.
    ///      Verifier inputs are byte-for-byte V2, with `input[5] = currentCommitteeRoot`
    ///      (the committee the contract already trusts, not the new root); built and checked in
    ///      `FalconQcBridgeV3Checks.checkHeaderProofs` (revert reasons pass through unchanged).
    ///      Holes in `headers` are allowed. After accept, the committee rotates.
    function submitHeaderWithProofs(
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
    ) external {
        require(hash != bytes32(0) && innerHash != bytes32(0), "hash");
        require(proofsNeeded == 4, "n");
        require(height > fplTip && height - fplTip <= maxJump, "range");
        require(!headers[height].finalized, "final");
        if (height == fplTip + 1) {
            require(parent == headers[fplTip].hash, "parent");
        }
        require(nextCommitteeRoot != bytes32(0), "root");
        require(
            checks.checkHeaderProofs(
                verifier,
                fplNetworkId,
                currentCommitteeRoot,
                height,
                hash,
                parent,
                innerHash,
                claimRoot,
                nextCommitteeRoot,
                nextLeaves,
                idx,
                a,
                b,
                c
            ),
            "proof"
        );
        headers[height] = FplHeader(hash, parent, innerHash, claimRoot, nextCommitteeRoot, true);
        fplTip = height;
        currentCommitteeRoot = nextCommitteeRoot;
        currentLeaves = nextLeaves;
        emit HeaderProved(height, hash, claimRoot, nextCommitteeRoot);
        emit CommitteeRotated(height, nextCommitteeRoot);
    }

    /// @dev Same bytes as V1/V2 and `evm_spv::withdraw_leaf`:
    ///      sha256("evm-wd-v1|" || note32 || dest20 || uint64_be || 0x01/0x00).
    ///      `openClaim` uses the identical `checks.withdrawLeaf`; this copy is the public view.
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

    /// @notice Open a cumulative-root leaf against any finalized header (holes are fine).
    /// @dev Refuses (`"anchor"`) any note V2's anchor header commits: `fplHeight <= anchorHeight`,
    ///      or a noteId that is in `anchorSet`, whatever its index. (Indices are not stable: FPL orders
    ///      a header's leaves as all ETH then all USDC, so a later ETH leaf shifts every USDC leaf.)
    ///      Rejects a noteId already recorded on V1, V2, or DestLock (`dest != 0`).
    ///      A refund leaf (`refundNoteId`) must pay `pegIns[depositId].sender` the original
    ///      amount and asset. Opening either the refund note or the mint note for a deposit
    ///      sets `resolved`, so the other cannot pay.
    function openClaim(
        bytes32 noteId,
        address payable dest,
        uint256 amount,
        bool isUsdc,
        bytes32[] calldata proof,
        uint32 index,
        uint64 fplHeight
    ) external {
        require(dest != address(0) && amount > 0 && amount <= type(uint64).max, "args");
        require(claims[noteId].dest == address(0), "exists");
        require(fplHeight > anchorHeight, "anchor");
        FplHeader storage hdr = headers[fplHeight];
        require(hdr.finalized, "header");
        bytes32 leaf = checks.checkClaim(v2, v1, destLock, noteId, dest, amount, isUsdc, proof, index, hdr.claimRoot, anchorSet);
        _consumeLinkedDeposit(noteId, dest, amount, isUsdc);
        if (isUsdc) {
            require(usdcPool >= amount, "pool");
            usdcPool -= amount;
        } else {
            require(ethPool >= amount, "pool");
            ethPool -= amount;
        }
        claims[noteId] = Claim(dest, amount, isUsdc, uint64(block.number), fplHeight, leaf, true, false);
        emit ClaimOpened(noteId, dest, amount, isUsdc, uint64(block.number));
    }

    /// @notice Pay an open claim. Only the recorded dest may call this.
    /// @dev The refund (and any claim) is paid only to the recorded dest, and only dest may call `take`
    ///      (`msg.sender == claim.dest`; anyone else reverts `"dest"`). `openClaim` reserves the pool
    ///      up front. A contract sender must be able to call `take` and accept ETH (no `receive()` means
    ///      the ETH refund reverts). A USDC-blacklisted sender can have `transfer` revert and strand the
    ///      USDC refund. Same as V2, and intentional.
    function take(bytes32 noteId) external {
        Claim storage c = claims[noteId];
        require(c.open && !c.taken, "state");
        require(msg.sender == c.dest, "dest");
        c.taken = true;
        c.open = false;
        if (c.usdc) {
            require(IERC20(usdc).transfer(c.dest, c.amount), "xfer");
        } else {
            (bool ok, ) = c.dest.call{value: c.amount}("");
            require(ok, "eth");
        }
        emit ClaimTaken(noteId, c.dest, c.amount);
    }

    function _pullDeposit(bytes20 dest20, address token, uint256 amount) internal returns (bytes32 depositId) {
        depositId = deriveDepositId(depositNonce);
        require(depositId != bytes32(0), "id");
        depositNonce += 1;
        bytes32 refundId = refundNoteId(depositId);
        bytes32 mintId = mintNoteId(depositId);
        require(refundId != mintId, "note");
        require(noteDeposit[refundId] == bytes32(0) && noteDeposit[mintId] == bytes32(0), "note");
        noteDeposit[refundId] = depositId;
        noteDeposit[mintId] = depositId;
        pegIns[depositId] = PegIn({
            sender: msg.sender,
            dest20: dest20,
            token: token,
            amount: amount,
            timestamp: uint64(block.timestamp),
            exists: true,
            resolved: false
        });
        emit Deposit(depositId, msg.sender, dest20, token, amount, uint64(block.timestamp));
    }

    /// @dev Judgment call: FPL should emit only one of mint-credit or refund-leaf.
    ///      On-chain, both namespaces map to the deposit and the first `openClaim` wins.
    function _consumeLinkedDeposit(bytes32 noteId, address dest, uint256 amount, bool isUsdc) internal {
        bytes32 depositId = noteDeposit[noteId];
        if (depositId == bytes32(0)) return;
        PegIn storage p = pegIns[depositId];
        require(p.exists && !p.resolved, "resolved");
        if (noteId == refundNoteId(depositId)) {
            require(dest == p.sender, "refund-dest");
            require(amount == p.amount, "refund-amt");
            require(isUsdc == (p.token != address(0)), "refund-asset");
        } else {
            require(noteId == mintNoteId(depositId), "note");
        }
        p.resolved = true;
    }
}
