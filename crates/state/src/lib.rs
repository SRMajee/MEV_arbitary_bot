//! In-memory market state tracking AMM pool reserves across blocks.

use alloy::primitives::Address;
use dashmap::DashMap;
use mev_types::{PoolDescriptor, PoolState};
use std::sync::Arc;

/// In-memory cache tracking AMM pool states.
#[derive(Debug, Default, Clone)]
pub struct MarketState {
    pools: Arc<DashMap<Address, (PoolDescriptor, PoolState)>>,
}

impl MarketState {
    /// Constructs a new empty market state cache.
    pub fn new() -> Self {
        Self {
            pools: Arc::new(DashMap::new()),
        }
    }

    /// Inserts or updates a tracked pool.
    pub fn update_pool(&self, descriptor: PoolDescriptor, state: PoolState) {
        self.pools.insert(descriptor.address, (descriptor, state));
    }

    /// Retrieves a tracked pool by address.
    pub fn get_pool(&self, address: &Address) -> Option<(PoolDescriptor, PoolState)> {
        self.pools.get(address).map(|entry| entry.value().clone())
    }

    /// Number of tracked pools.
    pub fn len(&self) -> usize {
        self.pools.len()
    }

    /// Returns `true` if no pools are tracked.
    pub fn is_empty(&self) -> bool {
        self.pools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use mev_types::{PoolVariant, V2PoolState};

    #[test]
    fn test_market_state_crud() {
        let state = MarketState::new();
        let pool_addr = address!("1111111111111111111111111111111111111111");
        let token_a = address!("2222222222222222222222222222222222222222");
        let token_b = address!("3333333333333333333333333333333333333333");

        let desc = PoolDescriptor::new(pool_addr, PoolVariant::UniswapV2, token_a, token_b, 30);
        let v2_state = PoolState::V2(V2PoolState {
            reserve0: 1_000_000,
            reserve1: 2_000_000,
            block_timestamp_last: 1234567,
        });

        state.update_pool(desc.clone(), v2_state.clone());
        assert_eq!(state.len(), 1);
        let fetched = state.get_pool(&pool_addr);
        assert_eq!(fetched, Some((desc, v2_state)));
    }
}
