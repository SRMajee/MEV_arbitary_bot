//! In-memory market state tracking AMM pool reserves, tick bitmaps, and real-time synchronizers.

pub mod market;
pub mod multicall;
pub mod registry;
pub mod sync;
pub mod tick_math;
pub mod whitelist;

pub use market::{HotMarketState, MarketState};
pub use multicall::{MulticallLoader, MULTICALL3_ADDRESS};
pub use registry::IdRegistry;
pub use sync::StateSynchronizer;
pub use tick_math::{
    compress_tick, next_initialized_tick_within_one_word, tick_to_bit_pos, tick_to_word_pos,
    MAX_TICK, MIN_TICK,
};
pub use whitelist::{
    default_canonical_v2_pools, default_canonical_v3_pools, DAI, SUSHISWAP_V2_WETH_USDC,
    SUSHISWAP_V2_WETH_USDT, UNISWAP_V2_DAI_WETH, UNISWAP_V2_WETH_USDC, UNISWAP_V2_WETH_USDT,
    UNISWAP_V3_WBTC_WETH_3000, UNISWAP_V3_WETH_USDC_3000, UNISWAP_V3_WETH_USDC_500,
    UNISWAP_V3_WETH_USDT_3000, UNISWAP_V3_WETH_USDT_500, USDC, USDT, WBTC, WETH,
};
