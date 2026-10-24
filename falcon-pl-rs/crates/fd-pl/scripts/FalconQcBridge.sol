// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

/// ETH/USDC dest-lock whose headers are Groth16 of **validator Falcon-512**.
/// Four OPEN=1 proofs with distinct public signer indices. No owner, no withdraw.
/// Not the BabyJub LC bridge. lcKeyRoot here is merkle of Falcon pks.

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

contract FalconQcBridge {
    address public immutable usdc;
    bytes32 public immutable ethLockId;
    bytes32 public immutable usdcLockId;
    bytes32 public immutable fplGenesisHash;
    IGroth16Verifier public immutable verifier;
    bytes32 public immutable falconKeyRoot;
    uint64 public immutable proofsNeeded;
    uint32 public immutable fplNetworkId;

    struct FplHeader {
        bytes32 hash;
        bytes32 parent;
        bytes32 claimRoot;
        bool finalized;
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
    mapping(bytes32 => Claim) public claims;
    uint256 public ethPool;
    uint256 public usdcPool;

    event Deposit(bytes32 indexed dest20, address indexed token, uint256 amount, bytes32 lockId);
    event HeaderProved(uint64 indexed height, bytes32 hash, bytes32 claimRoot);
    event HeaderFinalized(uint64 indexed height, bytes32 claimRoot);
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
        uint64 nProofs,
        uint32 networkId,
        uint64 startHeight
    ) {
        require(usdcToken != address(0) && genesisHash != bytes32(0), "ctor");
        require(groth16 != address(0) && keyRoot != bytes32(0) && nProofs >= 1 && networkId != 0, "qc");
        usdc = usdcToken;
        ethLockId = ethLock;
        usdcLockId = usdcLock;
        fplGenesisHash = genesisHash;
        verifier = IGroth16Verifier(groth16);
        falconKeyRoot = keyRoot;
        proofsNeeded = nProofs;
        fplNetworkId = networkId;
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

    /// Four Groth16 proofs of Falcon-512, distinct signer indices 0..7.
    function submitHeaderWithProofs(
        uint64 height,
        bytes32 hash,
        bytes32 parent,
        bytes32 claimRoot,
        uint64[4] calldata idx,
        uint256[2][4] calldata a,
        uint256[2][2][4] calldata b,
        uint256[2][4] calldata c
    ) external {
        require(hash != bytes32(0), "hash");
        require(!headers[height].finalized, "final");
        require(proofsNeeded == 4, "n");
        if (height == 1 && fplTip == 0) {
            require(parent == fplGenesisHash || parent == bytes32(0), "genesis");
        } else {
            require(fplTip > 0 && height > fplTip, "height");
            if (height == fplTip + 1) {
                require(parent == headers[fplTip].hash, "parent");
            }
        }
        uint256 seen;
        for (uint256 i = 0; i < 4; i++) {
            require(idx[i] < 8, "idx");
            uint256 bit = uint256(1) << idx[i];
            require(seen & bit == 0, "dup");
            seen |= bit;
            require(verifier.verifyProof(a[i], b[i], c[i], _qcInputs(height, hash, parent, claimRoot, idx[i])), "proof");
        }
        headers[height] = FplHeader(hash, parent, claimRoot, true);
        fplTip = height;
        emit HeaderProved(height, hash, claimRoot);
        emit HeaderFinalized(height, claimRoot);
    }

    function _qcInputs(
        uint64 height,
        bytes32 hash,
        bytes32 parent,
        bytes32 claimRoot,
        uint64 signerIndex
    ) internal view returns (uint256[] memory input) {
        input = new uint256[](11);
        input[0] = height;
        input[1] = uint256(parent) >> 128;
        input[2] = uint256(parent) & ((uint256(1) << 128) - 1);
        input[3] = uint256(hash) >> 128;
        input[4] = uint256(hash) & ((uint256(1) << 128) - 1);
        input[5] = uint256(claimRoot) >> 128;
        input[6] = uint256(claimRoot) & ((uint256(1) << 128) - 1);
        input[7] = uint256(falconKeyRoot);
        input[8] = 1; // one Falcon-512 per proof
        input[9] = fplNetworkId;
        input[10] = signerIndex;
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
        claims[noteId] = Claim(dest, amount, isUsdc, uint64(block.number), fplHeight, leaf, true, false, false);
        emit ClaimOpened(noteId, dest, amount, isUsdc, uint64(block.number));
    }

    function challengeClaim(bytes32 noteId) external {
        Claim storage c = claims[noteId];
        require(c.open && !c.challenged && !c.taken, "state");
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
