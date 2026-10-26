//! Core MEV domain primitives, types, and pool representations.
//!
//! Designed for zero-overhead passing between ingestion, state tracking, simulation,
//! and bundle dispatch modules.

use alloy::primitives::{Address, U256};
use serde::{Deserialize, Serialize};

/// Supported Automated Market Maker protocol variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PoolVariant {
    /// Uniswap V2, SushiSwap V2 (Constant Product: x * y = k)
    UniswapV2,
    /// Uniswap V3, PancakeSwap V3 (Concentrated Liquidity / Tick Bitmap)
    UniswapV3,
}

/// Token descriptor containing on-chain identification and formatting metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Token {
    /// ERC20 contract address.
    pub address: Address,
    /// Token symbol (e.g., "WETH", "USDC").
    pub symbol: String,
    /// Token decimal precision.
    pub decimals: u8,
}

impl Token {
    /// Creates a new token descriptor.
    pub fn new(address: Address, symbol: impl Into<String>, decimals: u8) -> Self {
        Self {
            address,
            symbol: symbol.into(),
            decimals,
        }
    }
}

/// Static immutables defining a liquidity pool.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PoolDescriptor {
    /// Pool contract address on-chain.
    pub address: Address,
    /// Protocol variant (V2 vs V3).
    pub variant: PoolVariant,
    /// Lower address token in canonical order.
    pub token0: Address,
    /// Higher address token in canonical order.
    pub token1: Address,
    /// Fee expressed in hundredths of a bip (e.g., 3000 = 0.3% for V3, or 30 for 0.3% in V2 convention).
    pub fee_pips: u32,
}

impl PoolDescriptor {
    /// Constructs a validated pool descriptor ensuring canonical token ordering (`token0 < token1`).
    pub fn new(
        address: Address,
        variant: PoolVariant,
        token_a: Address,
        token_b: Address,
        fee_pips: u32,
    ) -> Self {
        let (token0, token1) = if token_a < token_b {
            (token_a, token_b)
        } else {
            (token_b, token_a)
        };

        Self {
            address,
            variant,
            token0,
            token1,
            fee_pips,
        }
    }

    /// Returns `true` if this pool involves the given token address.
    #[inline]
    pub fn contains_token(&self, token: Address) -> bool {
        self.token0 == token || self.token1 == token
    }

    /// Given an input token, returns the corresponding output token address in the pair.
    #[inline]
    pub fn opposite_token(&self, token: Address) -> Option<Address> {
        if self.token0 == token {
            Some(self.token1)
        } else if self.token1 == token {
            Some(self.token0)
        } else {
            None
        }
    }
}

/// In-memory snapshot of a Uniswap V2 / SushiSwap reserve pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct V2PoolState {
    /// Current reserve of token0.
    pub reserve0: u128,
    /// Current reserve of token1.
    pub reserve1: u128,
    /// Last block timestamp when reserves were updated.
    pub block_timestamp_last: u32,
}

/// In-memory snapshot of a Uniswap V3 pool's active tick & price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct V3PoolState {
    /// Current Q64.96 square root price.
    pub sqrt_price_x96: U256,
    /// Current active tick.
    pub tick: i32,
    /// Total active liquidity in the current tick.
    pub liquidity: u128,
}

/// Dynamic pool reserve / tick state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PoolState {
    V2(V2PoolState),
    V3(V3PoolState),
}

/// Single step inside a multi-hop arbitrage cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleHop {
    /// Pool descriptor for this swap hop.
    pub pool: PoolDescriptor,
    /// Input token for this hop.
    pub token_in: Address,
    /// Output token for this hop.
    pub token_out: Address,
}

/// A closed directed arbitrage loop (e.g., WETH -> TokenA -> TokenB -> WETH).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cycle {
    /// Ordered hops forming the closed ring.
    pub hops: Vec<CycleHop>,
    /// Base denomination token (typically WETH).
    pub base_token: Address,
}

impl Cycle {
    /// Validates that the cycle forms a continuous closed directed loop.
    pub fn validate(&self) -> bool {
        if self.hops.is_empty() {
            return false;
        }

        if self.hops.first().unwrap().token_in != self.base_token {
            return false;
        }

        if self.hops.last().unwrap().token_out != self.base_token {
            return false;
        }

        for i in 0..self.hops.len() - 1 {
            if self.hops[i].token_out != self.hops[i + 1].token_in {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn test_canonical_token_ordering() {
        let addr1 = address!("1111111111111111111111111111111111111111");
        let addr2 = address!("2222222222222222222222222222222222222222");
        let pool_addr = address!("3333333333333333333333333333333333333333");

        let pool = PoolDescriptor::new(pool_addr, PoolVariant::UniswapV2, addr2, addr1, 30);
        assert_eq!(pool.token0, addr1);
        assert_eq!(pool.token1, addr2);
        assert_eq!(pool.opposite_token(addr1), Some(addr2));
        assert_eq!(pool.opposite_token(addr2), Some(addr1));
    }

    #[test]
    fn test_cycle_validation() {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        let dai = address!("6B175474E89094C44Da98b954EedeAC495271d0F");

        let p1 = PoolDescriptor::new(Address::ZERO, PoolVariant::UniswapV2, weth, usdc, 30);
        let p2 = PoolDescriptor::new(Address::ZERO, PoolVariant::UniswapV2, usdc, dai, 30);
        let p3 = PoolDescriptor::new(Address::ZERO, PoolVariant::UniswapV2, dai, weth, 30);

        let valid_cycle = Cycle {
            hops: vec![
                CycleHop { pool: p1.clone(), token_in: weth, token_out: usdc },
                CycleHop { pool: p2.clone(), token_in: usdc, token_out: dai },
                CycleHop { pool: p3.clone(), token_in: dai, token_out: weth },
            ],
            base_token: weth,
        };
        assert!(valid_cycle.validate());

        let broken_cycle = Cycle {
            hops: vec![
                CycleHop { pool: p1, token_in: weth, token_out: usdc },
                CycleHop { pool: p2, token_in: dai, token_out: weth }, // discontinuous hop
            ],
            base_token: weth,
        };
        assert!(!broken_cycle.validate());
    }
}
