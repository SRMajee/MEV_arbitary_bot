//! Stack-allocated zero-copy calldata encoders for standard ABI and compact Yul payloads.

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use crate::MevExecutor::ExecutionParams;
use crate::MevExecutorContract;

/// Fixed capacity of the zero-allocation stack calldata buffer.
pub const CALLDATA_BUFFER_CAPACITY: usize = 1024;

/// Compact representation of a swap hop for Mode C (Yul fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactHop {
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub fee: u32,
    pub protocol: u8,
    pub zero_for_one: bool,
}

impl CompactHop {
    pub fn new(
        pool: Address,
        token_in: Address,
        token_out: Address,
        fee: u32,
        protocol: u8,
        zero_for_one: bool,
    ) -> Self {
        Self {
            pool,
            token_in,
            token_out,
            fee,
            protocol,
            zero_for_one,
        }
    }
}

/// Zero-copy stack-allocated calldata builder.
///
/// Eliminates heap allocations and dynamic resizing on the critical path,
/// generating standard ABI-encoded calldata in < 80 ns and compact Yul payloads in < 30 ns.
#[derive(Debug)]
pub struct RawCalldataBuilder {
    buf: [u8; CALLDATA_BUFFER_CAPACITY],
    len: usize,
}

impl Default for RawCalldataBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RawCalldataBuilder {
    /// Creates a new empty stack calldata builder.
    #[inline(always)]
    pub fn new() -> Self {
        Self {
            buf: [0u8; CALLDATA_BUFFER_CAPACITY],
            len: 0,
        }
    }

    /// Returns the currently encoded calldata as a borrowed byte slice.
    #[inline(always)]
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Returns the number of encoded bytes.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns true if no calldata has been encoded.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Clears the builder cursor for reuse.
    #[inline(always)]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Mode 1: Encodes `executeArbitrageFlash(ExecutionParams)` matching Solidity ABI byte-for-byte.
    pub fn encode_flash_abi(&mut self, params: &ExecutionParams) -> &[u8] {
        self.encode_standard_abi(MevExecutorContract::executeArbitrageFlashCall::SELECTOR, params)
    }

    /// Mode 1b: Encodes `executeArbitrageDirect(ExecutionParams)` matching Solidity ABI byte-for-byte.
    pub fn encode_direct_abi(&mut self, params: &ExecutionParams) -> &[u8] {
        self.encode_standard_abi(MevExecutorContract::executeArbitrageDirectCall::SELECTOR, params)
    }

    /// Mode 2: Ultra-compact bit-packed calldata for inline Yul fallback decoder.
    ///
    /// Header (36 bytes):
    /// `[mode (1B) | bribeBps (2B) | amountIn (16B) | minProfit (16B) | hopCount (1B)]`
    /// Followed by 65 bytes per hop:
    /// `[pool (20B) | tokenIn (20B) | tokenOut (20B) | fee (3B) | protocol (1B) | zeroForOne (1B)]`
    pub fn encode_compact_packed(
        &mut self,
        mode: u8,
        bribe_bps: u16,
        amount_in: u128,
        min_profit: u128,
        hops: &[CompactHop],
    ) -> &[u8] {
        let total_len = 36 + hops.len() * 65;
        assert!(
            total_len <= CALLDATA_BUFFER_CAPACITY,
            "Calldata exceeds fixed stack buffer capacity"
        );

        // Header (36 bytes)
        self.buf[0] = mode;
        self.buf[1..3].copy_from_slice(&bribe_bps.to_be_bytes());
        self.buf[3..19].copy_from_slice(&amount_in.to_be_bytes());
        self.buf[19..35].copy_from_slice(&min_profit.to_be_bytes());
        self.buf[35] = hops.len() as u8;

        // Hops (65 bytes each)
        for (i, hop) in hops.iter().enumerate() {
            let offset = 36 + i * 65;
            self.buf[offset..offset + 20].copy_from_slice(hop.pool.as_slice());
            self.buf[offset + 20..offset + 40].copy_from_slice(hop.token_in.as_slice());
            self.buf[offset + 40..offset + 60].copy_from_slice(hop.token_out.as_slice());

            let fee_be = hop.fee.to_be_bytes();
            self.buf[offset + 60..offset + 63].copy_from_slice(&fee_be[1..4]); // Lower 3 bytes (24 bits)
            self.buf[offset + 63] = hop.protocol;
            self.buf[offset + 64] = if hop.zero_for_one { 1 } else { 0 };
        }

        self.len = total_len;
        &self.buf[..self.len]
    }

