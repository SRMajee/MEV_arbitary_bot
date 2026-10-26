//! Ingestion engine for mempool pending transactions and new block headers.

pub mod decoder;
pub mod routers;
pub mod stream;

pub use decoder::{parse_v3_path, CalldataDecoder};
pub use routers::{
    is_target_router, SUSHISWAP_V2_ROUTER, UNISWAP_UNIVERSAL_ROUTER,
    UNISWAP_UNIVERSAL_ROUTER_LEGACY, UNISWAP_V2_ROUTER_02, UNISWAP_V3_SWAP_ROUTER_02,
};
pub use stream::{Collector, CollectorConfig, CollectorMetrics, CollectorStream, TransportMode};
