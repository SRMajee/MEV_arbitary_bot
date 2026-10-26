//! Transaction execution, EIP-1559 formatting, and Flashbots bundle dispatch engine.

use alloy::primitives::Address;

/// Bundle executor configuration.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Address of deployed MevExecutor smart contract.
    pub executor_contract: Address,
}

impl ExecutorConfig {
    /// Creates a new executor configuration.
    pub fn new(executor_contract: Address) -> Self {
        Self { executor_contract }
    }
}
