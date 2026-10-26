//! Atomic MEV bundle definitions, EIP-1559 base fee math, and JSON-RPC payloads.

use alloy::primitives::{hex, B256, Bytes};
use serde::{Deserialize, Serialize};

/// Maximum base fee multiplier for next block (EIP-1559 elasticity multiplier: 1.125x = 9/8).
pub const EIP1559_BASE_FEE_MULTIPLIER_NUMERATOR: u64 = 1125;
pub const EIP1559_BASE_FEE_MULTIPLIER_DENOMINATOR: u64 = 1000;

/// Minimum base fee allowed by the Ethereum protocol (in wei).
pub const MIN_BASE_FEE_WEI: u64 = 7;

/// Calculates the maximum possible base fee for the next block under 100% full block capacity.
///
/// Under EIP-1559, if the current block is 100% full, the base fee increases by exactly 12.5%:
/// $$\text{next\_base\_fee} = \left\lfloor \frac{\text{current\_base\_fee} \times 1125}{1000} \right\rfloor$$
pub fn calculate_next_block_base_fee(current_base_fee: u64) -> u64 {
    let next_base_fee = (current_base_fee as u128 * EIP1559_BASE_FEE_MULTIPLIER_NUMERATOR as u128)
        / EIP1559_BASE_FEE_MULTIPLIER_DENOMINATOR as u128;
    (next_base_fee as u64).max(MIN_BASE_FEE_WEI)
}

/// A structured MEV bundle targeting block inclusion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MevBundle {
    /// 0x-prefixed hex-encoded signed raw transactions in strict execution order.
    /// Index 0: Victim transaction.
    /// Index 1: Searcher MevExecutor backrun transaction.
    pub txs: Vec<String>,
    /// Target block number for bundle execution.
    pub block_number: u64,
    /// Minimum timestamp (Unix seconds) after which bundle is valid.
    pub min_timestamp: Option<u64>,
    /// Maximum timestamp (Unix seconds) before which bundle is valid.
    pub max_timestamp: Option<u64>,
    /// Transaction hashes that are allowed to revert without invalidating the bundle.
    pub reverting_tx_hashes: Vec<String>,
}

impl MevBundle {
    /// Creates a new atomic bundle for a given target block.
    pub fn new(txs: Vec<String>, block_number: u64) -> Self {
        Self {
            txs,
            block_number,
            min_timestamp: None,
            max_timestamp: None,
            reverting_tx_hashes: Vec::new(),
        }
    }

    /// Creates a bundle directly from raw byte transactions.
    pub fn from_raw_txs(raw_txs: &[Bytes], block_number: u64) -> Self {
        let txs = raw_txs
            .iter()
            .map(|tx| format!("0x{}", hex::encode(tx)))
            .collect();
        Self::new(txs, block_number)
    }

    /// Sets valid Unix timestamp boundaries for the bundle.
    pub fn with_timestamps(mut self, min_ts: Option<u64>, max_ts: Option<u64>) -> Self {
        self.min_timestamp = min_ts;
        self.max_timestamp = max_ts;
        self
    }

    /// Sets allowed reverting transaction hashes.
    pub fn with_reverting_hashes(mut self, hashes: Vec<B256>) -> Self {
        self.reverting_tx_hashes = hashes
            .into_iter()
            .map(|h| format!("{:#x}", h))
            .collect();
        self
    }

    /// Formats the JSON-RPC request payload for `eth_sendBundle`.
    pub fn to_send_bundle_rpc(&self, id: u64) -> JsonRpcRequest<SendBundleParams> {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: "eth_sendBundle".to_string(),
            params: vec![SendBundleParams {
                txs: self.txs.clone(),
                block_number: format!("0x{:x}", self.block_number),
                min_timestamp: self.min_timestamp,
                max_timestamp: self.max_timestamp,
                reverting_tx_hashes: self.reverting_tx_hashes.clone(),
            }],
        }
    }

    /// Formats the JSON-RPC request payload for `eth_callBundle` simulation.
    pub fn to_call_bundle_rpc(&self, id: u64, state_block_number: u64) -> JsonRpcRequest<CallBundleParams> {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: "eth_callBundle".to_string(),
            params: vec![CallBundleParams {
                txs: self.txs.clone(),
                block_number: format!("0x{:x}", self.block_number),
                state_block_number: format!("0x{:x}", state_block_number),
                coinbase: None,
            }],
        }
    }
}

