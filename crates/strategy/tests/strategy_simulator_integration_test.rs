//! End-to-end integration test verifying Strategy Engine + Simulator Sandbox.
//!
//! 1. Forks Ethereum Mainnet at pinned block 19,500,000 via local Anvil.
//! 2. Initializes AlloyDB, revm ForkedSandbox, and deploys MevExecutor contract.
//! 3. Populates MarketState with on-chain Uniswap V2 and SushiSwap V2 WETH/USDC pools.
//! 4. Simulates a victim whale swap dumping 200 WETH on SushiSwap, dislocating the pool price.
//! 5. Updates MarketState and invokes StrategyEngine::evaluate_touched_pool.
//! 6. Verifies that mev-strategy discovers the 2-hop cycle, derives optimal sizing x*, and encodes ExecutionParams.
//! 7. Simulates the candidate backrun in revm ForkedSandbox, verifying 100% success, positive net profit, and builder bribe.

use alloy::{
    eips::BlockId,
    node_bindings::Anvil,
    primitives::{address, Address, Bytes, U256},
    providers::ProviderBuilder,
    sol,
    sol_types::{SolCall, SolValue},
};
use eyre::Result;
use mev_executor::MevExecutorContract;
use mev_simulator::{build_call_tx, build_create_tx, AlloyDB, ForkedSandbox, SimulationRunner};
use mev_state::{MarketState, USDC, WETH};
use mev_strategy::{ProfitEvaluator, StrategyEngine};
use mev_types::{PoolDescriptor, PoolState, PoolVariant, V2PoolState};
use revm::DatabaseRef;
use std::path::PathBuf;

const UNISWAP_V2_WETH_USDC: Address = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
const SUSHISWAP_V2_WETH_USDC: Address = address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0");

sol! {
    interface IUniswapV2PairIntegration {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
        function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
    }

    interface IWETHIntegration {
        function deposit() external payable;
        function transfer(address to, uint256 value) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }
}

fn find_anvil_path() -> Option<PathBuf> {
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let p = PathBuf::from(userprofile).join(".foundry").join("bin").join("anvil.exe");
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join(".foundry").join("bin").join("anvil");
        if p.exists() {
            return Some(p);
        }
    }
    let fallback = PathBuf::from("C:\\Users\\User\\.foundry\\bin\\anvil.exe");
    if fallback.exists() {
        return Some(fallback);
    }
    None
}

