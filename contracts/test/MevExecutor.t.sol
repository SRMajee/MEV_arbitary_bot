// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "forge-std/Test.sol";
import "../src/MevExecutor.sol";
import "../src/IWETH.sol";
import "../src/interfaces/IERC20.sol";
import "../src/interfaces/IUniswapV2Pair.sol";

contract MevExecutorTest is Test {
    address constant WETH_ADDR = 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2;
    address constant USDC_ADDR = 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48;

    address constant UNISWAP_V2_WETH_USDC = 0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc;
    address constant SUSHISWAP_V2_WETH_USDC = 0x397FF1542f962076d0BFE58eA045FfA2d347ACa0;

    uint256 constant PINNED_BLOCK = 19_500_000;

    MevExecutor public executor;
    IWETH public weth;
    IERC20 public usdc;

    address public owner;
    address public attacker;
    address public builder;

    function setUp() public {
        string memory rpcUrl = vm.envOr("MAINNET_RPC_URL", string("https://eth-mainnet.public.blastapi.io"));
        vm.createSelectFork(rpcUrl, PINNED_BLOCK);

        owner = address(this);
        attacker = makeAddr("attacker");
        builder = makeAddr("builder");

        vm.coinbase(builder);

        weth = IWETH(WETH_ADDR);
        usdc = IERC20(USDC_ADDR);

        executor = new MevExecutor(WETH_ADDR);
    }

    /// @notice Test non-owner access control rejection.
    function test_Security_NonOwnerCannotExecute() public {
        MevExecutor.ExecutionParams memory params;
        params.amountIn = 1 ether;

        vm.prank(attacker);
        vm.expectRevert("NotOwner");
        executor.executeArbitrageDirect(params);

        vm.prank(attacker);
        vm.expectRevert("NotOwner");
        executor.executeArbitrageFlash(params);

        vm.prank(attacker);
        vm.expectRevert("NotOwner");
        executor.withdrawToken(WETH_ADDR, 1 ether);

        vm.prank(attacker);
        vm.expectRevert("NotOwner");
        executor.withdrawETH(1 ether);
    }

    /// @notice Test that unauthorized direct invocation of callback hooks reverts.
    function test_Security_RevertsOnSpoofedCallbacks() public {
        vm.prank(attacker);
        vm.expectRevert("UnauthorizedV2Callback");
        executor.uniswapV2Call(address(this), 0, 0, "");

        vm.prank(attacker);
        vm.expectRevert("UnauthorizedV3Callback");
        executor.uniswapV3SwapCallback(0, 0, "");
    }

    /// @notice Test emergency rescue functions for tokens and native ETH.
    function test_EmergencyWithdrawal() public {
        // Fund executor with WETH
        deal(WETH_ADDR, address(executor), 5 ether);
        assertEq(weth.balanceOf(address(executor)), 5 ether);

        uint256 ownerWethBefore = weth.balanceOf(owner);
        executor.withdrawToken(WETH_ADDR, 5 ether);
        assertEq(weth.balanceOf(address(executor)), 0);
        assertEq(weth.balanceOf(owner), ownerWethBefore + 5 ether);

        // Fund executor with native ETH
        vm.deal(address(executor), 2 ether);
        uint256 ownerEthBefore = owner.balance;
        executor.withdrawETH(2 ether);
        assertEq(address(executor).balance, 0);
        assertEq(owner.balance, ownerEthBefore + 2 ether);
    }

    /// @notice Test self-funded multi-hop execution enforcing atomic revert on insufficient profit.
    function test_DirectSwap_RevertsWhen_InsufficientProfit() public {
        deal(WETH_ADDR, address(executor), 10 ether);

        MevExecutor.Hop[] memory hops = new MevExecutor.Hop[](2);
        // Hop 0: Uniswap V2 WETH -> USDC
        hops[0] = MevExecutor.Hop({
            protocol: MevExecutor.Protocol.UniswapV2,
            pool: UNISWAP_V2_WETH_USDC,
            tokenIn: WETH_ADDR,
            tokenOut: USDC_ADDR,
            fee: 0,
            sqrtPriceLimitX96: 0
        });
        // Hop 1: SushiSwap V2 USDC -> WETH
        hops[1] = MevExecutor.Hop({
            protocol: MevExecutor.Protocol.UniswapV2,
            pool: SUSHISWAP_V2_WETH_USDC,
            tokenIn: USDC_ADDR,
            tokenOut: WETH_ADDR,
            fee: 0,
            sqrtPriceLimitX96: 0
        });

        MevExecutor.ExecutionParams memory params = MevExecutor.ExecutionParams({
            amountIn: 1 ether,
            minProfit: 10 ether, // Unrealistic profit demand must trigger atomic revert
            bribeBps: 0,
            hops: hops
        });

        vm.expectRevert(); // Reverts with "NoProfit" or "InsufficientProfit"
        executor.executeArbitrageDirect(params);
    }

    /// @notice Test self-funded multi-hop execution on a real profitable price imbalance.
    function test_DirectSwap_ProfitableArbitrageAndBuilderBribe() public {
        // 1. Create a substantial synthetic price dislocation on SushiSwap:
        // A "whale" swaps 300 WETH for USDC on SushiSwap, driving down the price of WETH relative to USDC on SushiSwap
        address whale = makeAddr("whale");
        deal(WETH_ADDR, whale, 300 ether);

        vm.startPrank(whale);
        weth.transfer(SUSHISWAP_V2_WETH_USDC, 300 ether);
        (uint112 sR0, uint112 sR1, ) = IUniswapV2Pair(SUSHISWAP_V2_WETH_USDC).getReserves();
        // sR0 is USDC, sR1 is WETH. WETH is token1.
        uint256 amountInWithFee = 300 ether * 997;
        uint256 usdcOut = (amountInWithFee * uint256(sR0)) / ((uint256(sR1) * 1000) + amountInWithFee);
        IUniswapV2Pair(SUSHISWAP_V2_WETH_USDC).swap(usdcOut, 0, whale, "");
        vm.stopPrank();

        // 2. Fund executor with working capital
        deal(WETH_ADDR, address(executor), 10 ether);
        uint256 initialWeth = weth.balanceOf(address(executor));

        // 3. Construct arbitrage route:
        // Hop 0: Uniswap V2 (Sell WETH -> Buy USDC at normal rate)
        // Hop 1: SushiSwap V2 (Sell USDC -> Buy discounted WETH at dislocated rate)
        MevExecutor.Hop[] memory hops = new MevExecutor.Hop[](2);
        hops[0] = MevExecutor.Hop({
            protocol: MevExecutor.Protocol.UniswapV2,
            pool: UNISWAP_V2_WETH_USDC,
            tokenIn: WETH_ADDR,
            tokenOut: USDC_ADDR,
            fee: 0,
            sqrtPriceLimitX96: 0
        });
        hops[1] = MevExecutor.Hop({
            protocol: MevExecutor.Protocol.UniswapV2,
            pool: SUSHISWAP_V2_WETH_USDC,
            tokenIn: USDC_ADDR,
            tokenOut: WETH_ADDR,
            fee: 0,
            sqrtPriceLimitX96: 0
        });

        uint16 bribeBps = 5000; // 50% bribe to block.coinbase
        MevExecutor.ExecutionParams memory params = MevExecutor.ExecutionParams({
            amountIn: 2 ether,
            minProfit: 0.01 ether,
            bribeBps: bribeBps,
            hops: hops
        });

        uint256 builderEthBefore = builder.balance;

        // 4. Execute direct arbitrage
        executor.executeArbitrageDirect(params);

        // 5. Verify results
        uint256 finalWeth = weth.balanceOf(address(executor));
        assertGt(finalWeth, initialWeth, "Executor should capture net positive WETH profit");

        uint256 builderEthAfter = builder.balance;
        assertGt(builderEthAfter, builderEthBefore, "Builder should receive positive ETH bribe");
    }

    /// @notice Test zero-capital Flash Swap multi-hop arbitrage.
    function test_FlashSwap_ProfitableArbitrage() public {
        // 1. Create price dislocation on SushiSwap (dump 400 WETH into SushiSwap)
        address whale = makeAddr("whale2");
        deal(WETH_ADDR, whale, 400 ether);

        vm.startPrank(whale);
        weth.transfer(SUSHISWAP_V2_WETH_USDC, 400 ether);
        (uint112 sR0, uint112 sR1, ) = IUniswapV2Pair(SUSHISWAP_V2_WETH_USDC).getReserves();
        uint256 amountInWithFee = 400 ether * 997;
        uint256 usdcOut = (amountInWithFee * uint256(sR0)) / ((uint256(sR1) * 1000) + amountInWithFee);
        IUniswapV2Pair(SUSHISWAP_V2_WETH_USDC).swap(usdcOut, 0, whale, "");
        vm.stopPrank();

        // 2. Note: Executor starts with ZERO WETH balance! Pure flash swap.
        assertEq(weth.balanceOf(address(executor)), 0, "Executor starts with zero WETH");

        // 3. Construct arbitrage route:
        // Hop 0: Uniswap V2 (Flash-borrow USDC out, deliver WETH in callback)
        // Hop 1: SushiSwap V2 (Sell USDC -> Buy discounted WETH)
        MevExecutor.Hop[] memory hops = new MevExecutor.Hop[](2);
        hops[0] = MevExecutor.Hop({
            protocol: MevExecutor.Protocol.UniswapV2,
            pool: UNISWAP_V2_WETH_USDC,
            tokenIn: WETH_ADDR,
            tokenOut: USDC_ADDR,
            fee: 0,
            sqrtPriceLimitX96: 0
        });
        hops[1] = MevExecutor.Hop({
            protocol: MevExecutor.Protocol.UniswapV2,
            pool: SUSHISWAP_V2_WETH_USDC,
            tokenIn: USDC_ADDR,
            tokenOut: WETH_ADDR,
            fee: 0,
            sqrtPriceLimitX96: 0
        });

        uint16 bribeBps = 9000; // 90% bribe to block.coinbase
        MevExecutor.ExecutionParams memory params = MevExecutor.ExecutionParams({
            amountIn: 3 ether,
            minProfit: 0.001 ether,
            bribeBps: bribeBps,
            hops: hops
        });

        uint256 builderEthBefore = builder.balance;

        // 4. Execute zero-capital Flash Swap
        executor.executeArbitrageFlash(params);

        // 5. Verify executor retained profit and builder received 90% bribe
        uint256 retainedWeth = weth.balanceOf(address(executor));
        assertGt(retainedWeth, 0, "Executor should retain net WETH profit from flash swap");

        uint256 builderEthAfter = builder.balance;
        assertGt(builderEthAfter, builderEthBefore, "Builder should receive 90% of profit as ETH bribe");
    }

    /// @dev Helper to encode compact bit-packed calldata matching Phase 2.4 spec.
    function _encodeCompact(
        uint8 mode,
        uint16 bribeBps,
        uint128 amountIn,
        uint128 minProfit,
        MevExecutor.Hop[] memory hops
    ) internal pure returns (bytes memory) {
        bytes memory data = abi.encodePacked(
            mode,
            bribeBps,
            amountIn,
            minProfit,
            uint8(hops.length)
        );
        for (uint256 i = 0; i < hops.length; i++) {
            data = abi.encodePacked(
                data,
                hops[i].pool,
                hops[i].tokenIn,
                hops[i].tokenOut,
                hops[i].fee,
                uint8(hops[i].protocol),
                uint8(hops[i].tokenIn < hops[i].tokenOut ? 1 : 0)
            );
        }
        return data;
    }

    /// @notice Test compact Yul calldata fallback for direct self-funded arbitrage.
    function test_CompactYul_DirectSwap_ProfitableArbitrageAndBuilderBribe() public {
        address whale = makeAddr("whale_compact_direct");
        deal(WETH_ADDR, whale, 300 ether);

        vm.startPrank(whale);
        weth.transfer(SUSHISWAP_V2_WETH_USDC, 300 ether);
        (uint112 sR0, uint112 sR1, ) = IUniswapV2Pair(SUSHISWAP_V2_WETH_USDC).getReserves();
        uint256 amountInWithFee = 300 ether * 997;
        uint256 usdcOut = (amountInWithFee * uint256(sR0)) / ((uint256(sR1) * 1000) + amountInWithFee);
        IUniswapV2Pair(SUSHISWAP_V2_WETH_USDC).swap(usdcOut, 0, whale, "");
        vm.stopPrank();

        deal(WETH_ADDR, address(executor), 10 ether);
        uint256 initialWeth = weth.balanceOf(address(executor));

        MevExecutor.Hop[] memory hops = new MevExecutor.Hop[](2);
        hops[0] = MevExecutor.Hop({
            protocol: MevExecutor.Protocol.UniswapV2,
            pool: UNISWAP_V2_WETH_USDC,
            tokenIn: WETH_ADDR,
            tokenOut: USDC_ADDR,
            fee: 0,
            sqrtPriceLimitX96: 0
        });
        hops[1] = MevExecutor.Hop({
            protocol: MevExecutor.Protocol.UniswapV2,
            pool: SUSHISWAP_V2_WETH_USDC,
            tokenIn: USDC_ADDR,
            tokenOut: WETH_ADDR,
            fee: 0,
            sqrtPriceLimitX96: 0
        });

        bytes memory compactData = _encodeCompact(
            0, // mode 0 = Direct
            5000, // 50% bribe
            2 ether,
            0.01 ether,
            hops
        );

        assertEq(compactData.length, 36 + 2 * 65); // exactly 166 bytes!

        uint256 builderEthBefore = builder.balance;

        (bool ok, ) = address(executor).call(compactData);
        assertTrue(ok, "Compact Yul direct swap should succeed");

        uint256 finalWeth = weth.balanceOf(address(executor));
        assertGt(finalWeth, initialWeth, "Executor captured profit via compact Yul");

        uint256 builderEthAfter = builder.balance;
        assertGt(builderEthAfter, builderEthBefore, "Builder received ETH bribe via compact Yul");
    }

    /// @notice Test compact Yul calldata fallback for zero-capital flash swap arbitrage.
    function test_CompactYul_FlashSwap_ProfitableArbitrage() public {
        address whale = makeAddr("whale_compact_flash");
        deal(WETH_ADDR, whale, 400 ether);

        vm.startPrank(whale);
        weth.transfer(SUSHISWAP_V2_WETH_USDC, 400 ether);
        (uint112 sR0, uint112 sR1, ) = IUniswapV2Pair(SUSHISWAP_V2_WETH_USDC).getReserves();
        uint256 amountInWithFee = 400 ether * 997;
        uint256 usdcOut = (amountInWithFee * uint256(sR0)) / ((uint256(sR1) * 1000) + amountInWithFee);
        IUniswapV2Pair(SUSHISWAP_V2_WETH_USDC).swap(usdcOut, 0, whale, "");
        vm.stopPrank();

        assertEq(weth.balanceOf(address(executor)), 0);

        MevExecutor.Hop[] memory hops = new MevExecutor.Hop[](2);
        hops[0] = MevExecutor.Hop({
            protocol: MevExecutor.Protocol.UniswapV2,
            pool: UNISWAP_V2_WETH_USDC,
            tokenIn: WETH_ADDR,
            tokenOut: USDC_ADDR,
            fee: 0,
            sqrtPriceLimitX96: 0
        });
        hops[1] = MevExecutor.Hop({
            protocol: MevExecutor.Protocol.UniswapV2,
            pool: SUSHISWAP_V2_WETH_USDC,
            tokenIn: USDC_ADDR,
            tokenOut: WETH_ADDR,
            fee: 0,
            sqrtPriceLimitX96: 0
        });

        bytes memory compactData = _encodeCompact(
            1, // mode 1 = Flash
            9000, // 90% bribe
            3 ether,
            0.001 ether,
            hops
        );

        assertEq(compactData.length, 36 + 2 * 65);

        uint256 builderEthBefore = builder.balance;

        (bool ok, ) = address(executor).call(compactData);
        assertTrue(ok, "Compact Yul flash swap should succeed");

        uint256 retainedWeth = weth.balanceOf(address(executor));
        assertGt(retainedWeth, 0, "Executor retained profit via compact Yul flash swap");

        uint256 builderEthAfter = builder.balance;
        assertGt(builderEthAfter, builderEthBefore, "Builder received 90% bribe via compact Yul flash swap");
    }

    receive() external payable {}
}
