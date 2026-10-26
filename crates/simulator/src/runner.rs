//! EVM transaction execution runner wrapping revm MainnetEvm.

use alloy::primitives::{Address, Bytes, U256};
use revm::{
    context::{BlockEnv, CfgEnv, TxEnv},
    context_interface::result::{EVMError, ExecutionResult},
    Context, Database, DatabaseCommit, ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext,
};

/// High-level result of an in-process EVM simulation step.
#[derive(Debug, Clone)]
pub struct SimulationResult {
    /// True if the EVM executed to completion without reverting or halting.
    pub success: bool,
    /// Exact gas consumed by the transaction execution.
    pub gas_used: u64,
    /// Returned output bytes (or revert reason bytes).
    pub output: Bytes,
    /// Logs emitted during execution.
    pub logs: Vec<revm::primitives::Log>,
}

impl SimulationResult {
    /// Extracts a human-readable revert message if available.
    pub fn revert_reason(&self) -> Option<String> {
        if self.success || self.output.len() < 4 {
            return None;
        }

        // Standard Solidity Error(string) selector: 0x08c379a0
        if self.output.starts_with(&[0x08, 0xc3, 0x79, 0xa0]) && self.output.len() >= 68 {
            // ABI string decode: 32 bytes offset + 32 bytes length + string data
            let len_bytes = &self.output[36..68];
            let mut arr = [0u8; 32];
            arr.copy_from_slice(len_bytes);
            let str_len = U256::from_be_bytes(arr).to::<usize>();
            let start = 68;
            let end = (start + str_len).min(self.output.len());
            return String::from_utf8(self.output[start..end].to_vec()).ok();
        }

        // Standard Panic(uint256) selector: 0x4e487b71
        if self.output.starts_with(&[0x4e, 0x48, 0x7b, 0x71]) && self.output.len() >= 36 {
            let code_bytes = &self.output[4..36];
            let mut arr = [0u8; 32];
            arr.copy_from_slice(code_bytes);
            let panic_code = U256::from_be_bytes(arr);
            return Some(format!("Panic(0x{:x})", panic_code));
        }

        Some(format!("0x{}", alloy::hex::encode(&self.output)))
    }
}

/// In-process simulation runner executing transactions against a `revm` database.
#[derive(Debug, Clone)]
pub struct SimulationRunner {
    pub block: BlockEnv,
    pub cfg: CfgEnv,
}

impl Default for SimulationRunner {
    fn default() -> Self {
        Self::new(19_500_000, 1711000000)
    }
}

impl SimulationRunner {
    /// Creates a new simulation runner for a given block number and timestamp.
    pub fn new(block_number: u64, timestamp: u64) -> Self {
        let mut block = BlockEnv::default();
        block.number = U256::from(block_number);
        block.timestamp = U256::from(timestamp);
        block.gas_limit = 30_000_000;
        block.basefee = 20_000_000_000u64; // 20 gwei default

        let mut cfg = CfgEnv::default();
        cfg.spec = revm::primitives::hardfork::SpecId::CANCUN;
        cfg.disable_nonce_check = true;

        Self { block, cfg }
    }

    /// Sets the block coinbase fee recipient address.
    pub fn with_coinbase(mut self, coinbase: Address) -> Self {
        self.block.beneficiary = coinbase;
        self
    }

    /// Sets the block base fee in wei.
    pub fn with_basefee(mut self, basefee: u64) -> Self {
        self.block.basefee = basefee;
        self
    }

    /// Configures whether transaction nonce checks are enforced.
    pub fn with_nonce_check(mut self, enabled: bool) -> Self {
        self.cfg.disable_nonce_check = !enabled;
        self
    }

    /// Executes a transaction without committing state changes to the database.
    pub fn execute<DB: Database>(
        &self,
        db: &mut DB,
        tx: TxEnv,
    ) -> Result<SimulationResult, EVMError<<DB as Database>::Error>> {
        let ctx = Context::mainnet()
            .with_db(db)
            .with_block(self.block.clone())
            .with_cfg(self.cfg.clone());
        let mut evm = ctx.build_mainnet();

        let result = evm.transact(tx)?;
        Ok(Self::parse_result(result.result))
    }

    /// Executes a transaction AND commits its state mutations to the database.
    pub fn execute_and_commit<DB: Database + DatabaseCommit>(
        &self,
        db: &mut DB,
        tx: TxEnv,
    ) -> Result<SimulationResult, EVMError<<DB as Database>::Error>> {
        let ctx = Context::mainnet()
            .with_db(db)
            .with_block(self.block.clone())
            .with_cfg(self.cfg.clone());
        let mut evm = ctx.build_mainnet();

        let result = evm.transact_commit(tx)?;
        Ok(Self::parse_result(result))
    }

    fn parse_result(result: ExecutionResult) -> SimulationResult {
        match result {
            ExecutionResult::Success {
                gas, logs, output, ..
            } => SimulationResult {
                success: true,
                gas_used: gas.tx_gas_used(),
                output: output.into_data(),
                logs,
            },
            ExecutionResult::Revert {
                gas, output, logs, ..
            } => SimulationResult {
                success: false,
                gas_used: gas.tx_gas_used(),
                output,
                logs,
            },
            ExecutionResult::Halt { gas, logs, .. } => SimulationResult {
                success: false,
                gas_used: gas.tx_gas_used(),
                output: Bytes::default(),
                logs,
            },
        }
    }
}

/// Helper to construct a standard `TxEnv` for EVM execution.
pub fn build_call_tx(
    caller: Address,
    to: Address,
    calldata: Bytes,
    value: U256,
    gas_limit: u64,
    gas_price: Option<u128>,
) -> TxEnv {
    TxEnv {
        caller,
        kind: revm::primitives::TxKind::Call(to),
        data: calldata,
        value,
        gas_limit,
        gas_price: gas_price.unwrap_or(30_000_000_000u128),
        ..Default::default()
    }
}

