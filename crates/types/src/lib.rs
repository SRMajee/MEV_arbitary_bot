//! Core MEV domain primitives, types, and pool representations.
//!
//! Designed for zero-overhead passing between ingestion, state tracking, simulation,
//! and bundle dispatch modules.

use alloy::primitives::{Address, B256, U256};
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

/// In-memory snapshot of a Uniswap V3 pool's active tick, price, and sparse tick bitmap words.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct V3PoolState {
    /// Current Q64.96 square root price.
    pub sqrt_price_x96: U256,
    /// Current active tick.
    pub tick: i32,
    /// Total active liquidity in the current tick.
    pub liquidity: u128,
    /// Pool tick spacing (e.g., 10 for 500 bps, 60 for 3000 bps).
    pub tick_spacing: i32,
    /// Sparse map of initialized tick bitmap words around the active tick:
    /// word_pos (int16) -> bitmap word (256 bits).
    pub tick_bitmap: std::collections::BTreeMap<i16, U256>,
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

/// A normalized swap extracted from a pending victim transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodedSwap {
    /// Originating transaction hash.
    pub tx_hash: B256,
    /// Router contract target.
    pub router: Address,
    /// Protocol type (UniswapV2 or UniswapV3).
    pub protocol: PoolVariant,
    /// Address of input token.
    pub token_in: Address,
    /// Address of output token.
    pub token_out: Address,
    /// Token path (for multi-hop router calls).
    pub path: Vec<Address>,
    /// Exact or maximum amount of input token.
    pub amount_in: U256,
    /// Exact or minimum amount of output token.
    pub amount_out_min: U256,
    /// Recipient of the swap output.
    pub recipient: Address,
}

/// Compact 16-bit identifier for zero-overhead token indexing on the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct TokenId(pub u16);

impl From<u16> for TokenId {
    #[inline(always)]
    fn from(id: u16) -> Self {
        Self(id)
    }
}

impl From<TokenId> for u16 {
    #[inline(always)]
    fn from(id: TokenId) -> Self {
        id.0
    }
}

/// Compact 16-bit identifier for zero-overhead pool indexing on the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct PoolId(pub u16);

impl From<u16> for PoolId {
    #[inline(always)]
    fn from(id: u16) -> Self {
        Self(id)
    }
}

impl From<PoolId> for u16 {
    #[inline(always)]
    fn from(id: PoolId) -> Self {
        id.0
    }
}

/// A 64-byte L1 CPU cache-line aligned hot pricing state for Uniswap V2 and V3 pools.
///
/// Designed with zero padding overhead:
/// - Fits exactly inside one 64-byte L1 cache line (`size_of == 64`, `align_of == 64`).
/// - Replaces random heap pointer dereferences with direct array indexing `pools[pool_id.0 as usize]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C, align(64))]
pub struct HotPoolState {
    /// For V2: reserve0 (uint112). For V3: lower 128 bits of sqrtPriceX96 (uint160).
    pub slot0_lo: u128,              // 16 bytes (offset 0..16)
    /// For V2: reserve1 (uint112). For V3: active liquidity (uint128).
    pub slot1_liquidity: u128,       // 16 bytes (offset 16..32)
    /// For V3: upper 32 bits of sqrtPriceX96 (stored in u64 for alignment). Unused (0) in V2.
    pub sqrt_price_hi: u64,          // 8 bytes  (offset 32..40)
    pub tick: i32,                   // 4 bytes  (offset 40..44)
    pub fee_pips: u32,               // 4 bytes  (offset 44..48)
    pub block_timestamp_last: u32,   // 4 bytes  (offset 48..52)
    pub token0: TokenId,             // 2 bytes  (offset 52..54)
    pub token1: TokenId,             // 2 bytes  (offset 54..56)
    pub pool_id: PoolId,             // 2 bytes  (offset 56..58)
    pub variant: u8,                 // 1 byte   (offset 58..59) -> 0 = V2, 1 = V3
    pub _padding: [u8; 5],           // 5 bytes  (offset 59..64) -> Total = 64 bytes!
}

