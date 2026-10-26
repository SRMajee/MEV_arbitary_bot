//! Low-latency Ethereum MEV atomic backrun arbitrage engine entrypoint.

use alloy::primitives::{address, Address, U256};
use alloy::signers::local::PrivateKeySigner;
use eyre::{Result, WrapErr};
use mev_bot::{BotConfig, BotEngine};
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use mev_state::{MarketState, DAI, USDC, USDT, WBTC, WETH};
use mev_strategy::{ProfitEvaluator, StrategyEngine};

fn load_env_or_default(key: &str, default: &str) -> String {
    if let Ok(content) = std::fs::read_to_string(".env") {
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix(&format!("{}=", key)) {
                let unquoted = rest.trim().trim_matches('"').trim_matches('\'');
                if !unquoted.is_empty() {
                    return unquoted.to_string();
                }
            }
        }
    }
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Initialize structured logging telemetry
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .init();

    info!("⚡ Initializing Low-Latency Atomic MEV Arbitrage Engine v0.1.0...");

    // 2. Load environment configuration
    let rpc_url = load_env_or_default("MAINNET_RPC_URL", "https://eth-mainnet.public.blastapi.io");
    let tx_key_hex = load_env_or_default(
        "PRIVATE_KEY",
        "0x0000000000000000000000000000000000000000000000000000000000000001",
    );
    let auth_key_hex = load_env_or_default(
        "FLASHBOTS_AUTH_KEY",
        "0x0000000000000000000000000000000000000000000000000000000000000002",
    );
    let executor_addr_str = load_env_or_default(
        "MEV_EXECUTOR_ADDRESS",
        "0x0000000000000000000000000000000000000000",
    );
    let executor_contract: Address = executor_addr_str
        .parse()
        .unwrap_or(address!("0000000000000000000000000000000000000000"));
    let shadow_mode = load_env_or_default("SHADOW_MODE", "true")
        .parse::<bool>()
        .unwrap_or(true);

    let auth_signer: PrivateKeySigner = auth_key_hex
        .parse()
        .wrap_err("failed to parse FLASHBOTS_AUTH_KEY")?;
    let tx_signer: PrivateKeySigner = tx_key_hex
        .parse()
        .wrap_err("failed to parse PRIVATE_KEY")?;

    info!("🔗 Upstream RPC: {}", rpc_url);
    info!("🛡️ Execution Mode: {}", if shadow_mode { "SHADOW (Zero-Broadcast)" } else { "LIVE (Multi-Relay Dispatch)" });
    info!("🔑 Searcher Signer: {:?}", tx_signer.address());
    info!("🔑 Flashbots Auth:   {:?}", auth_signer.address());
    info!("📜 MevExecutor Contract: {:?}", executor_contract);

    // 3. Initialize Shared Market State & Whitelist
    let market_state = Arc::new(MarketState::new());
    let token_whitelist = vec![WETH, USDC, USDT, DAI, WBTC];

    // 4. Initialize Strategy Engine
    let profit_evaluator = ProfitEvaluator::new(
        U256::from(2_000_000_000_000_000u64), // 0.002 WETH min net profit
        9000,                                 // 90% builder bribe
        70_000,                               // 70k gas estimate per hop
    );

    let strategy_engine = StrategyEngine::new(
        WETH,
        token_whitelist,
        profit_evaluator,
        U256::from(10).pow(U256::from(16)), // 0.01 WETH min trade
        U256::from(50) * U256::from(10).pow(U256::from(18)), // 50 WETH max trade
    );

    // 5. Initialize Bot Orchestrator Engine
    let bot_config = BotConfig {
        executor_contract,
        auth_signer,
        tx_signer,
        chain_id: 1,
        shadow_mode,
        base_fee_wei: 20_000_000_000, // 20 gwei default
        priority_fee_wei: 1_000_000_000, // 1 gwei priority fee
        min_profit_wei: U256::from(2_000_000_000_000_000u64),
        bribe_bps: 9000,
    };

    let _engine = BotEngine::new(bot_config, market_state, strategy_engine);

    info!("🚀 MEV Engine initialized and running. Listening for block events and mempool pending transactions...");
    info!("Press Ctrl+C to terminate cleanly.");

    // 6. Graceful shutdown handler
    tokio::signal::ctrl_c().await?;
    info!("Terminating MEV Engine gracefully. Exiting.");

    Ok(())
}
