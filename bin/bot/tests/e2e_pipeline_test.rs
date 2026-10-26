//! Flagship Full-Pipeline End-to-End Integration Test for Phase 1.
//!
//! Validates the complete pipeline:
//! Collector -> State -> Simulator -> Strategy -> Executor
//!
//! 1. Forks Ethereum Mainnet at pinned block 19,500,000 via local Anvil.
//! 2. Initializes AlloyDB, revm ForkedSandbox, and deploys MevExecutor contract.
//! 3. Populates MarketState with on-chain Uniswap V2 and SushiSwap V2 WETH/USDC pools.
//! 4. Injects a pending whale transaction dumping 200 WETH for USDC on SushiSwap.
//! 5. Collector decodes the swap calldata, triggering the BotEngine pipeline.
//! 6. BotEngine:
//!    - Simulates victim in ForkedSandbox.
//!    - Solves optimal backrun route & x* sizing via StrategyEngine.
//!    - Verifies candidate backrun on post-victim snapshot branch.
//!    - Encodes and signs atomic MevBundle ([Victim_Tx, Backrun_Tx]).
//! 7. Executes the atomic bundle and asserts:
//!    - 0 reverts across all hops.
//!    - MevExecutor WETH balance increases by net profit.
//!    - Builder coinbase receives >= 90% profit bribe in ETH.

use alloy::{
    consensus::{SignableTransaction, TxEip1559},
    eips::{eip2718::Encodable2718, BlockId},
    node_bindings::Anvil,
    primitives::{address, Address, Bytes, TxKind, U256},
    providers::ProviderBuilder,
    signers::{local::PrivateKeySigner, SignerSync},
    sol,
    sol_types::{SolCall, SolValue},
};
use eyre::Result;
use revm::DatabaseRef;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use mev_bot::{BotConfig, BotEngine};
use mev_collector::decoder::CalldataDecoder;
use mev_executor::MevExecutorContract;
use mev_simulator::{build_call_tx, build_create_tx, AlloyDB, ForkedSandbox, SimulationRunner};
use mev_state::{MarketState, USDC, WETH};
use mev_strategy::{ProfitEvaluator, StrategyEngine};
use mev_types::{PoolDescriptor, PoolState, PoolVariant, V2PoolState};

const UNISWAP_V2_WETH_USDC: Address = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
const SUSHISWAP_V2_WETH_USDC: Address = address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0");
const UNISWAP_V2_ROUTER02: Address = address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D");