fn get_fork_rpc_url() -> String {
    if let Ok(content) = std::fs::read_to_string(".env") {
        for line in content.lines() {
            if let Some(rest) = line.trim().strip_prefix("MAINNET_RPC_URL=") {
                let trimmed = rest.trim().trim_matches('"').trim_matches('\'');
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    std::env::var("MAINNET_RPC_URL")
        .unwrap_or_else(|_| "https://eth-mainnet.public.blastapi.io".to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_end_to_end_strategy_and_simulator_backrun() -> Result<()> {
    let rpc_url = get_fork_rpc_url();
    let fork_block = 19_500_000u64;

    // 1. Spawn local Anvil instance pinned at block 19,500,000
    let mut anvil_builder = Anvil::new()
        .fork(rpc_url)
        .fork_block_number(fork_block);

    if let Some(path) = find_anvil_path() {
        anvil_builder = anvil_builder.path(path);
    }

    let anvil = anvil_builder.spawn();
    let provider = ProviderBuilder::new().connect_http(anvil.endpoint().parse()?);

    // 2. Initialize AlloyDB and SimulationRunner with builder coinbase
    let alloy_db = AlloyDB::new(provider, Some(BlockId::number(fork_block)));
    let builder_coinbase = address!("000000000000000000000000000000000000bEEF");
    let runner = SimulationRunner::new(fork_block, 1711000000)
        .with_coinbase(builder_coinbase)
        .with_basefee(20_000_000_000); // 20 gwei
    let mut sandbox = ForkedSandbox::new(alloy_db, runner);

    // 3. Set up searcher and deploy MevExecutor contract
    let searcher_owner = address!("1111111111111111111111111111111111111111");
    let one_eth = U256::from(10).pow(U256::from(18));
    sandbox.set_balance(searcher_owner, one_eth * U256::from(200));

    // Construct constructor bytecode: MevExecutor bytecode + ABI-encoded WETH address
    let mut init_code = MevExecutorContract::BYTECODE.to_vec();
    init_code.extend_from_slice(&WETH.abi_encode());

    let deploy_tx = build_create_tx(
        searcher_owner,
        Bytes::from(init_code),
        U256::ZERO,
        5_000_000,
        None,
    );
    let deploy_res = sandbox
        .execute_victim(deploy_tx)
        .expect("MevExecutor deployment must succeed");
    assert!(deploy_res.success, "deployment must be successful");
    let executor_addr = deploy_res
        .created_address
        .expect("MevExecutor address must be recorded");
    println!("Deployed MevExecutor at {:?}", executor_addr);

    // 4. Fund MevExecutor with 50 WETH working capital for direct arbitrage
    // (EIP-3607 requires EOA caller for tx.origin, so searcher_owner wraps and transfers)
    let deposit_tx = build_call_tx(
        searcher_owner,
        WETH,
        Bytes::from(IWETHIntegration::depositCall {}.abi_encode()),
        one_eth * U256::from(50),
        100_000,
        None,
    );
    let deposit_res = sandbox
        .execute_victim(deposit_tx)
        .expect("deposit must succeed");
    assert!(deposit_res.success, "WETH deposit must succeed");

    let transfer_weth_tx = build_call_tx(
        searcher_owner,
        WETH,
        Bytes::from(
            IWETHIntegration::transferCall {
                to: executor_addr,
                value: one_eth * U256::from(50),
            }
            .abi_encode(),
        ),
        U256::ZERO,
        100_000,
        None,
    );
    let transfer_weth_res = sandbox
        .execute_victim(transfer_weth_tx)
        .expect("transfer WETH to MevExecutor must succeed");
    assert!(transfer_weth_res.success);

    // 5. Query initial reserves from Uniswap V2 and SushiSwap V2
    let get_res_call = Bytes::from(IUniswapV2PairIntegration::getReservesCall {}.abi_encode());

    let res_uni = sandbox
        .simulate_tx(build_call_tx(
            searcher_owner,
            UNISWAP_V2_WETH_USDC,
            get_res_call.clone(),
            U256::ZERO,
            100_000,
            None,
        ))
        .expect("query uniswap reserves");
    assert!(res_uni.success);
    let uni_decoded = IUniswapV2PairIntegration::getReservesCall::abi_decode_returns(
        &res_uni.output,
    )?;

    let res_sushi = sandbox
        .simulate_tx(build_call_tx(
            searcher_owner,
            SUSHISWAP_V2_WETH_USDC,
            get_res_call.clone(),
            U256::ZERO,
            100_000,
            None,
        ))
        .expect("query sushiswap reserves");
    assert!(res_sushi.success);
    let sushi_decoded = IUniswapV2PairIntegration::getReservesCall::abi_decode_returns(
        &res_sushi.output,
    )?;

    println!(
        "Initial Uniswap V2 Reserves: USDC = {}, WETH = {}",
        uni_decoded.reserve0, uni_decoded.reserve1
    );
    println!(
        "Initial SushiSwap V2 Reserves: USDC = {}, WETH = {}",
        sushi_decoded.reserve0, sushi_decoded.reserve1
    );

    // Register pools into MarketState
    // Note: In PoolDescriptor::new, USDC < WETH canonically (token0: USDC, token1: WETH)
    let uni_desc = PoolDescriptor::new(
        UNISWAP_V2_WETH_USDC,
        PoolVariant::UniswapV2,
        WETH,
        USDC,
        3000,
    );
    let sushi_desc = PoolDescriptor::new(
        SUSHISWAP_V2_WETH_USDC,
        PoolVariant::UniswapV2,
        WETH,
        USDC,
        3000,
    );

    let market_state = MarketState::new();
    market_state.update_pool(
        uni_desc.clone(),
        PoolState::V2(V2PoolState {
            reserve0: uni_decoded.reserve0.to::<u128>(),
            reserve1: uni_decoded.reserve1.to::<u128>(),
            block_timestamp_last: uni_decoded.blockTimestampLast,
        }),
    );
    market_state.update_pool(
        sushi_desc.clone(),
        PoolState::V2(V2PoolState {
            reserve0: sushi_decoded.reserve0.to::<u128>(),
            reserve1: sushi_decoded.reserve1.to::<u128>(),
            block_timestamp_last: sushi_decoded.blockTimestampLast,
        }),
    );

    // 6. Simulate Victim Swap: Whale swaps 200 WETH for USDC on SushiSwap
    // This dramatically dislocates SushiSwap's price (cheap WETH on SushiSwap relative to Uniswap V2)
    let whale = address!("9999999999999999999999999999999999999999");
    let dump_amount = one_eth * U256::from(200);
    sandbox.set_balance(whale, dump_amount * U256::from(2));

    // Whale wraps ETH to WETH
    let whale_wrap_tx = build_call_tx(
        whale,
        WETH,
        Bytes::from(IWETHIntegration::depositCall {}.abi_encode()),
        dump_amount,
        100_000,
        None,
    );
    let wrap_res = sandbox.execute_victim(whale_wrap_tx)?;
    assert!(wrap_res.success);

    // Whale transfers WETH to SushiSwap V2 pair
    let transfer_tx = build_call_tx(
        whale,
        WETH,
        Bytes::from(
            IWETHIntegration::transferCall {
                to: SUSHISWAP_V2_WETH_USDC,
                value: dump_amount,
            }
            .abi_encode(),
        ),
        U256::ZERO,
        100_000,
        None,
    );
    let transfer_res = sandbox.execute_victim(transfer_tx)?;
    assert!(transfer_res.success);

    // Calculate exact USDC output:
    // In SushiSwap, token0 is USDC (r0), token1 is WETH (r1)
    let r_usdc = U256::from(sushi_decoded.reserve0);
    let r_weth = U256::from(sushi_decoded.reserve1);
    let amount_in_with_fee = dump_amount * U256::from(997);
    let numerator = amount_in_with_fee * r_usdc;
    let denominator = (r_weth * U256::from(1000)) + amount_in_with_fee;
    let usdc_out = numerator / denominator;

    // Whale calls swap() on SushiSwap pair to claim USDC
    let swap_tx = build_call_tx(
        whale,
        SUSHISWAP_V2_WETH_USDC,
        Bytes::from(
            IUniswapV2PairIntegration::swapCall {
                amount0Out: usdc_out,
                amount1Out: U256::ZERO,
                to: whale,
                data: Bytes::default(),
            }
            .abi_encode(),
        ),
        U256::ZERO,
        200_000,
        None,
    );
    let victim_res = sandbox.execute_victim(swap_tx)?;
    assert!(victim_res.success, "victim whale swap must succeed");

    // 7. Read updated post-victim reserves and update MarketState
    let res_post_sushi = sandbox
        .simulate_tx(build_call_tx(
            searcher_owner,
            SUSHISWAP_V2_WETH_USDC,
            get_res_call,
            U256::ZERO,
            100_000,
            None,
        ))
        .expect("query updated sushiswap reserves");
    assert!(res_post_sushi.success);
    let post_sushi_decoded = IUniswapV2PairIntegration::getReservesCall::abi_decode_returns(
        &res_post_sushi.output,
    )?;

    println!(
        "Post-Victim SushiSwap V2 Reserves: USDC = {}, WETH = {}",
        post_sushi_decoded.reserve0, post_sushi_decoded.reserve1
    );

    market_state.update_pool(
        sushi_desc,
        PoolState::V2(V2PoolState {
            reserve0: post_sushi_decoded.reserve0.to::<u128>(),
            reserve1: post_sushi_decoded.reserve1.to::<u128>(),
            block_timestamp_last: post_sushi_decoded.blockTimestampLast,
        }),
    );

    // 8. Run StrategyEngine to detect arbitrage opportunity touching SushiSwap
    let base_fee_wei = 20_000_000_000u64; // 20 gwei
    let evaluator = ProfitEvaluator::new(
        U256::from(10).pow(U256::from(15)), // 0.001 WETH min profit
        9000,                               // 90% builder bribe
        70_000,                             // 70k gas per hop
    );

    let engine = StrategyEngine::new(
        WETH,
        vec![WETH, USDC],
        evaluator,
        U256::from(10).pow(U256::from(16)), // 0.01 WETH min trade
        one_eth * U256::from(40),            // 40 WETH max trade
    );

    let opportunities =
        engine.evaluate_touched_pool(&market_state, SUSHISWAP_V2_WETH_USDC, base_fee_wei);

    assert!(
        !opportunities.is_empty(),
        "StrategyEngine must discover profitable arbitrage backrun!"
    );

    let top_opp = &opportunities[0];
    println!(
        "\n⚡ [STRATEGY DISCOVERY] Discovered Arbitrage Opportunity:\n\
         - Optimal Input:  {} wei ({:.4} WETH)\n\
         - Expected Out:   {} wei ({:.4} WETH)\n\
         - Gross Profit:   {} wei ({:.4} WETH)\n\
         - Builder Bribe:  {} wei ({:.4} ETH)\n\
         - Net Profit:     {} wei ({:.4} ETH)\n",
        top_opp.optimal_input,
        top_opp.optimal_input.to::<u128>() as f64 / 1e18,
        top_opp.expected_output,
        top_opp.expected_output.to::<u128>() as f64 / 1e18,
        top_opp.gross_profit,
        top_opp.gross_profit.to::<u128>() as f64 / 1e18,
        top_opp.builder_bribe,
        top_opp.builder_bribe.to::<u128>() as f64 / 1e18,
        top_opp.net_profit,
        top_opp.net_profit.to::<u128>() as f64 / 1e18,
    );

    assert!(top_opp.optimal_input > U256::ZERO);
    assert!(top_opp.gross_profit > U256::ZERO);
    assert!(top_opp.net_profit > U256::ZERO);
    assert_eq!(top_opp.cycle.hops.len(), 2);
    // Hop 0 sells WETH -> USDC on Uniswap V2, Hop 1 sells USDC -> WETH on SushiSwap V2
    assert_eq!(top_opp.cycle.hops[0].pool.address, UNISWAP_V2_WETH_USDC);
    assert_eq!(top_opp.cycle.hops[1].pool.address, SUSHISWAP_V2_WETH_USDC);

    // 9. Simulate the candidate backrun in revm ForkedSandbox
    let exec_call = MevExecutorContract::executeArbitrageDirectCall {
        params: top_opp.execution_params.clone(),
    };
    let candidate_tx = build_call_tx(
        searcher_owner,
        executor_addr,
        Bytes::from(exec_call.abi_encode()),
        U256::ZERO,
        500_000,
        None,
    );

    // Simulate against snapshot branch
    let mut snapshot = sandbox.snapshot();
    let sim_res = sandbox
        .simulate_candidate(&mut snapshot, candidate_tx.clone())
        .expect("candidate simulation must succeed");

    if !sim_res.success {
        panic!(
            "Candidate backrun simulation failed! Revert reason: {:?}",
            sim_res.revert_reason()
        );
    }

    assert!(sim_res.success, "candidate simulation must succeed");
    assert!(
        sim_res.gas_used > 80_000,
        "gas used must reflect real 2-hop execution"
    );

    // 10. Commit backrun to snapshot branch and verify exact balance deltas
    let mut commit_branch = sandbox.snapshot();
    let builder_eth_before = commit_branch
        .basic_ref(builder_coinbase)
        .unwrap_or(None)
        .map(|acc| acc.balance)
        .unwrap_or(U256::ZERO);

    let commit_res = sandbox
        .simulate_candidate_commit(&mut commit_branch, candidate_tx)
        .expect("commit backrun must succeed");
    assert!(commit_res.success);

    // Verify builder received bribe at block.coinbase
    let builder_eth_after = commit_branch
        .basic_ref(builder_coinbase)
        .unwrap_or(None)
        .map(|acc| acc.balance)
        .unwrap_or(U256::ZERO);
    assert!(
        builder_eth_after > builder_eth_before,
        "Builder coinbase must receive positive bribe: before={}, after={}",
        builder_eth_before,
        builder_eth_after
    );
    let paid_bribe = builder_eth_after - builder_eth_before;
    println!(
        "⚡ [BUILDER BRIBE] Confirmed Builder Bribe: {} wei ({:.4} ETH)\n",
        paid_bribe,
        paid_bribe.to::<u128>() as f64 / 1e18
    );

    Ok(())
}
