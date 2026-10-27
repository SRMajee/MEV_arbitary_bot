//! Multi-builder relay broadcasting client with concurrent fan-out and simulation support.

use eyre::{eyre, Result, WrapErr};
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, error, warn};

use crate::bundle::{CallBundleResponse, MevBundle};
use crate::signer::{BundleSigner, PreSignedBundlePayload};

/// Flashbots Builder Relay endpoint.
pub const FLASHBOTS_RELAY_URL: &str = "https://relay.flashbots.net";
/// Titan Builder Relay endpoint.
pub const TITAN_RELAY_URL: &str = "https://rpc.titanbuilder.xyz";
/// Beaver Builder Relay endpoint.
pub const BEAVER_RELAY_URL: &str = "https://rpc.beaverbuild.org";
/// Builder0x69 Relay endpoint.
pub const BUILDER0X69_RELAY_URL: &str = "https://builder0x69.io";

/// Result of dispatching a bundle to an individual builder relay.
#[derive(Debug, Clone)]
pub struct RelayBroadcastResult {
    /// Target relay endpoint URL.
    pub relay_url: String,
    /// HTTP status code or error.
    pub status: u16,
    /// Response body or error description.
    pub response: String,
    /// True if the relay accepted the bundle.
    pub success: bool,
}

/// Asynchronous client managing concurrent fan-out to block builder relays.
#[derive(Debug, Clone)]
pub struct RelayClient {
    client: reqwest::Client,
    pub endpoints: Vec<String>,
}

impl Default for RelayClient {
    fn default() -> Self {
        Self::default_mainnet_relays()
    }
}

impl RelayClient {
    /// Creates a new relay client with a custom list of builder endpoints.
    pub fn new(endpoints: Vec<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(1500))
            .connect_timeout(Duration::from_millis(500))
            .tcp_nodelay(true)
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .unwrap_or_default();

        Self { client, endpoints }
    }

    /// Initializes client targeting the premier mainnet builder relays.
    pub fn default_mainnet_relays() -> Self {
        Self::new(vec![
            FLASHBOTS_RELAY_URL.to_string(),
            TITAN_RELAY_URL.to_string(),
            BEAVER_RELAY_URL.to_string(),
            BUILDER0X69_RELAY_URL.to_string(),
        ])
    }

    /// Appends an additional builder endpoint to the client.
    pub fn add_endpoint(&mut self, endpoint: String) {
        if !self.endpoints.contains(&endpoint) {
            self.endpoints.push(endpoint);
        }
    }

    /// Broadcasts an atomic bundle concurrently to all configured builder relays.
    ///
    /// Automatically performs single-serialization and single EIP-191 signing before fan-out.
    pub async fn broadcast_bundle(
        &self,
        bundle: &MevBundle,
        signer: &BundleSigner,
    ) -> Vec<RelayBroadcastResult> {
        let payload = match signer.prepare_bundle_payload(bundle) {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to prepare bundle payload: {:?}", e);
                return Vec::new();
            }
        };

        self.broadcast_pre_signed(&payload).await
    }

    /// Broadcasts a pre-serialized, pre-signed bundle payload concurrently across all configured relays.
    ///
    /// Employs zero-copy sharing of the body and signature across tasks.
    pub async fn broadcast_pre_signed(
        &self,
        payload: &PreSignedBundlePayload,
    ) -> Vec<RelayBroadcastResult> {
        let mut set = tokio::task::JoinSet::new();
        for endpoint in &self.endpoints {
            let client = self.client.clone();
            let endpoint = endpoint.clone();
            let body = payload.body.to_vec();
            let sig = payload.flashbots_signature.clone();

            set.spawn(async move {
                let res = client
                    .post(&endpoint)
                    .header("Content-Type", "application/json")
                    .header("X-Flashbots-Signature", sig)
                    .body(body)
                    .send()
                    .await;

                match res {
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        let text = resp.text().await.unwrap_or_default();
                        let success = status == 200 && !text.contains("error");
                        if success {
                            debug!("Bundle accepted by relay {}: {}", endpoint, text);
                        } else {
                            warn!("Relay {} returned status {}: {}", endpoint, status, text);
                        }
                        RelayBroadcastResult {
                            relay_url: endpoint,
                            status,
                            response: text,
                            success,
                        }
                    }
                    Err(err) => {
                        error!("Failed to send bundle to relay {}: {:?}", endpoint, err);
                        RelayBroadcastResult {
                            relay_url: endpoint,
                            status: 0,
                            response: err.to_string(),
                            success: false,
                        }
                    }
                }
            });
        }

        let mut results = Vec::with_capacity(self.endpoints.len());
        while let Some(res) = set.join_next().await {
            if let Ok(item) = res {
                results.push(item);
            }
        }
        results
    }

    /// Simulates a bundle on a specific relay using `eth_callBundle`.
    pub async fn simulate_bundle(
        &self,
        relay_url: &str,
        bundle: &MevBundle,
        signer: &BundleSigner,
        state_block_number: u64,
    ) -> Result<CallBundleResponse> {
        let rpc_req = bundle.to_call_bundle_rpc(1, state_block_number);
        let body = serde_json::to_vec(&rpc_req)
            .wrap_err("failed to serialize eth_callBundle request")?;

        let signature_header = signer
            .sign_flashbots_request(&body)
            .wrap_err("failed to sign eth_callBundle request")?;

        let resp = self
            .client
            .post(relay_url)
            .header("Content-Type", "application/json")
            .header("X-Flashbots-Signature", signature_header)
            .body(body)
            .send()
            .await
            .wrap_err_with(|| format!("failed to call relay at {}", relay_url))?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(eyre!(
                "Relay simulation failed with status {}: {}",
                status,
                text
            ));
        }

        #[derive(Deserialize)]
        struct RpcResponse<T> {
            result: Option<T>,
            error: Option<serde_json::Value>,
        }

        let parsed: RpcResponse<CallBundleResponse> = serde_json::from_str(&text)
            .wrap_err_with(|| format!("failed to decode eth_callBundle response: {}", text))?;

        if let Some(err) = parsed.error {
            return Err(eyre!("eth_callBundle RPC error: {:?}", err));
        }

        parsed
            .result
            .ok_or_else(|| eyre!("Missing result in eth_callBundle response"))
    }
}
