//! Thread-safe in-memory cache tracking AMM pool states with O(1) lookups.

use alloy::primitives::{Address, U256};
use dashmap::DashMap;
use mev_types::{PoolDescriptor, PoolState};
use std::sync::Arc;

/// Fast, thread-safe in-memory market state cache.
#[derive(Debug, Default, Clone)]
pub struct MarketState {
    /// Primary pool map: pool address -> (PoolDescriptor, PoolState).
    pools: Arc<DashMap<Address, (PoolDescriptor, PoolState)>>,
    /// Secondary index: (canonical token0, canonical token1) -> list of pool addresses.
    pair_to_pools: Arc<DashMap<(Address, Address), Vec<Address>>>,
}

impl MarketState {
    /// Constructs a new empty market state cache.
    pub fn new() -> Self {
        Self {
            pools: Arc::new(DashMap::new()),
            pair_to_pools: Arc::new(DashMap::new()),
        }
    }

    /// Helper to canonicalize a token pair key for secondary index lookup.
    #[inline]
    pub fn canonical_pair_key(token_a: Address, token_b: Address) -> (Address, Address) {
        if token_a < token_b {
            (token_a, token_b)
        } else {
            (token_b, token_a)
        }
    }

    /// Inserts or replaces a pool descriptor and its state.
    /// Also updates the secondary token-pair index.
    pub fn update_pool(&self, descriptor: PoolDescriptor, state: PoolState) {
        let pool_addr = descriptor.address;
        let pair_key = Self::canonical_pair_key(descriptor.token0, descriptor.token1);

        // Update secondary pair index if not already recorded
        let mut entry = self.pair_to_pools.entry(pair_key).or_default();
        if !entry.contains(&pool_addr) {
            entry.push(pool_addr);
        }

        // Insert or overwrite primary pool entry
        self.pools.insert(pool_addr, (descriptor, state));
    }

    /// Retrieves a cloned pool descriptor and state by pool address.
    #[inline]
    pub fn get_pool(&self, address: &Address) -> Option<(PoolDescriptor, PoolState)> {
        self.pools.get(address).map(|entry| entry.value().clone())
    }

