//! Multicall3 batch pool state loader for fast startup warm-up.

use alloy::{
    eips::BlockId,
    primitives::{address, Address, Bytes, U256},
    providers::Provider,
    rpc::types::TransactionRequest,
    sol,
    sol_types::SolCall,
};
use eyre::Result;
use mev_types::{PoolDescriptor, PoolState, PoolVariant, V2PoolState, V3PoolState};
use std::collections::BTreeMap;
use tracing::{info, warn};

use crate::market::MarketState;
use crate::tick_math::{compress_tick, tick_to_word_pos};

/// Standard Ethereum mainnet Multicall3 contract address.
pub const MULTICALL3_ADDRESS: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");

sol! {
    #[derive(Debug, PartialEq, Eq)]
    struct Call3 {
        address target;
        bool allowFailure;
        bytes callData;
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Result3 {
        bool success;
        bytes returnData;
    }

    interface IMulticall3 {
        function aggregate3(Call3[] calldata calls) external payable returns (Result3[] memory returnData);
    }

    interface IUniswapV2Pair {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
        function token0() external view returns (address);
        function token1() external view returns (address);
    }

    interface IUniswapV3Pool {
        function slot0() external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );
        function liquidity() external view returns (uint128);
        function tickSpacing() external view returns (int24);
        function fee() external view returns (uint24);
        function token0() external view returns (address);
        function token1() external view returns (address);
        function tickBitmap(int16 wordPosition) external view returns (uint256);
    }
}

/// Batch loader using Multicall3 to fetch AMM reserves and states efficiently.
pub struct MulticallLoader {
    multicall_address: Address,
}

impl Default for MulticallLoader {
    fn default() -> Self {
        Self::new(MULTICALL3_ADDRESS)
    }
}

impl MulticallLoader {
    /// Creates a new loader with custom Multicall3 contract address.
    pub fn new(multicall_address: Address) -> Self {
        Self { multicall_address }
    }

