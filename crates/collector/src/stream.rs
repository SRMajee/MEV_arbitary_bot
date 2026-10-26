//! Ingestion engine streaming pending mempool transactions and new blocks.

use alloy::{
    consensus::Transaction,
    network::{Ethereum, TransactionResponse},
    primitives::B256,
    providers::{Provider, ProviderBuilder},
    rpc::client::WsConnect,
};
use eyre::{Result, WrapErr};
use futures_util::StreamExt;
use mev_types::{DecodedSwap, NewBlockHeader};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::decoder::CalldataDecoder;
use crate::routers::is_target_router;

/// Ingestion transport protocol.
#[derive(Debug, Clone)]
pub enum TransportMode {
    /// WebSocket endpoint URL (e.g., `ws://127.0.0.1:8545` or `wss://eth-mainnet.g.alchemy.com/...`).
    WebSocket(String),
    /// Local Unix Domain Socket / Windows Named Pipe path for co-located nodes.
    Ipc(String),
}

/// Ingestion configuration.
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// Ingestion connection transport.
    pub transport: TransportMode,
    /// Capacity of the lossy freshest-priority broadcast buffer.
    pub channel_capacity: usize,
    /// Number of concurrent workers fetching transactions when node only emits tx hashes.
    pub hash_workers: usize,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            transport: TransportMode::WebSocket("ws://127.0.0.1:8545".to_string()),
            channel_capacity: 4096,
            hash_workers: 8,
        }
    }
}

/// Atomic counters tracking real-time ingestion throughput.
#[derive(Debug, Default)]
pub struct CollectorMetrics {
    /// Total pending transactions received from stream.
    pub processed_txs: AtomicU64,
    /// Total transactions matching target DEX router addresses.
    pub matched_swaps: AtomicU64,
    /// Swaps dropped due to downstream channel capacity saturation.
    pub dropped_swaps: AtomicU64,
    /// Total new block headers received.
    pub blocks_received: AtomicU64,
}

/// Abstract streaming interface allowing seamless interchange between WebSocket/IPC and Reth ExEx.
pub trait CollectorStream: Send + Sync {
    /// Subscribes to the live stream of decoded DEX victim swaps.
    fn subscribe_swaps(&self) -> broadcast::Receiver<DecodedSwap>;
    /// Subscribes to the live stream of canonical new block headers.
    fn subscribe_blocks(&self) -> broadcast::Receiver<NewBlockHeader>;
}

/// Production mempool and block ingestion collector.
#[derive(Clone)]
pub struct Collector {
    config: CollectorConfig,
    metrics: Arc<CollectorMetrics>,
    swap_tx: broadcast::Sender<DecodedSwap>,
    block_tx: broadcast::Sender<NewBlockHeader>,
}

impl Collector {
    /// Creates a new collector instance.
    pub fn new(config: CollectorConfig) -> Self {
        let (swap_tx, _) = broadcast::channel(config.channel_capacity);
        let (block_tx, _) = broadcast::channel(128);

        Self {
            config,
            metrics: Arc::new(CollectorMetrics::default()),
            swap_tx,
            block_tx,
        }
    }

    /// Access metrics for observability and latency profiling.
    pub fn metrics(&self) -> Arc<CollectorMetrics> {
        self.metrics.clone()
    }

    /// Starts the ingestion stream loops, spawning background tasks for blocks and transactions.
    pub async fn start(&self) -> Result<()> {
        info!(transport = ?self.config.transport, "Starting MEV ingestion collector...");

        match &self.config.transport {
            TransportMode::WebSocket(url) => {
                let ws = WsConnect::new(url);
                let provider = ProviderBuilder::new()
                    .connect_ws(ws)
                    .await
                    .wrap_err_with(|| format!("Failed to connect WebSocket to {url}"))?;
                let provider = Arc::new(provider);

                // 1. Spawn Block Stream
                self.spawn_block_listener(provider.clone());

                // 2. Spawn Pending Transaction Stream
                self.spawn_mempool_listener(provider);
            }
            TransportMode::Ipc(path) => {
                let ipc = alloy::rpc::client::IpcConnect::new(path.clone());
                let provider = ProviderBuilder::new()
                    .connect_ipc(ipc)
                    .await
                    .wrap_err_with(|| format!("Failed to connect IPC to {path}"))?;
                let provider = Arc::new(provider);

                self.spawn_block_listener(provider.clone());
                self.spawn_mempool_listener(provider);
            }
        }

        Ok(())
    }

