//! Pre-computed line-graph cycle table and inverted pool-to-cycles index.
//!
//! Replaces runtime graph DFS traversals with O(1) table lookups, eliminating
//! heap allocations and address comparisons on the hot tick-to-trade path.

use alloy::primitives::Address;
use mev_state::{HotMarketState, MarketState};
use mev_types::{Cycle, CycleHop, HotCycle, HotCycleHop, PoolId, TokenId};
use smallvec::SmallVec;

/// Pre-computed cyclic arbitrage topology table with an inverted pool index.
#[derive(Debug, Clone, Default)]
pub struct CycleTable {
    /// Dense contiguous storage of all valid 2-hop and 3-hop cycles.
    pub cycles: Vec<HotCycle>,
    /// Inverted index mapping `PoolId` to indices of cycles touching that pool.
    pub pool_to_cycles: Vec<SmallVec<[u16; 32]>>,
    /// Base token ID (typically WETH).
    pub base_token: TokenId,
}

impl CycleTable {
    /// Constructs a pre-computed cycle table from current market state and whitelist.
    pub fn build(
        market_state: &MarketState,
        base_token_addr: Address,
        whitelist: &[Address],
    ) -> Self {
        let hot_guard = market_state.load_hot();
        let base_token_id = match hot_guard.token_registry.get_id(&base_token_addr) {
            Some(id) => id,
            None => return Self::default(),
        };

        let mut whitelisted_ids: Vec<TokenId> = Vec::new();
        for &addr in whitelist {
            if addr == base_token_addr {
                continue;
            }
            if let Some(id) = hot_guard.token_registry.get_id(&addr) {
                if !whitelisted_ids.contains(&id) {
                    whitelisted_ids.push(id);
                }
            }
        }

        let mut cycles = Vec::new();

        // 1. Pre-compute 2-hop cycles: WETH -> TokenA -> WETH
        for &token_a in &whitelisted_ids {
            let pool_ids = hot_guard.get_pools_for_pair_ids(base_token_id, token_a);
            if pool_ids.len() < 2 {
                continue;
            }

            for i in 0..pool_ids.len() {
                for j in (i + 1)..pool_ids.len() {
                    let p1_id = pool_ids[i];
                    let p2_id = pool_ids[j];
                    let p1 = match hot_guard.get_hot_pool(p1_id) {
                        Some(p) => p,
                        None => continue,
                    };
                    let p2 = match hot_guard.get_hot_pool(p2_id) {
                        Some(p) => p,
                        None => continue,
                    };

                    // Direction 1: p1 (base -> token_a), then p2 (token_a -> base)
                    let hop1 = HotCycleHop::new(
                        p1_id,
                        base_token_id,
                        token_a,
                        p1.token0 == base_token_id,
                        p1.variant,
                    );
                    let hop2 = HotCycleHop::new(
                        p2_id,
                        token_a,
                        base_token_id,
                        p2.token0 == token_a,
                        p2.variant,
                    );
                    cycles.push(HotCycle::new_2hop(hop1, hop2, base_token_id));

                    // Direction 2: p2 (base -> token_a), then p1 (token_a -> base)
                    let hop1 = HotCycleHop::new(
                        p2_id,
                        base_token_id,
                        token_a,
                        p2.token0 == base_token_id,
                        p2.variant,
                    );
                    let hop2 = HotCycleHop::new(
                        p1_id,
                        token_a,
                        base_token_id,
                        p1.token0 == token_a,
                        p1.variant,
                    );
                    cycles.push(HotCycle::new_2hop(hop1, hop2, base_token_id));
                }
            }
        }

        // 2. Pre-compute 3-hop cycles: WETH -> TokenA -> TokenB -> WETH
        for i in 0..whitelisted_ids.len() {
            let token_a = whitelisted_ids[i];
            let pools_base_a = hot_guard.get_pools_for_pair_ids(base_token_id, token_a);
            if pools_base_a.is_empty() {
                continue;
            }

            for j in 0..whitelisted_ids.len() {
                if i == j {
                    continue;
                }
                let token_b = whitelisted_ids[j];
                let pools_a_b = hot_guard.get_pools_for_pair_ids(token_a, token_b);
                if pools_a_b.is_empty() {
                    continue;
                }

                let pools_b_base = hot_guard.get_pools_for_pair_ids(token_b, base_token_id);
                if pools_b_base.is_empty() {
                    continue;
                }

                for &p1_id in pools_base_a {
                    for &p2_id in pools_a_b {
                        for &p3_id in pools_b_base {
                            let p1 = match hot_guard.get_hot_pool(p1_id) {
                                Some(p) => p,
                                None => continue,
                            };
                            let p2 = match hot_guard.get_hot_pool(p2_id) {
                                Some(p) => p,
                                None => continue,
                            };
                            let p3 = match hot_guard.get_hot_pool(p3_id) {
                                Some(p) => p,
                                None => continue,
                            };

                            let hop1 = HotCycleHop::new(
                                p1_id,
                                base_token_id,
                                token_a,
                                p1.token0 == base_token_id,
                                p1.variant,
                            );
                            let hop2 = HotCycleHop::new(
                                p2_id,
                                token_a,
                                token_b,
                                p2.token0 == token_a,
                                p2.variant,
                            );
                            let hop3 = HotCycleHop::new(
                                p3_id,
                                token_b,
                                base_token_id,
                                p3.token0 == token_b,
                                p3.variant,
                            );

                            cycles.push(HotCycle::new_3hop(hop1, hop2, hop3, base_token_id));
                        }
                    }
                }
            }
        }

        // 3. Build inverted pool_to_cycles index
        let total_pools = hot_guard.pools.len();
        let mut pool_to_cycles = vec![SmallVec::<[u16; 32]>::new(); total_pools];

        for (cycle_idx, cycle) in cycles.iter().enumerate() {
            let c_id = cycle_idx as u16;
            for hop in cycle.active_hops() {
                let pool_idx = hop.pool_id.0 as usize;
                if pool_idx < pool_to_cycles.len() {
                    if !pool_to_cycles[pool_idx].contains(&c_id) {
                        pool_to_cycles[pool_idx].push(c_id);
                    }
                }
            }
        }

        Self {
            cycles,
            pool_to_cycles,
            base_token: base_token_id,
        }
    }