impl HotPoolState {
    /// Constructs a HotPoolState from a Uniswap V2 pool state.
    #[inline(always)]
    pub fn from_v2(
        pool_id: PoolId,
        token0: TokenId,
        token1: TokenId,
        fee_pips: u32,
        reserve0: u128,
        reserve1: u128,
        block_timestamp_last: u32,
    ) -> Self {
        Self {
            slot0_lo: reserve0,
            slot1_liquidity: reserve1,
            sqrt_price_hi: 0,
            tick: 0,
            fee_pips,
            block_timestamp_last,
            token0,
            token1,
            pool_id,
            variant: 0,
            _padding: [0; 5],
        }
    }

    /// Constructs a HotPoolState from a Uniswap V3 pool state.
    #[inline(always)]
    pub fn from_v3(
        pool_id: PoolId,
        token0: TokenId,
        token1: TokenId,
        fee_pips: u32,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_timestamp_last: u32,
    ) -> Self {
        let slot0_lo: u128 = (sqrt_price_x96 & U256::from(u128::MAX)).to::<u128>();
        let shifted: U256 = sqrt_price_x96 >> 128usize;
        let sqrt_price_hi: u64 = (shifted & U256::from(u64::MAX)).to::<u64>();
        Self {
            slot0_lo,
            slot1_liquidity: liquidity,
            sqrt_price_hi,
            tick,
            fee_pips,
            block_timestamp_last,
            token0,
            token1,
            pool_id,
            variant: 1,
            _padding: [0; 5],
        }
    }

    /// Returns Uniswap V2 (reserve0, reserve1) with zero overhead.
    #[inline(always)]
    pub fn v2_reserves(&self) -> (u128, u128) {
        (self.slot0_lo, self.slot1_liquidity)
    }

    /// Reconstructs the 160-bit Uniswap V3 sqrtPriceX96 as U256 with zero overhead.
    #[inline(always)]
    pub fn v3_sqrt_price_x96(&self) -> U256 {
        let lo = U256::from(self.slot0_lo);
        let hi = U256::from(self.sqrt_price_hi) << 128;
        lo | hi
    }

    /// Returns active Uniswap V3 liquidity.
    #[inline(always)]
    pub fn v3_liquidity(&self) -> u128 {
        self.slot1_liquidity
    }

    /// True if pool is Uniswap V2 variant.
    #[inline(always)]
    pub fn is_v2(&self) -> bool {
        self.variant == 0
    }

    /// True if pool is Uniswap V3 variant.
    #[inline(always)]
    pub fn is_v3(&self) -> bool {
        self.variant == 1
    }
}

/// Compact 8-byte cache-friendly single swap hop for pre-computed cycle routing.
///
/// Designed with exact byte alignment:
/// - pool_id: PoolId (2 bytes)
/// - token_in: TokenId (2 bytes)
/// - token_out: TokenId (2 bytes)
/// - zero_for_one: bool (1 byte, true if token_in == pool.token0)
/// - variant: u8 (1 byte, 0 = V2, 1 = V3)
/// Total = exactly 8 bytes (size_of == 8, align_of == 2)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[repr(C)]
pub struct HotCycleHop {
    pub pool_id: PoolId,     // 2 bytes
    pub token_in: TokenId,   // 2 bytes
    pub token_out: TokenId,  // 2 bytes
    pub zero_for_one: bool,  // 1 byte (true if token_in == pool.token0)
    pub variant: u8,         // 1 byte (0 = V2, 1 = V3) -> Exact Total = 8 bytes!
}

impl HotCycleHop {
    #[inline(always)]
    pub fn new(
        pool_id: PoolId,
        token_in: TokenId,
        token_out: TokenId,
        zero_for_one: bool,
        variant: u8,
    ) -> Self {
        Self {
            pool_id,
            token_in,
            token_out,
            zero_for_one,
            variant,
        }
    }

    #[inline(always)]
    pub fn is_v2(&self) -> bool {
        self.variant == 0
    }

    #[inline(always)]
    pub fn is_v3(&self) -> bool {
        self.variant == 1
    }
}

/// A compact, heap-free pre-computed cyclic arbitrage route.
///
/// Supports 2, 3, or up to 4 hops inline with zero heap allocation:
/// - 4 hops * 8 bytes = 32 bytes
/// - hop_count: 1 byte
/// - _padding: 1 byte
/// - base_token: 2 bytes
/// Total = 36 bytes (fits inside a single 64-byte L1 cache line).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[repr(C)]
pub struct HotCycle {
    pub hops: [HotCycleHop; 4],
    pub hop_count: u8,
    pub _padding: [u8; 1],
    pub base_token: TokenId,
}

