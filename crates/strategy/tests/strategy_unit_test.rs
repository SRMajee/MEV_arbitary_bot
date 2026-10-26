//! Unit test suite for AMM pricing, analytical sizing, and Golden Section Search.

use alloy::primitives::{address, U256};
use mev_state::MarketState;
use mev_strategy::{
    compute_swap_step_v3, find_optimal_input, get_amount_out_v2, get_sqrt_ratio_at_tick,
    golden_section_search, optimal_input_v2_analytical, simulate_cycle, ProfitEvaluator,
    StrategyEngine, Q96,
};
use mev_types::{Cycle, CycleHop, PoolDescriptor, PoolState, PoolVariant, V2PoolState};
use std::time::Instant;

#[test]
fn test_uniswap_v2_exact_amount_out() {
    // Pool with 100 WETH and 300,000 USDC
    let reserve_in = U256::from(100) * U256::from(10).pow(U256::from(18));
    let reserve_out = U256::from(300_000) * U256::from(10).pow(U256::from(6));
    let amount_in = U256::from(1) * U256::from(10).pow(U256::from(18)); // 1 WETH

    // Expected: (1e18 * 997 * 300,000e6) / (100e18 * 1000 + 1e18 * 997)
    // = (997 * 300,000,000,000) / (100,000 + 997) = 299,100,000,000 / 100,997 = 2,961,474,103 (2,961.47 USDC)
    let amount_out = get_amount_out_v2(amount_in, reserve_in, reserve_out, 3000);
    assert_eq!(amount_out, U256::from(2_961_474_103u64));
}

#[test]
fn test_uniswap_v3_sqrt_ratio_and_swap_step() {
    // Tick 0 must equal exactly 2^96
    let sqrt_0 = get_sqrt_ratio_at_tick(0);
    assert_eq!(sqrt_0, Q96, "tick 0 sqrt ratio must equal 2^96");

    // Positive tick has price > 1.0 -> sqrt > 2^96
    let sqrt_100 = get_sqrt_ratio_at_tick(100);
    assert!(sqrt_100 > Q96);

    // Negative tick has price < 1.0 -> sqrt < 2^96
    let sqrt_neg_100 = get_sqrt_ratio_at_tick(-100);
    assert!(sqrt_neg_100 < Q96);

    // Test compute_swap_step_v3
    let liquidity = 10_000_000_000_000_000_000u128; // 10e18
    let amount_remaining = U256::from(10).pow(U256::from(18)); // 1 token
    let fee_pips = 3000;

    let (next_sqrt, amount_in, amount_out) = compute_swap_step_v3(
        sqrt_0,
        sqrt_neg_100,
        liquidity,
        amount_remaining,
        fee_pips,
    );

    assert!(next_sqrt <= sqrt_0);
    assert!(amount_in > U256::ZERO);
    assert!(amount_out > U256::ZERO);
}

