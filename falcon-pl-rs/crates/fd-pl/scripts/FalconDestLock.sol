// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

/// ETH/USDC dest-lock at the same bar as BTC BitVM2 Kickoff:
/// one claimer CHECKSIG analogue dest-locks a slice; dest takes after CSV.
/// No owner. No admin withdraw. No live n-of-n on unwrap.
/// Claimer is liveness (same role as the BTC instance Kickoff key), not take.

interface IERC20 {
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
    function transfer(address to, uint256 amount) external returns (bool);
}

contract FalconDestLock {
    address public immutable usdc;
    bytes32 public immutable ethLockId;
    bytes32 public immutable usdcLockId;
    address public immutable claimer;
    uint64 public immutable claimDelay;

    struct Claim {
        address payable dest;
        uint256 amount;
        bool usdc;
        uint64 readyBlock;
        bool open;
        bool taken;
    }

    mapping(bytes32 => Claim) public claims;
    uint256 public ethPool;
    uint256 public usdcPool;

    event Deposit(bytes32 indexed dest20, address indexed token, uint256 amount, bytes32 lockId);
    event ClaimOpened(bytes32 indexed noteId, address dest, uint256 amount, bool usdc, uint64 readyBlock);
    event ClaimTaken(bytes32 indexed noteId, address dest, uint256 amount);

    constructor(address usdcToken, bytes32 ethLock, bytes32 usdcLock, address claimer_, uint64 delay) {
        require(usdcToken != address(0) && claimer_ != address(0) && delay > 0, "ctor");
        usdc = usdcToken;
        ethLockId = ethLock;
        usdcLockId = usdcLock;
        claimer = claimer_;
        claimDelay = delay;
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

    function kickoff(bytes32 noteId, address payable dest, uint256 amount, bool isUsdc) external {
        require(msg.sender == claimer, "claimer");
        require(dest != address(0) && amount > 0, "args");
        require(claims[noteId].dest == address(0), "exists");
        if (isUsdc) {
            require(usdcPool >= amount, "pool");
            usdcPool -= amount;
        } else {
            require(ethPool >= amount, "pool");
            ethPool -= amount;
        }
        uint64 ready = uint64(block.number) + claimDelay;
        claims[noteId] = Claim(dest, amount, isUsdc, ready, true, false);
        emit ClaimOpened(noteId, dest, amount, isUsdc, ready);
    }

    function take(bytes32 noteId) external {
        Claim storage c = claims[noteId];
        require(c.open && !c.taken, "state");
        require(msg.sender == c.dest, "dest");
        require(block.number >= c.readyBlock, "csv");
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
}
