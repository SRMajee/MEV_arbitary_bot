//! Mainnet fork integration test for AlloyDB, in-process EVM simulation, and victim re-execution.
//!
//! Forks mainnet at pinned block 19,500,000 using Anvil, connects AlloyDB as the backend database,
//! executes a real Uniswap V2 Router02 swap transaction, commits the victim state, and verifies
//! sub-500 microsecond candidate backrun evaluation on the branched post-victim state.

use alloy::{
    eips::BlockId,
    node_bindings::Anvil,
    primitives::{address, Address, Bytes, U256},
    providers::ProviderBuilder,
    sol,
    sol_types::SolCall,
};
use eyre::Result;
use mev_simulator::{build_call_tx, AlloyDB, ForkedSandbox, SimulationRunner};
use mev_state::{USDC, WETH};
use std::path::PathBuf;
use std::time::Instant;

const UNISWAP_V2_ROUTER02: Address = address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D");

sol! {
    interface IUniswapV2Router02Fork {
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
async fn test_mainnet_fork_simulation_and_backrun_branching() -> Result<()> {
    let rpc_url = get_fork_rpc_url();
    let fork_block = 19_500_000u64;

    // 1. Spawn local Anvil instance forked at block 19,500,000
    let mut anvil_builder = Anvil::new()
        .fork(rpc_url)
        .fork_block_number(fork_block);

    if let Some(path) = find_anvil_path() {
        anvil_builder = anvil_builder.path(path);
    }

    let anvil = anvil_builder.spawn();
    let provider = ProviderBuilder::new().connect_http(anvil.endpoint().parse()?);

    // 2. Initialize AlloyDB with RPC fallback pinned to block 19,500,000
    let alloy_db = AlloyDB::new(provider, Some(BlockId::number(fork_block)));

    // 3. Initialize SimulationRunner & ForkedSandbox
    let runner = SimulationRunner::new(fork_block, 1711000000);
    let mut sandbox = ForkedSandbox::new(alloy_db, runner);

    // 4. Set up victim caller account with ETH in sandbox
    let victim_caller = address!("9999999999999999999999999999999999999999");
    let victim_recipient = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let one_eth = U256::from(10).pow(U256::from(18));
    sandbox.set_balance(victim_caller, one_eth * U256::from(10));

    // 5. Construct victim swap transaction: 1 ETH -> USDC via Uniswap V2 Router02
    let swap_call = IUniswapV2Router02Fork::swapExactETHForTokensCall {
        amountOutMin: U256::ZERO,
        path: vec![WETH, USDC],
        to: victim_recipient,
        deadline: U256::from(2000000000u64),
    };
    let calldata = Bytes::from(swap_call.abi_encode());

    let victim_tx = build_call_tx(
        victim_caller,
        UNISWAP_V2_ROUTER02,
        calldata.clone(),
        one_eth,
        300_000,
        None,
    );

    // 6. Execute victim transaction (lazy-fetching router & pool bytecode + storage via AlloyDB)
    println!("Executing victim transaction against forked block 19,500,000...");
    let victim_start = Instant::now();
    let victim_res = sandbox
        .execute_victim(victim_tx)
        .expect("victim transaction must execute");
    let initial_fetch_elapsed = victim_start.elapsed();

    println!(
        "Victim executed in {:?}: success={}, gas_used={}, logs={}",
        initial_fetch_elapsed,
        victim_res.success,
        victim_res.gas_used,
        victim_res.logs.len()
    );

    assert!(victim_res.success, "victim transaction must succeed on forked state");
    assert!(victim_res.gas_used > 50_000, "gas used must be substantial (actual swap)");
    assert!(!victim_res.logs.is_empty(), "victim swap must emit events (Sync, Swap, Transfer)");

    // 7. Snapshot post-victim state
    let snapshot = sandbox.snapshot();

    // 8. Warm-up candidate evaluation against snapshot branch
    let candidate_caller = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    sandbox.set_balance(candidate_caller, one_eth * U256::from(10));

    let candidate_tx = build_call_tx(
        victim_caller,
        UNISWAP_V2_ROUTER02,
        calldata,
        one_eth,
        300_000,
        None,
    );

    let mut warmup_branch = snapshot.clone();
    let warmup_res = sandbox
        .simulate_candidate(&mut warmup_branch, candidate_tx.clone())
        .expect("warmup candidate must succeed");
    assert!(warmup_res.success);

    // 9. Benchmark candidate evaluation latency on warm in-memory snapshot branches
    let iterations = 20;
    let bench_start = Instant::now();

    for _ in 0..iterations {
        let mut branch = snapshot.clone();
        let res = sandbox
            .simulate_candidate(&mut branch, candidate_tx.clone())
            .expect("candidate simulation must succeed");
        assert!(res.success);
    }

    let elapsed = bench_start.elapsed();
    let avg_micros = elapsed.as_micros() as f64 / iterations as f64;

    let target_micros = if cfg!(debug_assertions) { 5000.0 } else { 500.0 };
    println!(
        "\n⚡ [FORK BENCHMARK] Evaluated {} post-victim candidate backruns on forked mainnet state in {:?} (Average: {:.2} µs / backrun, Target: < {:.0} µs)\n",
        iterations, elapsed, avg_micros, target_micros
    );

    assert!(
        avg_micros < target_micros,
        "Candidate backrun evaluation on forked state must complete in < {:.0} µs, was {:.2} µs",
        target_micros,
        avg_micros
    );

    Ok(())
}
