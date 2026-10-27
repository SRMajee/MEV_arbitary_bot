//! Dual-key bundle signing engine for EIP-191 Flashbots authentication and EIP-1559 execution.

use alloy::{
    consensus::{SignableTransaction, TxEip1559},
    eips::eip2718::Encodable2718,
    primitives::{hex, keccak256, Address, Bytes, TxKind, U256},
    signers::{local::PrivateKeySigner, SignerSync},
};
use eyre::{Result, WrapErr};

use crate::bundle::{calculate_next_block_base_fee, MevBundle};

/// Dual-key signer managing Flashbots header authentication and transaction execution.
#[derive(Debug, Clone)]
pub struct BundleSigner {
    /// Signer for Flashbots identity header (`X-Flashbots-Signature`).
    auth_signer: PrivateKeySigner,
    /// Signer holding funds to execute the on-chain MevExecutor transaction.
    tx_signer: PrivateKeySigner,
    /// Target chain ID (e.g., 1 for Ethereum Mainnet).
    chain_id: u64,
}

impl BundleSigner {
    /// Creates a new bundle signer with dedicated authentication and transaction execution keys.
    pub fn new(auth_signer: PrivateKeySigner, tx_signer: PrivateKeySigner, chain_id: u64) -> Self {
        Self {
            auth_signer,
            tx_signer,
            chain_id,
        }
    }

    /// Creates a bundle signer from hex private key strings.
    pub fn from_hex_keys(auth_key_hex: &str, tx_key_hex: &str, chain_id: u64) -> Result<Self> {
        let auth_signer: PrivateKeySigner = auth_key_hex
            .trim()
            .parse()
            .wrap_err("failed to parse auth private key")?;
        let tx_signer: PrivateKeySigner = tx_key_hex
            .trim()
            .parse()
            .wrap_err("failed to parse tx private key")?;

        Ok(Self::new(auth_signer, tx_signer, chain_id))
    }

    /// Address corresponding to the Flashbots authentication key.
    pub fn auth_address(&self) -> Address {
        self.auth_signer.address()
    }

    /// Address corresponding to the transaction execution key.
    pub fn tx_address(&self) -> Address {
        self.tx_signer.address()
    }

    /// Generates the standard `X-Flashbots-Signature` header value for an HTTP request body.
    ///
    /// Specification:
    /// 1. Hash the JSON-RPC body bytes: `body_hash = keccak256(body)`
    /// 2. Format as a 66-character hex string: `"0x" + hex(body_hash)`
    /// 3. Sign the string via EIP-191 personal sign: `sign("\x19Ethereum Signed Message:\n66" + hex_hash)`
    /// 4. Format header strictly as `<auth_address>:<0x_signature>`
    pub fn sign_flashbots_request(&self, body: &[u8]) -> Result<String> {
        let body_hash = keccak256(body);
        let hash_hex = format!("0x{:x}", body_hash);
        let signature = self
            .auth_signer
            .sign_message_sync(hash_hex.as_bytes())
            .wrap_err("failed to sign Flashbots authentication payload")?;

        let header = format!(
            "{:#x}:0x{}",
            self.auth_signer.address(),
            hex::encode(signature.as_bytes())
        );
        Ok(header)
    }

    /// Signs an EIP-1559 backrun transaction calling the MevExecutor smart contract.
    ///
    /// Encodes into standard EIP-2718 typed transaction envelope (`0x02` prefix + RLP).
    pub fn sign_backrun_tx(
        &self,
        nonce: u64,
        executor_contract: Address,
        calldata: Bytes,
        gas_limit: u64,
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
    ) -> Result<Bytes> {
        let tx = TxEip1559 {
            chain_id: self.chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            to: TxKind::Call(executor_contract),
            value: U256::ZERO,
            access_list: Default::default(),
            input: calldata,
        };

        let sig_hash = tx.signature_hash();
        let sig = self
            .tx_signer
            .sign_hash_sync(&sig_hash)
            .wrap_err("failed to sign backrun transaction")?;
        let signed_tx = tx.into_signed(sig);

        let mut raw_bytes = Vec::new();
        signed_tx.encode_2718(&mut raw_bytes);

        Ok(Bytes::from(raw_bytes))
    }