/// Standard JSON-RPC 2.0 request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest<T> {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: Vec<T>,
}

/// Parameters for Flashbots `eth_sendBundle` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendBundleParams {
    pub txs: Vec<String>,
    #[serde(rename = "blockNumber")]
    pub block_number: String,
    #[serde(rename = "minTimestamp", skip_serializing_if = "Option::is_none")]
    pub min_timestamp: Option<u64>,
    #[serde(rename = "maxTimestamp", skip_serializing_if = "Option::is_none")]
    pub max_timestamp: Option<u64>,
    #[serde(rename = "revertingTxHashes", skip_serializing_if = "Vec::is_empty", default)]
    pub reverting_tx_hashes: Vec<String>,
}

/// Parameters for Flashbots `eth_callBundle` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallBundleParams {
    pub txs: Vec<String>,
    #[serde(rename = "blockNumber")]
    pub block_number: String,
    #[serde(rename = "stateBlockNumber")]
    pub state_block_number: String,
    #[serde(rename = "coinbase", skip_serializing_if = "Option::is_none")]
    pub coinbase: Option<String>,
}

/// Standard response from `eth_sendBundle`.
#[derive(Debug, Clone, Deserialize)]
pub struct SendBundleResponse {
    #[serde(rename = "bundleHash")]
    pub bundle_hash: Option<String>,
}

/// Standard response from `eth_callBundle`.
#[derive(Debug, Clone, Deserialize)]
pub struct CallBundleResponse {
    #[serde(rename = "bundleGasPrice")]
    pub bundle_gas_price: Option<String>,
    #[serde(rename = "bundleHash")]
    pub bundle_hash: Option<String>,
    #[serde(rename = "coinbaseDiff")]
    pub coinbase_diff: Option<String>,
    #[serde(rename = "ethSentToCoinbase")]
    pub eth_sent_to_coinbase: Option<String>,
    #[serde(rename = "gasFees")]
    pub gas_fees: Option<String>,
    #[serde(rename = "results")]
    pub results: Option<Vec<CallBundleTxResult>>,
}

/// Individual transaction result within `eth_callBundle`.
#[derive(Debug, Clone, Deserialize)]
pub struct CallBundleTxResult {
    pub hash: Option<String>,
    #[serde(rename = "gasUsed")]
    pub gas_used: Option<u64>,
    pub value: Option<String>,
    pub revert: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_fee_escalation_math() {
        // Base fee of 20 gwei
        let current_base_fee = 20_000_000_000u64;
        let next_base_fee = calculate_next_block_base_fee(current_base_fee);
        // 20 * 1.125 = 22.5 gwei
        assert_eq!(next_base_fee, 22_500_000_000u64);

        // Minimum base fee clamp
        assert_eq!(calculate_next_block_base_fee(0), MIN_BASE_FEE_WEI);
    }

    #[test]
    fn test_bundle_rpc_serialization() {
        let bundle = MevBundle::new(
            vec!["0x1234".to_string(), "0x5678".to_string()],
            19_500_001,
        );

        let send_rpc = bundle.to_send_bundle_rpc(1);
        let serialized = serde_json::to_string(&send_rpc).unwrap();
        assert!(serialized.contains("eth_sendBundle"));
        assert!(serialized.contains("0x1298be1")); // 19_500_001 in hex
        assert!(serialized.contains("0x1234"));
        assert!(serialized.contains("0x5678"));

        let call_rpc = bundle.to_call_bundle_rpc(2, 19_500_000);
        let serialized_call = serde_json::to_string(&call_rpc).unwrap();
        assert!(serialized_call.contains("eth_callBundle"));
        assert!(serialized_call.contains("stateBlockNumber"));
        assert!(serialized_call.contains("0x1298be0")); // 19_500_000 in hex
    }
}
