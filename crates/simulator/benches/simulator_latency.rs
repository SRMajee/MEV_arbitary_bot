//! Latency micro-benchmarks measuring single storage slot lookups and full EVM transaction
//! simulations against zero-RPC PrewarmedDB (< 15 µs target).

use alloy::primitives::{address, bytes, Bytes, U256};
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mev_simulator::{
    build_call_tx, ForkedSandbox, PrewarmedDB, SimulationRunner,
};
use revm::{bytecode::Bytecode, database::DatabaseRef, state::AccountInfo};

fn bench_simulator_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("simulator_latency");

    // 1. Setup PrewarmedDB with pre-warmed storage slots and realistic contract bytecode
    let mut db = PrewarmedDB::new();
    let contract_addr = address!("1111111111111111111111111111111111111111");
    let caller_addr = address!("2222222222222222222222222222222222222222");

    // EVM bytecode simulating an AMM state check and storage update:
    // SLOAD(slot 8) -> PUSH1 1 -> ADD -> SSTORE(slot 8) -> RETURN(0, 32)
    // 6008 54 6001 01 6008 55 6020 6000 f3
    let code = bytes!("60085460010160085560206000f3");
    let bytecode = Bytecode::new_raw(code);
    let contract_info = AccountInfo::new(
        U256::ZERO,
        1,
        bytecode.hash_slow(),
        bytecode,
    );
    let caller_info = AccountInfo::new(
        U256::from(100) * U256::from(10).pow(U256::from(18)),
        1,
        Default::default(),
        Bytecode::default(),
    );

    db.insert_account(contract_addr, contract_info);
    db.insert_account(caller_addr, caller_info);
    db.insert_storage(contract_addr, U256::from(8), U256::from(50_000_000_000_000u64));

    // -------------------------------------------------------------------------
    // Benchmark 1: Single Storage Slot Lookup Latency on PrewarmedDB (~5 ns)
    // -------------------------------------------------------------------------
    group.bench_function("prewarmed_storage_slot_lookup_ns", |b| {
        b.iter(|| {
            let val = db.storage_ref(black_box(contract_addr), black_box(U256::from(8))).unwrap();
            black_box(val);
        })
    });

    // -------------------------------------------------------------------------
    // Benchmark 2: Full In-Process EVM Transaction Execution (< 15 µs target)
    // -------------------------------------------------------------------------
    let runner = SimulationRunner::default();
    let tx = build_call_tx(
        caller_addr,
        contract_addr,
        Bytes::default(),
        U256::ZERO,
        150_000,
        None,
    );

    group.bench_function("zero_rpc_evm_transaction_execution", |b| {
        b.iter(|| {
            // Fresh ForkedSandbox over zero-RPC PrewarmedDB (zero network, zero locks)
            let mut sandbox = ForkedSandbox::new(db.clone(), runner.clone());
            let result = sandbox.simulate_tx(black_box(tx.clone())).unwrap();
            black_box(result);
        })
    });

    // -------------------------------------------------------------------------
    // Benchmark 3: Snapshot Branching (Clone CacheDB) Latency
    // -------------------------------------------------------------------------
    group.bench_function("sandbox_snapshot_branching_latency", |b| {
        let sandbox = ForkedSandbox::new(db.clone(), runner.clone());
        b.iter(|| {
            let branch = sandbox.snapshot();
            black_box(branch);
        })
    });


    group.finish();
}

criterion_group!(benches, bench_simulator_latency);
criterion_main!(benches);