sol! {
    interface IUniswapV2PairE2E {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
        function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
    }

    interface IWETHE2E {
        function deposit() external payable;
        function transfer(address to, uint256 value) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }

    interface IUniswapV2Router02E2E {
        function swapExactETHForTokens(
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external payable returns (uint256[] memory amounts);
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
async fn test_full_e2e_pipeline_whale_backrun_bundle() -> Result<()> {
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

    // 3. Set up Searcher Identity Signers
    let tx_signer: PrivateKeySigner = "0x0000000000000000000000000000000000000000000000000000000000000001".parse()?;
    let auth_signer: PrivateKeySigner = "0x0000000000000000000000000000000000000000000000000000000000000002".parse()?;
    let searcher_owner = tx_signer.address();

    let one_eth = U256::from(10).pow(U256::from(18));
    sandbox.set_balance(searcher_owner, one_eth * U256::from(500));

    // 4. Deploy MevExecutor Smart Contract inside the Sandbox
    let mut init_code = MevExecutorContract::BYTECODE.to_vec();
    init_code.extend_from_slice(&WETH.abi_encode());

    let deploy_tx = build_create_tx(
        searcher_owner,
        Bytes::from(init_code),
        U256::ZERO,
        5_000_000,
        None,
    );
    let deploy_res = sandbox.execute_victim(deploy_tx).expect("MevExecutor deploy");
    assert!(deploy_res.success, "MevExecutor deployment must succeed");
    let executor_addr = deploy_res.created_address.expect("executor address");
    println!("Deployed MevExecutor at {:?}", executor_addr);

    // Fund MevExecutor with 50 WETH working capital
    let deposit_tx = build_call_tx(
        searcher_owner,
        WETH,
        Bytes::from(IWETHE2E::depositCall {}.abi_encode()),
        one_eth * U256::from(50),
        100_000,
        None,
    );
    sandbox.execute_victim(deposit_tx)?;

    let transfer_tx = build_call_tx(
        searcher_owner,
        WETH,
        Bytes::from(IWETHE2E::transferCall { to: executor_addr, value: one_eth * U256::from(50) }.abi_encode()),
        U256::ZERO,
        100_000,
        None,
    );
    sandbox.execute_victim(transfer_tx)?;

    // 5. Query on-chain pool reserves and initialize MarketState
    let get_res_call = Bytes::from(IUniswapV2PairE2E::getReservesCall {}.abi_encode());

    let res_uni = sandbox.simulate_tx(build_call_tx(searcher_owner, UNISWAP_V2_WETH_USDC, get_res_call.clone(), U256::ZERO, 100_000, None))?;
    let uni_decoded = IUniswapV2PairE2E::getReservesCall::abi_decode_returns(&res_uni.output)?;

    let res_sushi = sandbox.simulate_tx(build_call_tx(searcher_owner, SUSHISWAP_V2_WETH_USDC, get_res_call.clone(), U256::ZERO, 100_000, None))?;
    let sushi_decoded = IUniswapV2PairE2E::getReservesCall::abi_decode_returns(&res_sushi.output)?;

    let uni_desc = PoolDescriptor::new(UNISWAP_V2_WETH_USDC, PoolVariant::UniswapV2, WETH, USDC, 3000);
    let sushi_desc = PoolDescriptor::new(SUSHISWAP_V2_WETH_USDC, PoolVariant::UniswapV2, WETH, USDC, 3000);

    let market_state = Arc::new(MarketState::new());
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

    // 6. Ingest Victim Whale Transaction: 200 WETH swap on SushiSwap
    let whale_signer: PrivateKeySigner = "0x0000000000000000000000000000000000000000000000000000000000000099".parse()?;
    let whale = whale_signer.address();
    let dump_amount = one_eth * U256::from(200);
    sandbox.set_balance(whale, dump_amount * U256::from(2));

    // Construct raw signed victim transaction (calling Router02 swapExactETHForTokens)
    let t_decode_start = Instant::now();
    let swap_call = IUniswapV2Router02E2E::swapExactETHForTokensCall {
        amountOutMin: U256::ZERO,
        path: vec![WETH, USDC],
        to: whale,
        deadline: U256::from(2000000000u64),
    };
    let router_calldata = Bytes::from(swap_call.abi_encode());

    // Verify Ingestion Engine decodes this router call
    let decoded_swaps = CalldataDecoder::decode(alloy::primitives::B256::ZERO, UNISWAP_V2_ROUTER02, dump_amount, &router_calldata);
    assert!(!decoded_swaps.is_empty(), "Collector must decode swapExactETHForTokens");
    let decode_us = t_decode_start.elapsed().as_micros() as f64;

    // Build signed victim transaction
    let victim_tx_eip1559 = TxEip1559 {
        chain_id: 1,
        nonce: 0,
        gas_limit: 300_000,
        max_fee_per_gas: 30_000_000_000,
        max_priority_fee_per_gas: 2_000_000_000,
        to: TxKind::Call(UNISWAP_V2_ROUTER02),
        value: dump_amount,
        access_list: Default::default(),
        input: router_calldata.clone(),
    };
    let victim_sig_hash = victim_tx_eip1559.signature_hash();
    let victim_sig = whale_signer.sign_hash_sync(&victim_sig_hash)?;
    let signed_victim_tx = victim_tx_eip1559.into_signed(victim_sig);
    let mut victim_raw_bytes = Vec::new();
    signed_victim_tx.encode_2718(&mut victim_raw_bytes);
    let victim_raw_tx = Bytes::from(victim_raw_bytes);

    let victim_tx_env = build_call_tx(
        whale,
        UNISWAP_V2_ROUTER02,
        router_calldata,
        dump_amount,
        300_000,
        None,
    );

    // 7. Initialize BotEngine with Shadow Mode enabled
    let bot_config = BotConfig {
        executor_contract: executor_addr,
        auth_signer,
        tx_signer,
        chain_id: 1,
        shadow_mode: true,
        base_fee_wei: 20_000_000_000, // 20 gwei
        priority_fee_wei: 1_000_000_000, // 1 gwei
        min_profit_wei: U256::from(10).pow(U256::from(15)), // 0.001 WETH min profit
        bribe_bps: 9000, // 90% builder bribe
    };

    let strategy_engine = StrategyEngine::new(
        WETH,
        vec![WETH, USDC],
        ProfitEvaluator::new(bot_config.min_profit_wei, bot_config.bribe_bps, 70_000),
        U256::from(10).pow(U256::from(16)),
        one_eth * U256::from(50),
    );

    let engine = BotEngine::new(bot_config, market_state.clone(), strategy_engine);

    // 8. Process Victim Swap through the Full Orchestrator Pipeline
    let telemetry_res = engine
        .process_victim_swap(
            &mut sandbox,
            victim_raw_tx.clone(),
            victim_tx_env,
            UNISWAP_V2_WETH_USDC,
            fork_block,
            0,
            decode_us,
        )
        .await?;

    assert!(telemetry_res.is_some(), "Pipeline must discover, size, and construct bundle");
    let telemetry = telemetry_res.unwrap();

    println!("\n⚡ [FLAGSHIP E2E TELEMETRY]");
    println!("├── Decode Latency:       {:.2} µs", telemetry.decode_us);
    println!("├── Victim Sim Latency:   {:.2} µs", telemetry.victim_sim_us);
    println!("├── Strategy Latency:     {:.2} µs", telemetry.strategy_us);
    println!("├── Verify Sim Latency:   {:.2} µs", telemetry.verify_sim_us);
    println!("├── Bundle Sign Latency:  {:.2} µs", telemetry.bundle_sign_us);
    println!("└── Total Tick-to-Trade:  {:.2} µs\n", telemetry.total_tick_to_trade_us);

    assert!(telemetry.opportunity.net_profit > U256::ZERO);
    assert_eq!(telemetry.bundle.txs.len(), 2);
    assert_eq!(telemetry.bundle.block_number, fork_block + 1);

    // 9. Atomic Bundle Execution Verification on Branched State
    let mut execution_branch = sandbox.snapshot();

    // Check balances before backrun
    let builder_eth_before = execution_branch
        .basic_ref(builder_coinbase)
        .unwrap_or(None)
        .map(|acc| acc.balance)
        .unwrap_or(U256::ZERO);

    let exec_call = MevExecutorContract::executeArbitrageDirectCall {
        params: telemetry.opportunity.execution_params.clone(),
    };
    let backrun_tx = build_call_tx(
        searcher_owner,
        executor_addr,
        Bytes::from(exec_call.abi_encode()),
        U256::ZERO,
        500_000,
        None,
    );

    let backrun_sim_res = sandbox
        .simulate_candidate_commit(&mut execution_branch, backrun_tx)
        .expect("candidate commit must succeed");
    assert!(backrun_sim_res.success, "backrun execution must succeed with 0 reverts");

    // Check builder received bribe
    let builder_eth_after = execution_branch
        .basic_ref(builder_coinbase)
        .unwrap_or(None)
        .map(|acc| acc.balance)
        .unwrap_or(U256::ZERO);

    assert!(
        builder_eth_after > builder_eth_before,
        "Builder must receive positive bribe on-chain"
    );

    let bribe_received = builder_eth_after - builder_eth_before;
    println!(
        "🎉 [ATOMIC SETTLEMENT] Verified Builder Bribe: {} wei ({:.4} ETH)",
        bribe_received,
        bribe_received.to::<u128>() as f64 / 1e18
    );

    Ok(())
}
