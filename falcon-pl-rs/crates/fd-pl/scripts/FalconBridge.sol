// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

/// Production ETH/USDC lock: no owner, no open withdraw.
/// Peg-in: dest-bound Deposit events (Falcon mints from logs).
/// Peg-out: merkle of FPL burn vs a Groth16-finalized FPL header; dest-locked take.
/// SNARK path: `submitHeaderWithProof` finalizes immediately (no header window).
/// Delay+veto remains only when `verifier == address(0)` (tests / fallback).
interface IERC20 {
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

contract FalconBridge {
    address public immutable usdc;
    bytes32 public immutable ethLockId;
    bytes32 public immutable usdcLockId;
    bytes32 public immutable fplGenesisHash;
    IGroth16Verifier public immutable verifier;
    bytes32 public immutable lcKeyRoot;
    uint64 public immutable lcThreshold;
    uint32 public immutable fplNetworkId;
    uint64 public immutable headerDelay;
    uint64 public immutable claimDelay;

    struct FplHeader {
        bytes32 hash;
        bytes32 parent;
        bytes32 claimRoot;
        bool finalized;
    }
    struct PendingHeader {
        bytes32 hash;
        bytes32 parent;
        bytes32 claimRoot;
        uint64 readyBlock;
        bool vetoed;
        bool exists;
    }
    struct Claim {
        address payable dest;
        uint256 amount;
        bool usdc;
        uint64 readyBlock;
        uint64 fplHeight;
        bytes32 leaf;
        bool open;
        bool challenged;
        bool taken;
    }

    uint64 public fplTip;
    mapping(uint64 => FplHeader) public headers;
    mapping(uint64 => PendingHeader) public pending;
    mapping(bytes32 => Claim) public claims;
    uint256 public ethPool;
    uint256 public usdcPool;

    event Deposit(bytes32 indexed dest20, address indexed token, uint256 amount, bytes32 lockId);
    event HeaderSubmitted(uint64 indexed height, bytes32 hash, bytes32 claimRoot, uint64 readyBlock);
    event HeaderVetoed(uint64 indexed height);
    event HeaderFinalized(uint64 indexed height, bytes32 claimRoot);
    event HeaderProved(uint64 indexed height, bytes32 hash, bytes32 claimRoot);
    event ClaimOpened(bytes32 indexed noteId, address dest, uint256 amount, bool usdc, uint64 readyBlock);
    event ClaimChallenged(bytes32 indexed noteId);
    event ClaimTaken(bytes32 indexed noteId, address dest, uint256 amount);

    constructor(
        address usdcToken,
        bytes32 ethLock,
        bytes32 usdcLock,
        bytes32 genesisHash,
        address groth16,
        bytes32 keyRoot,
        uint64 thresh,
        uint32 networkId,
        uint64 hdrDelay,
        uint64 clDelay,
        uint64 startHeight
    ) {
        require(usdcToken != address(0) && genesisHash != bytes32(0), "ctor");
        if (groth16 == address(0)) {
            require(hdrDelay > 0 && clDelay > 0, "delays");
        } else {
            require(keyRoot != bytes32(0) && thresh >= 1 && networkId != 0, "lc");
        }
        usdc = usdcToken;
        ethLockId = ethLock;
        usdcLockId = usdcLock;
        fplGenesisHash = genesisHash;
        verifier = IGroth16Verifier(groth16);
        lcKeyRoot = keyRoot;
        lcThreshold = thresh;
        fplNetworkId = networkId;
        headerDelay = hdrDelay;
        claimDelay = clDelay;
        if (startHeight > 0) {
            fplTip = startHeight;
            headers[startHeight] = FplHeader(genesisHash, bytes32(0), bytes32(0), true);
        }
    }

    function depositEth(bytes20 dest20) external payable {
        require(msg.value > 0, "zero");
        ethPool += msg.value;
        emit Deposit(bytes32(uint256(uint160(dest20))), address(0), msg.value, ethLockId);
    }

    function depositUsdc(bytes20 dest20, uint256 amount) external {
        require(amount > 0, "zero");
        require(IERC20(usdc).transferFrom(msg.sender, address(this), amount), "xfer");
        usdcPool += amount;
        emit Deposit(bytes32(uint256(uint160(dest20))), usdc, amount, usdcLockId);
    }

    function submitFplHeader(uint64 height, bytes32 hash, bytes32 parent, bytes32 claimRoot) external {
        require(address(verifier) == address(0), "proof-only");
        require(hash != bytes32(0), "hash");
        require(!pending[height].exists, "pending");
        require(!headers[height].finalized, "final");
        if (height == 1) {
            require(fplTip == 0, "started");
            require(parent == fplGenesisHash || parent == bytes32(0), "genesis");
        } else {
            require(fplTip > 0 && height == fplTip + 1, "height");
            require(parent == headers[fplTip].hash, "parent");
        }
        uint64 ready = uint64(block.number) + headerDelay;
        pending[height] = PendingHeader(hash, parent, claimRoot, ready, false, true);
        emit HeaderSubmitted(height, hash, claimRoot, ready);
    }

    /// 1-of-n: any watcher who sees a fake FPL header vetoes before it finalizes.
    function vetoHeader(uint64 height) external {
        PendingHeader storage p = pending[height];
        require(p.exists && !p.vetoed, "none");
        require(block.number < p.readyBlock, "late");
        p.vetoed = true;
        emit HeaderVetoed(height);
    }

    function finalizeHeader(uint64 height) external {
        PendingHeader storage p = pending[height];
        require(p.exists && !p.vetoed, "bad");
        require(block.number >= p.readyBlock, "delay");
        if (height == 1) {
            require(fplTip == 0, "tip");
        } else {
            require(height == fplTip + 1 && p.parent == headers[fplTip].hash, "link");
        }
        headers[height] = FplHeader(p.hash, p.parent, p.claimRoot, true);
        fplTip = height;
        delete pending[height];
        emit HeaderFinalized(height, p.claimRoot);
    }

    /// Groth16 of k-of-n LC Schnorr. Header is finalized in this tx (no delay).
    function submitHeaderWithProof(
        uint64 height,
        bytes32 hash,
        bytes32 parent,
        bytes32 claimRoot,
        uint256[2] calldata a,
        uint256[2][2] calldata b,
        uint256[2] calldata c
    ) external {
        require(address(verifier) != address(0), "no-snark");
        require(hash != bytes32(0), "hash");
        require(!headers[height].finalized, "final");
        if (height == 1 && fplTip == 0) {
            require(parent == fplGenesisHash || parent == bytes32(0), "genesis");
        } else {
            require(fplTip > 0 && height > fplTip, "height");
            if (height == fplTip + 1) {
                require(parent == headers[fplTip].hash, "parent");
            }
        }
        require(verifier.verifyProof(a, b, c, _lcInputs(height, hash, parent, claimRoot)), "proof");
        headers[height] = FplHeader(hash, parent, claimRoot, true);
        fplTip = height;
        emit HeaderProved(height, hash, claimRoot);
        emit HeaderFinalized(height, claimRoot);
    }

    function _lcInputs(
        uint64 height,
        bytes32 hash,
        bytes32 parent,
        bytes32 claimRoot
    ) internal view returns (uint256[] memory input) {
        input = new uint256[](10);
        input[0] = height;
        input[1] = uint256(parent) >> 128;
        input[2] = uint256(parent) & ((uint256(1) << 128) - 1);
        input[3] = uint256(hash) >> 128;
        input[4] = uint256(hash) & ((uint256(1) << 128) - 1);
        input[5] = uint256(claimRoot) >> 128;
        input[6] = uint256(claimRoot) & ((uint256(1) << 128) - 1);
        input[7] = uint256(lcKeyRoot);
        input[8] = lcThreshold;
        input[9] = fplNetworkId;
    }

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

    /// Kickoff: merkle of FPL burn must verify on-chain against a finalized header.
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
        FplHeader storage hdr = headers[fplHeight];
        require(hdr.finalized, "header");
        bytes32 leaf = withdrawLeaf(noteId, dest, amount, isUsdc);
        require(_merkleSha256(leaf, proof, index) == hdr.claimRoot, "merkle");
        if (isUsdc) {
            require(usdcPool >= amount, "pool");
            usdcPool -= amount;
        } else {
            require(ethPool >= amount, "pool");
            ethPool -= amount;
        }
        uint64 ready = uint64(block.number) + claimDelay;
        claims[noteId] = Claim(dest, amount, isUsdc, ready, fplHeight, leaf, true, false, false);
        emit ClaimOpened(noteId, dest, amount, isUsdc, ready);
    }

