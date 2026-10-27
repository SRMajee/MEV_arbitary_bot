use criterion::{black_box, criterion_group, criterion_main, Criterion};
use alloy::primitives::{address, Address, Bytes, B256, U256};
use revm::{bytecode::Bytecode, state::AccountInfo};
use std::sync::Arc;

use mev_collector::decoder::CalldataDecoder;
use mev_executor::{
    AtomicNonceManager, BundleSigner, CompactHop, FastBundleSigner, RawCalldataBuilder,
};
use mev_simulator::{
    build_call_tx, ForkedSandbox, PrewarmedDB, SimulationRunner,
};
use mev_state::{MarketState, USDC, WETH};
use mev_strategy::{ProfitEvaluator, StrategyEngine};
use mev_types::{PoolDescriptor, PoolState, PoolVariant, V2PoolState};

const UNISWAP_V2_WETH_USDC: Address = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
const SUSHISWAP_V2_WETH_USDC: Address = address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0");
const UNISWAP_V2_ROUTER02: Address = address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
const EXECUTOR_ADDR: Address = address!("8888888888888888888888888888888888888888");

struct BenchmarkFixture {
    market_state: Arc<MarketState>,
    strategy_engine: StrategyEngine,
    prewarmed_db: PrewarmedDB,
    runner: SimulationRunner,
    victim_raw_tx: Bytes,
    victim_calldata: Bytes,
    fast_signer: FastBundleSigner,
    nonce_manager: Arc<AtomicNonceManager>,
}

fn setup_benchmark_fixture() -> BenchmarkFixture {
    let market_state = Arc::new(MarketState::new());

    let desc_uni = PoolDescriptor::new(UNISWAP_V2_WETH_USDC, PoolVariant::UniswapV2, WETH, USDC, 30);
    let desc_sushi = PoolDescriptor::new(SUSHISWAP_V2_WETH_USDC, PoolVariant::UniswapV2, WETH, USDC, 30);

    // Uni: 1 ETH = 2500 USDC
    let state_uni = PoolState::V2(V2PoolState {
        reserve0: 25_000_000 * 10u128.pow(6),
        reserve1: 10_000 * 10u128.pow(18),
        block_timestamp_last: 1700000000,
    });
    // Sushi: 1 ETH = 2000 USDC (whale dump dislocation)
    let state_sushi = PoolState::V2(V2PoolState {
        reserve0: 20_000_000 * 10u128.pow(6),
        reserve1: 10_000 * 10u128.pow(18),
        block_timestamp_last: 1700000000,
    });

    market_state.update_pool(desc_uni, state_uni);
    market_state.update_pool(desc_sushi, state_sushi);

    let evaluator = ProfitEvaluator::new(
        U256::from(10).pow(U256::from(15)),
        9000,
        70_000,
    );
    let min_trade = U256::from(100_000_000_000_000_000u64);
    let max_trade = U256::from(50) * U256::from(10).pow(U256::from(18));

    let mut strategy_engine = StrategyEngine::new(
        WETH,
        vec![USDC],
        evaluator,
        min_trade,
        max_trade,
    );
    strategy_engine.rebuild_cycle_table(&market_state);

    // Setup zero-RPC PrewarmedDB
    let mut prewarmed_db = PrewarmedDB::new();
    let runner = SimulationRunner::new(19_500_000, 1711000000)
        .with_coinbase(address!("000000000000000000000000000000000000bEEF"))
        .with_basefee(20_000_000_000);

    // Fund executor and searcher accounts
    let searcher = address!("2B5AD5c4795c026514f8317c7a215E218DcCD6cF");
    let initial_balance = U256::from(100).pow(U256::from(18));
    prewarmed_db.insert_account(
        searcher,
        AccountInfo::new(initial_balance, 0, B256::ZERO, Bytecode::default()),
    );

    // Seed MevExecutor bytecode
    let executor_code = Bytecode::new_raw(Bytes::from_static(&[0x00; 128]));
    prewarmed_db.insert_account(
        EXECUTOR_ADDR,
        AccountInfo::new(initial_balance, 0, executor_code.hash_slow(), executor_code),
    );

    // Seed pool storage slots
    prewarmed_db.sync_from_hot_state(&market_state.load_hot());

    // Setup victim calldata
    let victim_calldata = Bytes::from_static(&[
        0x38, 0xed, 0x17, 0x39, // swapExactTokensForTokens
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x00,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xde, 0xad, 0xbe, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x77, 0x35, 0x94, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0x2a, 0xaa, 0x39, 0xb2, 0x23, 0xfe, 0x8d, 0x0a, 0x0e, 0x5c, 0x4f, 0x27, 0xea, 0xd9, 0x08, 0x3c, 0x75, 0x6c, 0xc2,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xa0, 0xb8, 0x69, 0x91, 0xc6, 0x21, 0x8b, 0x36, 0xc1, 0xd1, 0x9d, 0x4a, 0x2e, 0x9e, 0xb0, 0xce, 0x36, 0x06, 0xeb, 0x48,
    ]);

    let victim_raw_tx = Bytes::from_static(&[0x02, 0xf8, 0x70, 0x01, 0x01, 0x80]);

    let auth_key = "0x0000000000000000000000000000000000000000000000000000000000000001";
    let tx_key = "0x0000000000000000000000000000000000000000000000000000000000000002";
    let bundle_signer = BundleSigner::from_hex_keys(auth_key, tx_key, 1).unwrap();
    let fast_signer = FastBundleSigner::new(bundle_signer);
    let nonce_manager = Arc::new(AtomicNonceManager::new(5));

    BenchmarkFixture {
        market_state,
        strategy_engine,
        prewarmed_db,
        runner,
        victim_raw_tx,
        victim_calldata,
        fast_signer,
        nonce_manager,
    }
}

