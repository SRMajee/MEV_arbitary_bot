//! Core orchestrator engine linking Ingestion -> Simulation -> Strategy -> Bundle Execution.

use alloy::{
    primitives::{Address, Bytes, U256},
    signers::local::PrivateKeySigner,
    sol_types::SolCall,
};
use eyre::Result;
use revm::{context::TxEnv, DatabaseRef};
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

use mev_executor::{
    BundleSigner, MevBundle, MevExecutorContract, RelayBroadcastResult, RelayClient,
};
use mev_simulator::{build_call_tx, ForkedSandbox};
use mev_state::MarketState;
use mev_strategy::{ArbitrageOpportunity, StrategyEngine};

/// Configuration parameters for the MEV Bot engine.
#[derive(Debug, Clone)]
pub struct BotConfig {
    /// On-chain address of deployed MevExecutor contract.
    pub executor_contract: Address,
    /// EIP-191 Flashbots relay authentication signer.
    pub auth_signer: PrivateKeySigner,
    /// Funded wallet signer executing on-chain transactions.
    pub tx_signer: PrivateKeySigner,
    /// Target chain ID (1 for Ethereum Mainnet).
    pub chain_id: u64,
    /// When true, logs simulated PnL without dispatching to public builder relays.
    pub shadow_mode: bool,
    /// Current base fee in wei.
    pub base_fee_wei: u64,
    /// EIP-1559 priority fee (miner tip) in wei.
    pub priority_fee_wei: u64,
    /// Minimum acceptable net profit in wei.
    pub min_profit_wei: U256,
    /// Builder bribe in basis points (e.g. 9000 = 90%).
    pub bribe_bps: u16,
}

/// Detailed stage-by-stage latency and execution telemetry for an evaluated opportunity.
#[derive(Debug, Clone)]
pub struct PipelineTelemetry {
    pub opportunity: ArbitrageOpportunity,
    pub bundle: MevBundle,
    pub backrun_raw_tx: Bytes,
    pub decode_us: f64,
    pub victim_sim_us: f64,
    pub strategy_us: f64,
    pub verify_sim_us: f64,
    pub bundle_sign_us: f64,
    pub total_tick_to_trade_us: f64,
}

/// Central MEV Arbitrage Engine coordinating the full pipeline.
#[derive(Clone)]
pub struct BotEngine {
    pub config: BotConfig,
    pub market_state: Arc<MarketState>,
    pub strategy_engine: StrategyEngine,
    pub bundle_signer: BundleSigner,
    pub relay_client: RelayClient,
}

impl BotEngine {
    /// Creates a new bot engine instance.
    pub fn new(
        config: BotConfig,
        market_state: Arc<MarketState>,
        strategy_engine: StrategyEngine,
    ) -> Self {
        let bundle_signer = BundleSigner::new(
            config.auth_signer.clone(),
            config.tx_signer.clone(),
            config.chain_id,
        );
        let relay_client = RelayClient::default_mainnet_relays();

        Self {
            config,
            market_state,
            strategy_engine,
            bundle_signer,
            relay_client,
        }
    }