impl HotCycle {
    /// Creates a new 2-hop cycle.
    #[inline(always)]
    pub fn new_2hop(hop0: HotCycleHop, hop1: HotCycleHop, base_token: TokenId) -> Self {
        Self {
            hops: [hop0, hop1, HotCycleHop::default(), HotCycleHop::default()],
            hop_count: 2,
            _padding: [0],
            base_token,
        }
    }

    /// Creates a new 3-hop cycle.
    #[inline(always)]
    pub fn new_3hop(
        hop0: HotCycleHop,
        hop1: HotCycleHop,
        hop2: HotCycleHop,
        base_token: TokenId,
    ) -> Self {
        Self {
            hops: [hop0, hop1, hop2, HotCycleHop::default()],
            hop_count: 3,
            _padding: [0],
            base_token,
        }
    }

    /// Returns a slice of the active hops in this cycle.
    #[inline(always)]
    pub fn active_hops(&self) -> &[HotCycleHop] {
        &self.hops[..self.hop_count as usize]
    }

    /// Returns true if all hops in this cycle are Uniswap V2 variant.
    #[inline(always)]
    pub fn is_pure_v2(&self) -> bool {
        self.active_hops().iter().all(|h| h.is_v2())
    }
}


/// A block header notification event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewBlockHeader {
    /// Block height number.
    pub number: u64,
    /// Block hash.
    pub hash: B256,
    /// EIP-1559 base fee per gas.
    pub base_fee_per_gas: Option<u128>,
    /// Unix block timestamp.
    pub timestamp: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn test_hot_pool_state_cache_line_alignment_and_accessors() {
        assert_eq!(
            std::mem::size_of::<HotPoolState>(),
            64,
            "HotPoolState MUST be exactly 64 bytes (1 CPU cache line)"
        );
        assert_eq!(
            std::mem::align_of::<HotPoolState>(),
            64,
            "HotPoolState MUST be aligned to 64 bytes"
        );

        // Test V2 packing & unpacking
        let v2 = HotPoolState::from_v2(
            PoolId(42),
            TokenId(1),
            TokenId(2),
            3000,
            100_000 * 10u128.pow(6),
            50 * 10u128.pow(18),
            1711000000,
        );
        assert!(v2.is_v2());
        assert!(!v2.is_v3());
        assert_eq!(v2.pool_id, PoolId(42));
        assert_eq!(v2.token0, TokenId(1));
        assert_eq!(v2.token1, TokenId(2));
        assert_eq!(v2.fee_pips, 3000);
        assert_eq!(v2.v2_reserves(), (100_000 * 10u128.pow(6), 50 * 10u128.pow(18)));

        // Test V3 packing & unpacking
        let original_sqrt = U256::from_str_radix("79228162514264337593543950336", 10).unwrap(); // 1.0 in Q64.96
        let v3 = HotPoolState::from_v3(
            PoolId(99),
            TokenId(10),
            TokenId(20),
            500,
            original_sqrt,
            1_000_000_000u128,
            -1200,
            1711000000,
        );
        assert!(v3.is_v3());
        assert!(!v3.is_v2());
        assert_eq!(v3.pool_id, PoolId(99));
        assert_eq!(v3.v3_sqrt_price_x96(), original_sqrt);
        assert_eq!(v3.v3_liquidity(), 1_000_000_000u128);
        assert_eq!(v3.tick, -1200);
    }

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

    #[test]
    fn test_hot_cycle_hop_and_cycle_size() {
        assert_eq!(
            std::mem::size_of::<HotCycleHop>(),
            8,
            "HotCycleHop MUST be exactly 8 bytes"
        );
        assert_eq!(
            std::mem::size_of::<HotCycle>(),
            36,
            "HotCycle MUST be exactly 36 bytes"
        );

        let hop1 = HotCycleHop::new(PoolId(1), TokenId(0), TokenId(1), true, 0);
        let hop2 = HotCycleHop::new(PoolId(2), TokenId(1), TokenId(0), false, 0);
        let cycle = HotCycle::new_2hop(hop1, hop2, TokenId(0));
        assert_eq!(cycle.hop_count, 2);
        assert!(cycle.is_pure_v2());
        assert_eq!(cycle.active_hops().len(), 2);
    }
}

