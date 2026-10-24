// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

/// Falcon PL ETH + USDC lock (Sepolia / Ethereum).
/// Deposits emit a dest-bound event. Relayers build evm-dep-v1 merkle leaves
/// from these logs. Withdrawals consume an FPL claim note (noteId) so a
/// second spend of the same note reverts.
///
/// Not a light client. The FPL rail verifies the deposit leaf against a
/// submitted header; this contract holds the coins.
interface IERC20 {
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
    function transfer(address to, uint256 amount) external returns (bool);
}

contract FalconLock {
    address public immutable usdc;
    bytes32 public immutable ethLockId;
    bytes32 public immutable usdcLockId;

    mapping(bytes32 => bool) public claimed;

    event Deposit(
        bytes32 indexed dest20,
        address indexed token,
        uint256 amount,
        bytes32 lockId,
        uint32 logIndexHint
    );
    event Withdraw(bytes32 indexed noteId, address indexed token, address to, uint256 amount);

    constructor(address usdcToken, bytes32 ethLock, bytes32 usdcLock) {
        usdc = usdcToken;
        ethLockId = ethLock;
        usdcLockId = usdcLock;
    }

    function depositEth(bytes20 dest20) external payable {
        require(msg.value > 0, "zero");
        emit Deposit(bytes32(uint256(uint160(dest20))), address(0), msg.value, ethLockId, 0);
    }

    function depositUsdc(bytes20 dest20, uint256 amount) external {
        require(amount > 0, "zero");
        require(IERC20(usdc).transferFrom(msg.sender, address(this), amount), "xfer");
        emit Deposit(bytes32(uint256(uint160(dest20))), usdc, amount, usdcLockId, 0);
    }

    /// Retired open drain. Do not deploy. Live exits use FalconQcBridge.take.
    function withdraw(bytes32, address, address payable, uint256) external pure {
        revert("retired");
    }
}
