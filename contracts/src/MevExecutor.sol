// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "./interfaces/IERC20.sol";
import "./interfaces/IUniswapV2Pair.sol";
import "./interfaces/IUniswapV2Callee.sol";
import "./interfaces/IUniswapV3Pool.sol";
import "./interfaces/IUniswapV3SwapCallback.sol";
import "./IWETH.sol";

/// @title MevExecutor
/// @notice Low-latency atomic on-chain executor for Ethereum MEV backrun arbitrage.
/// @dev Supports self-funded execution and zero-capital Uniswap V2/V3 flash swaps with direct block.coinbase builder bribes.
contract MevExecutor is IUniswapV2Callee, IUniswapV3SwapCallback {
    enum Protocol {
        UniswapV2,
        UniswapV3
    }

    struct Hop {
        Protocol protocol;
        address pool;
        address tokenIn;
        address tokenOut;
        uint24 fee; // Required for Uniswap V3 (e.g. 500, 3000, 10000)
        uint160 sqrtPriceLimitX96; // For Uniswap V3 (0 defaults to no price limit)
    }

    struct ExecutionParams {
        uint256 amountIn;
        uint256 minProfit;
        uint16 bribeBps; // In basis points (e.g. 9000 = 90% to block.coinbase)
        Hop[] hops;
    }

    address public immutable owner;
    address public immutable WETH;

    /// @dev Transiently set to the active pool during external callbacks to prevent spoofing.
    address private expectedPool;

    event ArbitrageExecuted(
        uint256 indexed amountIn,
        uint256 indexed netProfit,
        uint256 indexed bribeAmount
    );

    modifier onlyOwner() {
        require(msg.sender == owner, "NotOwner");
        _;
    }

    constructor(address _weth) {
        require(_weth != address(0), "InvalidWETHAddress");
        owner = msg.sender;
        WETH = _weth;
    }

    /// @notice Mode A: Direct self-funded multi-hop arbitrage using contract WETH balance.
    /// @param params Complete execution parameters including hops and invariants.
    function executeArbitrageDirect(ExecutionParams calldata params) external onlyOwner {
        _validateParams(params);

        uint256 initialWeth = IERC20(WETH).balanceOf(address(this));
        require(initialWeth >= params.amountIn, "InsufficientWethBalance");

        uint256 hopsLength = params.hops.length;
        for (uint256 i = 0; i < hopsLength; i++) {
            Hop calldata hop = params.hops[i];
            uint256 currentIn = (i == 0)
                ? params.amountIn
                : IERC20(hop.tokenIn).balanceOf(address(this));

            _executeHop(hop, currentIn);
        }

        uint256 finalWeth = IERC20(WETH).balanceOf(address(this));
        require(finalWeth > initialWeth, "NoProfit");

        uint256 profit = finalWeth - initialWeth;
        require(profit >= params.minProfit, "InsufficientProfit");

        uint256 bribeAmount = (params.bribeBps > 0) ? (profit * uint256(params.bribeBps)) / 10_000 : 0;
        emit ArbitrageExecuted(params.amountIn, profit - bribeAmount, bribeAmount);

        _payBribe(bribeAmount);
    }

    /// @notice Mode B: Zero-capital multi-hop arbitrage initiated via Uniswap V2 optimistic flash swap.
    /// @param params Complete execution parameters including hops and invariants.
    function executeArbitrageFlash(ExecutionParams calldata params) external onlyOwner {
        _validateParams(params);
        require(params.hops[0].protocol == Protocol.UniswapV2, "Hop0MustBeUniswapV2");

        Hop calldata hop0 = params.hops[0];
        (uint112 r0, uint112 r1, ) = IUniswapV2Pair(hop0.pool).getReserves();
        (uint256 rIn, uint256 rOut) = (hop0.tokenIn == IUniswapV2Pair(hop0.pool).token0())
            ? (uint256(r0), uint256(r1))
            : (uint256(r1), uint256(r0));

        uint256 amountInWithFee = params.amountIn * 997;
        uint256 amountOut = (amountInWithFee * rOut) / ((rIn * 1000) + amountInWithFee);

        (uint256 a0, uint256 a1) = (hop0.tokenIn == IUniswapV2Pair(hop0.pool).token0())
            ? (uint256(0), amountOut)
            : (amountOut, uint256(0));

        expectedPool = hop0.pool;
        IUniswapV2Pair(hop0.pool).swap(a0, a1, address(this), abi.encode(params));
        expectedPool = address(0);
    }

    /// @notice Uniswap V2 flash swap callback.
    /// @dev Called by the Uniswap V2 pair during executeArbitrageFlash.
    function uniswapV2Call(
        address sender,
        uint256,
        uint256,
        bytes calldata data
    ) external override {
        require(msg.sender == expectedPool, "UnauthorizedV2Callback");
        require(sender == address(this), "UnauthorizedSender");

        ExecutionParams memory params = abi.decode(data, (ExecutionParams));

        // Execute remaining hops (1..N-1)
        uint256 hopsLength = params.hops.length;
        for (uint256 i = 1; i < hopsLength; i++) {
            Hop memory hop = params.hops[i];
            uint256 currentIn = IERC20(hop.tokenIn).balanceOf(address(this));
            _executeHopMemory(hop, currentIn);
        }

        // Repay borrowed WETH to Hop 0 pair
        _safeTransfer(WETH, msg.sender, params.amountIn);

        // Verify remaining profit invariant
        uint256 profit = IERC20(WETH).balanceOf(address(this));
        require(profit >= params.minProfit, "InsufficientProfit");

        uint256 bribeAmount = (params.bribeBps > 0) ? (profit * uint256(params.bribeBps)) / 10_000 : 0;
        emit ArbitrageExecuted(params.amountIn, profit - bribeAmount, bribeAmount);

        _payBribe(bribeAmount);
    }

    /// @notice Uniswap V3 swap callback.
    /// @dev Delivers the input token owed to the V3 pool during a V3 swap.
    function uniswapV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata data
    ) external override {
        require(msg.sender == expectedPool, "UnauthorizedV3Callback");

        address tokenToPay = abi.decode(data, (address));
        if (amount0Delta > 0) {
            _safeTransfer(tokenToPay, msg.sender, uint256(amount0Delta));
        } else if (amount1Delta > 0) {
            _safeTransfer(tokenToPay, msg.sender, uint256(amount1Delta));
        }
    }

    /// @dev Executes a single swap hop (calldata version).
    function _executeHop(Hop calldata hop, uint256 amountIn) internal {
        if (hop.protocol == Protocol.UniswapV2) {
            _executeV2(hop.pool, hop.tokenIn, amountIn);
        } else {
            _executeV3(hop.pool, hop.tokenIn, hop.tokenOut, amountIn, hop.sqrtPriceLimitX96);
        }
    }

    /// @dev Executes a single swap hop (memory version).
    function _executeHopMemory(Hop memory hop, uint256 amountIn) internal {
        if (hop.protocol == Protocol.UniswapV2) {
            _executeV2(hop.pool, hop.tokenIn, amountIn);
        } else {
            _executeV3(hop.pool, hop.tokenIn, hop.tokenOut, amountIn, hop.sqrtPriceLimitX96);
        }
    }

    /// @dev Internal Uniswap V2 hop execution.
    function _executeV2(address pool, address tokenIn, uint256 amountIn) internal {
        _safeTransfer(tokenIn, pool, amountIn);

        (uint112 r0, uint112 r1, ) = IUniswapV2Pair(pool).getReserves();
        (uint256 rIn, uint256 rOut) = (tokenIn == IUniswapV2Pair(pool).token0())
            ? (uint256(r0), uint256(r1))
            : (uint256(r1), uint256(r0));

        uint256 amountInWithFee = amountIn * 997;
        uint256 amountOut = (amountInWithFee * rOut) / ((rIn * 1000) + amountInWithFee);

        (uint256 a0, uint256 a1) = (tokenIn == IUniswapV2Pair(pool).token0())
            ? (uint256(0), amountOut)
            : (amountOut, uint256(0));

        IUniswapV2Pair(pool).swap(a0, a1, address(this), "");
    }

    /// @dev Internal Uniswap V3 hop execution.
    function _executeV3(
        address pool,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint160 customLimit
    ) internal {
        require(amountIn <= uint256(type(int256).max), "AmountInOverflow");
        bool zeroForOne = tokenIn < tokenOut;
        uint160 sqrtPriceLimit = (customLimit == 0)
            ? (zeroForOne ? 4295128740 : 1461446703485210103287273052203988822378723970341)
            : customLimit;

        address prevExpected = expectedPool;
        expectedPool = pool;
        IUniswapV3Pool(pool).swap(
            address(this),
            zeroForOne,
            int256(amountIn),
            sqrtPriceLimit,
            abi.encode(tokenIn)
        );
        expectedPool = prevExpected;
    }

    /// @dev Unwraps WETH and transfers builder bribe directly to block.coinbase in native ETH.
    function _payBribe(uint256 bribeAmount) internal {
        if (bribeAmount > 0) {
            IWETH(WETH).withdraw(bribeAmount);
            (bool ok, ) = block.coinbase.call{value: bribeAmount}("");
            require(ok, "BribeFailed");
        }
    }

    /// @dev Basic structural validation on execution parameters (memory version).
    function _validateParamsMemory(ExecutionParams memory params) internal view {
        require(params.hops.length >= 2, "HopsTooShort");
        require(params.hops[0].tokenIn == WETH, "StartMustBeWETH");
        require(params.hops[params.hops.length - 1].tokenOut == WETH, "EndMustBeWETH");
        require(params.bribeBps <= 10_000, "InvalidBribeBps");
        require(params.amountIn > 0, "ZeroAmountIn");
    }

    /// @dev Memory-based direct arbitrage execution.
    function _executeArbitrageDirectMemory(ExecutionParams memory params) internal {
        _validateParamsMemory(params);

        uint256 initialWeth = IERC20(WETH).balanceOf(address(this));
        require(initialWeth >= params.amountIn, "InsufficientWethBalance");

        uint256 hopsLength = params.hops.length;
        for (uint256 i = 0; i < hopsLength; i++) {
            Hop memory hop = params.hops[i];
            uint256 currentIn = (i == 0)
                ? params.amountIn
                : IERC20(hop.tokenIn).balanceOf(address(this));

            _executeHopMemory(hop, currentIn);
        }

        uint256 finalWeth = IERC20(WETH).balanceOf(address(this));
        require(finalWeth > initialWeth, "NoProfit");

        uint256 profit = finalWeth - initialWeth;
        require(profit >= params.minProfit, "InsufficientProfit");

        uint256 bribeAmount = (params.bribeBps > 0) ? (profit * uint256(params.bribeBps)) / 10_000 : 0;
        emit ArbitrageExecuted(params.amountIn, profit - bribeAmount, bribeAmount);

        _payBribe(bribeAmount);
    }

    /// @dev Memory-based flash swap arbitrage execution.
    function _executeArbitrageFlashMemory(ExecutionParams memory params) internal {
        _validateParamsMemory(params);
        require(params.hops[0].protocol == Protocol.UniswapV2, "Hop0MustBeUniswapV2");

        Hop memory hop0 = params.hops[0];
        (uint112 r0, uint112 r1, ) = IUniswapV2Pair(hop0.pool).getReserves();
        (uint256 rIn, uint256 rOut) = (hop0.tokenIn == IUniswapV2Pair(hop0.pool).token0())
            ? (uint256(r0), uint256(r1))
            : (uint256(r1), uint256(r0));

        uint256 amountInWithFee = params.amountIn * 997;
        uint256 amountOut = (amountInWithFee * rOut) / ((rIn * 1000) + amountInWithFee);

        (uint256 a0, uint256 a1) = (hop0.tokenIn == IUniswapV2Pair(hop0.pool).token0())
            ? (uint256(0), amountOut)
            : (amountOut, uint256(0));

        expectedPool = hop0.pool;
        IUniswapV2Pair(hop0.pool).swap(a0, a1, address(this), abi.encode(params));
        expectedPool = address(0);
    }

    /// @notice Mode C: Ultra-low latency compact bit-packed calldata entrypoint via fallback.
    /// @dev Decodes header [mode(1B) | bribeBps(2B) | amountIn(16B) | minProfit(16B) | hopCount(1B)]
    ///      and each 65B hop [pool(20B) | tokenIn(20B) | tokenOut(20B) | fee(3B) | protocol(1B) | zeroForOne(1B)]
    ///      using raw Yul assembly to eliminate ABI decoding overhead.
    fallback() external payable {
        require(msg.sender == owner, "NotOwner");
        require(msg.data.length >= 36, "InvalidCalldataLength");

        uint8 mode;
        uint16 bribeBps;
        uint256 amountIn;
        uint256 minProfit;
        uint8 hopCount;

        assembly {
            let word0 := calldataload(0)
            mode := shr(248, word0)
            bribeBps := and(shr(232, word0), 0xffff)
            amountIn := and(shr(104, word0), 0xffffffffffffffffffffffffffffffff)

            let wordProfit := calldataload(19)
            minProfit := shr(128, wordProfit)

            let wordHop := calldataload(35)
            hopCount := shr(248, wordHop)
        }

        require(msg.data.length == 36 + uint256(hopCount) * 65, "InvalidHopDataLength");
        require(hopCount >= 2, "HopsTooShort");

        Hop[] memory hops = new Hop[](hopCount);
        for (uint256 i = 0; i < hopCount; i++) {
            uint256 offset = 36 + i * 65;
            address pool;
            address tokenIn;
            address tokenOut;
            uint24 fee;
            uint8 protocol;

            assembly {
                let w0 := calldataload(offset)
                pool := shr(96, w0)

                let w1 := calldataload(add(offset, 20))
                tokenIn := shr(96, w1)

                let w2 := calldataload(add(offset, 40))
                tokenOut := shr(96, w2)

                let w3 := calldataload(add(offset, 60))
                fee := shr(232, w3)
                protocol := and(shr(224, w3), 0xff)
            }

            require(protocol <= 1, "InvalidProtocol");

            hops[i] = Hop({
                protocol: Protocol(protocol),
                pool: pool,
                tokenIn: tokenIn,
                tokenOut: tokenOut,
                fee: fee,
                sqrtPriceLimitX96: 0
            });
        }

        ExecutionParams memory params = ExecutionParams({
            amountIn: amountIn,
            minProfit: minProfit,
            bribeBps: bribeBps,
            hops: hops
        });

        if (mode == 0) {
            _executeArbitrageDirectMemory(params);
        } else {
            _executeArbitrageFlashMemory(params);
        }
    }

    /// @dev Basic structural validation on execution parameters.
    function _validateParams(ExecutionParams calldata params) internal view {
        require(params.hops.length >= 2, "HopsTooShort");
        require(params.hops[0].tokenIn == WETH, "StartMustBeWETH");
        require(params.hops[params.hops.length - 1].tokenOut == WETH, "EndMustBeWETH");
        require(params.bribeBps <= 10_000, "InvalidBribeBps");
        require(params.amountIn > 0, "ZeroAmountIn");
    }

    /// @dev ERC-20 safe transfer helper supporting non-standard (missing return value) implementations.
    function _safeTransfer(address token, address to, uint256 amount) internal {
        (bool success, bytes memory data) = token.call(
            abi.encodeWithSelector(IERC20.transfer.selector, to, amount)
        );
        require(success && (data.length == 0 || abi.decode(data, (bool))), "TransferFailed");
    }

    /// @notice Emergency token rescue function.
    function withdrawToken(address token, uint256 amount) external onlyOwner {
        require(token != address(0), "ZeroAddress");
        _safeTransfer(token, owner, amount);
    }

    /// @notice Emergency native ETH rescue function.
    function withdrawETH(uint256 amount) external onlyOwner {
        (bool ok, ) = owner.call{value: amount}("");
        require(ok, "EthTransferFailed");
    }

    /// @dev Required to receive native ETH when unwrapping WETH for block.coinbase bribes.
    receive() external payable {}
}
