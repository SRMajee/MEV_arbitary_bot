//! Ingestion engine for mempool pending transactions and new block headers.

use mev_types::Token;

/// Collector configuration.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// Target WETH token address.
    pub weth: Token,
}

impl CollectorConfig {
    /// Creates a new collector configuration.
    pub fn new(weth: Token) -> Self {
        Self { weth }
    }
}
