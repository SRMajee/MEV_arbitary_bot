//! Real-time event log synchronizer updating canonical pool states on confirmed blocks.

use alloy::{
    primitives::U256,
    rpc::types::Log,
    sol,
    sol_types::SolEvent,
};
use tracing::trace;

use crate::market::MarketState;

sol! {
    interface IUniswapV2Events {
        event Sync(uint112 reserve0, uint112 reserve1);
    }

    interface IUniswapV3Events {
        event Swap(
            address indexed sender,
            address indexed recipient,
            int256 amount0,
            int256 amount1,
            uint160 sqrtPriceX96,
            uint128 liquidity,
            int24 tick
        );
    }
}

/// Synchronizes confirmed block event logs to canonical `MarketState`.
#[derive(Debug, Clone)]
pub struct StateSynchronizer {
    state: MarketState,
}

impl StateSynchronizer {
    /// Creates a new synchronizer bound to a target `MarketState`.
    pub fn new(state: MarketState) -> Self {
        Self { state }
    }

    /// Processes a single confirmed log event.
    ///
    /// Returns `true` if the log corresponded to a tracked pool and updated state.
    pub fn process_log(&self, log: &Log, block_timestamp: u32) -> bool {
        let pool_addr = log.address();

        // Fast O(1) discard if pool is not tracked in our cache
        if self.state.get_pool(&pool_addr).is_none() {
            return false;
        }

        let topics = log.topics();
        let Some(topic0) = topics.first() else {
            return false;
        };

        // 1. Uniswap V2 / SushiSwap Sync event
        if *topic0 == IUniswapV2Events::Sync::SIGNATURE_HASH {
            if let Ok(sync) = IUniswapV2Events::Sync::decode_log_data(log.data()) {
                trace!(%pool_addr, reserve0 = %sync.reserve0, reserve1 = %sync.reserve1, "Applying V2 Sync");
                return self.state.update_v2_reserves(
                    &pool_addr,
                    sync.reserve0.to::<u128>(),
                    sync.reserve1.to::<u128>(),
                    block_timestamp,
                );
            }
        }

        // 2. Uniswap V3 Swap event
        if *topic0 == IUniswapV3Events::Swap::SIGNATURE_HASH {
            if let Ok(swap) = IUniswapV3Events::Swap::decode_log_data(log.data()) {
                trace!(
                    %pool_addr,
                    sqrt_price_x96 = %swap.sqrtPriceX96,
                    tick = swap.tick.as_i32(),
                    liquidity = swap.liquidity,
                    "Applying V3 Swap"
                );
                return self.state.update_v3_state(
                    &pool_addr,
                    U256::from(swap.sqrtPriceX96),
                    swap.tick.as_i32(),
                    swap.liquidity,
                );
            }
        }

        false
    }

    /// Processes a batch of confirmed logs from a newly mined block.
    pub fn process_block_logs(&self, logs: &[Log], block_timestamp: u32) -> usize {
        let mut updated = 0;
        for log in logs {
            if self.process_log(log, block_timestamp) {
                updated += 1;
            }
        }
        updated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, Uint};
    use mev_types::{PoolDescriptor, PoolState, PoolVariant, V2PoolState};

    #[test]
    fn test_sync_v2_event() {
        let state = MarketState::new();
        let sync_engine = StateSynchronizer::new(state.clone());

        let pool_addr = address!("1111111111111111111111111111111111111111");
        let token_a = address!("2222222222222222222222222222222222222222");
        let token_b = address!("3333333333333333333333333333333333333333");

        let desc = PoolDescriptor::new(pool_addr, PoolVariant::UniswapV2, token_a, token_b, 30);
        let v2_state = PoolState::V2(V2PoolState {
            reserve0: 100_000,
            reserve1: 200_000,
            block_timestamp_last: 1000,
        });
        state.update_pool(desc, v2_state);

        // Encode Sync event log
        let sync_event = IUniswapV2Events::Sync {
            reserve0: Uint::from(555_000u64),
            reserve1: Uint::from(999_000u64),
        };
        let log_data = sync_event.encode_log_data();

        let raw_log = alloy::primitives::Log {
            address: pool_addr,
            data: log_data,
        };
        let rpc_log: Log = Log {
            inner: raw_log,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        let applied = sync_engine.process_log(&rpc_log, 2000);
        assert!(applied);

        let (_, updated_state) = state.get_pool(&pool_addr).unwrap();
        if let PoolState::V2(v2) = updated_state {
            assert_eq!(v2.reserve0, 555_000);
            assert_eq!(v2.reserve1, 999_000);
            assert_eq!(v2.block_timestamp_last, 2000);
        } else {
            panic!("Expected V2 state");
        }
    }
}
