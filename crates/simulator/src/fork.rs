//! EVM fork sandbox providing victim re-execution and snapshot branching for backrun evaluation.

use alloy::primitives::{Address, Bytes, U256};
use revm::{
    bytecode::Bytecode,
    context::TxEnv,
    context_interface::result::EVMError,
    database::{CacheDB, DatabaseRef},
    Database,
};

use crate::runner::{SimulationResult, SimulationRunner};

/// An isolated EVM simulation sandbox backed by an in-memory `CacheDB`.
///
/// Supports full victim transaction re-execution, state commitment, cheap snapshot
/// branching, and parallel candidate backrun evaluation.
#[derive(Debug, Clone)]
pub struct ForkedSandbox<DB> {
    pub runner: SimulationRunner,
    pub db: CacheDB<DB>,
}

impl<DB: DatabaseRef> ForkedSandbox<DB> {
    /// Creates a new simulation sandbox wrapping the backend database.
    pub fn new(backend_db: DB, runner: SimulationRunner) -> Self {
        Self {
            runner,
            db: CacheDB::new(backend_db),
        }
    }

    /// Creates a new simulation sandbox from an existing `CacheDB`.
    pub fn with_cache_db(db: CacheDB<DB>, runner: SimulationRunner) -> Self {
        Self { runner, db }
    }

    /// Read-only access to the underlying `CacheDB`.
    pub fn db(&self) -> &CacheDB<DB> {
        &self.db
    }

    /// Mutable access to the underlying `CacheDB`.
    pub fn db_mut(&mut self) -> &mut CacheDB<DB> {
        &mut self.db
    }

    /// Sets or overrides the native ETH balance of an account in the sandbox.
    pub fn set_balance(&mut self, address: Address, balance: U256) {
        let mut info = self.db.basic_ref(address).unwrap_or(None).unwrap_or_default();
        info.balance = balance;
        self.db.insert_account_info(address, info);
    }

    /// Sets or overrides the contract bytecode of an account in the sandbox.
    pub fn set_code(&mut self, address: Address, code: Bytes) {
        let mut info = self.db.basic_ref(address).unwrap_or(None).unwrap_or_default();
        let bytecode = Bytecode::new_raw(code);
        info.code_hash = bytecode.hash_slow();
        info.code = Some(bytecode);
        self.db.insert_account_info(address, info);
    }

    /// Sets or overrides a specific storage slot in the sandbox.
    pub fn set_storage(&mut self, address: Address, slot: U256, value: U256) {
        self.db.insert_account_storage(address, slot, value).ok();
    }

    /// Executes a victim transaction, committing all its state mutations to this sandbox.
    ///
    /// This establishes the post-victim state against which candidate backruns will compete.
    pub fn execute_victim(
        &mut self,
        tx: TxEnv,
    ) -> Result<SimulationResult, EVMError<<CacheDB<DB> as Database>::Error>> {
        self.runner.execute_and_commit(&mut self.db, tx)
    }

    /// Simulates a single transaction against the current sandbox state WITHOUT committing mutations.
    pub fn simulate_tx(
        &mut self,
        tx: TxEnv,
    ) -> Result<SimulationResult, EVMError<<CacheDB<DB> as Database>::Error>> {
        self.runner.execute(&mut self.db, tx)
    }

    /// Takes a cheap, in-memory snapshot clone of the current sandbox state.
    pub fn snapshot(&self) -> CacheDB<DB>
    where
        DB: Clone,
    {
        self.db.clone()
    }

    /// Branches an entirely independent `ForkedSandbox` clone from the current state.
    pub fn branch(&self) -> Self
    where
        DB: Clone,
    {
        Self {
            runner: self.runner.clone(),
            db: self.db.clone(),
        }
    }

    /// Simulates a candidate backrun transaction against an isolated snapshot branch.
    pub fn simulate_candidate(
        &self,
        snapshot: &mut CacheDB<DB>,
        candidate_tx: TxEnv,
    ) -> Result<SimulationResult, EVMError<<CacheDB<DB> as Database>::Error>> {
        self.runner.execute(snapshot, candidate_tx)
    }

    /// Simulates and commits a candidate backrun transaction to an isolated snapshot branch.
    pub fn simulate_candidate_commit(
        &self,
        snapshot: &mut CacheDB<DB>,
        candidate_tx: TxEnv,
    ) -> Result<SimulationResult, EVMError<<CacheDB<DB> as Database>::Error>> {
        self.runner.execute_and_commit(snapshot, candidate_tx)
    }

    /// Evaluates multiple candidate transactions in sequence against fresh snapshot clones.
    pub fn evaluate_candidates(
        &self,
        snapshot: &CacheDB<DB>,
        candidates: impl IntoIterator<Item = TxEnv>,
    ) -> Vec<Result<SimulationResult, EVMError<<CacheDB<DB> as Database>::Error>>>
    where
        DB: Clone,
    {
        candidates
            .into_iter()
            .map(|tx| {
                let mut branch = snapshot.clone();
                self.runner.execute(&mut branch, tx)
            })
            .collect()
    }
}