fn bench_tick_to_trade_stages(c: &mut Criterion) {
    let fix = setup_benchmark_fixture();
    let mut group = c.benchmark_group("tick_to_trade_stages");

    // Stage 1: Zero-copy calldata decode + Touched PoolId resolution
    group.bench_function("stage1_decode_and_pool_id_ns", |b| {
        b.iter(|| {
            let swaps = CalldataDecoder::decode(
                B256::ZERO,
                black_box(UNISWAP_V2_ROUTER02),
                U256::ZERO,
                black_box(&fix.victim_calldata),
            );
            let pool_id = fix.market_state.get_pool_id(black_box(&UNISWAP_V2_WETH_USDC));
            black_box((swaps.len(), pool_id))
        });
    });

    // Stage 2: Hot strategy evaluation via CycleTable (< 4 µs)
    let pool_id = fix.market_state.get_pool_id(&UNISWAP_V2_WETH_USDC).unwrap();
    group.bench_function("stage2_hot_cycle_eval_and_sizing_us", |b| {
        b.iter(|| {
            let hot_state = fix.market_state.load_hot();
            let opps = fix.strategy_engine.evaluate_touched_pool_hot(
                black_box(&hot_state),
                black_box(pool_id),
                black_box(20_000_000_000),
            );
            black_box(opps.len())
        });
    });

    // Stage 3: Zero-RPC PrewarmedDB EVM simulation verification (< 15 µs)
    let sandbox = ForkedSandbox::new(fix.prewarmed_db.clone(), fix.runner.clone());
    let compact_hops = [
        CompactHop::new(UNISWAP_V2_WETH_USDC, WETH, USDC, 0, 0, false),
        CompactHop::new(SUSHISWAP_V2_WETH_USDC, USDC, WETH, 0, 0, true),
    ];
    let mut builder = RawCalldataBuilder::new();
    let calldata_slice = builder.encode_compact_packed(
        0,
        9000,
        2_000_000_000_000_000_000u128,
        10_000_000_000_000_000u128,
        &compact_hops,
    );
    let calldata = Bytes::copy_from_slice(calldata_slice);

    group.bench_function("stage3_zero_rpc_evm_verification_us", |b| {
        b.iter(|| {
            let mut snapshot = sandbox.snapshot();
            let tx = build_call_tx(
                black_box(fix.fast_signer.inner().tx_address()),
                black_box(EXECUTOR_ADDR),
                black_box(calldata.clone()),
                U256::ZERO,
                500_000,
                None,
            );
            let res = sandbox.simulate_candidate(&mut snapshot, tx).unwrap();
            black_box(res.success)
        });
    });

    // Stage 4: Zero-copy stack calldata encoding + atomic nonce peek (< 50 ns)
    group.bench_function("stage4_stack_calldata_and_nonce_ns", |b| {
        b.iter(|| {
            let mut bldr = RawCalldataBuilder::new();
            let slice = bldr.encode_compact_packed(
                black_box(0),
                black_box(9000),
                black_box(2_000_000_000_000_000_000u128),
                black_box(10_000_000_000_000_000u128),
                black_box(&compact_hops),
            );
            let nonce = fix.nonce_manager.current_nonce();
            black_box((slice.len(), nonce))
        });
    });

    group.finish();
}

