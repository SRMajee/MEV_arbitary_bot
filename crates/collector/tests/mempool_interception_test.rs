//! Live local Anvil integration test verifying real-time mempool interception and swap decoding.

use alloy::{
    network::TransactionBuilder,
    node_bindings::Anvil,
    primitives::{address, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
    sol,
    sol_types::SolCall,
};
use eyre::Result;
use mev_collector::{
    Collector, CollectorConfig, CollectorStream, TransportMode, UNISWAP_V2_ROUTER_02,
};
use mev_types::PoolVariant;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::timeout;

sol! {
    interface IUniswapV2RouterForTest {
        function swapExactTokensForTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);
    }
}

fn find_anvil_path() -> Option<PathBuf> {
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let p = PathBuf::from(userprofile).join(".foundry").join("bin").join("anvil.exe");
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join(".foundry").join("bin").join("anvil");
        if p.exists() {
            return Some(p);
        }
    }
    let fallback = PathBuf::from("C:\\Users\\User\\.foundry\\bin\\anvil.exe");
    if fallback.exists() {
        return Some(fallback);
    }
    None
}

#[tokio::test]
async fn test_anvil_mempool_interception() -> Result<()> {
    // 1. Spawn ephemeral Anvil sandbox instance
    let mut anvil_builder = Anvil::new();
    if let Some(path) = find_anvil_path() {
        anvil_builder = anvil_builder.path(path);
    }
    let anvil = anvil_builder.spawn();
    let ws_url = anvil.ws_endpoint();
    let http_url = anvil.endpoint();

    // 2. Connect signer provider to Anvil via HTTP
    let signer: PrivateKeySigner = anvil.keys()[0].clone().into();
    let sender_address = signer.address();

    let wallet = alloy::network::EthereumWallet::from(signer);
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(http_url.parse()?);

    // 3. Disable automining so submitted transactions sit in the pending mempool
    let _: () = provider.raw_request("evm_setAutomine".into(), [false]).await?;

    // 4. Initialize and start Collector connected to Anvil via WebSocket
    let config = CollectorConfig {
        transport: TransportMode::WebSocket(ws_url),
        channel_capacity: 512,
        hash_workers: 4,
    };
    let collector = Collector::new(config);
    let mut swap_rx = collector.subscribe_swaps();
    let mut block_rx = collector.subscribe_blocks();

    collector.start().await?;

    // Allow background subscriptions to attach
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 5. Construct a synthetic pending Uniswap V2 swap transaction
    let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    let swap_call = IUniswapV2RouterForTest::swapExactTokensForTokensCall {
        amountIn: U256::from(5_000_000_000_000_000_000u64), // 5 WETH
        amountOutMin: U256::from(15_000_000_000u64),         // 15,000 USDC
        path: vec![weth, usdc],
        to: sender_address,
        deadline: U256::from(1999999999),
    };

    let calldata = swap_call.abi_encode();
    let tx = TransactionRequest::default()
        .with_from(sender_address)
        .with_to(UNISWAP_V2_ROUTER_02)
        .with_input(calldata)
        .with_value(U256::ZERO)
        .with_gas_limit(300_000)
        .with_max_fee_per_gas(20_000_000_000)
        .with_max_priority_fee_per_gas(1_000_000_000)
        .with_nonce(0);

    // 6. Broadcast transaction to Anvil mempool without mining
    let pending_tx = provider.send_transaction(tx).await?;
    let pending_hash = *pending_tx.tx_hash();

    // 7. Await intercept by Collector from the pending mempool
    let received_swap = timeout(Duration::from_secs(5), async {
        loop {
            match swap_rx.recv().await {
                Ok(swap) => {
                    if swap.tx_hash == pending_hash {
                        return Ok(swap);
                    }
                }
                Err(err) => return Err(eyre::eyre!("Receiver error: {:?}", err)),
            }
        }
    })
    .await??;

    // 8. Assert exact decoded properties
    assert_eq!(received_swap.tx_hash, pending_hash);
    assert_eq!(received_swap.router, UNISWAP_V2_ROUTER_02);
    assert_eq!(received_swap.protocol, PoolVariant::UniswapV2);
    assert_eq!(received_swap.token_in, weth);
    assert_eq!(received_swap.token_out, usdc);
    assert_eq!(
        received_swap.amount_in,
        U256::from(5_000_000_000_000_000_000u64)
    );
    assert_eq!(
        received_swap.amount_out_min,
        U256::from(15_000_000_000u64)
    );
    assert_eq!(received_swap.recipient, sender_address);

    // 9. Mine a block and verify block stream delivers header
    let _: String = provider.raw_request("evm_mine".into(), ()).await?;
    let block_event = timeout(Duration::from_secs(5), block_rx.recv()).await??;
    assert_eq!(block_event.number, 1);

    Ok(())
}
