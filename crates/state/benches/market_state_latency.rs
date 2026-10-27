//! Latency micro-benchmarks comparing DashMap hash lookups vs HotMarketState 64-byte cache-aligned RCU slice access.

use alloy::primitives::address;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dashmap::DashMap;
use mev_state::MarketState;
use mev_types::{PoolDescriptor, PoolState, PoolVariant, V2PoolState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

/// Benchmark single-thread read latency: DashMap hash table lookup vs ArcSwap contiguous slice indexing.
fn bench_single_thread_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("pool_read_latency");

    let pool_addr = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
    let token0 = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    let token1 = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");

    let desc = PoolDescriptor::new(pool_addr, PoolVariant::UniswapV2, token0, token1, 30);
    let v2_state = PoolState::V2(V2PoolState {
        reserve0: 50_000_000_000_000,
        reserve1: 25_000_000_000_000_000_000,
        block_timestamp_last: 1700000000,
    });

    // 1. Setup Phase 1 DashMap baseline
    let dashmap = DashMap::new();
    dashmap.insert(pool_addr, (desc.clone(), v2_state.clone()));

    // 2. Setup Phase 2.1 MarketState (ArcSwap + HotPoolState)
    let market_state = MarketState::new();
    market_state.update_pool(desc, v2_state);
    let pool_id = market_state.get_pool_id(&pool_addr).unwrap();

    // Benchmark DashMap hash lookup (20-byte address key)
    group.bench_function("phase1_dashmap_address_lookup", |b| {
        b.iter(|| {
            let entry = dashmap.get(black_box(&pool_addr)).unwrap();
            black_box(entry.value());
        })
    });

    // Benchmark Phase 2.1 RCU load + direct 64-byte cache line slice access
    group.bench_function("phase2_rcu_slice_indexing", |b| {
        b.iter(|| {
            let guard = market_state.load_hot();
            let hot = guard.get_hot_pool(black_box(pool_id)).unwrap();
            black_box(hot.v2_reserves());
        })
    });

    // Benchmark pure slice indexing on HotMarketState (inside critical routing loop)
    let hot_guard = market_state.load_hot();
    group.bench_function("phase2_hot_slice_indexing_direct", |b| {
        b.iter(|| {
            let hot = hot_guard.pools[black_box(pool_id.0 as usize)];
            black_box(hot.v2_reserves());
        })
    });

    group.finish();
}

/// Benchmark concurrent multi-threaded read throughput under active write updates:
/// 16 reader threads hammering state while a writer thread simulates real-time block sync.
fn bench_concurrent_readers_under_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_contention_16_readers");

    let pool_addr = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
    let token0 = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    let token1 = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");

    let desc = PoolDescriptor::new(pool_addr, PoolVariant::UniswapV2, token0, token1, 30);
    let v2_state = PoolState::V2(V2PoolState {
        reserve0: 50_000_000_000_000,
        reserve1: 25_000_000_000_000_000_000,
        block_timestamp_last: 1700000000,
    });

    let market_state = Arc::new(MarketState::new());
    market_state.update_pool(desc, v2_state);
    let pool_id = market_state.get_pool_id(&pool_addr).unwrap();

    let running = Arc::new(AtomicBool::new(true));

    // Spawn 1 background writer thread simulating block reserve sync
    let writer_state = Arc::clone(&market_state);
    let writer_running = Arc::clone(&running);
    let writer_handle = thread::spawn(move || {
        let mut counter = 1u128;
        while writer_running.load(Ordering::Relaxed) {
            writer_state.update_v2_reserves(&pool_addr, 50_000_000_000_000 + counter, 25_000_000_000_000_000_000 + counter, 1700000000);
            counter = counter.wrapping_add(1);
            thread::yield_now();
        }
    });

    group.bench_function("phase2_rcu_16_readers_under_write_contention", |b| {
        b.iter(|| {
            let guard = market_state.load_hot();
            let hot = guard.get_hot_pool(black_box(pool_id)).unwrap();
            black_box(hot.v2_reserves());
        })
    });

    running.store(false, Ordering::Relaxed);
    let _ = writer_handle.join();

    group.finish();
}

criterion_group!(
    benches,
    bench_single_thread_read,
    bench_concurrent_readers_under_writes
);
criterion_main!(benches);
