//! Paradigm reth Execution Extension (ExEx) in-memory zero-copy stream adapter.
//!
//! When mounted directly inside an execution client node (e.g. Paradigm's reth ExEx),
//! the ingestion pipeline bypasses WebSocket JSON-RPC serialization, HTTP transport,
//! and string parsing completely. Transaction primitives are received as native in-memory
//! zero-copy byte buffers directly from the node's transaction pool.

use alloy::primitives::{Address, Bytes, B256, U256};
use crate::decoder::CalldataDecoder;
use mev_types::DecodedSwap;

/// An in-memory unconfirmed transaction received directly from a reth ExEx notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExExTransaction {
    pub tx_hash: B256,
    pub from: Address,
    pub to: Option<Address>,
    pub value: U256,
    pub calldata: Bytes,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
}

impl ExExTransaction {
    /// Creates a new in-memory transaction.
    pub fn new(
        tx_hash: B256,
        from: Address,
        to: Option<Address>,
        value: U256,
        calldata: Bytes,
        gas_limit: u64,
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
    ) -> Self {
        Self {
            tx_hash,
            from,
            to,
            value,
            calldata,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
        }
    }
}

/// Zero-copy trait interface for ingesting transactions directly from in-memory node pipelines.
pub trait InMemoryTxSource: Send + Sync {
    /// Ingests an in-memory transaction and immediately extracts target DEX swaps without serialization overhead.
    fn decode_in_memory(&self, tx: &ExExTransaction) -> Vec<DecodedSwap>;
}

/// In-process adapter bridging reth ExEx notifications to the MEV Bot pipeline.
#[derive(Debug, Default, Clone)]
pub struct ExExStreamAdapter;

impl InMemoryTxSource for ExExStreamAdapter {
    #[inline(always)]
    fn decode_in_memory(&self, tx: &ExExTransaction) -> Vec<DecodedSwap> {
        let to = match tx.to {
            Some(addr) => addr,
            None => return Vec::new(),
        };

        if !crate::routers::is_target_router(&to) {
            return Vec::new();
        }

        CalldataDecoder::decode(tx.tx_hash, to, tx.value, &tx.calldata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, b256};
    use alloy::sol_types::SolCall;
    use crate::decoder::IUniswapV2RouterABI;
    use crate::routers::UNISWAP_V2_ROUTER_02;

    #[test]
    fn test_exex_adapter_in_memory_decoding() {
        let adapter = ExExStreamAdapter;
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        let to = address!("1111111111111111111111111111111111111111");

        let call = IUniswapV2RouterABI::swapExactTokensForTokensCall {
            amountIn: U256::from(100_000_000),
            amountOutMin: U256::from(50_000_000),
            path: vec![weth, usdc],
            to,
            deadline: U256::from(1999999999),
        };

        let calldata = Bytes::from(call.abi_encode());

        let tx = ExExTransaction::new(
            b256!("1111111111111111111111111111111111111111111111111111111111111111"),
            address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            Some(UNISWAP_V2_ROUTER_02),
            U256::ZERO,
            calldata,
            300_000,
            30_000_000_000,
            2_000_000_000,
        );

        let swaps = adapter.decode_in_memory(&tx);
        assert_eq!(swaps.len(), 1);
        assert_eq!(swaps[0].router, UNISWAP_V2_ROUTER_02);
        assert_eq!(swaps[0].token_in, weth);
        assert_eq!(swaps[0].token_out, usdc);
    }
}