    /// Processes an intercepted victim transaction from end to end:
    /// 1. Simulates victim swap in ForkedSandbox.
    /// 2. Solves optimal backrun route and sizing x* via StrategyEngine.
    /// 3. Simulates winning backrun candidate on post-victim snapshot.
    /// 4. Signs atomic MevBundle targeting next block.
    /// 5. In Shadow Mode, logs telemetry; in Live Mode, dispatches to builder relays.
    pub async fn process_victim_swap<DB: DatabaseRef + Clone>(
        &self,
        sandbox: &mut ForkedSandbox<DB>,
        victim_raw_tx: Bytes,
        victim_tx_env: TxEnv,
        touched_pool: Address,
        current_block: u64,
        nonce: u64,
        decode_us: f64,
    ) -> Result<Option<PipelineTelemetry>> {
        let tick_start = Instant::now();

        // Stage 1: Victim Simulation & State Commitment
        let t_victim = Instant::now();
        let victim_res = sandbox.execute_victim(victim_tx_env)?;
        let victim_sim_us = t_victim.elapsed().as_micros() as f64;

        if !victim_res.success {
            warn!("Victim transaction simulation reverted. Aborting backrun.");
            return Ok(None);
        }

        // Sync post-victim reserves for the touched pool into MarketState
        let get_res_tx = build_call_tx(
            Address::ZERO,
            touched_pool,
            Bytes::from_static(&[0x09, 0x02, 0xf1, 0xac]), // getReserves()
            U256::ZERO,
            50_000,
            None,
        );
        if let Ok(res) = sandbox.simulate_tx(get_res_tx) {
            if res.success && res.output.len() >= 96 {
                let r0 = U256::from_be_slice(&res.output[0..32]).to::<u128>();
                let r1 = U256::from_be_slice(&res.output[32..64]).to::<u128>();
                let ts = U256::from_be_slice(&res.output[64..96]).to::<u32>();
                if let Some((desc, _)) = self.market_state.get_pool(&touched_pool) {
                    self.market_state.update_pool(
                        desc,
                        mev_types::PoolState::V2(mev_types::V2PoolState {
                            reserve0: r0,
                            reserve1: r1,
                            block_timestamp_last: ts,
                        }),
                    );
                }
            }
        }

        // Branch a post-victim snapshot for candidate evaluation
        let mut snapshot = sandbox.snapshot();

        // Stage 2: Strategy Evaluation & Optimal Input Sizing
        let t_strat = Instant::now();
        let opportunities = self.strategy_engine.evaluate_touched_pool(
            &self.market_state,
            touched_pool,
            self.config.base_fee_wei,
        );
        let strategy_us = t_strat.elapsed().as_micros() as f64;

        if opportunities.is_empty() {
            return Ok(None);
        }

        let top_opp = opportunities[0].clone();

        // Stage 3: Backrun Simulation Verification on Post-Victim State
        let t_verify = Instant::now();
        let exec_call = MevExecutorContract::executeArbitrageDirectCall {
            params: top_opp.execution_params.clone(),
        };
        let candidate_tx = build_call_tx(
            self.bundle_signer.tx_address(),
            self.config.executor_contract,
            Bytes::from(exec_call.abi_encode()),
            U256::ZERO,
            500_000,
            None,
        );

        let sim_res = sandbox.simulate_candidate(&mut snapshot, candidate_tx)?;
        let verify_sim_us = t_verify.elapsed().as_micros() as f64;

        if !sim_res.success {
            warn!(
                "Candidate backrun verification reverted: {:?}",
                sim_res.revert_reason()
            );
            return Ok(None);
        }

        // Stage 4: Atomic Bundle Construction & Signing
        let t_sign = Instant::now();
        let target_block = current_block + 1;
        let (bundle, backrun_raw_tx) = self.bundle_signer.construct_signed_bundle(
            victim_raw_tx,
            nonce,
            self.config.executor_contract,
            Bytes::from(exec_call.abi_encode()),
            sim_res.gas_used + 40_000, // exact simulated gas + safety margin
            self.config.base_fee_wei,
            self.config.priority_fee_wei,
            target_block,
        )?;
        let bundle_sign_us = t_sign.elapsed().as_micros() as f64;

        let total_tick_to_trade_us = tick_start.elapsed().as_micros() as f64;

        let telemetry = PipelineTelemetry {
            opportunity: top_opp.clone(),
            bundle: bundle.clone(),
            backrun_raw_tx,
            decode_us,
            victim_sim_us,
            strategy_us,
            verify_sim_us,
            bundle_sign_us,
            total_tick_to_trade_us,
        };

        // Stage 5: Execution Mode (Shadow vs Live Dispatch)
        if self.config.shadow_mode {
            info!(
                "\n🔮 [SHADOW MODE] Evaluated Arbitrage Backrun:\n\
                 ├── Target Block:   {}\n\
                 ├── Optimal Input:  {:.4} WETH\n\
                 ├── Gross Profit:   {:.4} WETH\n\
                 ├── Builder Bribe:  {:.4} ETH ({} bps)\n\
                 ├── Net Realized:   {:.4} ETH\n\
                 └── Tick-to-Trade Latency Breakdown:\n\
                     ├── Decode:       {:.2} µs\n\
                     ├── Victim Sim:   {:.2} µs\n\
                     ├── Strategy x*:  {:.2} µs\n\
                     ├── Verify Sim:   {:.2} µs\n\
                     ├── Bundle Sign:  {:.2} µs\n\
                     └── TOTAL:        {:.2} µs\n",
                target_block,
                top_opp.optimal_input.to::<u128>() as f64 / 1e18,
                top_opp.gross_profit.to::<u128>() as f64 / 1e18,
                top_opp.builder_bribe.to::<u128>() as f64 / 1e18,
                top_opp.execution_params.bribeBps,
                top_opp.net_profit.to::<u128>() as f64 / 1e18,
                decode_us,
                victim_sim_us,
                strategy_us,
                verify_sim_us,
                bundle_sign_us,
                total_tick_to_trade_us,
            );
        } else {
            info!(
                "🚀 [LIVE DISPATCH] Broadcasting bundle for block {} (Net Profit: {:.4} ETH)",
                target_block,
                top_opp.net_profit.to::<u128>() as f64 / 1e18
            );
            let results: Vec<RelayBroadcastResult> = self
                .relay_client
                .broadcast_bundle(&bundle, &self.bundle_signer)
                .await;
            for r in results {
                info!("Relay {} -> success={}", r.relay_url, r.success);
            }
        }

        Ok(Some(telemetry))
    }
}