    /// Dispatches a batch of calls to Multicall3 and decodes the results.
    pub async fn execute_multicall<P: Provider>(
        &self,
        provider: &P,
        calls: Vec<Call3>,
        block_id: Option<BlockId>,
    ) -> Result<Vec<Result3>> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }

        let aggregate_call = IMulticall3::aggregate3Call { calls };
        let calldata = aggregate_call.abi_encode();

        let req = TransactionRequest::default()
            .to(self.multicall_address)
            .input(Bytes::from(calldata).into());

        let mut call_builder = provider.call(req);
        if let Some(id) = block_id {
            call_builder = call_builder.block(id);
        }

        let raw_output = call_builder.await?;
        let decoded = IMulticall3::aggregate3Call::abi_decode_returns(&raw_output)?;

        Ok(decoded)
    }

    /// Loads multiple Uniswap V2 / SushiSwap pools in a single Multicall3 batch.
    pub async fn load_v2_pools<P: Provider>(
        &self,
        provider: &P,
        pools: &[Address],
        block_id: Option<BlockId>,
    ) -> Result<Vec<(PoolDescriptor, V2PoolState)>> {
        if pools.is_empty() {
            return Ok(Vec::new());
        }

        let mut calls = Vec::with_capacity(pools.len() * 3);
        for &pool in pools {
            calls.push(Call3 {
                target: pool,
                allowFailure: true,
                callData: Bytes::from(IUniswapV2Pair::getReservesCall {}.abi_encode()),
            });
            calls.push(Call3 {
                target: pool,
                allowFailure: true,
                callData: Bytes::from(IUniswapV2Pair::token0Call {}.abi_encode()),
            });
            calls.push(Call3 {
                target: pool,
                allowFailure: true,
                callData: Bytes::from(IUniswapV2Pair::token1Call {}.abi_encode()),
            });
        }

        let results = self.execute_multicall(provider, calls, block_id).await?;
        let mut loaded = Vec::with_capacity(pools.len());

        for (i, &pool) in pools.iter().enumerate() {
            let base_idx = i * 3;
            let res_reserves = &results[base_idx];
            let res_token0 = &results[base_idx + 1];
            let res_token1 = &results[base_idx + 2];

            if !res_reserves.success || !res_token0.success || !res_token1.success {
                warn!(%pool, "Failed to fetch V2 pool data via Multicall3");
                continue;
            }

            let reserves = match IUniswapV2Pair::getReservesCall::abi_decode_returns(
                &res_reserves.returnData,
            ) {
                Ok(r) => r,
                Err(err) => {
                    warn!(%pool, ?err, "Failed decoding getReserves");
                    continue;
                }
            };

            let token0 =
                match IUniswapV2Pair::token0Call::abi_decode_returns(&res_token0.returnData) {
                    Ok(t) => t,
                    Err(err) => {
                        warn!(%pool, ?err, "Failed decoding token0");
                        continue;
                    }
                };

            let token1 =
                match IUniswapV2Pair::token1Call::abi_decode_returns(&res_token1.returnData) {
                    Ok(t) => t,
                    Err(err) => {
                        warn!(%pool, ?err, "Failed decoding token1");
                        continue;
                    }
                };

            let descriptor =
                PoolDescriptor::new(pool, PoolVariant::UniswapV2, token0, token1, 30);
            let state = V2PoolState {
                reserve0: reserves.reserve0.to::<u128>(),
                reserve1: reserves.reserve1.to::<u128>(),
                block_timestamp_last: reserves.blockTimestampLast,
            };

            loaded.push((descriptor, state));
        }

        info!(count = loaded.len(), "Loaded V2 pools via Multicall3");
        Ok(loaded)
    }

    /// Loads multiple Uniswap V3 pools including their sparse tick bitmap words via Multicall3.
    ///
    /// `word_radius`: Number of 256-bit bitmap words to fetch on either side of active tick (e.g. 16).
    pub async fn load_v3_pools<P: Provider>(
        &self,
        provider: &P,
        pools: &[Address],
        word_radius: i16,
        block_id: Option<BlockId>,
    ) -> Result<Vec<(PoolDescriptor, V3PoolState)>> {
        if pools.is_empty() {
            return Ok(Vec::new());
        }

        // Pass 1: Fetch static parameters & current slot0 / liquidity
        let mut calls = Vec::with_capacity(pools.len() * 6);
        for &pool in pools {
            calls.push(Call3 {
                target: pool,
                allowFailure: true,
                callData: Bytes::from(IUniswapV3Pool::token0Call {}.abi_encode()),
            });
            calls.push(Call3 {
                target: pool,
                allowFailure: true,
                callData: Bytes::from(IUniswapV3Pool::token1Call {}.abi_encode()),
            });
            calls.push(Call3 {
                target: pool,
                allowFailure: true,
                callData: Bytes::from(IUniswapV3Pool::feeCall {}.abi_encode()),
            });
            calls.push(Call3 {
                target: pool,
                allowFailure: true,
                callData: Bytes::from(IUniswapV3Pool::tickSpacingCall {}.abi_encode()),
            });
            calls.push(Call3 {
                target: pool,
                allowFailure: true,
                callData: Bytes::from(IUniswapV3Pool::slot0Call {}.abi_encode()),
            });
            calls.push(Call3 {
                target: pool,
                allowFailure: true,
                callData: Bytes::from(IUniswapV3Pool::liquidityCall {}.abi_encode()),
            });
        }

        let pass1_results = self.execute_multicall(provider, calls, block_id).await?;

        struct IntermediateV3 {
            descriptor: PoolDescriptor,
            sqrt_price_x96: U256,
            tick: i32,
            liquidity: u128,
            tick_spacing: i32,
            word_start: i16,
            word_end: i16,
        }

        let mut intermediates = Vec::new();
        let mut bitmap_calls = Vec::new();

        for (i, &pool) in pools.iter().enumerate() {
            let base_idx = i * 6;
            let res_token0 = &pass1_results[base_idx];
            let res_token1 = &pass1_results[base_idx + 1];
            let res_fee = &pass1_results[base_idx + 2];
            let res_spacing = &pass1_results[base_idx + 3];
            let res_slot0 = &pass1_results[base_idx + 4];
            let res_liquidity = &pass1_results[base_idx + 5];

            if !res_token0.success
                || !res_token1.success
                || !res_fee.success
                || !res_spacing.success
                || !res_slot0.success
                || !res_liquidity.success
            {
                warn!(%pool, "Failed fetching V3 initial slot0/immutables via Multicall3");
                continue;
            }

            let token0 =
                match IUniswapV3Pool::token0Call::abi_decode_returns(&res_token0.returnData) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
            let token1 =
                match IUniswapV3Pool::token1Call::abi_decode_returns(&res_token1.returnData) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
            let fee = match IUniswapV3Pool::feeCall::abi_decode_returns(&res_fee.returnData) {
                Ok(f) => f.to::<u32>(),
                Err(_) => continue,
            };
            let tick_spacing = match IUniswapV3Pool::tickSpacingCall::abi_decode_returns(
                &res_spacing.returnData,
            ) {
                Ok(s) => s.as_i32(),
                Err(_) => continue,
            };
            let slot0 = match IUniswapV3Pool::slot0Call::abi_decode_returns(
                &res_slot0.returnData,
            ) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let liquidity = match IUniswapV3Pool::liquidityCall::abi_decode_returns(
                &res_liquidity.returnData,
            ) {
                Ok(l) => l,
                Err(_) => continue,
            };

            let tick = slot0.tick.as_i32();
            let compressed = compress_tick(tick, tick_spacing);
            let current_word = tick_to_word_pos(compressed);
            let word_start = current_word.saturating_sub(word_radius);
            let word_end = current_word.saturating_add(word_radius);

            let descriptor =
                PoolDescriptor::new(pool, PoolVariant::UniswapV3, token0, token1, fee);

            for w in word_start..=word_end {
                bitmap_calls.push(Call3 {
                    target: pool,
                    allowFailure: true,
                    callData: Bytes::from(
                        IUniswapV3Pool::tickBitmapCall { wordPosition: w }.abi_encode(),
                    ),
                });
            }

            intermediates.push(IntermediateV3 {
                descriptor,
                sqrt_price_x96: U256::from(slot0.sqrtPriceX96),
                tick,
                liquidity,
                tick_spacing,
                word_start,
                word_end,
            });
        }

        // Pass 2: Fetch tick bitmap words
        let bitmap_results = self
            .execute_multicall(provider, bitmap_calls, block_id)
            .await?;

        let mut loaded = Vec::with_capacity(intermediates.len());
        let mut cursor = 0;

        for item in intermediates {
            let mut tick_bitmap = BTreeMap::new();

            for w in item.word_start..=item.word_end {
                let res = &bitmap_results[cursor];
                cursor += 1;

                if res.success {
                    if let Ok(word_val) =
                        IUniswapV3Pool::tickBitmapCall::abi_decode_returns(&res.returnData)
                    {
                        if word_val != U256::ZERO {
                            tick_bitmap.insert(w, word_val);
                        }
                    }
                }
            }

            let state = V3PoolState {
                sqrt_price_x96: item.sqrt_price_x96,
                tick: item.tick,
                liquidity: item.liquidity,
                tick_spacing: item.tick_spacing,
                tick_bitmap,
            };

            loaded.push((item.descriptor, state));
        }

        info!(count = loaded.len(), "Loaded V3 pools via Multicall3");
        Ok(loaded)
    }

    /// Loads both V2 and V3 pools and stores them directly into the target `MarketState`.
    pub async fn load_into_market_state<P: Provider>(
        &self,
        provider: &P,
        state: &MarketState,
        v2_pools: &[Address],
        v3_pools: &[Address],
        v3_word_radius: i16,
        block_id: Option<BlockId>,
    ) -> Result<()> {
        let v2_data = self.load_v2_pools(provider, v2_pools, block_id).await?;
        for (desc, v2_state) in v2_data {
            state.update_pool(desc, PoolState::V2(v2_state));
        }

        let v3_data = self
            .load_v3_pools(provider, v3_pools, v3_word_radius, block_id)
            .await?;
        for (desc, v3_state) in v3_data {
            state.update_pool(desc, PoolState::V3(v3_state));
        }

        info!(
            total_pools = state.len(),
            "MarketState populated successfully via Multicall3"
        );
        Ok(())
    }
}
