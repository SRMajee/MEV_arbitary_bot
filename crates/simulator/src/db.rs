//! Hybrid in-memory EVM database backed by an Alloy RPC provider fallback.

use alloy::{
    eips::{BlockId, BlockNumberOrTag},
    primitives::{Address, Bytes, B256, U256},
    providers::Provider,
};
use revm::{
    bytecode::Bytecode,
    context::DBErrorMarker,
    database::DatabaseRef,
    primitives::KECCAK_EMPTY,
    state::AccountInfo,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Database error wrapping RPC errors or missing resources.
#[derive(Debug, thiserror::Error)]
pub enum AlloyDBError {
    #[error("RPC transport error: {0}")]
    Rpc(String),
}

impl DBErrorMarker for AlloyDBError {}

/// An RPC-backed database implementing `DatabaseRef` for revm.
///
/// Lazy-fetches account balances, nonces, bytecodes, and storage slots from an Ethereum RPC node
/// and caches them in memory. Uses `Arc<RwLock<...>>` internally, making cloning the `AlloyDB`
/// handle instant and cheap while sharing the underlying cache across snapshots.
#[derive(Clone)]
pub struct AlloyDB<P> {
    provider: P,
    block_id: Option<BlockId>,
    accounts: Arc<RwLock<HashMap<Address, Option<AccountInfo>>>>,
    contracts: Arc<RwLock<HashMap<B256, Bytecode>>>,
    storage: Arc<RwLock<HashMap<(Address, U256), U256>>>,
    block_hashes: Arc<RwLock<HashMap<u64, B256>>>,
    handle: Option<tokio::runtime::Handle>,
}

impl<P: Provider + Clone + 'static> AlloyDB<P> {
    /// Creates a new `AlloyDB` with the given provider, block id, and runtime handle.
    pub fn new(provider: P, block_id: Option<BlockId>) -> Self {
        Self {
            provider,
            block_id,
            accounts: Arc::new(RwLock::new(HashMap::new())),
            contracts: Arc::new(RwLock::new(HashMap::new())),
            storage: Arc::new(RwLock::new(HashMap::new())),
            block_hashes: Arc::new(RwLock::new(HashMap::new())),
            handle: tokio::runtime::Handle::try_current().ok(),
        }
    }

    /// Sets the pinned block ID for all subsequent RPC queries.
    pub fn set_block_id(&mut self, block_id: Option<BlockId>) {
        self.block_id = block_id;
    }

    /// Pre-seeds an account into the in-memory cache directly.
    pub fn insert_account(&self, address: Address, info: AccountInfo) {
        if let Some(code) = &info.code {
            self.contracts.write().unwrap().insert(info.code_hash, code.clone());
        }
        self.accounts.write().unwrap().insert(address, Some(info));
    }

    /// Pre-seeds a storage slot for an account directly.
    pub fn insert_storage(&self, address: Address, slot: U256, value: U256) {
        self.storage.write().unwrap().insert((address, slot), value);
    }

    /// Returns the number of cached accounts.
    pub fn accounts_cache_len(&self) -> usize {
        self.accounts.read().unwrap().len()
    }

    /// Returns the number of cached storage slots.
    pub fn storage_cache_len(&self) -> usize {
        self.storage.read().unwrap().len()
    }

    /// Returns a cloned snapshot of the cached accounts map.
    pub fn accounts_cache(&self) -> HashMap<Address, Option<AccountInfo>> {
        self.accounts.read().unwrap().clone()
    }

    /// Returns a cloned snapshot of the cached contract bytecodes map.
    pub fn contracts_cache(&self) -> HashMap<B256, Bytecode> {
        self.contracts.read().unwrap().clone()
    }

    /// Returns a cloned snapshot of the cached storage slots map.
    pub fn storage_cache(&self) -> HashMap<(Address, U256), U256> {
        self.storage.read().unwrap().clone()
    }

    /// Returns a cloned snapshot of the cached block hashes map.
    pub fn block_hashes_cache(&self) -> HashMap<u64, B256> {
        self.block_hashes.read().unwrap().clone()
    }


    /// Executes an async future synchronously, safely handling both current-thread and multi-thread runtimes.
    fn block_on<F: std::future::Future<Output = R> + Send, R: Send>(&self, f: F) -> R {
        let handle = self.handle.clone().or_else(|| tokio::runtime::Handle::try_current().ok());
        if let Some(handle) = handle {
            match handle.runtime_flavor() {
                tokio::runtime::RuntimeFlavor::CurrentThread => {
                    std::thread::scope(|s| {
                        s.spawn(|| handle.block_on(f)).join().unwrap()
                    })
                }
                _ => {
                    tokio::task::block_in_place(|| handle.block_on(f))
                }
            }
        } else {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to create tokio runtime for AlloyDB");
            rt.block_on(f)
        }
    }
}