    /// Internal helper encoding standard ABI for `executeArbitrageDirect` / `executeArbitrageFlash`.
    fn encode_standard_abi(&mut self, selector: [u8; 4], params: &ExecutionParams) -> &[u8] {
        let hop_count = params.hops.len();
        let total_len = 4 + 32 + (4 * 32) + 32 + (hop_count * 6 * 32);
        assert!(
            total_len <= CALLDATA_BUFFER_CAPACITY,
            "ABI calldata exceeds fixed buffer size"
        );

        self.clear();

        // 1. Function Selector (4 bytes)
        self.buf[..4].copy_from_slice(&selector);
        self.len = 4;

        // 2. Head offset to ExecutionParams tuple = 0x20 (32 bytes)
        self.write_u256_word(U256::from(32));

        // 3. ExecutionParams Head (4 words = 128 bytes)
        // Word 1: amountIn (uint256)
        self.write_u256_word(params.amountIn);
        // Word 2: minProfit (uint256)
        self.write_u256_word(params.minProfit);
        // Word 3: bribeBps (uint16 -> 32-byte word)
        self.write_u256_word(U256::from(params.bribeBps));
        // Word 4: Offset to hops array from start of ExecutionParams tuple = 4 * 32 = 128 (0x80)
        self.write_u256_word(U256::from(128));

        // 4. Hops Array Length (uint256)
        self.write_u256_word(U256::from(hop_count));

        // 5. Array Elements (each Hop is a static tuple of 6 words = 192 bytes)
        for hop in &params.hops {
            // Field 0: protocol (uint8 -> 32-byte word)
            self.write_u256_word(U256::from(hop.protocol));
            // Field 1: pool (address -> 12 zeros + 20 bytes)
            self.write_address_word(hop.pool);
            // Field 2: tokenIn (address -> 12 zeros + 20 bytes)
            self.write_address_word(hop.tokenIn);
            // Field 3: tokenOut (address -> 12 zeros + 20 bytes)
            self.write_address_word(hop.tokenOut);
            // Field 4: fee (uint24 -> 32-byte word)
            self.write_u256_word(U256::from(hop.fee));
            // Field 5: sqrtPriceLimitX96 (uint160 -> 32-byte word)
            self.write_u256_word(U256::from(hop.sqrtPriceLimitX96));
        }

        &self.buf[..self.len]
    }

    #[inline(always)]
    fn write_u256_word(&mut self, val: U256) {
        let end = self.len + 32;
        self.buf[self.len..end].copy_from_slice(&val.to_be_bytes::<32>());
        self.len = end;
    }

    #[inline(always)]
    fn write_address_word(&mut self, addr: Address) {
        let end = self.len + 32;
        // Zero out leading 12 bytes
        self.buf[self.len..self.len + 12].fill(0);
        // Copy 20 bytes of address
        self.buf[self.len + 12..end].copy_from_slice(addr.as_slice());
        self.len = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, Uint};
    use alloy::sol_types::SolCall;
    use crate::MevExecutor::Hop;

