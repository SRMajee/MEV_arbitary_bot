//! In-process EVM simulation sandbox for MEV victim re-execution and candidate backrun evaluation.

pub mod db;
pub mod fork;
pub mod prewarmed;
pub mod runner;

pub use db::{AlloyDB, AlloyDBError};
pub use fork::ForkedSandbox;
pub use prewarmed::{
    compute_mapping_slot, compute_nested_mapping_slot, pack_v2_reserves_slot8,
    pack_v3_liquidity_slot4, pack_v3_slot0, unpack_v2_reserves_slot8, PrewarmedDB,
    PrewarmedDBError, StateSnapshot,
};
pub use runner::{build_call_tx, build_create_tx, SimulationResult, SimulationRunner};


use mev_state::MarketState;
use revm::database::EmptyDB;

/// In-process simulation orchestrator coordinating market state with the revm execution sandbox.
#[derive(Debug, Clone)]
pub struct Simulator<DB = EmptyDB> {
    market_state: MarketState,
    runner: SimulationRunner,
    backend_db: DB,
}

impl Simulator<EmptyDB> {
    /// Creates a new simulator referencing the market state cache with an empty database.
    pub fn new(market_state: MarketState) -> Self {
        Self {
            market_state,
            runner: SimulationRunner::default(),
            backend_db: EmptyDB::default(),
        }
    }
}

impl<DB: Clone> Simulator<DB> {
    /// Creates a simulator with a custom backend database (e.g. `AlloyDB`).
    pub fn with_db(market_state: MarketState, backend_db: DB, runner: SimulationRunner) -> Self {
        Self {
            market_state,
            runner,
            backend_db,
        }
    }

    /// Accessor for market state.
    pub fn market_state(&self) -> &MarketState {
        &self.market_state
    }

    /// Accessor for simulation runner.
    pub fn runner(&self) -> &SimulationRunner {
        &self.runner
    }

    /// Spawns a fresh, isolated `ForkedSandbox` using this simulator's backend database and runner.
    pub fn new_sandbox(&self) -> ForkedSandbox<DB>
    where
        DB: revm::database::DatabaseRef,
    {
        ForkedSandbox::new(self.backend_db.clone(), self.runner.clone())
    }
}
