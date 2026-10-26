//! Profitability evaluation, dynamic gas models, and MevExecutor parameter encoding.

use alloy::primitives::{Uint, U256};
use mev_executor::MevExecutor::{ExecutionParams, Hop};
use mev_types::{Cycle, PoolVariant};

/// A fully qualified, profitable arbitrage opportunity ready for bundle construction.
#[derive(Clone)]
pub struct ArbitrageOpportunity {
    /// Discovered cyclic route.
    pub cycle: Cycle,
    /// Profit-maximizing input amount $x^*$ (in WETH wei).
    pub optimal_input: U256,
    /// Expected output amount (in WETH wei).
    pub expected_output: U256,
    /// Gross profit: $\Delta x_{out} - x_{in}$.
    pub gross_profit: U256,
    /// Estimated gas consumed by the execution contract.
    pub gas_used_estimate: u64,
    /// L1 gas cost in wei: $\text{gas\_estimate} \times \text{base\_fee}$.
    pub gas_cost: U256,
    /// Builder bribe sent to coinbase.
    pub builder_bribe: U256,
    /// Net realized profit after gas and bribe.
    pub net_profit: U256,
    /// Calldata parameters ready for `MevExecutor.execute(params)`.
    pub execution_params: ExecutionParams,
}

impl std::fmt::Debug for ArbitrageOpportunity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArbitrageOpportunity")
            .field("cycle", &self.cycle)
            .field("optimal_input", &self.optimal_input)
            .field("expected_output", &self.expected_output)
            .field("gross_profit", &self.gross_profit)
            .field("gas_cost", &self.gas_cost)
            .field("builder_bribe", &self.builder_bribe)
            .field("net_profit", &self.net_profit)
            .finish()
    }
}

/// Dynamic profitability evaluator enforcing gas thresholds, bribes, and min net margins.
#[derive(Debug, Clone)]
pub struct ProfitEvaluator {
    /// Minimum acceptable net profit in wei (e.g., 0.002 WETH).
    pub min_profit_wei: U256,
    /// Builder bribe percentage in basis points (e.g., 9000 = 90%).
    pub bribe_bps: u16,
    /// Base gas overhead for MevExecutor invocation (e.g., 140,000 for 2-hop, 190,000 for 3-hop).
    pub base_gas_per_hop: u64,
}

impl Default for ProfitEvaluator {
    fn default() -> Self {
        Self {
            // 0.002 WETH default minimum profit
            min_profit_wei: U256::from(2_000_000_000_000_000u64),
            // 80% default builder bribe
            bribe_bps: 8000,
            base_gas_per_hop: 70_000,
        }
    }
}

impl ProfitEvaluator {
    /// Creates a new profit evaluator with custom thresholds.
    pub fn new(min_profit_wei: U256, bribe_bps: u16, base_gas_per_hop: u64) -> Self {
        Self {
            min_profit_wei,
            bribe_bps,
            base_gas_per_hop,
        }
    }

    /// Evaluates whether a cycle and input amount meet the net profitability invariant.
    pub fn evaluate_opportunity(
        &self,
        cycle: Cycle,
        optimal_input: U256,
        expected_output: U256,
        base_fee_wei: u64,
    ) -> Option<ArbitrageOpportunity> {
        if expected_output <= optimal_input {
            return None;
        }

        let gross_profit = expected_output - optimal_input;

        // Estimate gas: base transaction overhead (40k) + per-hop overhead
        let hops_count = cycle.hops.len() as u64;
        let gas_used_estimate = 40_000 + (hops_count * self.base_gas_per_hop);
        let gas_cost = U256::from(gas_used_estimate) * U256::from(base_fee_wei);

        if gross_profit <= gas_cost {
            return None;
        }

        // On-chain bribe: (gross_profit * bribe_bps) / 10_000
        let builder_bribe = (gross_profit * U256::from(self.bribe_bps)) / U256::from(10_000);
        let net_profit = gross_profit.saturating_sub(gas_cost).saturating_sub(builder_bribe);

        if net_profit < self.min_profit_wei {
            return None;
        }

        // Encode MevExecutor hops
        let mut hops = Vec::with_capacity(cycle.hops.len());
        for hop in &cycle.hops {
            let protocol = match hop.pool.variant {
                PoolVariant::UniswapV2 => 0u8,
                PoolVariant::UniswapV3 => 1u8,
            };

            hops.push(Hop {
                protocol,
                pool: hop.pool.address,
                tokenIn: hop.token_in,
                tokenOut: hop.token_out,
                fee: Uint::from(hop.pool.fee_pips),
                sqrtPriceLimitX96: Uint::ZERO,
            });
        }

        // Set minProfit with a 10% safety margin for minor slippage
        let min_profit_onchain = gross_profit * U256::from(90) / U256::from(100);

        let execution_params = ExecutionParams {
            amountIn: optimal_input,
            minProfit: min_profit_onchain,
            bribeBps: self.bribe_bps,
            hops,
        };

        Some(ArbitrageOpportunity {
            cycle,
            optimal_input,
            expected_output,
            gross_profit,
            gas_used_estimate,
            gas_cost,
            builder_bribe,
            net_profit,
            execution_params,
        })
    }
}
