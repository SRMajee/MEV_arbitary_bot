//! Mathematical core and graph routing engine for optimal arbitrage sizing.

use mev_types::Cycle;

/// Strategy router responsible for cycle identification and optimal input sizing.
#[derive(Debug, Default, Clone)]
pub struct StrategyRouter;

impl StrategyRouter {
    /// Creates a new strategy router.
    pub fn new() -> Self {
        Self
    }

    /// Evaluates whether a cycle is structurally viable.
    pub fn is_viable(&self, cycle: &Cycle) -> bool {
        cycle.validate()
    }
}
