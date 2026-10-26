//! Unit test suite for in-process EVM simulation, snapshot branching, and microsecond latency.

use alloy::primitives::{address, bytes, Bytes, U256};
use mev_simulator::{build_call_tx, ForkedSandbox, SimulationRunner};
use revm::{database::EmptyDB, DatabaseRef};
use std::time::Instant;

#[test]
fn test_evm_execution_and_revert_decoding() {
    let runner = SimulationRunner::default();
    let mut sandbox = ForkedSandbox::new(EmptyDB::default(), runner);

    let contract_addr = address!("1111111111111111111111111111111111111111");
    let caller = address!("2222222222222222222222222222222222222222");

    // Bytecode that reverts with Error("InsufficientProfit")
    // Selector: 0x08c379a0
    // ABI: offset 0x20, length 18 (0x12), string "InsufficientProfit" padded to 32 bytes
    let revert_bytecode = bytes!(
        "6012"      // PUSH1 0x12 (length 18)
        "6020"      // PUSH1 0x20 (offset 32)
        "6308c379a0"// PUSH4 0x08c379a0 (selector)
        // Store selector at 0x00
        "60e01b600052"
        // Store offset at 0x04
        "600452"
        // Store length at 0x24
        "602452"
        // Store "InsufficientProfit" at 0x44
        "71496e73756666696369656e7450726f666974" // 18 bytes
        "60701b604452"
        // Revert 0x00..0x64 (100 bytes)
        "60646000fd"
    );

    sandbox.set_code(contract_addr, revert_bytecode);
    sandbox.set_balance(caller, U256::from(10) * U256::from(10).pow(U256::from(18)));

    let tx = build_call_tx(caller, contract_addr, Bytes::default(), U256::ZERO, 100_000, None);
    let result = sandbox.simulate_tx(tx).expect("simulation call succeeded");

    assert!(!result.success, "simulation must revert");
    assert_eq!(
        result.revert_reason(),
        Some("InsufficientProfit".to_string()),
        "must decode human-readable revert string"
    );
}

#[test]
fn test_victim_execution_and_state_commitment() {
    let runner = SimulationRunner::default();
    let mut sandbox = ForkedSandbox::new(EmptyDB::default(), runner);

    let counter_addr = address!("3333333333333333333333333333333333333333");
    let caller = address!("4444444444444444444444444444444444444444");

    // Bytecode that increments storage slot 0 by 1:
    // PUSH1 0x00 SLOAD PUSH1 0x01 ADD PUSH1 0x00 SSTORE STOP
    // 6000 54 6001 01 6000 55 00
    let counter_code = bytes!("60005460010160005500");

    sandbox.set_code(counter_addr, counter_code);
    sandbox.set_balance(caller, U256::from(10).pow(U256::from(18)));

    // Initial slot 0 is 0
    let initial_val = sandbox.db().storage_ref(counter_addr, U256::ZERO).unwrap_or(U256::ZERO);
    assert_eq!(initial_val, U256::ZERO);

    // Victim transaction 1 executes and commits
    let victim_tx_1 = build_call_tx(caller, counter_addr, Bytes::default(), U256::ZERO, 100_000, None);
    let res1 = sandbox.execute_victim(victim_tx_1).expect("victim tx 1 must succeed");
    assert!(res1.success);

    let val_after_1 = sandbox.db().storage_ref(counter_addr, U256::ZERO).unwrap_or(U256::ZERO);
    assert_eq!(val_after_1, U256::from(1), "slot 0 must be committed to 1");

    // Victim transaction 2 executes and commits
    let victim_tx_2 = build_call_tx(caller, counter_addr, Bytes::default(), U256::ZERO, 100_000, None);
    let res2 = sandbox.execute_victim(victim_tx_2).expect("victim tx 2 must succeed");
    assert!(res2.success);

    let val_after_2 = sandbox.db().storage_ref(counter_addr, U256::ZERO).unwrap_or(U256::ZERO);
    assert_eq!(val_after_2, U256::from(2), "slot 0 must be committed to 2");
}

