//! Embedded revm simulation engine for simulating pending swaps and extracting post-trade storage diffs.

use mev_state::MarketState;

/// Simulator instance configured to execute in-memory revm runs.
#[derive(Debug, Clone)]
pub struct Simulator {
    market_state: MarketState,
}

impl Simulator {
    /// Creates a new simulator referencing the market state cache.
    pub fn new(market_state: MarketState) -> Self {
        Self { market_state }
    }

    /// Accessor for market state.
    pub fn market_state(&self) -> &MarketState {
        &self.market_state
    }
}