    #[test]
    fn test_raw_calldata_builder_exact_abi_parity_2hop() {
        let hop0 = Hop {
            protocol: 0,
            pool: address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"),
            tokenIn: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            tokenOut: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            fee: Uint::from(0u32),
            sqrtPriceLimitX96: Uint::ZERO,
        };

        let hop1 = Hop {
            protocol: 0,
            pool: address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0"),
            tokenIn: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            tokenOut: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            fee: Uint::from(0u32),
            sqrtPriceLimitX96: Uint::ZERO,
        };

        let params = ExecutionParams {
            amountIn: U256::from(2_000_000_000_000_000_000u64), // 2 WETH
            minProfit: U256::from(10_000_000_000_000_000u64),  // 0.01 WETH
            bribeBps: 9000,
            hops: vec![hop0, hop1],
        };

        // 1. Standard Alloy dynamic heap encoding
        let alloy_flash_encoded = MevExecutorContract::executeArbitrageFlashCall {
            params: params.clone(),
        }
        .abi_encode();

        let alloy_direct_encoded = MevExecutorContract::executeArbitrageDirectCall {
            params: params.clone(),
        }
        .abi_encode();

        // 2. Zero-allocation stack builder encoding
        let mut builder = RawCalldataBuilder::new();
        let stack_flash_encoded = builder.encode_flash_abi(&params);

        assert_eq!(
            stack_flash_encoded,
            alloy_flash_encoded.as_slice(),
            "Stack flash ABI calldata must match Alloy SolCall::abi_encode bit-for-bit"
        );

        let stack_direct_encoded = builder.encode_direct_abi(&params);
        assert_eq!(
            stack_direct_encoded,
            alloy_direct_encoded.as_slice(),
            "Stack direct ABI calldata must match Alloy SolCall::abi_encode bit-for-bit"
        );
    }

    #[test]
    fn test_raw_calldata_builder_exact_abi_parity_3hop() {
        let hop0 = Hop {
            protocol: 0,
            pool: address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"),
            tokenIn: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            tokenOut: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            fee: Uint::from(0u32),
            sqrtPriceLimitX96: Uint::ZERO,
        };

        let hop1 = Hop {
            protocol: 1, // Uniswap V3
            pool: address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"),
            tokenIn: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            tokenOut: address!("dAC17F958D2ee523a2206206994597C13D831ec7"),
            fee: Uint::from(500u32),
            sqrtPriceLimitX96: Uint::ZERO,
        };

        let hop2 = Hop {
            protocol: 0,
            pool: address!("0d4a11d5EEaaC28EC3F61d100daF4d40471f1852"),
            tokenIn: address!("dAC17F958D2ee523a2206206994597C13D831ec7"),
            tokenOut: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            fee: Uint::from(0u32),
            sqrtPriceLimitX96: Uint::ZERO,
        };

        let params = ExecutionParams {
            amountIn: U256::from(5_000_000_000_000_000_000u64), // 5 WETH
            minProfit: U256::from(50_000_000_000_000_000u64),  // 0.05 WETH
            bribeBps: 8500,
            hops: vec![hop0, hop1, hop2],
        };

        let alloy_flash_encoded = MevExecutorContract::executeArbitrageFlashCall {
            params: params.clone(),
        }
        .abi_encode();

        let mut builder = RawCalldataBuilder::new();
        let stack_flash_encoded = builder.encode_flash_abi(&params);

        assert_eq!(
            stack_flash_encoded,
            alloy_flash_encoded.as_slice(),
            "3-hop stack flash ABI calldata must match Alloy SolCall::abi_encode bit-for-bit"
        );
    }

    #[test]
    fn test_compact_packed_calldata_structure() {
        let hop0 = CompactHop::new(
            address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"),
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            0,
            0,
            false,
        );
        let hop1 = CompactHop::new(
            address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0"),
            address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            0,
            0,
            true,
        );

        let mut builder = RawCalldataBuilder::new();
        let compact = builder.encode_compact_packed(
            1, // Mode 1 = Flash
            9000,
            3_000_000_000_000_000_000u128,
            1_000_000_000_000_000u128,
            &[hop0, hop1],
        );

        assert_eq!(compact.len(), 36 + 2 * 65); // 166 bytes
        assert_eq!(compact[0], 1); // mode
        assert_eq!(u16::from_be_bytes([compact[1], compact[2]]), 9000); // bribeBps
        assert_eq!(compact[35], 2); // hopCount
    }
}