#[test]
fn test_snapshot_branching_and_isolation() {
    let runner = SimulationRunner::default();
    let mut sandbox = ForkedSandbox::new(EmptyDB::default(), runner);

    let contract_addr = address!("5555555555555555555555555555555555555555");
    let caller = address!("6666666666666666666666666666666666666666");

    // Bytecode that increments storage slot 0 by 1
    let counter_code = bytes!("60005460010160005500");
    sandbox.set_code(contract_addr, counter_code);
    sandbox.set_balance(caller, U256::from(10).pow(U256::from(18)));

    // Victim transaction sets state to 10
    sandbox.set_storage(contract_addr, U256::ZERO, U256::from(10));

    // Snapshot post-victim state
    let snapshot = sandbox.snapshot();

    // Branch A evaluates candidate 1 (increment to 11)
    let mut branch_a = snapshot.clone();
    let candidate_a = build_call_tx(caller, contract_addr, Bytes::default(), U256::ZERO, 100_000, None);
    let res_a = sandbox.simulate_candidate_commit(&mut branch_a, candidate_a).expect("candidate a ok");
    assert!(res_a.success);
    assert_eq!(branch_a.storage_ref(contract_addr, U256::ZERO).unwrap_or(U256::ZERO), U256::from(11));

    // Branch B evaluates candidate 2 (increment twice to 12)
    let mut branch_b = snapshot.clone();
    let candidate_b1 = build_call_tx(caller, contract_addr, Bytes::default(), U256::ZERO, 100_000, None);
    let candidate_b2 = build_call_tx(caller, contract_addr, Bytes::default(), U256::ZERO, 100_000, None);
    sandbox.simulate_candidate_commit(&mut branch_b, candidate_b1).expect("b1 ok");
    sandbox.simulate_candidate_commit(&mut branch_b, candidate_b2).expect("b2 ok");
    assert_eq!(branch_b.storage_ref(contract_addr, U256::ZERO).unwrap_or(U256::ZERO), U256::from(12));

    // Assert Branch A and Branch B are strictly isolated
    assert_eq!(branch_a.storage_ref(contract_addr, U256::ZERO).unwrap_or(U256::ZERO), U256::from(11));

    // Assert original sandbox remains unchanged at 10
    assert_eq!(sandbox.db().storage_ref(contract_addr, U256::ZERO).unwrap_or(U256::ZERO), U256::from(10));
}

#[test]
fn test_candidate_evaluation_sub_500_microsecond_benchmark() {
    let runner = SimulationRunner::default();
    let mut sandbox = ForkedSandbox::new(EmptyDB::default(), runner);

    let contract_addr = address!("7777777777777777777777777777777777777777");
    let caller = address!("8888888888888888888888888888888888888888");

    // A realistic state modification bytecode: reads slot 0, adds 10, writes to slot 1, returns 32 bytes
    // PUSH1 0x00 SLOAD PUSH1 0x0a ADD PUSH1 0x01 SSTORE PUSH1 0x01 SLOAD PUSH1 0x00 MSTORE PUSH1 0x20 PUSH1 0x00 RETURN
    let bench_code = bytes!("600054600a0160015560015460005260206000f3");

    sandbox.set_code(contract_addr, bench_code);
    sandbox.set_balance(caller, U256::from(10).pow(U256::from(18)));
    sandbox.set_storage(contract_addr, U256::ZERO, U256::from(100));

    // Take snapshot of post-victim state
    let snapshot = sandbox.snapshot();

    // Warm-up run
    let mut warmup = snapshot.clone();
    let tx = build_call_tx(caller, contract_addr, Bytes::default(), U256::ZERO, 100_000, None);
    let _ = sandbox.simulate_candidate(&mut warmup, tx.clone()).expect("warmup ok");

    // Benchmark 100 candidate evaluations against cloned snapshots
    let iterations = 100;
    let start = Instant::now();

    for _ in 0..iterations {
        let mut branch = snapshot.clone();
        let res = sandbox.simulate_candidate(&mut branch, tx.clone()).expect("candidate ok");
        assert!(res.success);
    }

    let elapsed = start.elapsed();
    let avg_micros = elapsed.as_micros() as f64 / iterations as f64;

    println!(
        "\n⚡ [BENCHMARK] Evaluated {} candidates in {:?} (Average: {:.2} µs / evaluation, Target: < 500 µs)\n",
        iterations, elapsed, avg_micros
    );

    assert!(
        avg_micros < 500.0,
        "Candidate evaluation must complete in < 500 µs, was {:.2} µs",
        avg_micros
    );
}