impl<P: Provider + Clone + 'static> DatabaseRef for AlloyDB<P> {
    type Error = AlloyDBError;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        if let Some(cached) = self.accounts.read().unwrap().get(&address) {
            return Ok(cached.clone());
        }

        self.block_on(async {
            let mut balance_req = self.provider.get_balance(address);
            let mut nonce_req = self.provider.get_transaction_count(address);
            let mut code_req = self.provider.get_code_at(address);

            if let Some(block) = self.block_id {
                balance_req = balance_req.block_id(block);
                nonce_req = nonce_req.block_id(block);
                code_req = code_req.block_id(block);
            }

            let balance = balance_req
                .await
                .map_err(|e| AlloyDBError::Rpc(e.to_string()))?;
            let nonce = nonce_req
                .await
                .map_err(|e| AlloyDBError::Rpc(e.to_string()))?;
            let raw_code = code_req
                .await
                .map_err(|e| AlloyDBError::Rpc(e.to_string()))?;

            if balance.is_zero() && nonce == 0 && raw_code.is_empty() {
                self.accounts.write().unwrap().insert(address, None);
                return Ok(None);
            }

            let (bytecode, code_hash) = if raw_code.is_empty() {
                (Bytecode::default(), KECCAK_EMPTY)
            } else {
                let code = Bytecode::new_raw(Bytes::from(raw_code));
                let hash = code.hash_slow();
                self.contracts.write().unwrap().insert(hash, code.clone());
                (code, hash)
            };

            let info = AccountInfo::new(balance, nonce, code_hash, bytecode);
            self.accounts.write().unwrap().insert(address, Some(info.clone()));

            Ok(Some(info))
        })
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if let Some(code) = self.contracts.read().unwrap().get(&code_hash) {
            return Ok(code.clone());
        }
        Ok(Bytecode::default())
    }

    fn storage_ref(&self, address: Address, index: U256) -> Result<U256, Self::Error> {
        let key = (address, index);
        if let Some(&val) = self.storage.read().unwrap().get(&key) {
            return Ok(val);
        }

        self.block_on(async {
            let mut req = self.provider.get_storage_at(address, index);
            if let Some(block) = self.block_id {
                req = req.block_id(block);
            }

            let val = req.await.map_err(|e| AlloyDBError::Rpc(e.to_string()))?;
            self.storage.write().unwrap().insert(key, val);
            Ok(val)
        })
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        if let Some(&hash) = self.block_hashes.read().unwrap().get(&number) {
            return Ok(hash);
        }

        self.block_on(async {
            let block = self
                .provider
                .get_block_by_number(BlockNumberOrTag::Number(number))
                .await
                .map_err(|e| AlloyDBError::Rpc(e.to_string()))?
                .ok_or_else(|| {
                    AlloyDBError::Rpc(format!("Block not found for number: {}", number))
                })?;

            let hash = block.header.hash;
            self.block_hashes.write().unwrap().insert(number, hash);
            Ok(hash)
        })
    }
}