fn bench_tick_to_trade_composite(c: &mut Criterion) {
    let fix = setup_benchmark_fixture();
    let mut group = c.benchmark_group("tick_to_trade_composite");
    let pool_id = fix.market_state.get_pool_id(&UNISWAP_V2_WETH_USDC).unwrap();
    let sandbox = ForkedSandbox::new(fix.prewarmed_db.clone(), fix.runner.clone());

    // Composite Benchmark A: Core decision pipeline (< 50 µs target!)
    // Victim decode -> Hot sizing -> Zero-RPC EVM check -> Stack-encoded Yul calldata
    group.bench_function("core_tick_to_trade_decision_us", |b| {
        b.iter(|| {
            // 1. Decode & Resolve PoolId
            let _swaps = CalldataDecoder::decode(
                B256::ZERO,
                UNISWAP_V2_ROUTER02,
                U256::ZERO,
                &fix.victim_calldata,
            );

            // 2. Hot Strategy Sizing via CycleTable
            let hot_state = fix.market_state.load_hot();
            let opps = fix.strategy_engine.evaluate_touched_pool_hot(
                &hot_state,
                pool_id,
                20_000_000_000,
            );
            let opp = &opps[0];

            // 3. Stack Calldata Encoding
            let mut builder = RawCalldataBuilder::new();
            let compact_hops: Vec<CompactHop> = opp
                .execution_params
                .hops
                .iter()
                .map(|h| {
                    CompactHop::new(
                        h.pool,
                        h.tokenIn,
                        h.tokenOut,
                        h.fee.to::<u32>(),
                        h.protocol,
                        h.tokenIn < h.tokenOut,
                    )
                })
                .collect();
            let calldata_slice = builder.encode_compact_packed(
                0,
                opp.execution_params.bribeBps,
                opp.optimal_input.to::<u128>(),
                opp.net_profit.to::<u128>(),
                &compact_hops,
            );
            let calldata = Bytes::copy_from_slice(calldata_slice);

            // 4. EVM Simulation Verification on PrewarmedDB
            let mut snapshot = sandbox.snapshot();
            let tx = build_call_tx(
                fix.fast_signer.inner().tx_address(),
                EXECUTOR_ADDR,
                calldata,
                U256::ZERO,
                500_000,
                None,
            );
            let res = sandbox.simulate_candidate(&mut snapshot, tx).unwrap();
            let nonce = fix.nonce_manager.current_nonce();

            black_box((res.success, nonce))
        });
    });

    // Composite Benchmark B: Full wire signed bundle (inclusive of dual ECDSA signatures)
    group.bench_function("full_wire_signed_bundle_us", |b| {
        b.iter(|| {
            let hot_state = fix.market_state.load_hot();
            let opps = fix.strategy_engine.evaluate_touched_pool_hot(
                &hot_state,
                pool_id,
                20_000_000_000,
            );
            let opp = &opps[0];

            let mut builder = RawCalldataBuilder::new();
            let compact_hops: Vec<CompactHop> = opp
                .execution_params
                .hops
                .iter()
                .map(|h| {
                    CompactHop::new(
                        h.pool,
                        h.tokenIn,
                        h.tokenOut,
                        h.fee.to::<u32>(),
                        h.protocol,
                        h.tokenIn < h.tokenOut,
                    )
                })
                .collect();
            let calldata_slice = builder.encode_compact_packed(
                0,
                opp.execution_params.bribeBps,
                opp.optimal_input.to::<u128>(),
                opp.net_profit.to::<u128>(),
                &compact_hops,
            );

            let nonce = fix.nonce_manager.current_nonce();
            let (_, _, payload) = fix
                .fast_signer
                .construct_and_prepare_payload(
                    &fix.victim_raw_tx,
                    nonce,
                    EXECUTOR_ADDR,
                    calldata_slice,
                    150_000,
                    20_000_000_000,
                    1_000_000_000,
                    19_500_001,
                )
                .unwrap();

            black_box(payload.body.len())
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_tick_to_trade_stages,
    bench_tick_to_trade_composite
);
criterion_main!(benches);
