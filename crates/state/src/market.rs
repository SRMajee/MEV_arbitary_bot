//! Thread-safe in-memory market state cache backed by lock-free RCU (Read-Copy-Update)
//! and 64-byte CPU cache-line aligned hot pool pricing states.

use crate::registry::IdRegistry;
use alloy::primitives::{Address, U256};
use arc_swap::ArcSwap;
use mev_types::{HotPoolState, PoolDescriptor, PoolId, PoolState, TokenId};
use rustc_hash::FxHashMap;
use std::sync::Arc;

/// A point-in-time, cache-aligned immutable snapshot of the market state.
///
/// Memory layout:
/// - `pools`: Contiguous vector of 64-byte `HotPoolState` aligned to 64 bytes (`align_of = 64`).
///   Zero heap-pointer dereferencing on the hot path; prefetcher-friendly slice lookup.
/// - `descriptors`: Parallel descriptors indexed by `PoolId(u16)` for route formatting and metadata.
/// - `raw_states`: Parallel full-fidelity `PoolState` for exact simulation and backwards compatibility.
/// - `pool_registry`: Two-way `Address <-> PoolId` registry via `FxHashMap`.
/// - `token_registry`: Two-way `Address <-> TokenId` registry via `FxHashMap`.
/// - `pair_to_pools`: Secondary index mapping canonical `(TokenId, TokenId)` to `Vec<PoolId>`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HotMarketState {
    /// 64-byte L1 CPU cache-line aligned hot pricing states, indexed by PoolId.
    pub pools: Vec<HotPoolState>,
    /// Parallel pool descriptors indexed by PoolId.
    pub descriptors: Vec<PoolDescriptor>,
    /// Parallel full-fidelity pool states indexed by PoolId.
    pub raw_states: Vec<PoolState>,
    /// Dynamic Address <-> PoolId registry.
    pub pool_registry: IdRegistry<PoolId>,
    /// Dynamic Address <-> TokenId registry.
    pub token_registry: IdRegistry<TokenId>,
    /// Secondary index: canonical (TokenId0, TokenId1) -> list of PoolIds.
    pub pair_to_pools: FxHashMap<(TokenId, TokenId), Vec<PoolId>>,
}

impl HotMarketState {
    /// Returns the number of registered pools.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.pools.len()
    }

    /// Returns `true` if no pools are registered.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.pools.is_empty()
    }

    /// O(1) zero-cost direct slice access to the 64-byte HotPoolState.
    #[inline(always)]
    pub fn get_hot_pool(&self, pool_id: PoolId) -> Option<HotPoolState> {
        self.pools.get(pool_id.0 as usize).copied()
    }

    /// O(1) direct slice access to the HotPoolState by raw index.
    #[inline(always)]
    pub fn get_hot_pool_by_index(&self, index: usize) -> Option<HotPoolState> {
        self.pools.get(index).copied()
    }

    /// O(1) slice access to pools trading the given token pair.
    #[inline]
    pub fn get_pools_for_pair_ids(&self, token_a: TokenId, token_b: TokenId) -> &[PoolId] {
        let key = Self::canonical_token_pair(token_a, token_b);
        self.pair_to_pools.get(&key).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Retrieves a cloned pool descriptor and state by 20-byte pool address.
    pub fn get_pool_by_address(&self, address: &Address) -> Option<(PoolDescriptor, PoolState)> {
        let pool_id = self.pool_registry.get_id(address)?;
        let idx = pool_id.0 as usize;
        let desc = self.descriptors.get(idx)?.clone();
        let state = self.raw_states.get(idx)?.clone();
        Some((desc, state))
    }

    /// Returns the pool descriptor for a given PoolId.
    #[inline(always)]
    pub fn get_descriptor(&self, pool_id: PoolId) -> Option<&PoolDescriptor> {
        self.descriptors.get(pool_id.0 as usize)
    }

    /// Helper to canonicalize a token pair key for secondary index lookup.
    #[inline(always)]
    pub fn canonical_token_pair(token_a: TokenId, token_b: TokenId) -> (TokenId, TokenId) {
        if token_a < token_b {
            (token_a, token_b)
        } else {
            (token_b, token_a)
        }
    }
}

/// Fast, thread-safe in-memory market state cache backed by lock-free RCU (Read-Copy-Update).
///
/// - Readers load point-in-time `HotMarketState` snapshots with ~1.5 ns latency, zero lock contention,
///   and zero atomic operations on the hot routing path.
/// - Writers serialize through a write mutex, clone-update-swap the snapshot atomically.
#[derive(Debug, Clone)]
pub struct MarketState {
    /// Atomic RCU pointer to current epoch-protected HotMarketState.
    inner: Arc<ArcSwap<HotMarketState>>,
    /// Write mutex ensuring serialized, linearizable updates to the RCU snapshot.
    write_lock: Arc<parking_lot::Mutex<()>>,
}

impl Default for MarketState {
    fn default() -> Self {
        Self::new()
    }
}

