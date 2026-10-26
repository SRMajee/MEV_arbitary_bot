//! Graph traversal and bounded Depth-First Search for cyclic arbitrage discovery.
//!
//! Explores 2-hop and 3-hop directed cycles originating and terminating at the base token (WETH),
//! filtered by canonical tokens and targeted to pools touched by pending victim swaps.

use alloy::primitives::Address;
use mev_state::MarketState;
use mev_types::{Cycle, CycleHop, PoolDescriptor};

/// Bounded DFS graph traversal engine exploring cyclic arbitrage opportunities.
#[derive(Debug, Clone)]
pub struct GraphRouter {
    /// Base token starting and terminating all cycles (default: WETH).
    pub base_token: Address,
    /// Whitelist of canonical intermediate tokens to explore in multi-hop loops.
    pub token_whitelist: Vec<Address>,
}

impl GraphRouter {
    /// Creates a new graph router with the given base token and token whitelist.
    pub fn new(base_token: Address, token_whitelist: Vec<Address>) -> Self {
        Self {
            base_token,
            token_whitelist,
        }
    }

    /// Finds all 2-hop directed cycles involving the base token and whitelisted tokens.
    ///
    /// Loop structure: $WETH \xrightarrow{Pool_1} Token_A \xrightarrow{Pool_2} WETH$
    pub fn find_2hop_cycles(&self, market_state: &MarketState) -> Vec<Cycle> {
        let mut cycles = Vec::new();

        for &token_a in &self.token_whitelist {
            if token_a == self.base_token {
                continue;
            }

            let pool_addrs = market_state.get_pools_for_pair(self.base_token, token_a);
            let mut pools = Vec::new();
            for addr in pool_addrs {
                if let Some((desc, _)) = market_state.get_pool(&addr) {
                    pools.push(desc);
                }
            }

            if pools.len() < 2 {
                continue;
            }

            // Cross every distinct pair of pools for this token pair in both directions
            for i in 0..pools.len() {
                for j in (i + 1)..pools.len() {
                    let p1 = &pools[i];
                    let p2 = &pools[j];

                    // Direction 1: p1 then p2
                    let c1 = Cycle {
                        base_token: self.base_token,
                        hops: vec![
                            CycleHop {
                                pool: p1.clone(),
                                token_in: self.base_token,
                                token_out: token_a,
                            },
                            CycleHop {
                                pool: p2.clone(),
                                token_in: token_a,
                                token_out: self.base_token,
                            },
                        ],
                    };
                    if c1.validate() {
                        cycles.push(c1);
                    }

                    // Direction 2: p2 then p1
                    let c2 = Cycle {
                        base_token: self.base_token,
                        hops: vec![
                            CycleHop {
                                pool: p2.clone(),
                                token_in: self.base_token,
                                token_out: token_a,
                            },
                            CycleHop {
                                pool: p1.clone(),
                                token_in: token_a,
                                token_out: self.base_token,
                            },
                        ],
                    };
                    if c2.validate() {
                        cycles.push(c2);
                    }
                }
            }
        }

        cycles
    }

    /// Finds all 3-hop directed cycles involving the base token and pairs of whitelisted tokens.
    ///
    /// Loop structure: $WETH \xrightarrow{Pool_1} Token_A \xrightarrow{Pool_2} Token_B \xrightarrow{Pool_3} WETH$
    pub fn find_3hop_cycles(&self, market_state: &MarketState) -> Vec<Cycle> {
        let mut cycles = Vec::new();

        for i in 0..self.token_whitelist.len() {
            let token_a = self.token_whitelist[i];
            if token_a == self.base_token {
                continue;
            }

            let addrs_base_a = market_state.get_pools_for_pair(self.base_token, token_a);
            let pools_base_a: Vec<PoolDescriptor> = addrs_base_a
                .into_iter()
                .filter_map(|addr| market_state.get_pool(&addr).map(|(desc, _)| desc))
                .collect();
            if pools_base_a.is_empty() {
                continue;
            }

            for j in 0..self.token_whitelist.len() {
                if i == j {
                    continue;
                }
                let token_b = self.token_whitelist[j];
                if token_b == self.base_token {
                    continue;
                }

                let addrs_a_b = market_state.get_pools_for_pair(token_a, token_b);
                let pools_a_b: Vec<PoolDescriptor> = addrs_a_b
                    .into_iter()
                    .filter_map(|addr| market_state.get_pool(&addr).map(|(desc, _)| desc))
                    .collect();
                if pools_a_b.is_empty() {
                    continue;
                }

                let addrs_b_base = market_state.get_pools_for_pair(token_b, self.base_token);
                let pools_b_base: Vec<PoolDescriptor> = addrs_b_base
                    .into_iter()
                    .filter_map(|addr| market_state.get_pool(&addr).map(|(desc, _)| desc))
                    .collect();
                if pools_b_base.is_empty() {
                    continue;
                }

                for p1 in &pools_base_a {
                    for p2 in &pools_a_b {
                        for p3 in &pools_b_base {
                            let c = Cycle {
                                base_token: self.base_token,
                                hops: vec![
                                    CycleHop {
                                        pool: p1.clone(),
                                        token_in: self.base_token,
                                        token_out: token_a,
                                    },
                                    CycleHop {
                                        pool: p2.clone(),
                                        token_in: token_a,
                                        token_out: token_b,
                                    },
                                    CycleHop {
                                        pool: p3.clone(),
                                        token_in: token_b,
                                        token_out: self.base_token,
                                    },
                                ],
                            };
                            if c.validate() {
                                cycles.push(c);
                            }
                        }
                    }
                }
            }
        }

        cycles
    }

    /// Finds all candidate cycles (2-hop and 3-hop) that contain a specific pool touched by a victim.
    pub fn find_cycles_touching_pool(
        &self,
        market_state: &MarketState,
        touched_pool: Address,
    ) -> Vec<Cycle> {
        let mut matching = Vec::new();

        for cycle in self.find_2hop_cycles(market_state) {
            if cycle.hops.iter().any(|h| h.pool.address == touched_pool) {
                matching.push(cycle);
            }
        }

        for cycle in self.find_3hop_cycles(market_state) {
            if cycle.hops.iter().any(|h| h.pool.address == touched_pool) {
                matching.push(cycle);
            }
        }

        matching
    }
}
