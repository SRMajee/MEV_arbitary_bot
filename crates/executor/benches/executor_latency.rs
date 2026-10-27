//! Micro-benchmarks for calldata encoding, atomic nonce management, and bundle signing latency.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use alloy::primitives::{address, Bytes, Uint, U256};
use alloy::sol_types::SolCall;

use mev_executor::{
    AtomicNonceManager, BundleSigner, CompactHop, FastBundleSigner, MevExecutorContract,
    RawCalldataBuilder,
};
use mev_executor::MevExecutor::{ExecutionParams, Hop};

fn setup_test_cycle_params() -> (ExecutionParams, Vec<CompactHop>) {
    let pool0 = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
    let pool1 = address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0");
    let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");

    let hop0 = Hop {
        protocol: 0,
        pool: pool0,
        tokenIn: weth,
        tokenOut: usdc,
        fee: Uint::from(0u32),
        sqrtPriceLimitX96: Uint::ZERO,
    };

    let hop1 = Hop {
        protocol: 0,
        pool: pool1,
        tokenIn: usdc,
        tokenOut: weth,
        fee: Uint::from(0u32),
        sqrtPriceLimitX96: Uint::ZERO,
    };

    let params = ExecutionParams {
        amountIn: U256::from(3_000_000_000_000_000_000u64), // 3 WETH
        minProfit: U256::from(10_000_000_000_000_000u64),   // 0.01 WETH
        bribeBps: 9000,
        hops: vec![hop0, hop1],
    };

    let compact_hops = vec![
        CompactHop::new(pool0, weth, usdc, 0, 0, false),
        CompactHop::new(pool1, usdc, weth, 0, 0, true),
    ];

    (params, compact_hops)
}

fn bench_calldata_encoding(c: &mut Criterion) {
    let mut group = c.benchmark_group("calldata_encoding");
    let (params, compact_hops) = setup_test_cycle_params();

    // 1. Phase 1 Dynamic Heap SolCall::abi_encode
    group.bench_function("phase1_dynamic_solcall_abi_encode", |b| {
        b.iter(|| {
            let call = MevExecutorContract::executeArbitrageFlashCall {
                params: black_box(params.clone()),
            };
            black_box(call.abi_encode())
        });
    });

    // 2. Phase 2.4 Stack-allocated RawCalldataBuilder (Standard ABI)
    group.bench_function("phase2_stack_raw_calldata_builder_abi", |b| {
        b.iter(|| {
            let mut builder = RawCalldataBuilder::new();
            black_box(builder.encode_flash_abi(black_box(&params)).len())
        });
    });

    // 3. Phase 2.4 Ultra-compact Bit-packed Yul Payload
    group.bench_function("phase2_compact_yul_calldata_builder", |b| {
        b.iter(|| {
            let mut builder = RawCalldataBuilder::new();
            black_box(
                builder
                    .encode_compact_packed(
                        black_box(1),
                        black_box(9000),
                        black_box(3_000_000_000_000_000_000u128),
                        black_box(10_000_000_000_000_000u128),
                        black_box(&compact_hops),
                    )
                    .len(),
            )
        });
    });

    group.finish();
}

fn bench_nonce_manager(c: &mut Criterion) {
    let mut group = c.benchmark_group("nonce_management");
    let manager = AtomicNonceManager::new(100);

    // Read latency (~1 ns)
    group.bench_function("atomic_nonce_peek_acquire", |b| {
        b.iter(|| black_box(manager.current_nonce()));
    });

    // Increment latency (~3 ns)
    group.bench_function("atomic_nonce_fetch_and_increment", |b| {
        b.iter(|| black_box(manager.fetch_and_increment()));
    });

    group.finish();
}

fn bench_bundle_signing(c: &mut Criterion) {
    let mut group = c.benchmark_group("bundle_signing");

    let auth_key = "0x0000000000000000000000000000000000000000000000000000000000000001";
    let tx_key = "0x0000000000000000000000000000000000000000000000000000000000000002";
    let signer = BundleSigner::from_hex_keys(auth_key, tx_key, 1).unwrap();
    let fast_signer = FastBundleSigner::new(signer.clone());

    let victim_tx = Bytes::from_static(&[0x02, 0xf8, 0x60, 0x01]);
    let executor_contract = address!("8888888888888888888888888888888888888888");
    let dummy_calldata = [0x12, 0x34, 0x56, 0x78];

    // Phase 1 bundle construction
    group.bench_function("phase1_construct_signed_bundle", |b| {
        b.iter(|| {
            black_box(
                signer
                    .construct_signed_bundle(
                        black_box(victim_tx.clone()),
                        black_box(1),
                        black_box(executor_contract),
                        black_box(Bytes::copy_from_slice(&dummy_calldata)),
                        black_box(250_000),
                        black_box(20_000_000_000),
                        black_box(1_000_000_000),
                        black_box(19_500_001),
                    )
                    .unwrap(),
            )
        });
    });

    // Phase 2.4 FastBundleSigner + single-sign payload preparation
    group.bench_function("phase2_construct_and_prepare_payload", |b| {
        b.iter(|| {
            black_box(
                fast_signer
                    .construct_and_prepare_payload(
                        black_box(&victim_tx),
                        black_box(1),
                        black_box(executor_contract),
                        black_box(&dummy_calldata),
                        black_box(250_000),
                        black_box(20_000_000_000),
                        black_box(1_000_000_000),
                        black_box(19_500_001),
                    )
                    .unwrap(),
            )
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_calldata_encoding,
    bench_nonce_manager,
    bench_bundle_signing
);
criterion_main!(benches);