#[test]
fn test_analytical_vs_golden_section_parity() {
    // Setup an imbalanced 2-hop V2 cycle:
    // Pool 1 (Uniswap V2): 100 WETH, 300,000 USDC (Price: 3,000 USDC/ETH)
    // Pool 2 (SushiSwap V2): 120 WETH, 315,000 USDC (Price: 2,625 USDC/ETH)
    // Arbitrage: Buy cheap on SushiSwap (ETH -> USDC), sell high on Uniswap (USDC -> ETH)
    // Or: Buy USDC on Uniswap (ETH -> USDC), sell USDC on SushiSwap (USDC -> ETH)
    let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");

    let p1_addr = address!("1111111111111111111111111111111111111111");
    let p2_addr = address!("2222222222222222222222222222222222222222");

    let p1 = PoolDescriptor::new(p1_addr, PoolVariant::UniswapV2, weth, usdc, 3000);
    let p2 = PoolDescriptor::new(p2_addr, PoolVariant::UniswapV2, weth, usdc, 3000);

    // Pool 1: 300,000 USDC and 100 WETH (Price: 3,000 USDC/ETH)
    let state1 = V2PoolState {
        reserve0: 300_000 * 10u128.pow(6),  // token0: USDC
        reserve1: 100 * 10u128.pow(18),      // token1: WETH
        block_timestamp_last: 0,
    };

    // Pool 2: 315,000 USDC and 120 WETH (Price: 2,625 USDC/ETH)
    let state2 = V2PoolState {
        reserve0: 315_000 * 10u128.pow(6),  // token0: USDC
        reserve1: 120 * 10u128.pow(18),      // token1: WETH
        block_timestamp_last: 0,
    };

    let market_state = MarketState::new();
    market_state.update_pool(p1.clone(), PoolState::V2(state1));
    market_state.update_pool(p2.clone(), PoolState::V2(state2));

    // Hop 1: Pool 1 (WETH -> USDC)
    // Hop 2: Pool 2 (USDC -> WETH)
    let cycle = Cycle {
        base_token: weth,
        hops: vec![
            CycleHop { pool: p1, token_in: weth, token_out: usdc },
            CycleHop { pool: p2, token_in: usdc, token_out: weth },
        ],
    };
    assert!(cycle.validate());

    // 1. Analytical optimal input
    let analytical_x = optimal_input_v2_analytical(
        U256::from(state1.reserve1), // ax (WETH)
        U256::from(state1.reserve0), // ay (USDC)
        U256::from(state2.reserve0), // by (USDC)
        U256::from(state2.reserve1), // bx (WETH)
        3000,
        3000,
    );

    assert!(analytical_x > U256::ZERO, "analytical optimal input must be positive");

    // 2. Golden Section Search
    let min_in = U256::from(10).pow(U256::from(16)); // 0.01 WETH
    let max_in = U256::from(20) * U256::from(10).pow(U256::from(18)); // 20 WETH
    let (golden_x, golden_out) = golden_section_search(&cycle, &market_state, min_in, max_in, 30);
    let golden_profit = golden_out.saturating_sub(golden_x);

    let analytical_out = simulate_cycle(&cycle, analytical_x, &market_state);
    let analytical_profit = analytical_out.saturating_sub(analytical_x);

    let diff = if analytical_x > golden_x {
        analytical_x - golden_x
    } else {
        golden_x - analytical_x
    };

    let relative_error = (diff.to::<u128>() as f64) / (golden_x.to::<u128>() as f64);

    println!(
        "\n⚡ [PARITY CHECK] Analytical x*: {} wei | Golden Section x*: {} wei | Relative Error: {:.5}%\n",
        analytical_x, golden_x, relative_error * 100.0
    );
    println!(
        "⚡ Analytical Profit: {} wei | Golden Profit: {} wei\n",
        analytical_profit, golden_profit
    );

    // Must match to within 0.01% (0.0001)
    assert!(
        relative_error < 0.0001,
        "Analytical optimal sizing must match Golden Section Search within 0.01%, error was {:.4}%",
        relative_error * 100.0
    );
    assert!(analytical_profit > U256::ZERO);
}

