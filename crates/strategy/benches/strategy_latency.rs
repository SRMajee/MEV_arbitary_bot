//! Strategy latency micro-benchmarks comparing Phase 1 Runtime DFS + Address Lookups
//! vs Phase 2.2 Pre-computed O(1) CycleTable + HotPoolState Slice Sizing.

use alloy::primitives::{address, U256};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mev_state::{MarketState, DAI, USDC, USDT, WETH};
use mev_strategy::{ProfitEvaluator, StrategyEngine};
use mev_types::{PoolDescriptor, PoolState, PoolVariant, V2PoolState, V3PoolState};
use std::collections::BTreeMap;

fn bench_strategy_evaluation(c: &mut Criterion) {
    let mut group = c.benchmark_group("strategy_latency");

    let market_state = MarketState::new();

    let pool_univ2 = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
    let pool_sushiv2 = address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0");
    let pool_univ3 = address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640");
    let pool_tri_v2 = address!("06da0fd433C1A5d7a4faa01111c044910A184553");

    let desc_uni = PoolDescriptor::new(pool_univ2, PoolVariant::UniswapV2, WETH, USDC, 30);
    let desc_sushi = PoolDescriptor::new(pool_sushiv2, PoolVariant::UniswapV2, WETH, USDC, 30);
    let desc_univ3 = PoolDescriptor::new(pool_univ3, PoolVariant::UniswapV3, WETH, USDC, 500);
    let desc_tri = PoolDescriptor::new(pool_tri_v2, PoolVariant::UniswapV2, USDC, USDT, 30);

    // Uni V2: 1 ETH = 2500 USDC
    let state_uni = PoolState::V2(V2PoolState {
        reserve0: 25_000_000 * 10u128.pow(6),
        reserve1: 10_000 * 10u128.pow(18),
        block_timestamp_last: 1700000000,
    });
    // Sushi V2: 1 ETH = 2400 USDC (arbitrage opportunity)
    let state_sushi = PoolState::V2(V2PoolState {
        reserve0: 24_000_000 * 10u128.pow(6),
        reserve1: 10_000 * 10u128.pow(18),
        block_timestamp_last: 1700000000,
    });
    // Uni V3
    let state_univ3 = PoolState::V3(V3PoolState {
        sqrt_price_x96: U256::from(1583255271842000000000000000000u128),
        tick: 200000,
        liquidity: 50_000_000_000_000_000u128,
        tick_spacing: 10,
        tick_bitmap: BTreeMap::new(),
    });
    // Tri V2: USDC/USDT
    let state_tri = PoolState::V2(V2PoolState {
        reserve0: 10_000_000 * 10u128.pow(6),
        reserve1: 10_000_000 * 10u128.pow(6),
        block_timestamp_last: 1700000000,
    });

    market_state.update_pool(desc_uni, state_uni);
    market_state.update_pool(desc_sushi, state_sushi);
    market_state.update_pool(desc_univ3, state_univ3);
    market_state.update_pool(desc_tri, state_tri);

    let evaluator = ProfitEvaluator::new(U256::ZERO, 1000, 70_000);
    let min_trade = U256::from(100_000_000_000_000_000u64); // 0.1 ETH
    let max_trade = U256::from(10_000_000_000_000_000_000u128); // 10 ETH

    let mut engine = StrategyEngine::new(
        WETH,
        vec![USDC, USDT, DAI],
        evaluator,
        min_trade,
        max_trade,
    );
    engine.rebuild_cycle_table(&market_state);

    let touched_pool_addr = pool_univ2;
    let pool_id_uni = market_state.get_pool_id(&touched_pool_addr).unwrap();
    let base_fee_wei = 30_000_000_000u64;

    // Benchmark Phase 1: Full Runtime DFS + Address Lookups + Sizing
    group.bench_function("phase1_runtime_dfs_and_address_sizing", |b| {
        b.iter(|| {
            let opps = engine.evaluate_touched_pool(
                black_box(&market_state),
                black_box(touched_pool_addr),
                black_box(base_fee_wei),
            );
            black_box(opps);
        })
    });

    // Benchmark Phase 2.2: O(1) Pre-computed CycleTable Candidate Lookup
    group.bench_function("phase2_cycle_table_lookup_o1", |b| {
        b.iter(|| {
            let cycles = engine.cycle_table.get_cycles_touching_pool(black_box(pool_id_uni));
            black_box(cycles);
        })
    });

    // Benchmark Phase 2.2: Single 2-Hop HotCycle Sizing (Analytical + Slice Reads)
    let hot_guard = market_state.load_hot();
    let hot_cycle = engine.cycle_table.get_cycle(0).unwrap();
    group.bench_function("phase2_single_2hop_cycle_sizing_hot", |b| {
        b.iter(|| {
            let res = mev_strategy::find_optimal_input_hot(
                black_box(hot_cycle),
                black_box(&hot_guard.pools),
                black_box(min_trade),
                black_box(max_trade),
            );
            black_box(res);
        })
    });

    // Benchmark Phase 2.2: Single HotCycle Simulation (Pure Integer Slice Math)
    let test_amount = U256::from(1_000_000_000_000_000_000u64); // 1 ETH
    group.bench_function("phase2_single_hot_cycle_simulation", |b| {
        b.iter(|| {
            let res = mev_strategy::simulate_hot_cycle(
                black_box(hot_cycle),
                black_box(test_amount),
                black_box(&hot_guard.pools),
            );
            black_box(res);
        })
    });

    // Benchmark Phase 2.2: Full Touched Pool Hot Evaluation across All Candidates
    group.bench_function("phase2_cycle_table_and_hot_slice_sizing", |b| {
        b.iter(|| {
            let guard = market_state.load_hot();
            let opps = engine.evaluate_touched_pool_hot(
                black_box(&guard),
                black_box(pool_id_uni),
                black_box(base_fee_wei),
            );
            black_box(opps);
        })
    });

    group.finish();
}


criterion_group!(benches, bench_strategy_evaluation);
criterion_main!(benches);