    /// Constructs an atomic 2-transaction bundle (`[Victim_Tx, Backrun_Tx]`) targeting `target_block`.
    ///
    /// Computes `max_fee_per_gas` using the deterministic 1.125x next-block base fee bound.
    pub fn construct_signed_bundle(
        &self,
        victim_raw_tx: Bytes,
        nonce: u64,
        executor_contract: Address,
        calldata: Bytes,
        gas_limit: u64,
        current_base_fee: u64,
        priority_fee: u64,
        target_block: u64,
    ) -> Result<(MevBundle, Bytes)> {
        let next_base_fee = calculate_next_block_base_fee(current_base_fee);
        let max_fee_per_gas = (next_base_fee as u128) + (priority_fee as u128);
        let max_priority_fee_per_gas = priority_fee as u128;

        let backrun_tx_bytes = self.sign_backrun_tx(
            nonce,
            executor_contract,
            calldata,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
        )?;

        let victim_hex = format!("0x{}", hex::encode(&victim_raw_tx));
        let backrun_hex = format!("0x{}", hex::encode(&backrun_tx_bytes));

        let bundle = MevBundle::new(vec![victim_hex, backrun_hex], target_block);
        Ok((bundle, backrun_tx_bytes))
    }

    /// Pre-serializes the JSON-RPC request and signs the Flashbots authentication header once.
    pub fn prepare_bundle_payload(&self, bundle: &MevBundle) -> Result<PreSignedBundlePayload> {
        let rpc_req = bundle.to_send_bundle_rpc(1);
        let body_bytes = serde_json::to_vec(&rpc_req)
            .wrap_err("failed to serialize eth_sendBundle request")?;
        let signature = self.sign_flashbots_request(&body_bytes)?;
        Ok(PreSignedBundlePayload {
            body: Bytes::from(body_bytes),
            flashbots_signature: signature,
            block_number: bundle.block_number,
        })
    }
}

/// Pre-serialized and pre-signed bundle request payload ready for zero-copy concurrent relay fan-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreSignedBundlePayload {
    /// Shared JSON-RPC request body bytes (`Bytes` for O(1) Arc clone across async tasks).
    pub body: Bytes,
    /// Shared pre-computed `X-Flashbots-Signature` header value.
    pub flashbots_signature: String,
    /// Target block number.
    pub block_number: u64,
}

/// High-throughput bundle signing and payload preparation engine.
#[derive(Debug, Clone)]
pub struct FastBundleSigner {
    signer: BundleSigner,
}

impl FastBundleSigner {
    /// Creates a new fast bundle signer wrapping a dual-key `BundleSigner`.
    pub fn new(signer: BundleSigner) -> Self {
        Self { signer }
    }

    /// Access the underlying `BundleSigner`.
    pub fn inner(&self) -> &BundleSigner {
        &self.signer
    }

    /// Signs an EIP-1559 backrun transaction with stack-allocated or pre-encoded calldata.
    pub fn sign_backrun_raw(
        &self,
        nonce: u64,
        executor_contract: Address,
        calldata: &[u8],
        gas_limit: u64,
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
    ) -> Result<Bytes> {
        self.signer.sign_backrun_tx(
            nonce,
            executor_contract,
            Bytes::copy_from_slice(calldata),
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
        )
    }

