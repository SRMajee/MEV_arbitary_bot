//! Transaction execution, EIP-1559 formatting, and Flashbots bundle dispatch engine.

pub mod bundle;
pub mod relay;
pub mod signer;

use alloy::{primitives::Address, sol};

pub use bundle::{
    calculate_next_block_base_fee, CallBundleParams, CallBundleResponse, CallBundleTxResult,
    JsonRpcRequest, MevBundle, SendBundleParams, SendBundleResponse, MIN_BASE_FEE_WEI,
};
pub use relay::{
    RelayBroadcastResult, RelayClient, BEAVER_RELAY_URL, BUILDER0X69_RELAY_URL,
    FLASHBOTS_RELAY_URL, TITAN_RELAY_URL,
};
pub use signer::BundleSigner;

// Ingest compiled Foundry artifact directly at compile-time for zero-drift type safety
sol!(
    #[sol(rpc)]
    MevExecutorContract,
    "../../contracts/out/MevExecutor.sol/MevExecutor.json"
);

/// Bundle executor configuration.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Address of deployed MevExecutor smart contract.
    pub executor_contract: Address,
    /// EIP-1559 Priority Fee (tip) in wei (default: 0 or 1 gwei).
    pub priority_fee_wei: u64,
    /// Execution gas limit for MevExecutor direct backruns.
    pub gas_limit: u64,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            executor_contract: Address::ZERO,
            priority_fee_wei: 1_000_000_000, // 1 gwei
            gas_limit: 500_000,
        }
    }
}

impl ExecutorConfig {
    /// Creates a new executor configuration.
    pub fn new(executor_contract: Address) -> Self {
        Self {
            executor_contract,
            ..Default::default()
        }
    }

    /// Sets custom priority fee.
    pub fn with_priority_fee(mut self, fee: u64) -> Self {
        self.priority_fee_wei = fee;
        self
    }

    /// Sets custom gas limit.
    pub fn with_gas_limit(mut self, gas_limit: u64) -> Self {
        self.gas_limit = gas_limit;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, U256};
    use MevExecutor::*;

    #[test]
    fn test_sol_bindings_and_executor_types() {
        let hop = Hop {
            protocol: 0, // UniswapV2
            pool: address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"),
            tokenIn: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            tokenOut: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            fee: alloy::primitives::Uint::from(3000u32),
            sqrtPriceLimitX96: alloy::primitives::Uint::ZERO,
        };

        let params = ExecutionParams {
            amountIn: U256::from(1_000_000_000_000_000_000u64),
            minProfit: U256::from(10_000_000_000_000_000u64),
            bribeBps: 9000,
            hops: vec![hop],
        };

        assert_eq!(params.bribeBps, 9000);
        assert_eq!(params.hops.len(), 1);

        assert!(!MevExecutorContract::BYTECODE.is_empty());

        use alloy::sol_types::SolCall;
        let call = MevExecutorContract::executeArbitrageDirectCall { params };
        let encoded = call.abi_encode();
        assert!(!encoded.is_empty());
    }
}
