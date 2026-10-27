//! Optimal trade sizing engine combining analytical closed-form and Golden Section Search.
//!
//! Computes the profit-maximizing input amount $x^*$ for 2-hop V2-V2 cycles in closed form,
//! and optimizes multi-hop V2/V3 triangles via convex Golden Section Search.

use alloy::primitives::U256;
use mev_state::MarketState;
use mev_types::{Cycle, HotCycle, HotPoolState, PoolState};

use crate::pricing::{simulate_cycle, simulate_hot_cycle};


/// Golden ratio conjugate $\frac{\sqrt{5} - 1}{2} \approx 0.6180339887$ scaled by 1,000,000.
pub const PHI_RECIPROCAL_PPM: u64 = 618_034;

/// Calculates the analytical closed-form optimal input $x^*$ for a 2-hop Uniswap V2 arbitrage loop.
///
/// Given Pool A ($x \to y$) with reserves $(R_{Ax}, R_{Ay})$ and Pool B ($y \to x$) with reserves $(R_{By}, R_{Bx})$:
/// $$x^* = \frac{\sqrt{N \cdot D_0} - D_0}{D_1}$$
/// where:
/// - $\gamma_A = (1000000 - \text{fee}_A) / 1000000$
/// - $\gamma_B = (1000000 - \text{fee}_B) / 1000000$
/// - $N = R_{Bx} \cdot R_{Ay} \cdot \gamma_A \cdot \gamma_B$
/// - $D_0 = R_{Ax} \cdot R_{By}$
/// - $D_1 = \gamma_A \cdot (R_{By} + R_{Ay} \cdot \gamma_B)$
pub fn optimal_input_v2_analytical(
    reserve_ax: U256,
    reserve_ay: U256,
    reserve_by: U256,
    reserve_bx: U256,
    fee_a_pips: u32,
    fee_b_pips: u32,
) -> U256 {
    if reserve_ax.is_zero() || reserve_ay.is_zero() || reserve_by.is_zero() || reserve_bx.is_zero() {
        return U256::ZERO;
    }

    let gamma_a = (1_000_000.0 - fee_a_pips as f64) / 1_000_000.0;
    let gamma_b = (1_000_000.0 - fee_b_pips as f64) / 1_000_000.0;

    let r_ax = reserve_ax.to::<u128>() as f64;
    let r_ay = reserve_ay.to::<u128>() as f64;
    let r_by = reserve_by.to::<u128>() as f64;
    let r_bx = reserve_bx.to::<u128>() as f64;

    let n = r_bx * r_ay * gamma_a * gamma_b;
    let d0 = r_ax * r_by;
    let d1 = gamma_a * (r_by + r_ay * gamma_b);

    if n <= d0 || d1 <= 0.0 {
        return U256::ZERO;
    }

    let sqrt_n_d0 = (n * d0).sqrt();
    if sqrt_n_d0 <= d0 {
        return U256::ZERO;
    }

    let opt_x = (sqrt_n_d0 - d0) / d1;
    if opt_x <= 0.0 || opt_x.is_nan() || opt_x.is_infinite() {
        U256::ZERO
    } else {
        U256::from(opt_x as u128)
    }
}

/// Optimizes the input amount for any cyclic arbitrage loop using Golden Section Search.
///
/// Since the CFMM cyclic profit function $P(x) = x_{out}(x) - x$ is strictly concave,
/// Golden Section Search converges exponentially to the exact global maximum.
/// Uses the invariant $P(c) > P(d) \iff x_{out}(c) + d > x_{out}(d) + c$ to prevent
/// loss-clamping artifacts across arbitrary wide search intervals.
pub fn golden_section_search(
    cycle: &Cycle,
    market_state: &MarketState,
    min_in: U256,
    max_in: U256,
    max_iterations: usize,
) -> (U256, U256) {
    if max_in <= min_in {
        return (U256::ZERO, U256::ZERO);
    }

    let mut a = min_in;
    let mut b = max_in;

    for _ in 0..max_iterations {
        let span = b - a;
        if span <= U256::from(100_000_000_000u64) {
            // Converged to within 0.0000001 ETH
            break;
        }

        let delta = (span * U256::from(PHI_RECIPROCAL_PPM)) / U256::from(1_000_000);
        let c = b - delta;
        let d = a + delta;

        let out_c = simulate_cycle(cycle, c, market_state);
        let out_d = simulate_cycle(cycle, d, market_state);

        // P(c) > P(d) <=> out_c - c > out_d - d <=> out_c + d > out_d + c
        if out_c + d > out_d + c {
            b = d;
        } else {
            a = c;
        }
    }

    let best_x = (a + b) / U256::from(2);
    let best_out = simulate_cycle(cycle, best_x, market_state);
    (best_x, best_out)
}