    /// Fast bundle pipeline: builds signed backrun, formats bundle, and single-signs payload for multi-relay broadcast.
    pub fn construct_and_prepare_payload(
        &self,
        victim_raw_tx: &Bytes,
        nonce: u64,
        executor_contract: Address,
        calldata: &[u8],
        gas_limit: u64,
        current_base_fee: u64,
        priority_fee: u64,
        target_block: u64,
    ) -> Result<(MevBundle, Bytes, PreSignedBundlePayload)> {
        let (bundle, backrun_bytes) = self.signer.construct_signed_bundle(
            victim_raw_tx.clone(),
            nonce,
            executor_contract,
            Bytes::copy_from_slice(calldata),
            gas_limit,
            current_base_fee,
            priority_fee,
            target_block,
        )?;

        let payload = self.signer.prepare_bundle_payload(&bundle)?;
        Ok((bundle, backrun_bytes, payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn test_dual_key_signer_and_flashbots_signature() {
        let auth_key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let tx_key = "0x0000000000000000000000000000000000000000000000000000000000000002";

        let signer = BundleSigner::from_hex_keys(auth_key, tx_key, 1).unwrap();
        assert_eq!(
            signer.auth_address(),
            address!("7E5F4552091A69125d5DfCb7b8C2659029395Bdf")
        );
        assert_eq!(
            signer.tx_address(),
            address!("2B5AD5c4795c026514f8317c7a215E218DcCD6cF")
        );

        let body = br#"{"jsonrpc":"2.0","id":1,"method":"eth_sendBundle","params":[]}"#;
        let header = signer.sign_flashbots_request(body).unwrap();

        assert!(header.to_lowercase().starts_with("0x7e5f4552091a69125d5dfcb7b8c2659029395bdf:0x"));
        assert_eq!(header.len(), 42 + 1 + 2 + 130);
    }

    #[test]
    fn test_bundle_construction_and_raw_tx_generation() {
        let auth_key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let tx_key = "0x0000000000000000000000000000000000000000000000000000000000000002";
        let signer = BundleSigner::from_hex_keys(auth_key, tx_key, 1).unwrap();

        let dummy_victim = Bytes::from_static(&[0x02, 0xf8, 0x60, 0x01]);
        let executor_contract = address!("8888888888888888888888888888888888888888");
        let dummy_calldata = Bytes::from_static(&[0x12, 0x34, 0x56, 0x78]);

        let (bundle, backrun_bytes) = signer
            .construct_signed_bundle(
                dummy_victim.clone(),
                0,
                executor_contract,
                dummy_calldata,
                250_000,
                20_000_000_000, // 20 gwei
                1_000_000_000,  // 1 gwei priority fee
                19_500_001,
            )
            .unwrap();

        assert_eq!(bundle.block_number, 19_500_001);
        assert_eq!(bundle.txs.len(), 2);
        assert_eq!(bundle.txs[0], "0x02f86001");
        assert!(bundle.txs[1].starts_with("0x02"));
        assert_eq!(backrun_bytes[0], 0x02);
    }

    #[test]
    fn test_fast_bundle_signer_and_payload_preparation() {
        let auth_key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let tx_key = "0x0000000000000000000000000000000000000000000000000000000000000002";
        let signer = BundleSigner::from_hex_keys(auth_key, tx_key, 1).unwrap();
        let fast_signer = FastBundleSigner::new(signer);

        let dummy_victim = Bytes::from_static(&[0x02, 0xf8, 0x60, 0x01]);
        let executor_contract = address!("8888888888888888888888888888888888888888");
        let dummy_calldata = [0x12, 0x34, 0x56, 0x78];

        let (bundle, backrun_bytes, payload) = fast_signer
            .construct_and_prepare_payload(
                &dummy_victim,
                5,
                executor_contract,
                &dummy_calldata,
                250_000,
                20_000_000_000,
                1_000_000_000,
                19_500_001,
            )
            .unwrap();

        assert_eq!(bundle.block_number, 19_500_001);
        assert_eq!(bundle.txs.len(), 2);
        assert!(!backrun_bytes.is_empty());
        assert_eq!(payload.block_number, 19_500_001);
        assert!(!payload.body.is_empty());
        assert!(payload.flashbots_signature.starts_with("0x7e5f4552091a69125d5dfcb7b8c2659029395bdf:0x"));
    }
}