    /// Background task listening for new block headers.
    fn spawn_block_listener<P>(&self, provider: Arc<P>)
    where
        P: Provider<Ethereum> + 'static,
    {
        let block_tx = self.block_tx.clone();
        let metrics = self.metrics.clone();

        tokio::spawn(async move {
            info!("Subscribing to new canonical block headers...");
            match provider.subscribe_blocks().await {
                Ok(stream) => {
                    let mut stream = stream.into_stream();
                    while let Some(header) = stream.next().await {
                        metrics.blocks_received.fetch_add(1, Ordering::Relaxed);
                        let event = NewBlockHeader {
                            number: header.number,
                            hash: header.hash,
                            base_fee_per_gas: header.base_fee_per_gas.map(|fee| fee as u128),
                            timestamp: header.timestamp,
                        };

                        debug!(block = event.number, hash = ?event.hash, "New block header received");
                        let _ = block_tx.send(event);
                    }
                }
                Err(err) => {
                    error!(error = ?err, "Failed to subscribe to block headers");
                }
            }
        });
    }

    /// Background task streaming pending transactions.
    fn spawn_mempool_listener<P>(&self, provider: Arc<P>)
    where
        P: Provider<Ethereum> + 'static,
    {
        let swap_tx = self.swap_tx.clone();
        let metrics = self.metrics.clone();
        let hash_workers = self.config.hash_workers;

        tokio::spawn(async move {
            info!("Subscribing to pending mempool transactions (attempting full tx stream)...");

            // Attempt Mode A: Full Transaction Stream
            match provider.subscribe_full_pending_transactions().await {
                Ok(full_stream) => {
                    info!("Node supports full pending transactions stream. Running in Mode A.");
                    let mut stream = full_stream.into_stream();
                    while let Some(tx) = stream.next().await {
                        metrics.processed_txs.fetch_add(1, Ordering::Relaxed);

                        if let Some(to) = tx.to() {
                            if is_target_router(&to) {
                                metrics.matched_swaps.fetch_add(1, Ordering::Relaxed);
                                let swaps = CalldataDecoder::decode(
                                    tx.tx_hash(),
                                    to,
                                    tx.value(),
                                    tx.input(),
                                );

                                for swap in swaps {
                                    if swap_tx.send(swap).is_err() {
                                        metrics.dropped_swaps.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    warn!("Full pending tx stream unavailable. Falling back to Mode B (hash stream + worker pool).");

                    match provider.subscribe_pending_transactions().await {
                        Ok(hash_stream) => {
                            let mut stream = hash_stream.into_stream();
                            let (hash_tx, hash_rx) = tokio::sync::mpsc::channel::<B256>(4096);
                            let hash_rx = Arc::new(tokio::sync::Mutex::new(hash_rx));

                            // Spawn worker pool to fetch tx bodies
                            for worker_id in 0..hash_workers {
                                let worker_rx = hash_rx.clone();
                                let worker_provider = provider.clone();
                                let worker_swap_tx = swap_tx.clone();
                                let worker_metrics = metrics.clone();

                                tokio::spawn(async move {
                                    debug!(worker_id, "Pending tx hash worker started");
                                    loop {
                                        let maybe_hash = {
                                            let mut lock = worker_rx.lock().await;
                                            lock.recv().await
                                        };

                                        match maybe_hash {
                                            Some(hash) => {
                                                if let Ok(Some(tx)) = worker_provider.get_transaction_by_hash(hash).await {
                                                    worker_metrics.processed_txs.fetch_add(1, Ordering::Relaxed);
                                                    if let Some(to) = tx.to() {
                                                        if is_target_router(&to) {
                                                            worker_metrics.matched_swaps.fetch_add(1, Ordering::Relaxed);
                                                            let swaps = CalldataDecoder::decode(
                                                                tx.tx_hash(),
                                                                to,
                                                                tx.value(),
                                                                tx.input(),
                                                            );

                                                            for swap in swaps {
                                                                if worker_swap_tx.send(swap).is_err() {
                                                                    worker_metrics.dropped_swaps.fetch_add(1, Ordering::Relaxed);
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            None => break,
                                        }
                                    }
                                });
                            }

                            // Feed hashes to worker channel
                            while let Some(hash) = stream.next().await {
                                let _ = hash_tx.try_send(hash);
                            }
                        }
                        Err(err) => {
                            error!(error = ?err, "Failed to subscribe to pending transaction hashes");
                        }
                    }
                }
            }
        });
    }
}

impl CollectorStream for Collector {
    fn subscribe_swaps(&self) -> broadcast::Receiver<DecodedSwap> {
        self.swap_tx.subscribe()
    }

    fn subscribe_blocks(&self) -> broadcast::Receiver<NewBlockHeader> {
        self.block_tx.subscribe()
    }
}
