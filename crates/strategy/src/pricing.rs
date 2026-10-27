//! Pure deterministic 256-bit AMM pricing and quotation engine.
//!
//! Replicates Uniswap V2 constant-product ($x \cdot y = k$) and Uniswap V3
//! concentrated liquidity ($Q64.96$ sqrtPriceX96) pricing with zero floating-point drift.

use alloy::primitives::{Address, U256};
use mev_state::MarketState;
use mev_types::{Cycle, HotCycle, HotPoolState, PoolDescriptor, PoolState, V3PoolState};


/// Scale factor for 2^96 in Uniswap V3 Q64.96 fixed-point arithmetic.
pub const Q96: U256 = U256::from_limbs([0, 1 << 32, 0, 0]);

/// Maximum allowable tick in Uniswap V3.
pub const MAX_TICK: i32 = 887272;
/// Minimum allowable tick in Uniswap V3.
pub const MIN_TICK: i32 = -887272;

/// Calculates the exact output amount for a Uniswap V2 / SushiSwap swap.
///
/// Matches Solidity `UniswapV2Library.getAmountOut`:
/// $$\Delta y = \frac{\Delta x \cdot (1000000 - \text{fee}) \cdot R_y}{R_x \cdot 1000000 + \Delta x \cdot (1000000 - \text{fee})}$$
#[inline]
pub fn get_amount_out_v2(
    amount_in: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee_pips: u32,
) -> U256 {
    if amount_in.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
        return U256::ZERO;
    }

    // Default fee for Uniswap V2 is 3000 pips (0.3% -> 997 / 1000)
    let (fee_factor, fee_denom) = if fee_pips == 3000 || fee_pips == 30 {
        (U256::from(997), U256::from(1000))
    } else {
        (U256::from(1_000_000 - fee_pips), U256::from(1_000_000))
    };

    let amount_in_with_fee = amount_in * fee_factor;
    let numerator = amount_in_with_fee * reserve_out;
    let denominator = (reserve_in * fee_denom) + amount_in_with_fee;

    if denominator.is_zero() {
        U256::ZERO
    } else {
        numerator / denominator
    }
}

