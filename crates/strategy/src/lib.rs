//! Mathematical core, graph routing, and optimal sizing engine for cyclic arbitrage.

pub mod graph;
pub mod pricing;
pub mod profit;
pub mod sizing;

pub use graph::GraphRouter;
pub use pricing::{
    compute_swap_step_v3, get_amount_out, get_amount_out_v2, get_amount_out_v3,
    get_sqrt_ratio_at_tick, simulate_cycle, Q96,
};
pub use profit::{ArbitrageOpportunity, ProfitEvaluator};
pub use sizing::{find_optimal_input, golden_section_search, optimal_input_v2_analytical};

use alloy::primitives::{Address, U256};
use mev_state::MarketState;

/// High-level strategy engine coordinating graph discovery, sizing, and profitability filtering.
#[derive(Debug, Clone)]
pub struct StrategyEngine {
    pub router: GraphRouter,
    pub evaluator: ProfitEvaluator,
    pub min_trade_weth: U256,
    pub max_trade_weth: U256,
}

impl StrategyEngine {
    /// Creates a new strategy engine with configured parameters.
    pub fn new(
        base_token: Address,
        token_whitelist: Vec<Address>,
        evaluator: ProfitEvaluator,
        min_trade_weth: U256,
        max_trade_weth: U256,
    ) -> Self {
        Self {
            router: GraphRouter::new(base_token, token_whitelist),
            evaluator,
            min_trade_weth,
            max_trade_weth,
        }
    }

    /// Evaluates all cycles touching a specific pool that was mutated by a victim transaction.
    ///
    /// Returns all profitable opportunities sorted descending by net profit.
    pub fn evaluate_touched_pool(
        &self,
        market_state: &MarketState,
        touched_pool: Address,
        base_fee_wei: u64,
    ) -> Vec<ArbitrageOpportunity> {
        let cycles = self.router.find_cycles_touching_pool(market_state, touched_pool);
        let mut opportunities = Vec::new();

        for cycle in cycles {
            let (opt_in, expected_out) = find_optimal_input(
                &cycle,
                market_state,
                self.min_trade_weth,
                self.max_trade_weth,
            );

            if opt_in.is_zero() || expected_out <= opt_in {
                continue;
            }

            if let Some(opp) = self.evaluator.evaluate_opportunity(
                cycle,
                opt_in,
                expected_out,
                base_fee_wei,
            ) {
                opportunities.push(opp);
            }
        }

        // Sort by net profit descending (highest net profit first)
        opportunities.sort_by(|a, b| b.net_profit.cmp(&a.net_profit));
        opportunities
    }
}