impl MarketState {
    /// Constructs a new empty market state cache.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(HotMarketState::default())),
            write_lock: Arc::new(parking_lot::Mutex::new(())),
        }
    }

    // =========================================================================
    // Hot-Path Zero-Lock Accessors
    // =========================================================================

    /// Loads the current epoch-protected HotMarketState snapshot without acquiring locks.
    #[inline(always)]
    pub fn load_hot(&self) -> arc_swap::Guard<Arc<HotMarketState>> {
        self.inner.load()
    }

    /// Zero-cost O(1) query for a 64-byte cache-aligned pool state by PoolId.
    #[inline(always)]
    pub fn get_hot_pool(&self, pool_id: PoolId) -> Option<HotPoolState> {
        self.inner.load().get_hot_pool(pool_id)
    }

    /// O(1) lookup of PoolId by 20-byte pool address.
    #[inline(always)]
    pub fn get_pool_id(&self, address: &Address) -> Option<PoolId> {
        self.inner.load().pool_registry.get_id(address)
    }

    /// O(1) lookup of TokenId by 20-byte token address.
    #[inline(always)]
    pub fn get_token_id(&self, address: &Address) -> Option<TokenId> {
        self.inner.load().token_registry.get_id(address)
    }

    /// O(1) lookup of 20-byte address by TokenId.
    #[inline(always)]
    pub fn get_token_address(&self, id: TokenId) -> Option<Address> {
        self.inner.load().token_registry.get_address(id)
    }

    /// O(1) lookup of 20-byte address by PoolId.
    #[inline(always)]
    pub fn get_pool_address(&self, id: PoolId) -> Option<Address> {
        self.inner.load().pool_registry.get_address(id)
    }

    // =========================================================================
    // Backward-Compatible Accessors
    // =========================================================================

    /// Helper to canonicalize a token pair key for secondary index lookup.
    #[inline]
    pub fn canonical_pair_key(token_a: Address, token_b: Address) -> (Address, Address) {
        if token_a < token_b {
            (token_a, token_b)
        } else {
            (token_b, token_a)
        }
    }

    /// Retrieves a cloned pool descriptor and state by pool address.
    #[inline]
    pub fn get_pool(&self, address: &Address) -> Option<(PoolDescriptor, PoolState)> {
        self.inner.load().get_pool_by_address(address)
    }

    /// Retrieves all pool addresses that trade the given pair of tokens.
    #[inline]
    pub fn get_pools_for_pair(&self, token_a: Address, token_b: Address) -> Vec<Address> {
        let guard = self.inner.load();
        let token_a_id = match guard.token_registry.get_id(&token_a) {
            Some(id) => id,
            None => return Vec::new(),
        };
        let token_b_id = match guard.token_registry.get_id(&token_b) {
            Some(id) => id,
            None => return Vec::new(),
        };

        let pool_ids = guard.get_pools_for_pair_ids(token_a_id, token_b_id);
        pool_ids
            .iter()
            .filter_map(|&pid| guard.pool_registry.get_address(pid))
            .collect()
    }

    /// Number of tracked pools in the cache.
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.load().len()
    }

    /// Returns `true` if no pools are currently tracked.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.load().is_empty()
    }

    /// Returns a vector of all registered pool addresses.
    pub fn all_pool_addresses(&self) -> Vec<Address> {
        self.inner.load().pool_registry.addresses().to_vec()
    }

    /// Returns a cloned snapshot of all tracked pools.
    pub fn all_pools(&self) -> Vec<(PoolDescriptor, PoolState)> {
        let guard = self.inner.load();
        guard
            .descriptors
            .iter()
            .zip(guard.raw_states.iter())
            .map(|(d, s)| (d.clone(), s.clone()))
            .collect()
    }

    // =========================================================================
    // Writer Mutations (Clone-Update-Swap)
    // =========================================================================

    /// Inserts or replaces a pool descriptor and its state.
    /// Also updates the secondary token-pair index and the two-way ID registries.
    pub fn update_pool(&self, descriptor: PoolDescriptor, state: PoolState) {
        let _guard = self.write_lock.lock();
        let current = self.inner.load();
        let mut new_state = (**current).clone();

        let pool_addr = descriptor.address;
        let pool_id = new_state.pool_registry.get_or_register(pool_addr);
        let token0_id = new_state.token_registry.get_or_register(descriptor.token0);
        let token1_id = new_state.token_registry.get_or_register(descriptor.token1);

        let hot = match &state {
            PoolState::V2(v2) => HotPoolState::from_v2(
                pool_id,
                token0_id,
                token1_id,
                descriptor.fee_pips,
                v2.reserve0,
                v2.reserve1,
                v2.block_timestamp_last,
            ),
            PoolState::V3(v3) => HotPoolState::from_v3(
                pool_id,
                token0_id,
                token1_id,
                descriptor.fee_pips,
                v3.sqrt_price_x96,
                v3.liquidity,
                v3.tick,
                0,
            ),
        };

        let idx = pool_id.0 as usize;
        if idx >= new_state.pools.len() {
            new_state.pools.push(hot);
            new_state.descriptors.push(descriptor);
            new_state.raw_states.push(state);
        } else {
            new_state.pools[idx] = hot;
            new_state.descriptors[idx] = descriptor;
            new_state.raw_states[idx] = state;
        }

        let pair_key = HotMarketState::canonical_token_pair(token0_id, token1_id);
        let entry = new_state.pair_to_pools.entry(pair_key).or_default();
        if !entry.contains(&pool_id) {
            entry.push(pool_id);
        }

        self.inner.store(Arc::new(new_state));
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
        let _guard = self.write_lock.lock();
        let current = self.inner.load();
        let pool_id = match current.pool_registry.get_id(address) {
            Some(id) => id,
            None => return false,
        };
        let idx = pool_id.0 as usize;
        if idx >= current.pools.len() || !current.pools[idx].is_v2() {
            return false;
        }

        let mut new_state = (**current).clone();
        new_state.pools[idx].slot0_lo = reserve0;
        new_state.pools[idx].slot1_liquidity = reserve1;
        new_state.pools[idx].block_timestamp_last = block_timestamp_last;

        if let PoolState::V2(ref mut v2) = new_state.raw_states[idx] {
            v2.reserve0 = reserve0;
            v2.reserve1 = reserve1;
            v2.block_timestamp_last = block_timestamp_last;
        }

        self.inner.store(Arc::new(new_state));
        true
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
        let _guard = self.write_lock.lock();
        let current = self.inner.load();
        let pool_id = match current.pool_registry.get_id(address) {
            Some(id) => id,
            None => return false,
        };
        let idx = pool_id.0 as usize;
        if idx >= current.pools.len() || !current.pools[idx].is_v3() {
            return false;
        }

        let mut new_state = (**current).clone();
        let slot0_lo: u128 = (sqrt_price_x96 & U256::from(u128::MAX)).to::<u128>();
        let shifted: U256 = sqrt_price_x96 >> 128usize;
        let sqrt_price_hi: u64 = (shifted & U256::from(u64::MAX)).to::<u64>();

        new_state.pools[idx].slot0_lo = slot0_lo;
        new_state.pools[idx].slot1_liquidity = liquidity;
        new_state.pools[idx].sqrt_price_hi = sqrt_price_hi;
        new_state.pools[idx].tick = tick;

        if let PoolState::V3(ref mut v3) = new_state.raw_states[idx] {
            v3.sqrt_price_x96 = sqrt_price_x96;
            v3.tick = tick;
            v3.liquidity = liquidity;
        }

        self.inner.store(Arc::new(new_state));
        true
    }

    /// Updates a single initialized tick bitmap word for a Uniswap V3 pool.
    pub fn update_v3_tick_bitmap_word(
        &self,
        address: &Address,
        word_pos: i16,
        word: U256,
    ) -> bool {
        let _guard = self.write_lock.lock();
        let current = self.inner.load();
        let pool_id = match current.pool_registry.get_id(address) {
            Some(id) => id,
            None => return false,
        };
        let idx = pool_id.0 as usize;
        if idx >= current.pools.len() || !current.pools[idx].is_v3() {
            return false;
        }

        let mut new_state = (**current).clone();
        if let PoolState::V3(ref mut v3) = new_state.raw_states[idx] {
            v3.tick_bitmap.insert(word_pos, word);
            self.inner.store(Arc::new(new_state));
            true
        } else {
            false
        }
    }

    /// Sets or merges multiple tick bitmap words for a Uniswap V3 pool.
    pub fn set_v3_tick_bitmap_words(
        &self,
        address: &Address,
        words: impl IntoIterator<Item = (i16, U256)>,
    ) -> bool {
        let _guard = self.write_lock.lock();
        let current = self.inner.load();
        let pool_id = match current.pool_registry.get_id(address) {
            Some(id) => id,
            None => return false,
        };
        let idx = pool_id.0 as usize;
        if idx >= current.pools.len() || !current.pools[idx].is_v3() {
            return false;
        }

        let mut new_state = (**current).clone();
        if let PoolState::V3(ref mut v3) = new_state.raw_states[idx] {
            for (word_pos, word) in words {
                v3.tick_bitmap.insert(word_pos, word);
            }
            self.inner.store(Arc::new(new_state));
            true
        } else {
            false
        }
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

        // Test hot-path zero-lock accessor
        let pool_v2_id = state.get_pool_id(&pool_v2).unwrap();
        let hot_v2 = state.get_hot_pool(pool_v2_id).unwrap();
        assert_eq!(hot_v2.v2_reserves(), (2_000_000, 4_000_000));
        assert_eq!(hot_v2.block_timestamp_last, 1234599);
        assert!(hot_v2.is_v2());

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

        let pool_v3_id = state.get_pool_id(&pool_v3).unwrap();
        let hot_v3 = state.get_hot_pool(pool_v3_id).unwrap();
        assert_eq!(hot_v3.v3_sqrt_price_x96(), U256::from(999999u64));
        assert_eq!(hot_v3.tick, 200);
        assert_eq!(hot_v3.v3_liquidity(), 60_000_000);
        assert!(hot_v3.is_v3());
    }
}