#[test]
fn test_3pool_triangle_arbitrage_discovery() {
    let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    let dai = address!("6B175474E89094C44Da98b954EedeAC495271d0F");

    let p1_addr = address!("1111111111111111111111111111111111111111");
    let p2_addr = address!("2222222222222222222222222222222222222222");
    let p3_addr = address!("3333333333333333333333333333333333333333");

    // Triangle: WETH -> USDC -> DAI -> WETH
    let p1 = PoolDescriptor::new(p1_addr, PoolVariant::UniswapV2, weth, usdc, 3000);
    let p2 = PoolDescriptor::new(p2_addr, PoolVariant::UniswapV2, usdc, dai, 3000);
    let p3 = PoolDescriptor::new(p3_addr, PoolVariant::UniswapV2, dai, weth, 3000);

    // Pool 1: 100 WETH, 300,000 USDC (Price: 3,000)
    // Pool 1: 300,000 USDC, 100 WETH (token0: USDC, token1: WETH)
    let s1 = V2PoolState {
        reserve0: 300_000 * 10u128.pow(6),
        reserve1: 100 * 10u128.pow(18),
        block_timestamp_last: 0,
    };
    // Pool 2 (imbalanced): 600,000 DAI, 500,000 USDC (token0: DAI, token1: USDC, 1 USDC = 1.20 DAI)
    let s2 = V2PoolState {
        reserve0: 600_000 * 10u128.pow(18),
        reserve1: 500_000 * 10u128.pow(6),
        block_timestamp_last: 0,
    };
    // Pool 3: 300,000 DAI, 100 WETH (token0: DAI, token1: WETH)
    let s3 = V2PoolState {
        reserve0: 300_000 * 10u128.pow(18),
        reserve1: 100 * 10u128.pow(18),
        block_timestamp_last: 0,
    };

    let market_state = MarketState::new();
    market_state.update_pool(p1, PoolState::V2(s1));
    market_state.update_pool(p2, PoolState::V2(s2));
    market_state.update_pool(p3, PoolState::V2(s3));

    let engine = StrategyEngine::new(
        weth,
        vec![weth, usdc, dai],
        ProfitEvaluator::default(),
        U256::from(10).pow(U256::from(16)), // 0.01 WETH min
        U256::from(20) * U256::from(10).pow(U256::from(18)), // 20 WETH max
    );

    let opportunities = engine.evaluate_touched_pool(&market_state, p2_addr, 20_000_000_000);
    assert!(!opportunities.is_empty(), "must find profitable 3-hop triangle arbitrage");

    let top_opp = &opportunities[0];
    assert!(top_opp.net_profit > U256::ZERO);
    assert_eq!(top_opp.cycle.hops.len(), 3);
    assert_eq!(top_opp.execution_params.hops.len(), 3);
    println!(
        "\n⚡ Discovered 3-hop triangle opportunity: optimal input = {} wei, net profit = {} wei\n",
        top_opp.optimal_input, top_opp.net_profit
    );
}

#[test]
fn test_strategy_optimization_latency_benchmark() {
    let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");

    let p1_addr = address!("1111111111111111111111111111111111111111");
    let p2_addr = address!("2222222222222222222222222222222222222222");

    let p1 = PoolDescriptor::new(p1_addr, PoolVariant::UniswapV2, weth, usdc, 3000);
    let p2 = PoolDescriptor::new(p2_addr, PoolVariant::UniswapV2, weth, usdc, 3000);

    let s1 = V2PoolState {
        reserve0: 300_000 * 10u128.pow(6),
        reserve1: 100 * 10u128.pow(18),
        block_timestamp_last: 0,
    };
    let s2 = V2PoolState {
        reserve0: 315_000 * 10u128.pow(6),
        reserve1: 120 * 10u128.pow(18),
        block_timestamp_last: 0,
    };

    let market_state = MarketState::new();
    market_state.update_pool(p1.clone(), PoolState::V2(s1));
    market_state.update_pool(p2.clone(), PoolState::V2(s2));

    let cycle = Cycle {
        base_token: weth,
        hops: vec![
            CycleHop { pool: p1, token_in: weth, token_out: usdc },
            CycleHop { pool: p2, token_in: usdc, token_out: weth },
        ],
    };

    let iterations = 1000;
    let start = Instant::now();

    for _ in 0..iterations {
        let (opt_in, _) = find_optimal_input(
            &cycle,
            &market_state,
            U256::from(10).pow(U256::from(16)),
            U256::from(50) * U256::from(10).pow(U256::from(18)),
        );
        assert!(opt_in > U256::ZERO);
    }

    let elapsed = start.elapsed();
    let avg_micros = elapsed.as_micros() as f64 / iterations as f64;

    let target_micros = if cfg!(debug_assertions) { 150.0 } else { 10.0 };
    println!(
        "\n⚡ [STRATEGY BENCHMARK] Optimized {} cycles in {:?} (Average: {:.3} µs / cycle, Target: < {:.1} µs)\n",
        iterations, elapsed, avg_micros, target_micros
    );

    assert!(
        avg_micros < target_micros,
        "Optimal sizing must complete in < {:.1} µs per cycle, was {:.3} µs",
        target_micros,
        avg_micros
    );
}
