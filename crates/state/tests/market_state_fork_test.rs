//! Mainnet fork verification test for MarketState and MulticallLoader.
//!
//! Spawns an Anvil instance forked at pinned block 19,500,000, batch loads
//! canonical Uniswap V2, SushiSwap V2, and Uniswap V3 pools via Multicall3,
//! and asserts bit-for-bit parity against direct on-chain contract state queries.

use alloy::{
    node_bindings::Anvil,
    primitives::{Bytes, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    sol,
    sol_types::SolCall,
};
use eyre::Result;
use mev_state::{
    MarketState, MulticallLoader, SUSHISWAP_V2_WETH_USDC, UNISWAP_V2_WETH_USDC,
    UNISWAP_V3_WETH_USDC_500, USDC, WETH,
};
use mev_types::{PoolState, PoolVariant};
use std::path::PathBuf;

sol! {
    interface IUniswapV2PairDirect {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
        function token0() external view returns (address);
        function token1() external view returns (address);
    }

    interface IUniswapV3PoolDirect {
        function slot0() external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );
        function liquidity() external view returns (uint128);
        function tickSpacing() external view returns (int24);
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

#[tokio::test]
async fn test_market_state_multicall_fork_verification() -> Result<()> {
    let rpc_url = get_fork_rpc_url();
    let fork_block = 19_500_000u64;

    // 1. Spawn Anvil forked at pinned block 19,500,000
    let mut anvil_builder = Anvil::new()
        .fork(rpc_url)
        .fork_block_number(fork_block);

    if let Some(path) = find_anvil_path() {
        anvil_builder = anvil_builder.path(path);
    }

    let anvil = anvil_builder.spawn();
    let provider = ProviderBuilder::new().connect_http(anvil.endpoint().parse()?);

    // 2. Target canonical pools
    let v2_targets = vec![UNISWAP_V2_WETH_USDC, SUSHISWAP_V2_WETH_USDC];
    let v3_targets = vec![UNISWAP_V3_WETH_USDC_500];

    // 3. Batch load pools using MulticallLoader into MarketState
    let state = MarketState::new();
    let loader = MulticallLoader::default();

    loader
        .load_into_market_state(
            &provider,
            &state,
            &v2_targets,
            &v3_targets,
            16, // +-16 bitmap words around active tick
            None,
        )
        .await?;

    // 4. Assert all pools are registered in MarketState
    assert_eq!(state.len(), 3);

    // 5. Verify Uniswap V2 WETH/USDC state parity against direct on-chain contract query
    let (v2_desc, v2_pool_state) = state
        .get_pool(&UNISWAP_V2_WETH_USDC)
        .expect("Uniswap V2 WETH/USDC pool not found in MarketState");

    assert_eq!(v2_desc.address, UNISWAP_V2_WETH_USDC);
    assert_eq!(v2_desc.variant, PoolVariant::UniswapV2);
    assert!(v2_desc.contains_token(WETH));
    assert!(v2_desc.contains_token(USDC));

    let PoolState::V2(v2_cached) = v2_pool_state else {
        panic!("Expected V2 pool state");
    };

    // Direct on-chain query to Uniswap V2 contract
    let req = TransactionRequest::default()
        .to(UNISWAP_V2_WETH_USDC)
        .input(Bytes::from(IUniswapV2PairDirect::getReservesCall {}.abi_encode()).into());
    let raw_reserves = provider.call(req).await?;
    let direct_reserves =
        IUniswapV2PairDirect::getReservesCall::abi_decode_returns(&raw_reserves)?;

    assert_eq!(v2_cached.reserve0, direct_reserves.reserve0.to::<u128>());
    assert_eq!(v2_cached.reserve1, direct_reserves.reserve1.to::<u128>());
    assert!(v2_cached.reserve0 > 0);
    assert!(v2_cached.reserve1 > 0);

    // 6. Verify Uniswap V3 WETH/USDC (500 bps) state parity against direct on-chain query
    let (v3_desc, v3_pool_state) = state
        .get_pool(&UNISWAP_V3_WETH_USDC_500)
        .expect("Uniswap V3 WETH/USDC 500 pool not found in MarketState");

    assert_eq!(v3_desc.address, UNISWAP_V3_WETH_USDC_500);
    assert_eq!(v3_desc.variant, PoolVariant::UniswapV3);
    assert_eq!(v3_desc.fee_pips, 500);

    let PoolState::V3(v3_cached) = v3_pool_state else {
        panic!("Expected V3 pool state");
    };

    // Direct on-chain query to Uniswap V3 contract
    let req_slot0 = TransactionRequest::default()
        .to(UNISWAP_V3_WETH_USDC_500)
        .input(Bytes::from(IUniswapV3PoolDirect::slot0Call {}.abi_encode()).into());
    let raw_slot0 = provider.call(req_slot0).await?;
    let direct_slot0 = IUniswapV3PoolDirect::slot0Call::abi_decode_returns(&raw_slot0)?;

    let req_liq = TransactionRequest::default()
        .to(UNISWAP_V3_WETH_USDC_500)
        .input(Bytes::from(IUniswapV3PoolDirect::liquidityCall {}.abi_encode()).into());
    let raw_liq = provider.call(req_liq).await?;
    let direct_liq = IUniswapV3PoolDirect::liquidityCall::abi_decode_returns(&raw_liq)?;

    assert_eq!(v3_cached.sqrt_price_x96, U256::from(direct_slot0.sqrtPriceX96));
    assert_eq!(v3_cached.tick, direct_slot0.tick.as_i32());
    assert_eq!(v3_cached.liquidity, direct_liq);
    assert_eq!(v3_cached.tick_spacing, 10);
    assert!(!v3_cached.tick_bitmap.is_empty(), "Expected initialized tick bitmap words");

    // 7. Verify secondary token-pair lookup index
    let weth_usdc_pools = state.get_pools_for_pair(WETH, USDC);
    assert_eq!(weth_usdc_pools.len(), 3);
    assert!(weth_usdc_pools.contains(&UNISWAP_V2_WETH_USDC));
    assert!(weth_usdc_pools.contains(&SUSHISWAP_V2_WETH_USDC));
    assert!(weth_usdc_pools.contains(&UNISWAP_V3_WETH_USDC_500));

    // Verify reverse order lookup
    let usdc_weth_pools = state.get_pools_for_pair(USDC, WETH);
    assert_eq!(usdc_weth_pools, weth_usdc_pools);

    Ok(())
}
