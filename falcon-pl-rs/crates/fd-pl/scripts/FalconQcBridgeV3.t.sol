// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "forge-std/console.sol";
import "forge-std/StdJson.sol";
import "./FalconQcBridgeV3.sol";
import "./Groth16Verifier.sol";

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

/// @dev Accepts only when the proof commits to the old committee root (`a[0]`)
///      and the contract passes that same word as input[5], plus the V2 input layout.
contract ExpectQcInputs is IGroth16Verifier {
    uint256 public immutable height;
    uint256 public immutable parentHi;
    uint256 public immutable parentLo;
    uint256 public immutable hashHi;
    uint256 public immutable hashLo;
    uint256 public immutable oldRoot;
    uint256 public immutable newRoot;
    uint256 public immutable networkId;
    bytes32[6] public leaves;

    constructor(
        uint64 height_,
        bytes32 parent_,
        bytes32 hash_,
        bytes32 oldRoot_,
        bytes32 newRoot_,
        uint32 networkId_,
        bytes32[6] memory leaves_
    ) {
        height = height_;
        parentHi = uint256(parent_) >> 128;
        parentLo = uint256(parent_) & ((uint256(1) << 128) - 1);
        hashHi = uint256(hash_) >> 128;
        hashLo = uint256(hash_) & ((uint256(1) << 128) - 1);
        oldRoot = uint256(oldRoot_);
        newRoot = uint256(newRoot_);
        networkId = networkId_;
        leaves = leaves_;
    }

    function verifyProof(
        uint256[2] calldata a,
        uint256[2][2] calldata,
        uint256[2] calldata,
        uint256[] calldata input
    ) external view returns (bool) {
        if (input.length != 16) return false;
        if (a[0] != oldRoot || input[5] != oldRoot) return false;
        if (input[0] != height) return false;
        if (input[1] != parentHi || input[2] != parentLo) return false;
        if (input[3] != hashHi || input[4] != hashLo) return false;
        if (input[6] != newRoot) return false;
        if (input[7] != 1 || input[8] != networkId) return false;
        if (input[9] != a[1]) return false;
        for (uint256 j = 0; j < 6; j++) {
            if (input[10 + j] != uint256(leaves[j])) return false;
        }
        return true;
    }
}

contract MockUSDC {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        require(balanceOf[from] >= amount && allowance[from][msg.sender] >= amount, "bal");
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(balanceOf[msg.sender] >= amount, "bal");
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}