    /// Permissionless abort during dest-lock window if the reserved leaf
    /// does not match dest/amount (should not happen after merkle check;
    /// kept so a watcher can bounce a buggy/malicious claim).
    function challengeClaim(bytes32 noteId) external {
        Claim storage c = claims[noteId];
        require(c.open && !c.challenged && !c.taken, "state");
        require(block.number < c.readyBlock, "late");
        bytes32 expect = withdrawLeaf(noteId, c.dest, c.amount, c.usdc);
        require(expect != c.leaf, "honest");
        c.challenged = true;
        c.open = false;
        if (c.usdc) usdcPool += c.amount;
        else ethPool += c.amount;
        emit ClaimChallenged(noteId);
    }

    function take(bytes32 noteId) external {
        Claim storage c = claims[noteId];
        require(c.open && !c.challenged && !c.taken, "state");
        require(block.number >= c.readyBlock, "csv");
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

    function _merkleSha256(bytes32 leaf, bytes32[] memory proof, uint32 index) internal pure returns (bytes32) {
        bytes32 h = leaf;
        uint32 idx = index;
        for (uint256 i = 0; i < proof.length; i++) {
            bytes32 p = proof[i];
            if (idx % 2 == 0) {
                h = sha256(abi.encodePacked(h, p));
            } else {
                h = sha256(abi.encodePacked(p, h));
            }
            idx /= 2;
        }
        return h;
    }
}