/// Hybrid sizing optimizer finding the profit-maximizing input amount for any candidate cycle.
///
/// Uses closed-form derivation for 2-hop V2-V2 loops and refines via Golden Section Search.
/// For 3-hop or V3-containing loops, applies full Golden Section Search across configured bounds.
pub fn find_optimal_input(
    cycle: &Cycle,
    market_state: &MarketState,
    min_trade: U256,
    max_trade: U256,
) -> (U256, U256) {
    if cycle.hops.len() == 2 {
        let hop1 = &cycle.hops[0];
        let hop2 = &cycle.hops[1];

        let state1 = market_state.get_pool(&hop1.pool.address);
        let state2 = market_state.get_pool(&hop2.pool.address);

        if let (Some((_, PoolState::V2(v2_1))), Some((_, PoolState::V2(v2_2)))) = (state1, state2) {
            let (r1x, r1y) = if hop1.token_in == hop1.pool.token0 {
                (U256::from(v2_1.reserve0), U256::from(v2_1.reserve1))
            } else {
                (U256::from(v2_1.reserve1), U256::from(v2_1.reserve0))
            };

            let (r2y, r2x) = if hop2.token_in == hop2.pool.token0 {
                (U256::from(v2_2.reserve0), U256::from(v2_2.reserve1))
            } else {
                (U256::from(v2_2.reserve1), U256::from(v2_2.reserve0))
            };

            let analytical_x = optimal_input_v2_analytical(
                r1x,
                r1y,
                r2y,
                r2x,
                hop1.pool.fee_pips,
                hop2.pool.fee_pips,
            );

            if analytical_x.is_zero() {
                return (U256::ZERO, U256::ZERO);
            }

            // Clamp to allowed range and refine in a tight +/- 5% window using Golden Section
            let clamped_x = analytical_x.clamp(min_trade, max_trade);
            let search_min = clamped_x * U256::from(95) / U256::from(100);
            let search_max = clamped_x * U256::from(105) / U256::from(100);

            return golden_section_search(cycle, market_state, search_min, search_max, 12);
        }
    }

    // Default for V3 or 3-hop cycles: full Golden Section Search
    golden_section_search(cycle, market_state, min_trade, max_trade, 30)
}

/// Optimizes input amount for any HotCycle directly against &[HotPoolState] without locks or allocations.
pub fn golden_section_search_hot(
    cycle: &HotCycle,
    pools: &[HotPoolState],
    min_in: U256,
    max_in: U256,
    max_iterations: usize,
) -> (U256, U256) {
    if max_in <= min_in {
        return (U256::ZERO, U256::ZERO);
    }

    let mut a = min_in;
    let mut b = max_in;

    for _ in 0..max_iterations {
        let span = b - a;
        if span <= U256::from(100_000_000_000u64) {
            // Converged to within 0.0000001 ETH
            break;
        }

        let delta = (span * U256::from(PHI_RECIPROCAL_PPM)) / U256::from(1_000_000);
        let c = b - delta;
        let d = a + delta;

        let out_c = simulate_hot_cycle(cycle, c, pools);
        let out_d = simulate_hot_cycle(cycle, d, pools);

        if out_c + d > out_d + c {
            b = d;
        } else {
            a = c;
        }
    }

    let best_x = (a + b) / U256::from(2);
    let best_out = simulate_hot_cycle(cycle, best_x, pools);
    (best_x, best_out)
}

/// Hot-path optimal sizing for HotCycle against &[HotPoolState]:
/// - Pure 2-hop V2 cycles: analytical closed-form x* directly from &[HotPoolState] reserves (~15 ns).
/// - 3-hop and mixed V2/V3 cycles: 16-step Golden Section Search over &[HotPoolState] (zero allocations).
pub fn find_optimal_input_hot(
    cycle: &HotCycle,
    pools: &[HotPoolState],
    min_trade: U256,
    max_trade: U256,
) -> (U256, U256) {
    if cycle.hop_count == 2 && cycle.is_pure_v2() {
        let hop1 = &cycle.hops[0];
        let hop2 = &cycle.hops[1];

        let p1 = match pools.get(hop1.pool_id.0 as usize) {
            Some(p) => p,
            None => return (U256::ZERO, U256::ZERO),
        };
        let p2 = match pools.get(hop2.pool_id.0 as usize) {
            Some(p) => p,
            None => return (U256::ZERO, U256::ZERO),
        };

        let (r1x, r1y) = if hop1.zero_for_one {
            (U256::from(p1.slot0_lo), U256::from(p1.slot1_liquidity))
        } else {
            (U256::from(p1.slot1_liquidity), U256::from(p1.slot0_lo))
        };

        let (r2y, r2x) = if hop2.zero_for_one {
            (U256::from(p2.slot0_lo), U256::from(p2.slot1_liquidity))
        } else {
            (U256::from(p2.slot1_liquidity), U256::from(p2.slot0_lo))
        };

        let analytical_x = optimal_input_v2_analytical(
            r1x,
            r1y,
            r2y,
            r2x,
            p1.fee_pips,
            p2.fee_pips,
        );

        if analytical_x.is_zero() {
            return (U256::ZERO, U256::ZERO);
        }

        let clamped_x = analytical_x.clamp(min_trade, max_trade);
        let search_min = clamped_x * U256::from(95) / U256::from(100);
        let search_max = clamped_x * U256::from(105) / U256::from(100);

        return golden_section_search_hot(cycle, pools, search_min, search_max, 12);
    }


    // Default for 3-hop or mixed V2/V3 cycles
    golden_section_search_hot(cycle, pools, min_trade, max_trade, 16)
}