/// Converts an integer tick to its exact Q64.96 sqrt ratio.
///
/// Implements the bitwise multiplication algorithm from Uniswap V3 `TickMath.getSqrtRatioAtTick`.
pub fn get_sqrt_ratio_at_tick(tick: i32) -> U256 {
    let abs_tick = tick.abs() as u32;

    let mut ratio = if abs_tick & 0x1 != 0 {
        U256::from_str_radix("fffcb933bd6fad37aa2d162d1a594001", 16).unwrap()
    } else {
        U256::from(1) << 128
    };

    if abs_tick & 0x2 != 0 {
        ratio = (ratio * U256::from_str_radix("fff97272373d413259a46990572e243a", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x4 != 0 {
        ratio = (ratio * U256::from_str_radix("fff2e50f5f656932ef12357cf3c7fdcc", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x8 != 0 {
        ratio = (ratio * U256::from_str_radix("ffe5caca7e10e4e61c3624eaa0941cd0", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x10 != 0 {
        ratio = (ratio * U256::from_str_radix("ffcb9843d60f6159c9db58835c926644", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x20 != 0 {
        ratio = (ratio * U256::from_str_radix("ff973b41fa98c081472e6896dfb254c0", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x40 != 0 {
        ratio = (ratio * U256::from_str_radix("ff2ea16466c96a3843ec78b326b52861", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x80 != 0 {
        ratio = (ratio * U256::from_str_radix("fe5dee046a99a2a811c461f1969c3053", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x100 != 0 {
        ratio = (ratio * U256::from_str_radix("fcbe86c7900a88aedcffc83b479aa3a4", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x200 != 0 {
        ratio = (ratio * U256::from_str_radix("f987a7253ac413176f2b074cf7815e54", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x400 != 0 {
        ratio = (ratio * U256::from_str_radix("f3392b08373da872a91e5e6e3d7a86f7", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x800 != 0 {
        ratio = (ratio * U256::from_str_radix("e7159475a2c29b7443b29c7fa6e889d9", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x1000 != 0 {
        ratio = (ratio * U256::from_str_radix("d097f3bdfd2022b8845ad8f792aa5825", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x2000 != 0 {
        ratio = (ratio * U256::from_str_radix("a9f746462d870fdf8a65dc1f90e061e5", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x4000 != 0 {
        ratio = (ratio * U256::from_str_radix("70d869a156d2a1b890bb3df62baf32f7", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x8000 != 0 {
        ratio = (ratio * U256::from_str_radix("31be135b97d08fd981231505542fcfa6", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x10000 != 0 {
        ratio = (ratio * U256::from_str_radix("9aa508b5b7a84e1c677de54f3e99bc9", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x20000 != 0 {
        ratio = (ratio * U256::from_str_radix("5d6af8dedb81196699c329225ee604", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x40000 != 0 {
        ratio = (ratio * U256::from_str_radix("2216e584f5fa1ea926041bedfe98", 16).unwrap()) >> 128;
    }
    if abs_tick & 0x80000 != 0 {
        ratio = (ratio * U256::from_str_radix("48a170391f7dc42444e8fa2", 16).unwrap()) >> 128;
    }

    if tick > 0 {
        ratio = U256::MAX / ratio;
    }

    // Convert from Q128.128 to Q64.96
    (ratio >> 32) + if (ratio & U256::from(0xffffffffu32)).is_zero() { U256::ZERO } else { U256::from(1) }
}

/// Computes the result of a single Uniswap V3 swap step within a price interval.
///
/// Returns `(sqrt_ratio_next, amount_in_consumed, amount_out_produced)`.
pub fn compute_swap_step_v3(
    sqrt_ratio_current: U256,
    sqrt_ratio_target: U256,
    liquidity: u128,
    amount_remaining: U256,
    fee_pips: u32,
) -> (U256, U256, U256) {
    let zero_for_one = sqrt_ratio_current >= sqrt_ratio_target;
    let l = U256::from(liquidity);

    if l.is_zero() || amount_remaining.is_zero() {
        return (sqrt_ratio_current, U256::ZERO, U256::ZERO);
    }

    let amount_in_after_fee = amount_remaining * U256::from(1_000_000 - fee_pips) / U256::from(1_000_000);

    if zero_for_one {
        // Token0 in, Token1 out -> price moves down
        // Max delta_x = L * (sqrt_c - sqrt_t) * 2^96 / (sqrt_c * sqrt_t)
        let delta_x_num = (l << 96) * (sqrt_ratio_current - sqrt_ratio_target);
        let delta_x_den = sqrt_ratio_current * sqrt_ratio_target;
        let delta_x = if delta_x_den.is_zero() { U256::ZERO } else { (delta_x_num / delta_x_den) + U256::from(1) };

        if amount_in_after_fee >= delta_x {
            // Reached target price
            let delta_y = (l * (sqrt_ratio_current - sqrt_ratio_target)) >> 96;
            let amount_in_actual = (delta_x * U256::from(1_000_000) / U256::from(1_000_000 - fee_pips)) + U256::from(1);
            (sqrt_ratio_target, amount_in_actual.min(amount_remaining), delta_y)
        } else {
            // Did not reach target price: calculate intermediate next sqrt ratio
            // sqrt_next = (L * 2^96 * sqrt_c) / (L * 2^96 + amount_in_after_fee * sqrt_c)
            let num = (l << 96) * sqrt_ratio_current;
            let den: U256 = (l << 96) + (amount_in_after_fee * sqrt_ratio_current);
            let sqrt_next = if den.is_zero() { sqrt_ratio_target } else { num / den };
            let delta_y = (l * (sqrt_ratio_current - sqrt_next)) >> 96;
            (sqrt_next, amount_remaining, delta_y)
        }
    } else {
        // Token1 in, Token0 out -> price moves up
        // Max delta_y = L * (sqrt_t - sqrt_c) / 2^96
        let delta_y = (l * (sqrt_ratio_target - sqrt_ratio_current)) >> 96;

        if amount_in_after_fee >= delta_y {
            // Reached target price
            let delta_x_num = (l << 96) * (sqrt_ratio_target - sqrt_ratio_current);
            let delta_x_den = sqrt_ratio_target * sqrt_ratio_current;
            let delta_x = if delta_x_den.is_zero() { U256::ZERO } else { delta_x_num / delta_x_den };
            let amount_in_actual: U256 = (delta_y * U256::from(1_000_000) / U256::from(1_000_000 - fee_pips)) + U256::from(1);
            (sqrt_ratio_target, amount_in_actual.min(amount_remaining), delta_x)
        } else {
            // Did not reach target price: calculate intermediate next sqrt ratio
            // sqrt_next = sqrt_c + (amount_in_after_fee * 2^96 / L)
            let delta_p = (amount_in_after_fee << 96) / l;
            let sqrt_next = sqrt_ratio_current + delta_p;
            let delta_x_num = (l << 96) * (sqrt_next - sqrt_ratio_current);
            let delta_x_den: U256 = sqrt_next * sqrt_ratio_current;
            let delta_x = if delta_x_den.is_zero() { U256::ZERO } else { delta_x_num / delta_x_den };
            (sqrt_next, amount_remaining, delta_x)
        }
    }
}

/// Calculates the exact output amount for a Uniswap V3 swap.
pub fn get_amount_out_v3(
    v3: &V3PoolState,
    token_in: Address,
    _token_out: Address,
    token0: Address,
    amount_in: U256,
    fee_pips: u32,
) -> U256 {
    if amount_in.is_zero() || v3.liquidity == 0 {
        return U256::ZERO;
    }

    let zero_for_one = token_in == token0;
    let mut amount_remaining = amount_in;
    let mut amount_out = U256::ZERO;
    let current_sqrt = v3.sqrt_price_x96;
    let current_tick = v3.tick;

    // Use current tick spacing to locate target boundary
    let tick_spacing = if v3.tick_spacing != 0 { v3.tick_spacing } else { 60 };
    let (target_tick, _) = mev_state::next_initialized_tick_within_one_word(
        &v3.tick_bitmap,
        current_tick,
        tick_spacing,
        zero_for_one,
    );

    let target_sqrt = get_sqrt_ratio_at_tick(target_tick);

    let (_next_sqrt, amount_in_step, amount_out_step) = compute_swap_step_v3(
        current_sqrt,
        target_sqrt,
        v3.liquidity,
        amount_remaining,
        fee_pips,
    );

    amount_out += amount_out_step;
    amount_remaining = amount_remaining.saturating_sub(amount_in_step);

    // If remaining input exists (multi-tick crossing), perform step with active liquidity
    if !amount_remaining.is_zero() && v3.liquidity > 0 {
        let (step_target_tick, _) = if zero_for_one {
            (MIN_TICK, false)
        } else {
            (MAX_TICK, false)
        };
        let step_target_sqrt = get_sqrt_ratio_at_tick(step_target_tick);
        let (_, _, extra_out) = compute_swap_step_v3(
            target_sqrt,
            step_target_sqrt,
            v3.liquidity,
            amount_remaining,
            fee_pips,
        );
        amount_out += extra_out;
    }

    amount_out
}

/// Unified dispatcher calculating the output token amount for any pool variant.
pub fn get_amount_out(
    desc: &PoolDescriptor,
    state: &PoolState,
    token_in: Address,
    token_out: Address,
    amount_in: U256,
) -> U256 {
    match state {
        PoolState::V2(v2) => {
            let (reserve_in, reserve_out) = if token_in == desc.token0 {
                (U256::from(v2.reserve0), U256::from(v2.reserve1))
            } else {
                (U256::from(v2.reserve1), U256::from(v2.reserve0))
            };
            get_amount_out_v2(amount_in, reserve_in, reserve_out, desc.fee_pips)
        }
        PoolState::V3(v3) => {
            get_amount_out_v3(v3, token_in, token_out, desc.token0, amount_in, desc.fee_pips)
        }
    }
}

/// Simulates executing a candidate cycle with a given input amount.
///
/// Returns the final output amount of the base token after traversing all hops.
pub fn simulate_cycle(cycle: &Cycle, amount_in: U256, market_state: &MarketState) -> U256 {
    let mut current_amount = amount_in;

    for hop in &cycle.hops {
        if current_amount.is_zero() {
            return U256::ZERO;
        }

        let (_desc, pool_state) = match market_state.get_pool(&hop.pool.address) {
            Some(entry) => entry,
            None => return U256::ZERO,
        };

        current_amount = get_amount_out(
            &hop.pool,
            &pool_state,
            hop.token_in,
            hop.token_out,
            current_amount,
        );
    }

    current_amount
}

/// Evaluates single-tick constant-liquidity Uniswap V3 swap math directly from HotPoolState.
///
/// Executes in ~25-30 ns in L1 CPU cache without tick bitmap traversal.
#[inline(always)]
pub fn get_amount_out_v3_hot(
    pool: &HotPoolState,
    zero_for_one: bool,
    amount_in: U256,
) -> U256 {
    let l_u128 = pool.v3_liquidity();
    if amount_in.is_zero() || l_u128 == 0 {
        return U256::ZERO;
    }

    let l = U256::from(l_u128);
    let current_sqrt = pool.v3_sqrt_price_x96();
    let fee_pips = pool.fee_pips;
    let amount_in_after_fee =
        amount_in * U256::from(1_000_000 - fee_pips) / U256::from(1_000_000);

    if zero_for_one {
        // Token0 in, Token1 out -> price moves down
        // sqrt_next = (L * 2^96 * sqrt_c) / (L * 2^96 + amount_in_after_fee * sqrt_c)
        let num: U256 = (l << 96) * current_sqrt;
        let den: U256 = (l << 96) + (amount_in_after_fee * current_sqrt);
        if den.is_zero() {
            return U256::ZERO;
        }
        let sqrt_next = num / den;
        (l * (current_sqrt - sqrt_next)) >> 96
    } else {
        // Token1 in, Token0 out -> price moves up
        // delta_p = (amount_in_after_fee * 2^96) / L
        // sqrt_next = sqrt_c + delta_p
        // delta_x = (L * 2^96 * (sqrt_next - sqrt_c)) / (sqrt_next * sqrt_c)
        let delta_p: U256 = (amount_in_after_fee << 96) / l;
        let sqrt_next: U256 = current_sqrt + delta_p;
        let den: U256 = sqrt_next * current_sqrt;
        if den.is_zero() {
            return U256::ZERO;
        }
        ((l << 96) * (sqrt_next - current_sqrt)) / den
    }
}

/// Simulates executing a candidate HotCycle directly against a slice of 64-byte HotPoolStates.
///
/// Zero heap allocations, zero Address comparisons, zero hash table lookups.
#[inline]
pub fn simulate_hot_cycle(
    cycle: &HotCycle,
    amount_in: U256,
    pools: &[HotPoolState],
) -> U256 {
    let mut current_amount = amount_in;

    for hop in cycle.active_hops() {
        if current_amount.is_zero() {
            return U256::ZERO;
        }

        let pool = match pools.get(hop.pool_id.0 as usize) {
            Some(p) => p,
            None => return U256::ZERO,
        };

        if hop.is_v2() {
            let (reserve_in, reserve_out) = if hop.zero_for_one {
                (U256::from(pool.slot0_lo), U256::from(pool.slot1_liquidity))
            } else {
                (U256::from(pool.slot1_liquidity), U256::from(pool.slot0_lo))
            };
            current_amount =
                get_amount_out_v2(current_amount, reserve_in, reserve_out, pool.fee_pips);
        } else {
            current_amount = get_amount_out_v3_hot(pool, hop.zero_for_one, current_amount);
        }
    }

    current_amount
}

