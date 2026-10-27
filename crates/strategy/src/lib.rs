//! Mathematical core, graph routing, and optimal sizing engine for cyclic arbitrage.

pub mod graph;
pub mod pricing;
pub mod profit;
pub mod sizing;
pub mod table;

pub use graph::GraphRouter;
pub use pricing::{
    compute_swap_step_v3, get_amount_out, get_amount_out_v2, get_amount_out_v3,
    get_amount_out_v3_hot, get_sqrt_ratio_at_tick, simulate_cycle, simulate_hot_cycle, Q96,
};
pub use profit::{ArbitrageOpportunity, ProfitEvaluator};
pub use sizing::{
    find_optimal_input, find_optimal_input_hot, golden_section_search, golden_section_search_hot,
    optimal_input_v2_analytical,
};
pub use table::CycleTable;

use alloy::primitives::{Address, U256};
use mev_state::{HotMarketState, MarketState};
use mev_types::{HotCycle, PoolId};


/// High-level strategy engine coordinating graph discovery, sizing, and profitability filtering.
#[derive(Debug, Clone)]
pub struct StrategyEngine {
    pub router: GraphRouter,
    pub cycle_table: CycleTable,
    pub evaluator: ProfitEvaluator,
    pub min_trade_weth: U256,
    pub max_trade_weth: U256,
}

impl StrategyEngine {
    /// Creates a new strategy engine with configured parameters and an empty pre-computed table.
    pub fn new(
        base_token: Address,
        token_whitelist: Vec<Address>,
        evaluator: ProfitEvaluator,
        min_trade_weth: U256,
        max_trade_weth: U256,
    ) -> Self {
        Self {
            router: GraphRouter::new(base_token, token_whitelist),
            cycle_table: CycleTable::default(),
            evaluator,
            min_trade_weth,
            max_trade_weth,
        }
    }

    /// Creates a strategy engine and eagerly pre-computes the line-graph CycleTable from MarketState.
    pub fn new_with_table(
        market_state: &MarketState,
        base_token: Address,
        token_whitelist: Vec<Address>,
        evaluator: ProfitEvaluator,
        min_trade_weth: U256,
        max_trade_weth: U256,
    ) -> Self {
        let cycle_table = CycleTable::build(market_state, base_token, &token_whitelist);
        Self {
            router: GraphRouter::new(base_token, token_whitelist),
            cycle_table,
            evaluator,
            min_trade_weth,
            max_trade_weth,
        }
    }

    /// Rebuilds the pre-computed CycleTable when pools or tokens are added.
    pub fn rebuild_cycle_table(&mut self, market_state: &MarketState) {
        self.cycle_table = CycleTable::build(
            market_state,
            self.router.base_token,
            &self.router.token_whitelist,
        );
    }

    /// Evaluates all candidate cycles touching a specific pool using the pre-computed O(1) CycleTable
    /// and hot-path slice indexing against HotMarketState (< 5 µs).
    ///
    /// Evaluates all candidate cycles in L1 cache with zero heap allocations, and only materializes
    /// the full Cycle and MevExecutor calldata once for the single winning opportunity that passes the net profit threshold.
    pub fn evaluate_touched_pool_hot(
        &self,
        hot_state: &HotMarketState,
        touched_pool_id: PoolId,
        base_fee_wei: u64,
    ) -> Vec<ArbitrageOpportunity> {
        let cycle_indices = self.cycle_table.get_cycles_touching_pool(touched_pool_id);
        if cycle_indices.is_empty() {
            return Vec::new();
        }

        let mut best_candidate: Option<(&HotCycle, U256, U256, U256)> = None;

        for &c_idx in cycle_indices {
            let hot_cycle = match self.cycle_table.get_cycle(c_idx) {
                Some(c) => c,
                None => continue,
            };

            let (opt_in, expected_out) = find_optimal_input_hot(
                hot_cycle,
                &hot_state.pools,
                self.min_trade_weth,
                self.max_trade_weth,
            );

            if opt_in.is_zero() || expected_out <= opt_in {
                continue;
            }

            let gross_profit = expected_out - opt_in;
            let hops_count = hot_cycle.hop_count as u64;
            let gas_used = 40_000 + (hops_count * self.evaluator.base_gas_per_hop);
            let gas_cost = U256::from(gas_used) * U256::from(base_fee_wei);

            if gross_profit <= gas_cost {
                continue;
            }

            let bribe = (gross_profit * U256::from(self.evaluator.bribe_bps)) / U256::from(10_000);
            let net_profit = gross_profit.saturating_sub(gas_cost).saturating_sub(bribe);

            if net_profit < self.evaluator.min_profit_wei {
                continue;
            }

            match &best_candidate {
                Some((_, _, _, best_net)) => {
                    if net_profit > *best_net {
                        best_candidate = Some((hot_cycle, opt_in, expected_out, net_profit));
                    }
                }
                None => {
                    best_candidate = Some((hot_cycle, opt_in, expected_out, net_profit));
                }
            }
        }

        // Materialize only the single winning opportunity that passes threshold
        if let Some((hot_cycle, opt_in, expected_out, _)) = best_candidate {
            if let Some(cycle) = self.cycle_table.materialize_cycle(hot_cycle, hot_state) {
                if let Some(opp) = self.evaluator.evaluate_opportunity(
                    cycle,
                    opt_in,
                    expected_out,
                    base_fee_wei,
                ) {
                    return vec![opp];
                }
            }
        }

        Vec::new()
    }