    /// O(1) query returning all cycle IDs that touch the given pool.
    #[inline(always)]
    pub fn get_cycles_touching_pool(&self, pool_id: PoolId) -> &[u16] {
        let idx = pool_id.0 as usize;
        self.pool_to_cycles.get(idx).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Direct slice access to a pre-computed HotCycle by index.
    #[inline(always)]
    pub fn get_cycle(&self, cycle_id: u16) -> Option<&HotCycle> {
        self.cycles.get(cycle_id as usize)
    }

    /// Number of pre-computed cycles.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.cycles.len()
    }

    /// Returns true if no cycles are stored.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.cycles.is_empty()
    }

    /// Reconstructs a full `mev_types::Cycle` from a `HotCycle` using descriptor metadata.
    /// Used only once when materializing the winning opportunity into an execution bundle.
    pub fn materialize_cycle(
        &self,
        hot_cycle: &HotCycle,
        hot_state: &HotMarketState,
    ) -> Option<Cycle> {
        let base_token_addr = hot_state.token_registry.get_address(hot_cycle.base_token)?;
        let mut hops = Vec::with_capacity(hot_cycle.hop_count as usize);

        for hop in hot_cycle.active_hops() {
            let desc = hot_state.descriptors.get(hop.pool_id.0 as usize)?.clone();
            let token_in = hot_state.token_registry.get_address(hop.token_in)?;
            let token_out = hot_state.token_registry.get_address(hop.token_out)?;
            hops.push(CycleHop {
                pool: desc,
                token_in,
                token_out,
            });
        }

        let cycle = Cycle {
            hops,
            base_token: base_token_addr,
        };

        if cycle.validate() {
            Some(cycle)
        } else {
            None
        }
    }
}