/// @dev `transferFrom` / `transfer` return false and do not revert. Not the setUp token.
contract FalseReturnUSDC {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    bool public transferFromReturnsFalse;
    bool public transferReturnsFalse;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function setTransferFromReturnsFalse(bool v) external {
        transferFromReturnsFalse = v;
    }

    function setTransferReturnsFalse(bool v) external {
        transferReturnsFalse = v;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        if (transferFromReturnsFalse) return false;
        require(balanceOf[from] >= amount && allowance[from][msg.sender] >= amount, "bal");
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        if (transferReturnsFalse) return false;
        require(balanceOf[msg.sender] >= amount, "bal");
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}

/// @dev `transferFrom` returns true but the recipient balance rises by `amount - 1`.
contract FeeOnTransferUSDC {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        require(balanceOf[from] >= amount && allowance[from][msg.sender] >= amount, "bal");
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount - 1;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(balanceOf[msg.sender] >= amount, "bal");
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}

/// @dev Claim dest. `receive` low-level-calls `take` again and keeps the revert data.
contract ReentrantDest {
    FalconQcBridgeV3 public immutable bridge;
    bytes32 public immutable noteId;
    bool public sawReenter;
    bool public reenterOk;
    bytes public reenterData;

    constructor(FalconQcBridgeV3 bridge_, bytes32 noteId_) {
        bridge = bridge_;
        noteId = noteId_;
    }

    function take() external {
        bridge.take(noteId);
    }

    receive() external payable {
        if (sawReenter) return;
        sawReenter = true;
        (reenterOk, reenterData) = address(bridge).call(abi.encodeCall(FalconQcBridgeV3.take, (noteId)));
    }
}

contract MockQcV2 {
    uint64 public fplTip;
    bytes32 public currentCommitteeRoot;
    uint32 public fplNetworkId = 2300;
    address public usdc;
    address public verifier;
    bytes32[6] public currentLeaves;

    struct FplHeader {
        bytes32 hash;
        bytes32 parent;
        bytes32 innerHash;
        bytes32 claimRoot;
        bytes32 nextCommitteeRoot;
        bool finalized;
    }

    mapping(uint64 => FplHeader) public headers;

    struct Claim {
        address dest;
        uint256 amount;
        bool usdc;
        uint64 readyBlock;
        uint64 fplHeight;
        bytes32 leaf;
        bool open;
        bool taken;
    }

    mapping(bytes32 => Claim) public claims;

    constructor(
        address usdc_,
        address verifier_,
        uint64 tip,
        bytes32 hash,
        bytes32 parent,
        bytes32 innerHash,
        bytes32 claimRoot,
        bytes32 committeeRoot,
        bytes32[6] memory leaves_,
        bool finalized,
        bool rootMatch
    ) {
        usdc = usdc_;
        verifier = verifier_;
        fplTip = tip;
        currentLeaves = leaves_;
        currentCommitteeRoot = rootMatch ? committeeRoot : keccak256("mismatch");
        headers[tip] = FplHeader(hash, parent, innerHash, claimRoot, committeeRoot, finalized);
    }

    function setPaid(bytes32 noteId, address dest) external {
        claims[noteId].dest = dest;
    }
}

contract MockQcV1 {
    struct Claim {
        address dest;
        uint256 amount;
        bool usdc;
        uint64 readyBlock;
        uint64 fplHeight;
        bytes32 leaf;
        bool open;
        bool challenged;
        bool taken;
    }

    mapping(bytes32 => Claim) public claims;

    function setPaid(bytes32 noteId, address dest) external {
        claims[noteId].dest = dest;
    }
}

contract MockDestLock {
    struct Claim {
        address dest;
        uint256 amount;
        bool usdc;
        uint64 readyBlock;
        bool open;
        bool taken;
    }

    mapping(bytes32 => Claim) public claims;

    function setPaid(bytes32 noteId, address dest) external {
        claims[noteId].dest = dest;
    }
}

contract FalconQcBridgeV3Test is Test {
    bytes32 constant REAL_ANCHOR_ROOT = 0x9671942a853984682f915a3418cc02d37ff19148692f117c149eeb34da86cfa5;
    using stdJson for string;

    uint64 constant TIP = 353130;
    uint64 constant MAX_JUMP = 50_000;
    /// @dev BN254 base-field prime. Same value as `Groth16Verifier.PRIME_Q`.
    uint256 constant BN254_P =
        21888242871839275222246405745257275088696311157297823662689037894645226208583;
    bytes32 constant DEPOSIT_TOPIC = keccak256("Deposit(bytes32,address,bytes20,address,uint256,uint64)");
    bytes32 constant OLD_DEPOSIT_TOPIC = keccak256("Deposit(bytes32,address,uint256,bytes32)");
    /// @dev keccak256 of the 48 big-endian words alpha(2) || beta(4) || gamma(4) || delta(4) || ic(34),
    ///      read from Sepolia Groth16Verifier 0x2Cb70e9f082F2DF91E9A5e6E7C9DF6b8b11B4F80 public getters
    ///      on 2026-10-08 (icLength 17). Not derived from the fixture.
    ///      `run/snark/falcon-qc-v2-one/vk.sol.json` is not in this repo, and scripts/Groth16Verifier.sol
    ///      takes the VK as constructor arguments with no embedded constants.
    bytes32 constant CANONICAL_V2_VK_HASH =
        0xe996b9af4000a3c42ee84d4d52da974906613962feb41651f02acf174a3200a0;

    MockUSDC usdc;
    AlwaysTrueVerifier ver;
    MockQcV2 v2m;
    MockQcV1 v1m;
    MockDestLock dlm;
    FalconQcBridgeV3 br;
    FalconQcBridgeV3Checks ck;

    bytes32 committee;
    bytes32 anchorHash;
    bytes32 anchorParent;
    bytes32 anchorInner;
    bytes32 anchorClaim;
    bytes32[6] leaves;

    function setUp() public {
        usdc = new MockUSDC();
        ver = new AlwaysTrueVerifier();
        committee = keccak256("committee");
        anchorParent = keccak256("parent");
        anchorInner = keccak256("inner");
        // Empty anchor tree (zero root, 0 committed leaves), so the generic tests can open index 0.
        // Anchor-leaf refusal is covered by testOpenClaimRefusesV2AnchorNotes and the real fixture.
        anchorClaim = bytes32(0);
        anchorHash = _seal(anchorInner, committee, anchorClaim);
        for (uint256 i = 0; i < 6; i++) {
            leaves[i] = keccak256(abi.encodePacked("leaf", i));
        }
        br = _deploy(address(ver), anchorClaim, true, true);
        vm.deal(address(this), 100 ether);
    }

    function testDepositTopic0() public pure {
        assertEq(DEPOSIT_TOPIC, 0xe7783c72791736d4d8af95660d4367bed2f98d8331a095422373e557a4318728);
        assertTrue(DEPOSIT_TOPIC != OLD_DEPOSIT_TOPIC);
    }

    function testConstructorAnchorsFromV2() public view {
        assertEq(br.fplTip(), TIP);
        assertEq(br.currentCommitteeRoot(), committee);
        assertEq(br.maxJump(), MAX_JUMP);
        assertEq(br.proofsNeeded(), 4);
        assertEq(br.fplNetworkId(), 2300);
        assertEq(br.usdc(), address(usdc));
        assertEq(address(br.verifier()), address(ver));
        assertEq(address(br.v2()), address(v2m));
        (bytes32 hash, bytes32 parent, bytes32 innerHash, bytes32 claimRoot, bytes32 nextRoot, bool fin) =
            br.headers(TIP);
        assertEq(hash, anchorHash);
        // Only hash + finalized are stored for the anchor (nothing opens against it on V3).
        assertEq(parent, bytes32(0));
        assertEq(innerHash, bytes32(0));
        assertEq(claimRoot, bytes32(0));
        assertEq(nextRoot, bytes32(0));
        assertTrue(fin);
        assertEq(br.anchorHeight(), TIP);
        assertEq(br.anchorLeafCount(), 0);
        assertEq(br.anchorSet(), address(0));
        for (uint256 i = 0; i < 6; i++) {
            assertEq(br.currentLeaves(i), leaves[i]);
        }
        assertEq(keccak256(br.checks().SEAL_PREFIX()), keccak256(bytes("pl-ledger:v4-seal")));
        assertEq(address(br.checks()), address(ck));
    }

    /// @dev Checks is a separate immutable deployment; V3 accepts only its exact runtime code.
    function testConstructorPinsChecksCode() public {
        MockQcV1 v1 = new MockQcV1();
        MockDestLock dl = new MockDestLock();
        // EOA / empty account
        vm.expectRevert(bytes("checks"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(v2m), address(v1), address(dl), MAX_JUMP, address(0xC0DE), TIP, address(0));
        vm.expectRevert(bytes("checks"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(v2m), address(v1), address(dl), MAX_JUMP, address(0), TIP, address(0));
        // Some other contract (here: a mock that answers every call with true)
        vm.expectRevert(bytes("checks"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(v2m), address(v1), address(dl), MAX_JUMP, address(ver), TIP, address(0));
        // A second deployment of the real Checks code is accepted (no deployer or address pinning).
        FalconQcBridgeV3Checks other = new FalconQcBridgeV3Checks();
        FalconQcBridgeV3 b2 =
            new FalconQcBridgeV3(address(usdc), address(ver), address(v2m), address(v1), address(dl), MAX_JUMP, address(other), TIP, address(0));
        assertEq(address(b2.checks()), address(other));
        assertEq(address(ck).codehash, keccak256(type(FalconQcBridgeV3Checks).runtimeCode));
    }

    function testWithdrawLeafMatchesChecks() public view {
        bytes32 note = keccak256("n");
        assertEq(br.withdrawLeaf(note, address(0xA11CE), 7, true), ck.withdrawLeaf(note, address(0xA11CE), 7, true));
        assertEq(br.withdrawLeaf(note, address(0xA11CE), 7, false), ck.withdrawLeaf(note, address(0xA11CE), 7, false));
    }

    /// @dev RO nit 4: Merkle path length and index bounds.
    function testMerkleDepthAndIndexBounds() public {
        bytes32 leaf = keccak256("leaf");
        bytes32 sib = keccak256("sib");
        bytes32[] memory one = new bytes32[](1);
        one[0] = sib;
        assertEq(ck.merkleRoot(leaf, one, 0), sha256(abi.encodePacked(leaf, sib)));
        assertEq(ck.merkleRoot(leaf, one, 1), sha256(abi.encodePacked(sib, leaf)));
        // index must fit in proof.length bits
        vm.expectRevert(bytes("depth"));
        ck.merkleRoot(leaf, one, 2);
        bytes32[] memory none;
        assertEq(ck.merkleRoot(leaf, none, 0), leaf);
        vm.expectRevert(bytes("depth"));
        ck.merkleRoot(leaf, none, 1);
        bytes32[] memory deep = new bytes32[](33);
        vm.expectRevert(bytes("depth"));
        ck.merkleRoot(leaf, deep, 0);
        bytes32[] memory max = new bytes32[](32);
        ck.merkleRoot(leaf, max, type(uint32).max);
    }

    function testOpenClaimRejectsIndexOutsideProofDepth() public {
        address alice = address(0xA11CE);
        uint256 amount = 1 ether;
        bytes32 note = keccak256("depth-note");
        bytes32 leaf = br.withdrawLeaf(note, alice, amount, false);
        br.fundPool{value: amount}(0);
        uint64 height = TIP + 27;
        _submit(br, height, bytes32(uint256(27)), leaf, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());
        bytes32[] memory proof;
        vm.expectRevert(bytes("depth"));
        br.openClaim(note, payable(alice), amount, false, proof, 1, height);
        br.openClaim(note, payable(alice), amount, false, proof, 0, height);
        assertEq(br.ethPool(), 0);
    }

    function testConstructorRejectsUnfinalized() public {
        MockQcV2 bad = _mockV2(address(ver), anchorClaim, false, true);
        MockQcV1 v1 = new MockQcV1();
        MockDestLock dl = new MockDestLock();
        vm.expectRevert(bytes("fin"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(bad), address(v1), address(dl), MAX_JUMP, address(_ck()), TIP, address(0));
    }

    function testConstructorRejectsRootMismatch() public {
        MockQcV2 bad = _mockV2(address(ver), anchorClaim, true, false);
        MockQcV1 v1 = new MockQcV1();
        MockDestLock dl = new MockDestLock();
        vm.expectRevert(bytes("root"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(bad), address(v1), address(dl), MAX_JUMP, address(_ck()), TIP, address(0));
    }

    function testSequentialPlusOneLikeV2() public {
        bytes32 nextRoot = keccak256("next-root");
        bytes32[6] memory nl = _nextLeaves();
        bytes32 claim = keccak256("claim-101");
        uint64[4] memory idx = _idx();
        bytes32 parent = anchorHash;
        (bytes32 innerHash, bytes32 hash) = _sealed(TIP + 1, nextRoot, claim);
        vm.expectEmit(true, false, false, true, address(br));
        emit HeaderProved(TIP + 1, hash, claim, nextRoot);
        vm.expectEmit(true, false, false, true, address(br));
        emit CommitteeRotated(TIP + 1, nextRoot);
        _submit(br, TIP + 1, parent, claim, nextRoot, nl, idx, _zeroA());
        assertEq(br.fplTip(), TIP + 1);
        assertEq(br.currentCommitteeRoot(), nextRoot);
        for (uint256 i = 0; i < 6; i++) {
            assertEq(br.currentLeaves(i), nl[i]);
        }
        (bytes32 storedHash, bytes32 storedParent, bytes32 storedInner,,, bool fin) = br.headers(TIP + 1);
        assertEq(storedHash, hash);
        assertEq(storedParent, parent);
        assertEq(storedInner, innerHash);
        assertTrue(fin);

        // After a jump, the next +1 links to the jumped header and a wrong parent is rejected.
        uint64 jumpH = TIP + 40;
        _submit(br, jumpH, bytes32(uint256(0xabc)), keccak256("cj"), keccak256("nj"), nl, idx, _zeroA());
        (bytes32 jumped,,,,,) = br.headers(jumpH);
        vm.expectRevert(bytes("parent"));
        this.submitFor(br, jumpH + 1, bytes32(uint256(1)), keccak256("c3"), keccak256("n3"), nl, idx, _zeroA());
        _submit(br, jumpH + 1, jumped, keccak256("c3"), keccak256("n3"), nl, idx, _zeroA());
        assertEq(br.fplTip(), jumpH + 1);
    }

    function testPlusOneRejectsBadParent() public {
        vm.expectRevert(bytes("parent"));
        this.submitFor(br, TIP + 1, keccak256("nope"), keccak256("c"), keccak256("n"), _nextLeaves(), _idx(), _zeroA());
    }

    function testJumpWithProofsAgainstOldRootRotates() public {
        bytes32 nextRoot = keccak256("jump-root");
        bytes32[6] memory nl = _nextLeaves();
        bytes32 claim = keccak256("jump-claim");
        uint64 height = TIP + 1000;
        bytes32 parent = bytes32(uint256(0x1234));
        assertTrue(parent != anchorHash);
        (bytes32 innerHash, bytes32 hash) = _sealed(height, nextRoot, claim);
        ExpectQcInputs strict = new ExpectQcInputs(height, parent, hash, committee, nextRoot, 2300, nl);
        FalconQcBridgeV3 strictBr = _deploy(address(strict), anchorClaim, true, true);
        uint64[4] memory idx = _idx();
        _submit(strictBr, height, parent, claim, nextRoot, nl, idx, _boundA(uint256(committee), idx));
        assertEq(strictBr.fplTip(), height);
        assertEq(strictBr.currentCommitteeRoot(), nextRoot);
        for (uint256 i = 0; i < 6; i++) {
            assertEq(strictBr.currentLeaves(i), nl[i]);
        }
        (bytes32 storedHash, bytes32 storedParent, bytes32 storedInner,,, bool fin) = strictBr.headers(height);
        assertEq(storedHash, hash);
        assertEq(storedParent, parent);
        assertEq(storedInner, innerHash);
        assertTrue(fin);
        (,,,,, bool hole) = strictBr.headers(TIP + 1);
        assertFalse(hole);
    }

    function testJumpWithProofsAgainstNewRootRejected() public {
        bytes32 nextRoot = keccak256("jump-root");
        bytes32[6] memory nl = _nextLeaves();
        bytes32 claim = keccak256("jump-claim");
        uint64 height = TIP + 1000;
        bytes32 parent = bytes32(uint256(0x1234));
        (, bytes32 hash) = _sealed(height, nextRoot, claim);
        ExpectQcInputs strict = new ExpectQcInputs(height, parent, hash, committee, nextRoot, 2300, nl);
        FalconQcBridgeV3 strictBr = _deploy(address(strict), anchorClaim, true, true);
        uint64[4] memory idx = _idx();
        vm.expectRevert(bytes("proof"));
        this.submitFor(strictBr, height, parent, claim, nextRoot, nl, idx, _boundA(uint256(nextRoot), idx));
        assertEq(strictBr.fplTip(), TIP);
        assertEq(strictBr.currentCommitteeRoot(), committee);
    }

    function testJumpRejectsIdxAndDup() public {
        bytes32 nextRoot = keccak256("n");
        bytes32 claim = keccak256("c");
        uint64[4] memory badIdx = [uint64(0), 1, 2, 6];
        vm.expectRevert(bytes("idx"));
        this.submitFor(br, TIP + 7, bytes32(uint256(1)), claim, nextRoot, _nextLeaves(), badIdx, _zeroA());
        uint64[4] memory dup = [uint64(0), 1, 2, 0];
        vm.expectRevert(bytes("dup"));
        this.submitFor(br, TIP + 7, bytes32(uint256(1)), claim, nextRoot, _nextLeaves(), dup, _zeroA());
        assertEq(br.fplTip(), TIP);
    }

    function testRejectsHeightOutOfRange() public {
        bytes32 nextRoot = keccak256("n");
        bytes32 claim = keccak256("c");
        bytes32[6] memory nl = _nextLeaves();
        uint64[4] memory idx = _idx();
        vm.expectRevert(bytes("range"));
        this.submitFor(br, TIP, anchorHash, claim, nextRoot, nl, idx, _zeroA());
        vm.expectRevert(bytes("range"));
        this.submitFor(br, TIP - 1, anchorHash, claim, nextRoot, nl, idx, _zeroA());
        vm.expectRevert(bytes("range"));
        this.submitFor(br, TIP + MAX_JUMP + 1, bytes32(uint256(9)), claim, nextRoot, nl, idx, _zeroA());
        _submit(br, TIP + MAX_JUMP, bytes32(uint256(9)), claim, nextRoot, nl, idx, _zeroA());
        assertEq(br.fplTip(), TIP + MAX_JUMP);
    }

    function testMutatedClaimRootFailsOpen() public {
        bytes32 nextRoot = keccak256("n");
        bytes32 claim = keccak256("c");
        (bytes32 innerHash, bytes32 hash) = _sealed(TIP + 4, nextRoot, claim);
        uint256[2][2][4] memory b;
        uint256[2][4] memory c;
        vm.expectRevert(bytes("open"));
        br.submitHeaderWithProofs(
            TIP + 4,
            hash,
            bytes32(uint256(1)),
            innerHash,
            keccak256("mutated-claim"),
            nextRoot,
            _nextLeaves(),
            _idx(),
            _zeroA(),
            b,
            c
        );
    }

    function testMutatedNextLeavesFailsProof() public {
        bytes32 nextRoot = keccak256("n");
        bytes32 claim = keccak256("c");
        uint64 height = TIP + 4;
        bytes32 parent = bytes32(uint256(1));
        bytes32[6] memory honest = _nextLeaves();
        (bytes32 innerHash, bytes32 hash) = _sealed(height, nextRoot, claim);
        ExpectQcInputs strict = new ExpectQcInputs(height, parent, hash, committee, nextRoot, 2300, honest);
        FalconQcBridgeV3 strictBr = _deploy(address(strict), anchorClaim, true, true);
        bytes32[6] memory bad = honest;
        bad[0] = keccak256("mutated-leaf");
        uint256[2][2][4] memory b;
        uint256[2][4] memory c;
        vm.expectRevert(bytes("proof"));
        strictBr.submitHeaderWithProofs(
            height, hash, parent, innerHash, claim, nextRoot, bad, _idx(), _boundA(uint256(committee), _idx()), b, c
        );
    }

    function testOpenClaimAgainstJumpHeader() public {
        bytes32 noteOld = keccak256("old-note");
        bytes32 noteNew = keccak256("new-note");
        address payable dest = payable(address(0xBEEF));
        uint256 amount = 1 ether;
        bytes32 leafOld = br.withdrawLeaf(noteOld, dest, amount, false);
        bytes32 leafNew = br.withdrawLeaf(noteNew, dest, amount, false);
        // Redeploy so the anchor root is the older cumulative leaf.
        FalconQcBridgeV3 jumpBr = _deployN(address(ver), leafOld, true, true, _one(noteOld, dest, amount, false));
        br = jumpBr;
        jumpBr.fundPool{value: 3 ether}(0);
        bytes32 jumpRoot = sha256(abi.encodePacked(leafOld, leafNew));
        uint64 jumpH = TIP + 777;
        _submit(jumpBr, jumpH, bytes32(uint256(0x99)), jumpRoot, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());

        // leafOld is in V2's anchor root (anchorLeafCount 1): V2 can pay it, so V3 refuses it.
        bytes32[] memory proofOld = new bytes32[](1);
        proofOld[0] = leafNew;
        vm.expectRevert(bytes("anchor"));
        jumpBr.openClaim(noteOld, dest, amount, false, proofOld, 0, jumpH);

        bytes32[] memory proofNew = new bytes32[](1);
        proofNew[0] = leafOld;
        jumpBr.openClaim(noteNew, dest, amount, false, proofNew, 1, jumpH);

        bytes32[] memory empty;
        vm.expectRevert(bytes("header"));
        jumpBr.openClaim(keccak256("hole-note"), dest, amount, false, empty, 1, TIP + 100);

        // The anchor header itself is V2's: nothing opens against it on V3.
        FalconQcBridgeV3 anchorBr = _deployN(address(ver), leafOld, true, true, _one(noteOld, dest, amount, false));
        anchorBr.fundPool{value: 1 ether}(0);
        vm.expectRevert(bytes("anchor"));
        anchorBr.openClaim(noteOld, dest, amount, false, empty, 0, TIP);
        uint256 before = dest.balance;
        vm.prank(dest);
        jumpBr.take(noteNew);
        assertEq(dest.balance, before + amount);
    }

    /// @dev FM ruling (Copilot r4226045086 on #38): V3 never pays a note V2's anchor header commits.
    ///      Copilot on #45: indices are not stable, because FPL orders a header's leaves as all ETH
    ///      then all USDC. Anchor = [e0, e1, u0]; a later header = [e0, e1, e2, u0, u1], so the
    ///      anchor USDC note u0 moves from index 2 to 3 (>= the anchor's 3 leaves). V3 must still
    ///      refuse it: the check is set membership, not an index cutoff.
    function testOpenClaimRefusesV2AnchorNotesAfterRailShift() public {
        address payable dest = payable(address(0xBEEF));
        uint256 amount = 1 ether;
        uint256 uamt = 1e6;
        bytes32[] memory notes = new bytes32[](5); // e0, e1, e2, u0, u1
        bytes32[] memory lv = new bytes32[](5);
        for (uint256 i = 0; i < 5; i++) {
            notes[i] = keccak256(abi.encodePacked("anchor-note", i));
            lv[i] = br.withdrawLeaf(notes[i], dest, i >= 3 ? uamt : amount, i >= 3);
        }
        bytes32[] memory an = new bytes32[](3);
        address[] memory ad = new address[](3);
        uint64[] memory aa = new uint64[](3);
        bool[] memory au = new bool[](3);
        (an[0], an[1], an[2]) = (notes[0], notes[1], notes[3]);
        (ad[0], ad[1], ad[2]) = (dest, dest, dest);
        (aa[0], aa[1], aa[2]) = (uint64(amount), uint64(amount), uint64(uamt));
        (au[0], au[1], au[2]) = (false, false, true);
        bytes32 a01 = sha256(abi.encodePacked(lv[0], lv[1]));
        bytes32 anchorRoot = sha256(abi.encodePacked(a01, sha256(abi.encodePacked(lv[3], lv[3]))));
        // later: [e0, e1, e2, u0, u1] -> layers: (e0e1)(e2u0)(u1u1) -> ((..)(..))((u1u1)(u1u1))
        bytes32 n23 = sha256(abi.encodePacked(lv[2], lv[3]));
        bytes32 n44 = sha256(abi.encodePacked(lv[4], lv[4]));
        bytes32 m01 = sha256(abi.encodePacked(a01, n23));
        bytes32 m22 = sha256(abi.encodePacked(n44, n44));
        bytes32 laterRoot = sha256(abi.encodePacked(m01, m22));

        FalconQcBridgeV3 b = _deployN(address(ver), anchorRoot, true, true, _setOf(an, ad, aa, au));
        assertEq(b.anchorHeight(), TIP);
        assertEq(b.anchorLeafCount(), 3);
        assertTrue(b.anchorSet() != address(0));
        br = b;
        b.fundPool{value: 10 ether}(0);
        usdc.mint(address(this), 10e6);
        usdc.approve(address(b), 10e6);
        b.fundPool(10e6);
        uint64 h = TIP + 5;
        _submit(b, h, bytes32(uint256(5)), laterRoot, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());

        // u0 (V2-committed) at its new index 3 in the later root: proof is valid, still "anchor".
        bytes32[] memory p = new bytes32[](3);
        p[0] = lv[2];
        p[1] = a01;
        p[2] = m22;
        assertEq(ck.merkleRoot(lv[3], p, 3), laterRoot);
        vm.expectRevert(bytes("anchor"));
        b.openClaim(notes[3], dest, uamt, true, p, 3, h);
        // e1 (V2-committed, index unchanged) -> "anchor".
        bytes32[] memory q = new bytes32[](3);
        q[0] = lv[0];
        q[1] = n23;
        q[2] = m22;
        vm.expectRevert(bytes("anchor"));
        b.openClaim(notes[1], dest, amount, false, q, 1, h);
        // Against the anchor header itself -> "anchor" (height rule), whatever the proof.
        vm.expectRevert(bytes("anchor"));
        b.openClaim(notes[3], dest, uamt, true, p, 3, TIP);
        // e2 (post-anchor, index 2 < 3 leaves in the anchor) opens: no index cutoff.
        bytes32[] memory r = new bytes32[](3);
        r[0] = lv[3];
        r[1] = a01;
        r[2] = m22;
        b.openClaim(notes[2], dest, amount, false, r, 2, h);
        // u1 (post-anchor) opens at index 4.
        bytes32[] memory t = new bytes32[](3);
        t[0] = lv[4];
        t[1] = n44;
        t[2] = m01;
        b.openClaim(notes[4], dest, uamt, true, t, 4, h);
        assertEq(b.ethPool(), 10 ether - amount);
        assertEq(b.usdcPool(), 10e6 - uamt);
    }

    /// @dev The real anchor: the 9 notes of V2 header 353130 rebuild its claimRoot on-chain.
    function testRealAnchorSetMatchesV2Header353130() public {
        address set = _realAnchorSet();
        (bytes32 root, uint256 n) = ck.anchorSetRoot(set);
        assertEq(root, REAL_ANCHOR_ROOT);
        assertEq(n, 9);
        assertEq(set.code.length, 1 + 9 * 61);
        (bytes32[] memory ids,,,) = _realAnchorEntries();
        for (uint256 i = 0; i < 9; i++) {
            assertTrue(ck.inAnchorSet(set, ids[i]));
        }
        // note 9 (fixture leaf index 9) is not in the anchor set.
        assertFalse(ck.inAnchorSet(set, 0xf43f5db6d119e50f509f2c3b3b85e9565e9f82f89a951d33e089b26d29b39c25));
        assertFalse(ck.inAnchorSet(address(0), ids[0]));
        // Data-only: a call to the set does nothing and returns nothing.
        (bool ok, bytes memory ret) = set.call(abi.encodeWithSignature("owner()"));
        assertTrue(ok);
        assertEq(ret.length, 0);
    }

    /// @dev Copilot on #45: with raw leaf hashes, the root's two children (or any inner layer)
    ///      would rebuild the same root while protecting no real note. Entries are withdrawal
    ///      preimages, so only the real 9 notes rebuild V2's root; anything else reverts "set".
    function testAnchorSetMustBeTheRealNotes() public {
        MockQcV1 v1 = new MockQcV1();
        MockDestLock dl = new MockDestLock();
        MockQcV2 real = _mockV2(address(ver), REAL_ANCHOR_ROOT, true, true);
        address c = address(_ck());
        (bytes32[] memory n, address[] memory d, uint64[] memory a, bool[] memory u) = _realAnchorEntries();
        // Two inner-node-like "notes": refused.
        bytes32[] memory n2 = new bytes32[](2);
        address[] memory d2 = new address[](2);
        uint64[] memory a2 = new uint64[](2);
        bool[] memory u2 = new bool[](2);
        n2[0] = keccak256("left-child");
        n2[1] = keccak256("right-child");
        address s2 = _setOf(n2, d2, a2, u2);
        vm.expectRevert(bytes("set"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(real), address(v1), address(dl), MAX_JUMP, c, TIP, s2);
        // Raw code holding two 32-byte hashes (not 61-byte entries): refused.
        address raw = address(0xA11C);
        vm.etch(raw, abi.encodePacked(bytes1(0x00), keccak256("l"), keccak256("r")));
        vm.expectRevert(bytes("set"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(real), address(v1), address(dl), MAX_JUMP, c, TIP, raw);
        // 8 of the 9: refused.
        bytes32[] memory n8 = new bytes32[](8);
        address[] memory d8 = new address[](8);
        uint64[] memory a8 = new uint64[](8);
        bool[] memory u8 = new bool[](8);
        for (uint256 i = 0; i < 8; i++) {
            (n8[i], d8[i], a8[i], u8[i]) = (n[i], d[i], a[i], u[i]);
        }
        address s8 = _setOf(n8, d8, a8, u8);
        vm.expectRevert(bytes("set"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(real), address(v1), address(dl), MAX_JUMP, c, TIP, s8);
        // A flag byte other than 0/1: refused.
        vm.etch(raw, abi.encodePacked(bytes1(0x00), n[0], d[0], a[0], bytes1(0x02)));
        vm.expectRevert(bytes("set"));
        ck.anchorSetRoot(raw);
        // The 9 out of order: refused.
        (n[0], n[1]) = (n[1], n[0]);
        (a[0], a[1]) = (a[1], a[0]);
        address sw = _setOf(n, d, a, u);
        vm.expectRevert(bytes("set"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(real), address(v1), address(dl), MAX_JUMP, c, TIP, sw);
        // The real 9 in FPL order: accepted.
        address good = _realAnchorSet();
        FalconQcBridgeV3 ok =
            new FalconQcBridgeV3(address(usdc), address(ver), address(real), address(v1), address(dl), MAX_JUMP, c, TIP, good);
        assertEq(ok.anchorLeafCount(), 9);
    }

    /// @dev A padded copy (right child equal to its left sibling) is not a canonical index:
    ///      otherwise V2's last anchor leaf could reappear at an index >= anchorLeafCount.
    function testMerkleRejectsPaddedCopy() public {
        address payable dest = payable(address(0xBEEF));
        uint256 amount = 1 ether;
        bytes32 n2 = keccak256("p2");
        bytes32 l0 = br.withdrawLeaf(keccak256("p0"), dest, amount, false);
        bytes32 l1 = br.withdrawLeaf(keccak256("p1"), dest, amount, false);
        bytes32 l2 = br.withdrawLeaf(n2, dest, amount, false);
        bytes32 a01 = sha256(abi.encodePacked(l0, l1));
        bytes32 root3 = sha256(abi.encodePacked(a01, sha256(abi.encodePacked(l2, l2))));
        bytes32[] memory p = new bytes32[](2);
        p[0] = l2;
        p[1] = a01;
        assertEq(ck.merkleRoot(l2, p, 2), root3);
        vm.expectRevert(bytes("pad"));
        ck.merkleRoot(l2, p, 3);
        // On V3 (empty anchor): leaf 2 opens at index 2 only.
        br.fundPool{value: amount}(0);
        uint64 h = TIP + 9;
        _submit(br, h, bytes32(uint256(9)), root3, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());
        vm.expectRevert(bytes("pad"));
        br.openClaim(n2, dest, amount, false, p, 3, h);
        br.openClaim(n2, dest, amount, false, p, 2, h);
    }

    function testConstructorAnchorArgs() public {
        MockQcV1 v1 = new MockQcV1();
        MockDestLock dl = new MockDestLock();
        address c = address(_ck());
        // anchorHeight must equal V2's fplTip at deploy (V2 moved on, or a typo -> revert).
        vm.expectRevert(bytes("tip"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(v2m), address(v1), address(dl), MAX_JUMP, c, TIP + 1, address(0));
        vm.expectRevert(bytes("tip"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(v2m), address(v1), address(dl), MAX_JUMP, c, 0, address(0));
        // setUp's anchor root is empty: a non-empty set is refused.
        address s1 = _one(keccak256("x"), address(0xBEEF), 1, false);
        vm.expectRevert(bytes("set"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(v2m), address(v1), address(dl), MAX_JUMP, c, TIP, s1);
        // A non-empty root: no set, the wrong note, or a non-set contract are refused.
        bytes32 oneRoot = ck.withdrawLeaf(keccak256("root-note"), address(0xBEEF), 1, false);
        MockQcV2 full = _mockV2(address(ver), oneRoot, true, true);
        vm.expectRevert(bytes("set"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(full), address(v1), address(dl), MAX_JUMP, c, TIP, address(0));
        vm.expectRevert(bytes("set"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(full), address(v1), address(dl), MAX_JUMP, c, TIP, s1);
        vm.expectRevert(bytes("set"));
        new FalconQcBridgeV3(address(usdc), address(ver), address(full), address(v1), address(dl), MAX_JUMP, c, TIP, address(ver));
        // The matching one-note set is accepted.
        address good = _one(keccak256("root-note"), address(0xBEEF), 1, false);
        FalconQcBridgeV3 ok =
            new FalconQcBridgeV3(address(usdc), address(ver), address(full), address(v1), address(dl), MAX_JUMP, c, TIP, good);
        assertEq(ok.anchorLeafCount(), 1);
        // The set contract itself refuses to be empty.
        vm.expectRevert(bytes("len"));
        new FalconQcBridgeV3AnchorSet(new bytes32[](0), new address[](0), new uint64[](0), new bool[](0));
    }

    function testOpenClaimRejectedWhenV1Paid() public {
        _expectPaid("paid-v1", true, false, false);
    }

    function testOpenClaimRejectedWhenV2Paid() public {
        _expectPaid("paid-v2", false, true, false);
    }

    function testOpenClaimRejectedWhenDestLockPaid() public {
        _expectPaid("paid-dl", false, false, true);
    }

    function testFundPoolEmitsNoDeposit() public {
        usdc.mint(address(this), 50);
        usdc.approve(address(br), 50);
        uint256 ethBefore = br.ethPool();
        uint256 usdcBefore = br.usdcPool();
        vm.recordLogs();
        br.fundPool{value: 2 ether}(50);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 2);
        bytes32 funded = keccak256("PoolFunded(address,address,uint256)");
        for (uint256 i = 0; i < logs.length; i++) {
            assertEq(logs[i].topics[0], funded);
            assertTrue(logs[i].topics[0] != DEPOSIT_TOPIC);
            assertTrue(logs[i].topics[0] != OLD_DEPOSIT_TOPIC);
        }
        assertEq(br.ethPool(), ethBefore + 2 ether);
        assertEq(br.usdcPool(), usdcBefore + 50);
        assertEq(br.depositNonce(), 0);
    }

    function testDepositsEmitExactLayout() public {
        vm.warp(1_700_000_000);
        address alice = address(0xA11CE);
        bytes20 dest = bytes20(uint160(0xD257));
        vm.deal(alice, 5 ether);
        uint256 nonce = br.depositNonce();
        bytes32 expectId = br.deriveDepositId(nonce);

        vm.recordLogs();
        vm.prank(alice);
        bytes32 id = br.depositEth{value: 1 ether}(dest);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(id, expectId);
        assertEq(logs.length, 1);
        assertEq(logs[0].topics.length, 3);
        assertEq(logs[0].topics[0], DEPOSIT_TOPIC);
        assertEq(logs[0].topics[1], id);
        assertEq(address(uint160(uint256(logs[0].topics[2]))), alice);
        (bytes20 gotDest, address token, uint256 amount, uint64 ts) =
            abi.decode(logs[0].data, (bytes20, address, uint256, uint64));
        assertEq(uint160(gotDest), uint160(dest));
        assertEq(token, address(0));
        assertEq(amount, 1 ether);
        assertEq(ts, 1_700_000_000);
        assertEq(br.ethPool(), 1 ether);
        assertEq(br.noteDeposit(br.refundNoteId(id)), id);
        assertEq(br.noteDeposit(br.mintNoteId(id)), id);
        assertTrue(br.refundNoteId(id) != id);
        assertTrue(br.refundNoteId(id) != br.mintNoteId(id));

        usdc.mint(alice, 1_000_000);
        vm.prank(alice);
        usdc.approve(address(br), 1_000_000);
        bytes32 expectUsdc = br.deriveDepositId(1);
        vm.recordLogs();
        vm.prank(alice);
        bytes32 usdcId = br.depositUsdc(dest, 1_000_000);
        logs = vm.getRecordedLogs();
        assertEq(usdcId, expectUsdc);
        assertEq(logs.length, 1);
        assertEq(logs[0].topics[0], DEPOSIT_TOPIC);
        assertEq(logs[0].topics[1], usdcId);
        assertEq(address(uint160(uint256(logs[0].topics[2]))), alice);
        (gotDest, token, amount, ts) = abi.decode(logs[0].data, (bytes20, address, uint256, uint64));
        assertEq(token, address(usdc));
        assertEq(amount, 1_000_000);
        assertEq(br.depositNonce(), 2);
        (address sender, bytes20 storedDest, address storedToken, uint256 storedAmt, uint64 storedTs, bool exists, bool resolved)
            = br.pegIns(id);
        assertEq(sender, alice);
        assertEq(uint160(storedDest), uint160(dest));
        assertEq(storedToken, address(0));
        assertEq(storedAmt, 1 ether);
        assertEq(storedTs, 1_700_000_000);
        assertTrue(exists);
        assertFalse(resolved);
    }

    function testDepositRejectsZero() public {
        vm.expectRevert(bytes("zero"));
        br.depositEth{value: 0}(bytes20(uint160(1)));
        vm.expectRevert(bytes("dest"));
        br.depositEth{value: 1}(bytes20(0));
        vm.expectRevert(bytes("zero"));
        br.depositUsdc(bytes20(uint160(1)), 0);
        vm.expectRevert(bytes("dest"));
        br.depositUsdc(bytes20(0), 1);
        vm.deal(address(this), uint256(type(uint64).max) + 1);
        vm.expectRevert(bytes("amount"));
        br.depositEth{value: uint256(type(uint64).max) + 1}(bytes20(uint160(1)));
        vm.expectRevert(bytes("zero"));
        br.fundPool(0);
    }

    function testRefundPaysSenderOnce() public {
        address alice = address(0xA11CE);
        vm.deal(alice, 5 ether);
        vm.prank(alice);
        bytes32 id = br.depositEth{value: 1 ether}(bytes20(uint160(0xD257)));
        // Extra liquidity so a second payout would not fail merely for an empty pool.
        br.fundPool{value: 5 ether}(0);
        assertEq(br.ethPool(), 6 ether);

        bytes32 rid = br.refundNoteId(id);
        bytes32 leaf = br.withdrawLeaf(rid, alice, 1 ether, false);
        uint64 height = TIP + 20;
        _submit(br, height, bytes32(uint256(7)), leaf, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());

        bytes32[] memory proof;
        br.openClaim(rid, payable(alice), 1 ether, false, proof, 0, height);
        assertEq(br.ethPool(), 5 ether);
        assertTrue(_resolved(id));

        vm.expectRevert(bytes("exists"));
        br.openClaim(rid, payable(alice), 1 ether, false, proof, 0, height);

        vm.prank(address(0xB0B));
        vm.expectRevert(bytes("dest"));
        br.take(rid);

        uint256 before = alice.balance;
        vm.prank(alice);
        br.take(rid);
        assertEq(alice.balance, before + 1 ether);

        vm.prank(alice);
        vm.expectRevert(bytes("state"));
        br.take(rid);
    }

    function testRefundRejectsWrongDestEvenIfLeafMatches() public {
        address alice = address(0xA11CE);
        address attacker = address(0xBAD);
        vm.deal(alice, 2 ether);
        vm.prank(alice);
        bytes32 id = br.depositEth{value: 1 ether}(bytes20(uint160(0xD257)));
        br.fundPool{value: 1 ether}(0);
        bytes32 rid = br.refundNoteId(id);
        bytes32 leaf = br.withdrawLeaf(rid, attacker, 1 ether, false);
        _submit(br, TIP + 3, bytes32(uint256(1)), leaf, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());
        bytes32[] memory proof;
        vm.expectRevert(bytes("refund-dest"));
        br.openClaim(rid, payable(attacker), 1 ether, false, proof, 0, TIP + 3);
        assertFalse(_resolved(id));
    }

    function testMintAndRefundCannotBothPay() public {
        address alice = address(0xA11CE);
        address beef = address(0xBEEF);
        vm.deal(alice, 2 ether);
        vm.prank(alice);
        bytes32 id = br.depositEth{value: 1 ether}(bytes20(uint160(0xD257)));
        br.fundPool{value: 10 ether}(0);

        bytes32 rid = br.refundNoteId(id);
        bytes32 mid = br.mintNoteId(id);
        bytes32 refundLeaf = br.withdrawLeaf(rid, alice, 1 ether, false);
        bytes32 mintLeaf = br.withdrawLeaf(mid, beef, 1 ether, false);
        bytes32 root = sha256(abi.encodePacked(refundLeaf, mintLeaf));
        uint64 height = TIP + 30;
        _submit(br, height, bytes32(uint256(3)), root, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());

        bytes32[] memory refundProof = new bytes32[](1);
        refundProof[0] = mintLeaf;
        br.openClaim(rid, payable(alice), 1 ether, false, refundProof, 0, height);
        uint256 poolAfterRefund = br.ethPool();

        bytes32[] memory mintProof = new bytes32[](1);
        mintProof[0] = refundLeaf;
        vm.expectRevert(bytes("resolved"));
        br.openClaim(mid, payable(beef), 1 ether, false, mintProof, 1, height);
        assertEq(br.ethPool(), poolAfterRefund);
        assertTrue(_resolved(id));

        vm.prank(alice);
        br.take(rid);
    }

    function testMintFirstBlocksRefund() public {
        address alice = address(0xA11CE);
        address beef = address(0xBEEF);
        vm.deal(alice, 2 ether);
        vm.prank(alice);
        bytes32 id = br.depositEth{value: 1 ether}(bytes20(uint160(0xD257)));
        br.fundPool{value: 10 ether}(0);

        bytes32 rid = br.refundNoteId(id);
        bytes32 mid = br.mintNoteId(id);
        bytes32 refundLeaf = br.withdrawLeaf(rid, alice, 1 ether, false);
        bytes32 mintLeaf = br.withdrawLeaf(mid, beef, 1 ether, false);
        bytes32 root = sha256(abi.encodePacked(refundLeaf, mintLeaf));
        uint64 height = TIP + 31;
        _submit(br, height, bytes32(uint256(4)), root, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());

        bytes32[] memory mintProof = new bytes32[](1);
        mintProof[0] = refundLeaf;
        br.openClaim(mid, payable(beef), 1 ether, false, mintProof, 1, height);

        bytes32[] memory refundProof = new bytes32[](1);
        refundProof[0] = mintLeaf;
        vm.expectRevert(bytes("resolved"));
        br.openClaim(rid, payable(alice), 1 ether, false, refundProof, 0, height);
        assertTrue(_resolved(id));
    }

    function testUsdcRefundPaysOnce() public {
        address alice = address(0xA11CE);
        usdc.mint(alice, 5_000_000);
        vm.prank(alice);
        usdc.approve(address(br), 5_000_000);
        vm.prank(alice);
        bytes32 id = br.depositUsdc(bytes20(uint160(0xD257)), 1_000_000);
        bytes32 rid = br.refundNoteId(id);
        bytes32 leaf = br.withdrawLeaf(rid, alice, 1_000_000, true);
        _submit(br, TIP + 8, bytes32(uint256(8)), leaf, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());
        bytes32[] memory proof;
        br.openClaim(rid, payable(alice), 1_000_000, true, proof, 0, TIP + 8);
        vm.expectRevert(bytes("exists"));
        br.openClaim(rid, payable(alice), 1_000_000, true, proof, 0, TIP + 8);
        vm.prank(alice);
        br.take(rid);
        assertEq(usdc.balanceOf(alice), 5_000_000);
    }

    function testReentrantTakeFailsStateAndPaysOnce() public {
        // Non-deposit leaf, so refund checks do not run.
        uint256 amount = 1 ether;
        bytes32 note = keccak256("reenter-note");
        ReentrantDest recv = new ReentrantDest(br, note);
        br.fundPool{value: amount}(0);
        bytes32 leaf = br.withdrawLeaf(note, address(recv), amount, false);
        uint64 height = TIP + 21;
        _submit(br, height, bytes32(uint256(21)), leaf, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());
        bytes32[] memory proof;
        br.openClaim(note, payable(address(recv)), amount, false, proof, 0, height);

        uint256 before = address(recv).balance;
        uint256 bridgeBefore = address(br).balance;
        recv.take();
        assertTrue(recv.sawReenter());
        assertFalse(recv.reenterOk());
        bytes memory data = recv.reenterData();
        assertEq(_revertSelector(data), bytes4(hex"08c379a0"));
        assertEq(data, abi.encodeWithSignature("Error(string)", "state"));
        assertEq(address(recv).balance, before + amount);
        assertEq(address(br).balance, bridgeBefore - amount);
    }

    function testRefundRejectsWrongAmount() public {
        address alice = address(0xA11CE);
        vm.deal(alice, 2 ether);
        vm.prank(alice);
        bytes32 id = br.depositEth{value: 1 ether}(bytes20(uint160(0xD257)));
        bytes32 rid = br.refundNoteId(id);
        uint256 wrongAmount = 1 ether - 1;
        bytes32 leaf = br.withdrawLeaf(rid, alice, wrongAmount, false);
        uint64 height = TIP + 22;
        _submit(br, height, bytes32(uint256(22)), leaf, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());
        bytes32[] memory proof;
        vm.expectRevert(bytes("refund-amt"));
        br.openClaim(rid, payable(alice), wrongAmount, false, proof, 0, height);
        assertFalse(_resolved(id));
        assertEq(br.ethPool(), 1 ether);
    }

    function testRefundRejectsWrongAsset() public {
        address alice = address(0xA11CE);
        vm.deal(alice, 2 ether);
        vm.prank(alice);
        bytes32 id = br.depositEth{value: 1 ether}(bytes20(uint160(0xD257)));
        bytes32 rid = br.refundNoteId(id);
        bytes32 leaf = br.withdrawLeaf(rid, alice, 1 ether, true);
        uint64 height = TIP + 23;
        _submit(br, height, bytes32(uint256(23)), leaf, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());
        bytes32[] memory proof;
        vm.expectRevert(bytes("refund-asset"));
        br.openClaim(rid, payable(alice), 1 ether, true, proof, 0, height);
        assertFalse(_resolved(id));
        assertEq(br.ethPool(), 1 ether);
        assertEq(br.usdcPool(), 0);
    }

    function testFalseUsdcReturnsRevertXfer() public {
        FalseReturnUSDC token = new FalseReturnUSDC();
        FalconQcBridgeV3 local = _deployWithToken(address(token));
        token.setTransferFromReturnsFalse(true);
        vm.expectRevert(bytes("xfer"));
        local.depositUsdc(bytes20(uint160(0xD257)), 100);
        assertEq(local.depositNonce(), 0);
        assertEq(local.usdcPool(), 0);

        token.setTransferFromReturnsFalse(false);
        token.mint(address(this), 500);
        token.approve(address(local), 500);
        local.fundPool(200);
        assertEq(local.usdcPool(), 200);

        address payable dest = payable(address(0xBEEF));
        uint256 amount = 50;
        bytes32 note = keccak256("false-xfer-note");
        bytes32 leaf = local.withdrawLeaf(note, dest, amount, true);
        uint64 height = TIP + 24;
        _submit(local, height, bytes32(uint256(24)), leaf, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());
        bytes32[] memory proof;
        local.openClaim(note, dest, amount, true, proof, 0, height);
        token.setTransferReturnsFalse(true);
        vm.prank(dest);
        vm.expectRevert(bytes("xfer"));
        local.take(note);
        assertEq(token.balanceOf(dest), 0);
        assertEq(local.usdcPool(), 150);
    }

    function testOpenClaimWrongRecipientFailsMerkle() public {
        address alice = address(0xA11CE);
        address bob = address(0xB0B);
        uint256 amount = 1 ether;
        bytes32 note = keccak256("recip-note");
        bytes32 leaf = br.withdrawLeaf(note, alice, amount, false);
        br.fundPool{value: amount}(0);
        uint64 height = TIP + 25;
        _submit(br, height, bytes32(uint256(25)), leaf, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());
        bytes32[] memory proof;
        vm.expectRevert(bytes("merkle"));
        br.openClaim(note, payable(bob), amount, false, proof, 0, height);
        assertEq(br.ethPool(), amount);
    }

    function testOpenClaimWrongAmountFailsMerkle() public {
        address alice = address(0xA11CE);
        uint256 amount = 1 ether;
        uint256 otherAmount = 2 ether;
        bytes32 note = keccak256("amt-note");
        bytes32 leaf = br.withdrawLeaf(note, alice, amount, false);
        br.fundPool{value: otherAmount}(0);
        uint64 height = TIP + 26;
        _submit(br, height, bytes32(uint256(26)), leaf, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());
        bytes32[] memory proof;
        vm.expectRevert(bytes("merkle"));
        br.openClaim(note, payable(alice), otherAmount, false, proof, 0, height);
        assertEq(br.ethPool(), otherAmount);
    }

    function testFeeOnTransferUsdcRevertsFee() public {
        FeeOnTransferUSDC token = new FeeOnTransferUSDC();
        FalconQcBridgeV3 local = _deployWithToken(address(token));
        uint256 amount = 100;
        token.mint(address(this), amount * 2);
        token.approve(address(local), amount * 2);

        vm.expectRevert(bytes("fee"));
        local.depositUsdc(bytes20(uint160(0xD257)), amount);
        assertEq(local.depositNonce(), 0);
        assertEq(local.usdcPool(), 0);
        assertEq(token.balanceOf(address(local)), 0);

        vm.expectRevert(bytes("fee"));
        local.fundPool(amount);
        assertEq(local.usdcPool(), 0);
        assertEq(local.ethPool(), 0);
        assertEq(token.balanceOf(address(local)), 0);

        bytes32 id = local.depositEth{value: 1 ether}(bytes20(uint160(0xD257)));
        assertEq(id, local.deriveDepositId(0));
        assertEq(local.ethPool(), 1 ether);
        assertEq(local.usdcPool(), 0);
        (address sender, , address storedToken, uint256 storedAmt, , bool exists, bool resolved) = local.pegIns(id);
        assertEq(sender, address(this));
        assertEq(storedToken, address(0));
        assertEq(storedAmt, 1 ether);
        assertTrue(exists);
        assertFalse(resolved);
    }

    function testNoOwnerGuardianOrAdmin() public {
        bytes4[8] memory forbidden = [
            bytes4(keccak256("owner()")),
            bytes4(keccak256("guardian()")),
            bytes4(keccak256("pause()")),
            bytes4(keccak256("unpause()")),
            bytes4(keccak256("rescue(address,uint256)")),
            bytes4(keccak256("refund(bytes32)")),
            bytes4(keccak256("reanchor(uint64)")),
            bytes4(keccak256("transferOwnership(address)"))
        ];
        for (uint256 i = 0; i < forbidden.length; i++) {
            (bool ok, ) = address(br).call(abi.encodePacked(forbidden[i]));
            assertFalse(ok);
        }
        (bool bare, ) = address(br).call{value: 1 ether}("");
        assertFalse(bare);
        assertEq(br.ethPool(), 0);
    }

    /// @dev Production Groth16Verifier against a committed V2 proof bundle.
    ///      Skips until `fixtures/v2-groth16/plus1-submit.json` exists. No synthesized proof.
    function testRealGroth16VerifierV2Fixture() public {
        string memory path = "fixtures/v2-groth16/plus1-submit.json";
        if (!vm.exists(path)) {
            console.log(
                "TODO: prover must commit falcon-pl-rs/crates/fd-pl/scripts/fixtures/v2-groth16/plus1-submit.json copied from real outputs, not synthesized. VK: run/snark/falcon-qc-v2-one/vk.sol.json (the key deployed as Groth16Verifier 0x2Cb70e9f082F2DF91E9A5e6E7C9DF6b8b11B4F80). Proofs: four proof.json files from run/snark/falcon-qc-v2-prover/h-<height>/idx-<signerIndex>-<seat>/proof.json for one accepted V2 header (a +1 step). Optional second file fixtures/v2-groth16/jump-submit.json for a jump. Schema: { \"vk\": SolidityVk, \"anchor\": { \"tip\",\"hash\",\"parent\",\"innerHash\",\"claimRoot\",\"committeeRoot\",\"leaves\"[6] all 0x bytes32 }, \"height\",\"hash\",\"parent\",\"innerHash\",\"claimRoot\",\"nextCommitteeRoot\",\"nextLeaves\"[6],\"idx\"[4],\"proofs\"[4] each SolidityProof }."
            );
            vm.skip(true);
            return;
        }
        _acceptRealFixture(path);
        _rejectTamperedProof(path);
        _rejectTamperedLeaves(path);
        string memory jumpPath = "fixtures/v2-groth16/jump-submit.json";
        if (vm.exists(jumpPath)) {
            _acceptRealFixture(jumpPath);
        }
    }

    function _expectPaid(bytes memory reason, bool onV1, bool onV2, bool onDl) internal {
        bytes32 note = keccak256(abi.encodePacked(reason));
        address payable dest = payable(address(0xBEEF));
        uint256 amount = 1 ether;
        bytes32 leaf = br.withdrawLeaf(note, dest, amount, false);
        br.fundPool{value: amount}(0);
        uint64 height = TIP + 11;
        _submit(br, height, bytes32(uint256(11)), leaf, keccak256("nr"), _nextLeaves(), _idx(), _zeroA());
        if (onV1) v1m.setPaid(note, dest);
        if (onV2) v2m.setPaid(note, dest);
        if (onDl) dlm.setPaid(note, dest);
        bytes32[] memory proof;
        vm.expectRevert(reason);
        br.openClaim(note, dest, amount, false, proof, 0, height);
        assertEq(br.ethPool(), amount);
    }

    function _mockV2(address verifier_, bytes32 claimRoot, bool finalized, bool rootMatch)
        internal
        returns (MockQcV2)
    {
        return new MockQcV2(
            address(usdc),
            verifier_,
            TIP,
            anchorHash,
            anchorParent,
            anchorInner,
            claimRoot,
            committee,
            leaves,
            finalized,
            rootMatch
        );
    }

    function _deploy(address verifier_, bytes32 claimRoot, bool finalized, bool rootMatch)
        internal
        returns (FalconQcBridgeV3 bridge)
    {
        // Only empty anchor roots here; tests with a non-empty anchor use `_deployN` with a set.
        require(claimRoot == bytes32(0), "use _deployN");
        bridge = _deployN(verifier_, claimRoot, finalized, rootMatch, address(0));
    }

    function _deployN(address verifier_, bytes32 claimRoot, bool finalized, bool rootMatch, address anchorSet_)
        internal
        returns (FalconQcBridgeV3 bridge)
    {
        v2m = _mockV2(verifier_, claimRoot, finalized, rootMatch);
        v1m = new MockQcV1();
        dlm = new MockDestLock();
        bridge = new FalconQcBridgeV3(
            address(usdc), verifier_, address(v2m), address(v1m), address(dlm), MAX_JUMP, address(_ck()), TIP, anchorSet_
        );
    }

    function _setOf(bytes32[] memory n, address[] memory d, uint64[] memory a, bool[] memory u) internal returns (address) {
        return address(new FalconQcBridgeV3AnchorSet(n, d, a, u));
    }

    function _one(bytes32 note, address dest, uint256 amount, bool isUsdc) internal returns (address) {
        bytes32[] memory n = new bytes32[](1);
        address[] memory d = new address[](1);
        uint64[] memory a = new uint64[](1);
        bool[] memory u = new bool[](1);
        n[0] = note;
        d[0] = dest;
        a[0] = uint64(amount);
        u[0] = isUsdc;
        return _setOf(n, d, a, u);
    }

    /// @dev The 9 notes V2 header 353130 commits (5 ETH then 4 USDC, FPL leaf order), from
    ///      fixtures/eth-claim-proof/v2-header-353130.json (#33): `status.rails[].withdrawals`
    ///      matched to `status.lc.claim_leaves[0..9]`. Their leaves' FPL root is that header's
    ///      claimRoot 0x9671942a...; note 9 (leaf index 9) is not among them.
    function _realAnchorEntries()
        internal
        pure
        returns (bytes32[] memory n, address[] memory d, uint64[] memory a, bool[] memory u)
    {
        n = new bytes32[](9);
        d = new address[](9);
        a = new uint64[](9);
        u = new bool[](9);
        n[0] = 0x2c6600f856d7932fc602d47b52d82664e4692aaa39fc2359074b53480ca0e6cc;
        d[0] = 0xa978781b96aFDFe5653BEFbE935F3C235795F5AE;
        a[0] = 5000000000000000;
        u[0] = false;
        n[1] = 0xb3d78ce7d70b073b164cb7c8aa139cd224d4001bb07c0ee1aa5b7f24522a98b0;
        d[1] = 0xa978781b96aFDFe5653BEFbE935F3C235795F5AE;
        a[1] = 1000000000000000;
        u[1] = false;
        n[2] = 0x05760deea01033660a80f38f70a4f361398dc1b4f72474ad9aa4e1fcd591089f;
        d[2] = 0xDb52847EE70cEd3128f49309c3DC65b69d7466f4;
        a[2] = 1000000000000000;
        u[2] = false;
        n[3] = 0xfb13ddc0bd773488cddbe9e067047a299a75c7a8e8e92240cc39383c9f4c2dc9;
        d[3] = 0xDb52847EE70cEd3128f49309c3DC65b69d7466f4;
        a[3] = 1000000000000000;
        u[3] = false;
        n[4] = 0xbb8a1517e7945d7a8f68191f2b1700921253674afe240019176d150e550e2ee4;
        d[4] = 0x2E0630BB9339AC427feB2aaae31DFEf90fD1181e;
        a[4] = 1000000000000000;
        u[4] = false;
        n[5] = 0x8ff47f4a84b73ab15c3f13cbc253cb2037454e58b7560897de7e16b9ee6d62ba;
        d[5] = 0xa978781b96aFDFe5653BEFbE935F3C235795F5AE;
        a[5] = 2000000;
        u[5] = true;
        n[6] = 0x9c8717994691f133fad183379b88083d3a9fbafc8f0859d41f9885104260b647;
        d[6] = 0xDb52847EE70cEd3128f49309c3DC65b69d7466f4;
        a[6] = 2000000;
        u[6] = true;
        n[7] = 0xc3d31eff0c5c610b5e7caeaae20d7636b62e272a08be2e2a68f0874534860ddd;
        d[7] = 0xDb52847EE70cEd3128f49309c3DC65b69d7466f4;
        a[7] = 2000000;
        u[7] = true;
        n[8] = 0x90d58e3c4d18cbff1e236c5281932b39316df424f048f64b810808761db4a605;
        d[8] = 0xDb52847EE70cEd3128f49309c3DC65b69d7466f4;
        a[8] = 2000000;
        u[8] = true;
    }

    function _realAnchorSet() internal returns (address) {
        (bytes32[] memory n, address[] memory d, uint64[] memory a, bool[] memory u) = _realAnchorEntries();
        return _setOf(n, d, a, u);
    }

    function _ck() internal returns (FalconQcBridgeV3Checks) {
        if (address(ck) == address(0)) {
            ck = new FalconQcBridgeV3Checks();
        }
        return ck;
    }

    function _seal(bytes32 innerHash, bytes32 nextRoot, bytes32 claimRoot) internal pure returns (bytes32) {
        return sha256(abi.encodePacked(bytes("pl-ledger:v4-seal"), innerHash, nextRoot, claimRoot));
    }

    function _sealed(uint64 height, bytes32 nextRoot, bytes32 claimRoot)
        internal
        pure
        returns (bytes32 innerHash, bytes32 hash)
    {
        innerHash = keccak256(abi.encodePacked("inner", height));
        hash = _seal(innerHash, nextRoot, claimRoot);
    }

    /// @dev External so `vm.expectRevert` sees this call, not the inner `sha256` precompile.
    function submitFor(
        FalconQcBridgeV3 target,
        uint64 height,
        bytes32 parent,
        bytes32 claimRoot,
        bytes32 nextRoot,
        bytes32[6] memory nl,
        uint64[4] memory idx,
        uint256[2][4] memory a
    ) external {
        _submit(target, height, parent, claimRoot, nextRoot, nl, idx, a);
    }

    function _submit(
        FalconQcBridgeV3 target,
        uint64 height,
        bytes32 parent,
        bytes32 claimRoot,
        bytes32 nextRoot,
        bytes32[6] memory nl,
        uint64[4] memory idx,
        uint256[2][4] memory a
    ) internal {
        (bytes32 innerHash, bytes32 hash) = _sealed(height, nextRoot, claimRoot);
        uint256[2][2][4] memory b;
        uint256[2][4] memory c;
        target.submitHeaderWithProofs(height, hash, parent, innerHash, claimRoot, nextRoot, nl, idx, a, b, c);
    }

    function _idx() internal pure returns (uint64[4] memory idx) {
        idx = [uint64(1), 2, 4, 5];
    }

    function _nextLeaves() internal pure returns (bytes32[6] memory nl) {
        for (uint256 i = 0; i < 6; i++) {
            nl[i] = keccak256(abi.encodePacked("next", i));
        }
    }

    function _zeroA() internal pure returns (uint256[2][4] memory a) {}

    function _boundA(uint256 rootWord, uint64[4] memory idx) internal pure returns (uint256[2][4] memory a) {
        for (uint256 i = 0; i < 4; i++) {
            a[i][0] = rootWord;
            a[i][1] = idx[i];
        }
    }

    function _resolved(bytes32 id) internal view returns (bool resolved) {
        (, , , , , , resolved) = br.pegIns(id);
    }

    /// @dev Fresh V3 whose V2 mock reports `usdcToken` (constructor requires `usdc == v2.usdc()`).
    ///      Does not replace setUp's `br` or `MockUSDC`.
    function _deployWithToken(address usdcToken) internal returns (FalconQcBridgeV3) {
        MockQcV2 v2local = new MockQcV2(
            usdcToken,
            address(ver),
            TIP,
            anchorHash,
            anchorParent,
            anchorInner,
            anchorClaim,
            committee,
            leaves,
            true,
            true
        );
        return new FalconQcBridgeV3(
            usdcToken, address(ver), address(v2local), address(new MockQcV1()), address(new MockDestLock()), MAX_JUMP, address(_ck()), TIP, address(0)
        );
    }

    function _revertSelector(bytes memory data) internal pure returns (bytes4 sel) {
        require(data.length >= 4, "short");
        assembly {
            sel := mload(add(data, 32))
        }
    }

    struct V2Submit {
        uint64 anchorTip;
        bytes32 anchorHash;
        bytes32 anchorParent;
        bytes32 anchorInner;
        bytes32 anchorClaim;
        bytes32 anchorRoot;
        bytes32[6] anchorLeaves;
        uint64 height;
        bytes32 hash;
        bytes32 parent;
        bytes32 innerHash;
        bytes32 claimRoot;
        bytes32 nextRoot;
        bytes32[6] nextLeaves;
        uint64[4] idx;
    }

    function _acceptRealFixture(string memory path) internal {
        string memory json = vm.readFile(path);
        Groth16Verifier gv = _deployVk(json);
        V2Submit memory sub = _readSubmit(json);
        (uint256[2][4] memory a, uint256[2][2][4] memory b, uint256[2][4] memory c) = _readProofs(json, sub);
        FalconQcBridgeV3 bridge = _deployAnchored(address(gv), sub);
        _submitLoaded(bridge, sub, sub.nextLeaves, a, b, c);
        assertEq(bridge.fplTip(), sub.height);
        assertEq(bridge.currentCommitteeRoot(), sub.nextRoot);
    }

    function _rejectTamperedProof(string memory path) internal {
        string memory json = vm.readFile(path);
        Groth16Verifier gv = _deployVk(json);
        V2Submit memory sub = _readSubmit(json);
        (uint256[2][4] memory a, uint256[2][2][4] memory b, uint256[2][4] memory c) = _readProofs(json, sub);
        uint256[2][4] memory badA = _copyA(a);
        // A.x + 1 is off-curve: the pairing precompile reverts "pair" and the bridge
        // never reaches require(..., "proof"). Negate A.y so the point stays on the curve.
        uint256 y = badA[0][1] % BN254_P;
        if (y == 0) {
            badA[0][0] = 1;
            badA[0][1] = 2;
        } else {
            badA[0][1] = BN254_P - y;
        }
        assertTrue(badA[0][0] != a[0][0] || badA[0][1] != a[0][1]);
        FalconQcBridgeV3 bridge = _deployAnchored(address(gv), sub);
        vm.expectRevert(bytes("proof"));
        _submitLoaded(bridge, sub, sub.nextLeaves, badA, b, c);
    }

    function _rejectTamperedLeaves(string memory path) internal {
        string memory json = vm.readFile(path);
        Groth16Verifier gv = _deployVk(json);
        V2Submit memory sub = _readSubmit(json);
        (uint256[2][4] memory a, uint256[2][2][4] memory b, uint256[2][4] memory c) = _readProofs(json, sub);
        bytes32[6] memory badLeaves;
        for (uint256 i = 0; i < 6; i++) {
            badLeaves[i] = sub.nextLeaves[i];
        }
        badLeaves[0] = keccak256("mutated-leaf");
        FalconQcBridgeV3 bridge = _deployAnchored(address(gv), sub);
        vm.expectRevert(bytes("proof"));
        _submitLoaded(bridge, sub, badLeaves, a, b, c);
    }

    function _deployVk(string memory json) internal returns (Groth16Verifier) {
        _assertCanonicalVk(json);
        uint256[2] memory alpha = [json.readUint(".vk.alpha[0]"), json.readUint(".vk.alpha[1]")];
        uint256[2][2] memory beta = _readG2(json, ".vk.beta");
        uint256[2][2] memory gamma = _readG2(json, ".vk.gamma");
        uint256[2][2] memory delta = _readG2(json, ".vk.delta");
        uint256 nIc;
        while (json.keyExists(string.concat(".vk.ic[", vm.toString(nIc), "]"))) {
            nIc++;
        }
        uint256[2][] memory ic = new uint256[2][](nIc);
        for (uint256 i = 0; i < nIc; i++) {
            string memory base = string.concat(".vk.ic[", vm.toString(i), "]");
            ic[i][0] = json.readUint(string.concat(base, "[0]"));
            ic[i][1] = json.readUint(string.concat(base, "[1]"));
        }
        return new Groth16Verifier(alpha, beta, gamma, delta, ic);
    }

    function _readG2(string memory json, string memory key) internal pure returns (uint256[2][2] memory p) {
        p[0][0] = json.readUint(string.concat(key, "[0][0]"));
        p[0][1] = json.readUint(string.concat(key, "[0][1]"));
        p[1][0] = json.readUint(string.concat(key, "[1][0]"));
        p[1][1] = json.readUint(string.concat(key, "[1][1]"));
    }

    function _readSubmit(string memory json) internal pure returns (V2Submit memory sub) {
        uint256 tip = json.readUint(".anchor.tip");
        assertLe(tip, type(uint64).max);
        sub.anchorTip = uint64(tip);
        sub.anchorHash = json.readBytes32(".anchor.hash");
        sub.anchorParent = json.readBytes32(".anchor.parent");
        sub.anchorInner = json.readBytes32(".anchor.innerHash");
        sub.anchorClaim = json.readBytes32(".anchor.claimRoot");
        sub.anchorRoot = json.readBytes32(".anchor.committeeRoot");
        bytes32[] memory leaves = json.readBytes32Array(".anchor.leaves");
        assertEq(leaves.length, 6);
        for (uint256 i = 0; i < 6; i++) {
            sub.anchorLeaves[i] = leaves[i];
        }
        uint256 height = json.readUint(".height");
        assertLe(height, type(uint64).max);
        sub.height = uint64(height);
        sub.hash = json.readBytes32(".hash");
        sub.parent = json.readBytes32(".parent");
        sub.innerHash = json.readBytes32(".innerHash");
        sub.claimRoot = json.readBytes32(".claimRoot");
        sub.nextRoot = json.readBytes32(".nextCommitteeRoot");
        bytes32[] memory nextLeaves = json.readBytes32Array(".nextLeaves");
        assertEq(nextLeaves.length, 6);
        for (uint256 i = 0; i < 6; i++) {
            sub.nextLeaves[i] = nextLeaves[i];
        }
        uint256[] memory idx = json.readUintArray(".idx");
        assertEq(idx.length, 4);
        for (uint256 i = 0; i < 4; i++) {
            assertLe(idx[i], type(uint64).max);
            sub.idx[i] = uint64(idx[i]);
        }
    }

    /// @dev Same 16 words as FalconQcBridgeV3._qcInputs / ExpectQcInputs:
    ///      height, parent hi/lo, hash hi/lo, old committee root, next root, 1, network 2300,
    ///      signer index, six next leaves. ExpectQcInputs compares input[9] to a[1] only because
    ///      the mock proofs store the index there (`_boundA`); the contract passes `idx[i]`.
    function _qcInputWords(V2Submit memory sub, uint64 signerIndex)
        internal
        pure
        returns (uint256[16] memory input)
    {
        input[0] = sub.height;
        input[1] = uint256(sub.parent) >> 128;
        input[2] = uint256(sub.parent) & ((uint256(1) << 128) - 1);
        input[3] = uint256(sub.hash) >> 128;
        input[4] = uint256(sub.hash) & ((uint256(1) << 128) - 1);
        input[5] = uint256(sub.anchorRoot);
        input[6] = uint256(sub.nextRoot);
        input[7] = 1;
        input[8] = 2300;
        input[9] = signerIndex;
        for (uint256 j = 0; j < 6; j++) {
            input[10 + j] = uint256(sub.nextLeaves[j]);
        }
    }

    function _assertCanonicalVk(string memory json) internal view {
        uint256 nIc;
        while (json.keyExists(string.concat(".vk.ic[", vm.toString(nIc), "]"))) {
            nIc++;
        }
        // alpha 2 + beta 4 + gamma 4 + delta 4 + ic*2
        uint256[] memory words = new uint256[](14 + nIc * 2);
        uint256 n;
        words[n++] = json.readUint(".vk.alpha[0]");
        words[n++] = json.readUint(".vk.alpha[1]");
        words[n++] = json.readUint(".vk.beta[0][0]");
        words[n++] = json.readUint(".vk.beta[0][1]");
        words[n++] = json.readUint(".vk.beta[1][0]");
        words[n++] = json.readUint(".vk.beta[1][1]");
        words[n++] = json.readUint(".vk.gamma[0][0]");
        words[n++] = json.readUint(".vk.gamma[0][1]");
        words[n++] = json.readUint(".vk.gamma[1][0]");
        words[n++] = json.readUint(".vk.gamma[1][1]");
        words[n++] = json.readUint(".vk.delta[0][0]");
        words[n++] = json.readUint(".vk.delta[0][1]");
        words[n++] = json.readUint(".vk.delta[1][0]");
        words[n++] = json.readUint(".vk.delta[1][1]");
        for (uint256 i = 0; i < nIc; i++) {
            string memory base = string.concat(".vk.ic[", vm.toString(i), "]");
            words[n++] = json.readUint(string.concat(base, "[0]"));
            words[n++] = json.readUint(string.concat(base, "[1]"));
        }
        assertEq(n, words.length);
        assertEq(keccak256(abi.encodePacked(words)), CANONICAL_V2_VK_HASH);
    }

    function _readProofs(string memory json, V2Submit memory sub)
        internal
        pure
        returns (uint256[2][4] memory a, uint256[2][2][4] memory b, uint256[2][4] memory c)
    {
        for (uint256 i = 0; i < 4; i++) {
            string memory base = string.concat(".proofs[", vm.toString(i), "]");
            uint256[] memory input = json.readUintArray(string.concat(base, ".input"));
            assertEq(input.length, 16);
            uint256[16] memory expect = _qcInputWords(sub, sub.idx[i]);
            for (uint256 k = 0; k < 16; k++) {
                assertEq(input[k], expect[k]);
            }
            a[i][0] = json.readUint(string.concat(base, ".a[0]"));
            a[i][1] = json.readUint(string.concat(base, ".a[1]"));
            b[i][0][0] = json.readUint(string.concat(base, ".b[0][0]"));
            b[i][0][1] = json.readUint(string.concat(base, ".b[0][1]"));
            b[i][1][0] = json.readUint(string.concat(base, ".b[1][0]"));
            b[i][1][1] = json.readUint(string.concat(base, ".b[1][1]"));
            c[i][0] = json.readUint(string.concat(base, ".c[0]"));
            c[i][1] = json.readUint(string.concat(base, ".c[1]"));
        }
    }

    function _copyA(uint256[2][4] memory a) internal pure returns (uint256[2][4] memory bad) {
        for (uint256 i = 0; i < 4; i++) {
            bad[i][0] = a[i][0];
            bad[i][1] = a[i][1];
        }
    }

    /// @dev Same constructor arguments as `_deploy`: usdc, groth16, v2, v1, destLock, maxJump, checks,
    ///      anchorHeight, anchorSet. The real 353130 claim root commits 9 leaves (#33 fixture).
    ///      The mock V2 tip, header, committee root, and leaves are the fixture anchor.
    function _deployAnchored(address groth, V2Submit memory sub) internal returns (FalconQcBridgeV3) {
        MockQcV2 mock = new MockQcV2(
            address(usdc),
            groth,
            sub.anchorTip,
            sub.anchorHash,
            sub.anchorParent,
            sub.anchorInner,
            sub.anchorClaim,
            sub.anchorRoot,
            sub.anchorLeaves,
            true,
            true
        );
        return new FalconQcBridgeV3(
            address(usdc), groth, address(mock), address(new MockQcV1()), address(new MockDestLock()), MAX_JUMP, address(_ck()),
            sub.anchorTip, _realAnchorSet()
        );
    }

    function _submitLoaded(
        FalconQcBridgeV3 bridge,
        V2Submit memory sub,
        bytes32[6] memory nextLeaves,
        uint256[2][4] memory a,
        uint256[2][2][4] memory b,
        uint256[2][4] memory c
    ) internal {
        bridge.submitHeaderWithProofs(
            sub.height,
            sub.hash,
            sub.parent,
            sub.innerHash,
            sub.claimRoot,
            sub.nextRoot,
            nextLeaves,
            sub.idx,
            a,
            b,
            c
        );
    }

    event HeaderProved(uint64 indexed height, bytes32 hash, bytes32 claimRoot, bytes32 nextCommitteeRoot);
    event CommitteeRotated(uint64 indexed height, bytes32 newRoot);
}