    /// Evaluates all cycles touching a specific pool that was mutated by a victim transaction.
    ///
    /// (Phase 1 fallback retaining 100% backward compatibility for existing callers).
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use mev_state::{MarketState, USDC, WETH};
    use mev_types::{HotCycleHop, PoolDescriptor, PoolState, PoolVariant, V2PoolState};

    #[test]
    fn test_hot_cycle_hop_size_in_strategy() {
        assert_eq!(std::mem::size_of::<HotCycleHop>(), 8);
    }

    #[test]
    fn test_cycle_table_precomputation_and_hot_evaluation_parity() {
        let market_state = MarketState::new();

        let pool_univ2 = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
        let pool_sushiv2 = address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0");

        let desc_uni = PoolDescriptor::new(pool_univ2, PoolVariant::UniswapV2, WETH, USDC, 30);
        let desc_sushi = PoolDescriptor::new(pool_sushiv2, PoolVariant::UniswapV2, WETH, USDC, 30);

        // Uni: 1 ETH = 2500 USDC
        let state_uni = PoolState::V2(V2PoolState {
            reserve0: 25_000_000 * 10u128.pow(6),
            reserve1: 10_000 * 10u128.pow(18),
            block_timestamp_last: 1700000000,
        });
        // Sushi: 1 ETH = 2000 USDC (arbitrage opportunity exists)
        let state_sushi = PoolState::V2(V2PoolState {
            reserve0: 20_000_000 * 10u128.pow(6),
            reserve1: 10_000 * 10u128.pow(18),
            block_timestamp_last: 1700000000,
        });

        market_state.update_pool(desc_uni, state_uni);
        market_state.update_pool(desc_sushi, state_sushi);

        let evaluator = ProfitEvaluator::new(U256::ZERO, 1000, 70_000);
        let min_trade = U256::from(100_000_000_000_000_000u64); // 0.1 ETH
        let max_trade = U256::from(10_000_000_000_000_000_000u128); // 10 ETH

        let mut engine = StrategyEngine::new(
            WETH,
            vec![USDC],
            evaluator,
            min_trade,
            max_trade,
        );
        engine.rebuild_cycle_table(&market_state);

        assert_eq!(engine.cycle_table.len(), 2);

        let pool_id_uni = market_state.get_pool_id(&pool_univ2).unwrap();
        let cycles_touching = engine.cycle_table.get_cycles_touching_pool(pool_id_uni);
        assert_eq!(cycles_touching.len(), 2);

        // Compare Phase 1 evaluate_touched_pool vs Phase 2.2 evaluate_touched_pool_hot
        let p1_opps = engine.evaluate_touched_pool(&market_state, pool_univ2, 30_000_000_000);
        let hot_guard = market_state.load_hot();
        let p2_opps = engine.evaluate_touched_pool_hot(&hot_guard, pool_id_uni, 30_000_000_000);

        assert_eq!(p1_opps.len(), p2_opps.len());
        assert!(!p1_opps.is_empty());
        assert_eq!(p1_opps[0].optimal_input, p2_opps[0].optimal_input);
        assert_eq!(p1_opps[0].expected_output, p2_opps[0].expected_output);
        assert_eq!(p1_opps[0].net_profit, p2_opps[0].net_profit);
    }
}

