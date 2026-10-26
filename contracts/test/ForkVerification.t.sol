// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import "forge-std/Test.sol";
import "../src/IWETH.sol";

contract ForkVerificationTest is Test {
    address constant WETH_MAINNET = 0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2;
    uint256 constant PINNED_BLOCK = 19_500_000;

    IWETH public weth;

    function setUp() public {
        string memory rpcUrl = vm.envOr("MAINNET_RPC_URL", string("https://eth-mainnet.public.blastapi.io"));
        vm.createSelectFork(rpcUrl, PINNED_BLOCK);
        weth = IWETH(WETH_MAINNET);
    }

    function test_ForkStateIntegrity() public view {
        // Assert block number matches pinned snapshot
        assertEq(block.number, PINNED_BLOCK, "Fork block number mismatch");

        // Verify WETH contract bytecode exists
        uint256 codeSize;
        assembly {
            codeSize := extcodesize(WETH_MAINNET)
        }
        assertGt(codeSize, 0, "WETH contract bytecode must exist on mainnet fork");

        // Verify total supply exceeds 1M WETH (sanity check on real ledger state)
        uint256 supply = weth.totalSupply();
        assertGt(supply, 1_000_000 ether, "WETH total supply should exceed 1M ether");
    }

    function test_WethDepositAndWithdrawal() public {
        address alice = makeAddr("alice");
        vm.deal(alice, 10 ether);

        vm.startPrank(alice);
        // Deposit 5 ETH into WETH9
        weth.deposit{value: 5 ether}();
        assertEq(weth.balanceOf(alice), 5 ether, "Alice WETH balance should be 5 ether");
        assertEq(alice.balance, 5 ether, "Alice native ETH balance should be 5 ether");

        // Withdraw 2 WETH back to ETH
        weth.withdraw(2 ether);
        assertEq(weth.balanceOf(alice), 3 ether, "Alice WETH balance should be 3 ether after withdraw");
        assertEq(alice.balance, 7 ether, "Alice native ETH balance should be 7 ether after withdraw");
        vm.stopPrank();
    }
}