    /// Retrieves all pool addresses that trade the given pair of tokens.
    #[inline]
    pub fn get_pools_for_pair(&self, token_a: Address, token_b: Address) -> Vec<Address> {
        let pair_key = Self::canonical_pair_key(token_a, token_b);
        self.pair_to_pools
            .get(&pair_key)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Atomically updates reserves for a Uniswap V2 / SushiSwap pool.
    /// Returns `true` if the pool existed and was updated, or `false` if not found or pool is not V2.
    pub fn update_v2_reserves(
        &self,
        address: &Address,
        reserve0: u128,
        reserve1: u128,
        block_timestamp_last: u32,
    ) -> bool {
        if let Some(mut entry) = self.pools.get_mut(address) {
            let (_, state) = entry.value_mut();
            if let PoolState::V2(v2) = state {
                v2.reserve0 = reserve0;
                v2.reserve1 = reserve1;
                v2.block_timestamp_last = block_timestamp_last;
                return true;
            }
        }
        false
    }

    /// Atomically updates active price, tick, and liquidity for a Uniswap V3 pool.
    /// Returns `true` if the pool existed and was updated, or `false` if not found or pool is not V3.
    pub fn update_v3_state(
        &self,
        address: &Address,
        sqrt_price_x96: U256,
        tick: i32,
        liquidity: u128,
    ) -> bool {
        if let Some(mut entry) = self.pools.get_mut(address) {
            let (_, state) = entry.value_mut();
            if let PoolState::V3(v3) = state {
                v3.sqrt_price_x96 = sqrt_price_x96;
                v3.tick = tick;
                v3.liquidity = liquidity;
                return true;
            }
        }
        false
    }

    /// Updates a single initialized tick bitmap word for a Uniswap V3 pool.
    pub fn update_v3_tick_bitmap_word(
        &self,
        address: &Address,
        word_pos: i16,
        word: U256,
    ) -> bool {
        if let Some(mut entry) = self.pools.get_mut(address) {
            let (_, state) = entry.value_mut();
            if let PoolState::V3(v3) = state {
                v3.tick_bitmap.insert(word_pos, word);
                return true;
            }
        }
        false
    }

    /// Sets or merges multiple tick bitmap words for a Uniswap V3 pool.
    pub fn set_v3_tick_bitmap_words(
        &self,
        address: &Address,
        words: impl IntoIterator<Item = (i16, U256)>,
    ) -> bool {
        if let Some(mut entry) = self.pools.get_mut(address) {
            let (_, state) = entry.value_mut();
            if let PoolState::V3(v3) = state {
                for (word_pos, word) in words {
                    v3.tick_bitmap.insert(word_pos, word);
                }
                return true;
            }
        }
        false
    }

    /// Returns a vector of all registered pool addresses.
    pub fn all_pool_addresses(&self) -> Vec<Address> {
        self.pools.iter().map(|entry| *entry.key()).collect()
    }

    /// Returns a cloned snapshot of all tracked pools.
    pub fn all_pools(&self) -> Vec<(PoolDescriptor, PoolState)> {
        self.pools.iter().map(|entry| entry.value().clone()).collect()
    }

    /// Number of tracked pools in the cache.
    #[inline]
    pub fn len(&self) -> usize {
        self.pools.len()
    }

    /// Returns `true` if no pools are currently tracked.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.pools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use mev_types::{PoolVariant, V2PoolState, V3PoolState};
    use std::collections::BTreeMap;

    #[test]
    fn test_market_state_crud_and_indexing() {
        let state = MarketState::new();
        let pool_v2 = address!("1111111111111111111111111111111111111111");
        let pool_v3 = address!("4444444444444444444444444444444444444444");
        let token_a = address!("2222222222222222222222222222222222222222");
        let token_b = address!("3333333333333333333333333333333333333333");

        let desc_v2 = PoolDescriptor::new(pool_v2, PoolVariant::UniswapV2, token_a, token_b, 30);
        let v2_state = PoolState::V2(V2PoolState {
            reserve0: 1_000_000,
            reserve1: 2_000_000,
            block_timestamp_last: 1234567,
        });

        let desc_v3 = PoolDescriptor::new(pool_v3, PoolVariant::UniswapV3, token_a, token_b, 3000);
        let v3_state = PoolState::V3(V3PoolState {
            sqrt_price_x96: U256::from(1234567890u64),
            tick: 100,
            liquidity: 50_000_000,
            tick_spacing: 60,
            tick_bitmap: BTreeMap::new(),
        });

        state.update_pool(desc_v2.clone(), v2_state.clone());
        state.update_pool(desc_v3.clone(), v3_state.clone());

        assert_eq!(state.len(), 2);

        // Test pair index lookup in both token orders
        let pairs_ab = state.get_pools_for_pair(token_a, token_b);
        assert_eq!(pairs_ab.len(), 2);
        assert!(pairs_ab.contains(&pool_v2));
        assert!(pairs_ab.contains(&pool_v3));

        let pairs_ba = state.get_pools_for_pair(token_b, token_a);
        assert_eq!(pairs_ba, pairs_ab);

        // Test V2 reserve update
        let updated = state.update_v2_reserves(&pool_v2, 2_000_000, 4_000_000, 1234599);
        assert!(updated);
        let (_, fetched_v2) = state.get_pool(&pool_v2).unwrap();
        if let PoolState::V2(v2) = fetched_v2 {
            assert_eq!(v2.reserve0, 2_000_000);
            assert_eq!(v2.reserve1, 4_000_000);
            assert_eq!(v2.block_timestamp_last, 1234599);
        } else {
            panic!("Expected V2 pool state");
        }

        // Test V3 state update
        let updated_v3 = state.update_v3_state(&pool_v3, U256::from(999999u64), 200, 60_000_000);
        assert!(updated_v3);
        let updated_bitmap =
            state.update_v3_tick_bitmap_word(&pool_v3, 0, U256::from(0x12345678u64));
        assert!(updated_bitmap);

        let (_, fetched_v3) = state.get_pool(&pool_v3).unwrap();
        if let PoolState::V3(v3) = fetched_v3 {
            assert_eq!(v3.sqrt_price_x96, U256::from(999999u64));
            assert_eq!(v3.tick, 200);
            assert_eq!(v3.liquidity, 60_000_000);
            assert_eq!(v3.tick_bitmap.get(&0), Some(&U256::from(0x12345678u64)));
        } else {
            panic!("Expected V3 pool state");
        }
    }
}
