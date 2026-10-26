//! High-throughput calldata decoders for Uniswap V2, SushiSwap, Uniswap V3, and Universal Router.

use alloy::{
    primitives::{Address, B256, U256},
    sol,
    sol_types::{SolInterface, SolValue},
};
use mev_types::{DecodedSwap, PoolVariant};

// Sol interface bindings for DEX Routers
sol! {
    interface IUniswapV2RouterABI {
        function swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin, address[] calldata path, address to, uint256 deadline) external returns (uint256[] memory amounts);
        function swapTokensForExactTokens(uint256 amountOut, uint256 amountInMax, address[] calldata path, address to, uint256 deadline) external returns (uint256[] memory amounts);
        function swapExactETHForTokens(uint256 amountOutMin, address[] calldata path, address to, uint256 deadline) external payable returns (uint256[] memory amounts);
        function swapTokensForExactETH(uint256 amountOut, uint256 amountInMax, address[] calldata path, address to, uint256 deadline) external returns (uint256[] memory amounts);
        function swapExactTokensForETH(uint256 amountIn, uint256 amountOutMin, address[] calldata path, address to, uint256 deadline) external returns (uint256[] memory amounts);
        function swapETHForExactTokens(uint256 amountOut, address[] calldata path, address to, uint256 deadline) external payable returns (uint256[] memory amounts);

        function swapExactTokensForTokensSupportingFeeOnTransferTokens(uint256 amountIn, uint256 amountOutMin, address[] calldata path, address to, uint256 deadline) external;
        function swapExactETHForTokensSupportingFeeOnTransferTokens(uint256 amountOutMin, address[] calldata path, address to, uint256 deadline) external payable;
        function swapExactTokensForETHSupportingFeeOnTransferTokens(uint256 amountIn, uint256 amountOutMin, address[] calldata path, address to, uint256 deadline) external;
    }

    interface ISwapRouter02ABI {
        struct ExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint24 fee;
            address recipient;
            uint256 amountIn;
            uint256 amountOutMinimum;
            uint160 sqrtPriceLimitX96;
        }

        struct ExactInputParams {
            bytes path;
            address recipient;
            uint256 amountIn;
            uint256 amountOutMinimum;
        }

        struct ExactOutputSingleParams {
            address tokenIn;
            address tokenOut;
            uint24 fee;
            address recipient;
            uint256 amountOut;
            uint256 amountInMaximum;
            uint160 sqrtPriceLimitX96;
        }

        function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
        function exactInput(ExactInputParams calldata params) external payable returns (uint256 amountOut);
        function exactOutputSingle(ExactOutputSingleParams calldata params) external payable returns (uint256 amountIn);
        function multicall(bytes[] calldata data) external payable returns (bytes[] memory results);
        function multicall(uint256 deadline, bytes[] calldata data) external payable returns (bytes[] memory results);
    }

    interface IUniversalRouterABI {
        function execute(bytes calldata commands, bytes[] calldata inputs, uint256 deadline) external payable;
        function execute(bytes calldata commands, bytes[] calldata inputs) external payable;
    }

    // Universal Router Command ABI types
    struct UniversalRouterV3ExactIn {
        address recipient;
        uint256 amountIn;
        uint256 amountOutMin;
        bytes path;
        bool payerIsUser;
    }

    struct UniversalRouterV3ExactOut {
        address recipient;
        uint256 amountOut;
        uint256 amountInMax;
        bytes path;
        bool payerIsUser;
    }

    struct UniversalRouterV2ExactIn {
        address recipient;
        uint256 amountIn;
        uint256 amountOutMin;
        address[] path;
        bool payerIsUser;
    }

    struct UniversalRouterV2ExactOut {
        address recipient;
        uint256 amountOut;
        uint256 amountInMax;
        address[] path;
        bool payerIsUser;
    }
}

/// Universal Router command flags (first 5 bits of command byte).
pub const UR_FLAG_V3_SWAP_EXACT_IN: u8 = 0x00;
pub const UR_FLAG_V3_SWAP_EXACT_OUT: u8 = 0x01;
pub const UR_FLAG_V2_SWAP_EXACT_IN: u8 = 0x08;
pub const UR_FLAG_V2_SWAP_EXACT_OUT: u8 = 0x09;

/// Primary decoder extracting DecodedSwap instances from transaction calldata.
pub struct CalldataDecoder;

impl CalldataDecoder {
    /// Attempts to decode arbitrary transaction calldata targeting one of our watched DEX routers.
    ///
    /// Can return multiple swaps if the transaction invokes `multicall()` or Universal Router `execute()`.
    pub fn decode(
        tx_hash: B256,
        router: Address,
        value: U256,
        calldata: &[u8],
    ) -> Vec<DecodedSwap> {
        if calldata.len() < 4 {
            return Vec::new();
        }

        // 1. Try Uniswap V2 / SushiSwap Router
        if let Some(swaps) = Self::decode_v2(tx_hash, router, value, calldata) {
            return swaps;
        }

        // 2. Try Uniswap V3 SwapRouter02
        if let Some(swaps) = Self::decode_v3(tx_hash, router, calldata) {
            return swaps;
        }

        // 3. Try Uniswap Universal Router
        if let Some(swaps) = Self::decode_universal_router(tx_hash, router, calldata) {
            return swaps;
        }

        Vec::new()
    }

    /// Decodes Uniswap V2 / SushiSwap Router calldata.
    fn decode_v2(
        tx_hash: B256,
        router: Address,
        value: U256,
        calldata: &[u8],
    ) -> Option<Vec<DecodedSwap>> {
        use IUniswapV2RouterABI::IUniswapV2RouterABICalls::*;

        match IUniswapV2RouterABI::IUniswapV2RouterABICalls::abi_decode(calldata) {
            Ok(swapExactTokensForTokens(call)) => {
                if call.path.len() >= 2 {
                    Some(vec![DecodedSwap {
                        tx_hash,
                        router,
                        protocol: PoolVariant::UniswapV2,
                        token_in: call.path[0],
                        token_out: *call.path.last().unwrap(),
                        path: call.path,
                        amount_in: call.amountIn,
                        amount_out_min: call.amountOutMin,
                        recipient: call.to,
                    }])
                } else {
                    None
                }
            }
            Ok(swapTokensForExactTokens(call)) => {
                if call.path.len() >= 2 {
                    Some(vec![DecodedSwap {
                        tx_hash,
                        router,
                        protocol: PoolVariant::UniswapV2,
                        token_in: call.path[0],
                        token_out: *call.path.last().unwrap(),
                        path: call.path,
                        amount_in: call.amountInMax,
                        amount_out_min: call.amountOut,
                        recipient: call.to,
                    }])
                } else {
                    None
                }
            }
            Ok(swapExactETHForTokens(call)) => {
                if call.path.len() >= 2 {
                    Some(vec![DecodedSwap {
                        tx_hash,
                        router,
                        protocol: PoolVariant::UniswapV2,
                        token_in: call.path[0],
                        token_out: *call.path.last().unwrap(),
                        path: call.path,
                        amount_in: value,
                        amount_out_min: call.amountOutMin,
                        recipient: call.to,
                    }])
                } else {
                    None
                }
            }
            Ok(swapExactTokensForETH(call)) => {
                if call.path.len() >= 2 {
                    Some(vec![DecodedSwap {
                        tx_hash,
                        router,
                        protocol: PoolVariant::UniswapV2,
                        token_in: call.path[0],
                        token_out: *call.path.last().unwrap(),
                        path: call.path,
                        amount_in: call.amountIn,
                        amount_out_min: call.amountOutMin,
                        recipient: call.to,
                    }])
                } else {
                    None
                }
            }
            Ok(swapExactTokensForTokensSupportingFeeOnTransferTokens(call)) => {
                if call.path.len() >= 2 {
                    Some(vec![DecodedSwap {
                        tx_hash,
                        router,
                        protocol: PoolVariant::UniswapV2,
                        token_in: call.path[0],
                        token_out: *call.path.last().unwrap(),
                        path: call.path,
                        amount_in: call.amountIn,
                        amount_out_min: call.amountOutMin,
                        recipient: call.to,
                    }])
                } else {
                    None
                }
            }
            Ok(swapExactETHForTokensSupportingFeeOnTransferTokens(call)) => {
                if call.path.len() >= 2 {
                    Some(vec![DecodedSwap {
                        tx_hash,
                        router,
                        protocol: PoolVariant::UniswapV2,
                        token_in: call.path[0],
                        token_out: *call.path.last().unwrap(),
                        path: call.path,
                        amount_in: value,
                        amount_out_min: call.amountOutMin,
                        recipient: call.to,
                    }])
                } else {
                    None
                }
            }
            Ok(swapExactTokensForETHSupportingFeeOnTransferTokens(call)) => {
                if call.path.len() >= 2 {
                    Some(vec![DecodedSwap {
                        tx_hash,
                        router,
                        protocol: PoolVariant::UniswapV2,
                        token_in: call.path[0],
                        token_out: *call.path.last().unwrap(),
                        path: call.path,
                        amount_in: call.amountIn,
                        amount_out_min: call.amountOutMin,
                        recipient: call.to,
                    }])
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Decodes Uniswap V3 SwapRouter02 calldata (including multicall recursive unpacking).
    fn decode_v3(
        tx_hash: B256,
        router: Address,
        calldata: &[u8],
    ) -> Option<Vec<DecodedSwap>> {
        use ISwapRouter02ABI::ISwapRouter02ABICalls::*;

        match ISwapRouter02ABI::ISwapRouter02ABICalls::abi_decode(calldata) {
            Ok(exactInputSingle(call)) => {
                Some(vec![DecodedSwap {
                    tx_hash,
                    router,
                    protocol: PoolVariant::UniswapV3,
                    token_in: call.params.tokenIn,
                    token_out: call.params.tokenOut,
                    path: vec![call.params.tokenIn, call.params.tokenOut],
                    amount_in: call.params.amountIn,
                    amount_out_min: call.params.amountOutMinimum,
                    recipient: call.params.recipient,
                }])
            }
            Ok(exactInput(call)) => {
                if let Some((first, last, path)) = parse_v3_path(&call.params.path) {
                    Some(vec![DecodedSwap {
                        tx_hash,
                        router,
                        protocol: PoolVariant::UniswapV3,
                        token_in: first,
                        token_out: last,
                        path,
                        amount_in: call.params.amountIn,
                        amount_out_min: call.params.amountOutMinimum,
                        recipient: call.params.recipient,
                    }])
                } else {
                    None
                }
            }
            Ok(exactOutputSingle(call)) => {
                Some(vec![DecodedSwap {
                    tx_hash,
                    router,
                    protocol: PoolVariant::UniswapV3,
                    token_in: call.params.tokenIn,
                    token_out: call.params.tokenOut,
                    path: vec![call.params.tokenIn, call.params.tokenOut],
                    amount_in: call.params.amountInMaximum,
                    amount_out_min: call.params.amountOut,
                    recipient: call.params.recipient,
                }])
            }
            Ok(multicall_0(call)) => {
                let mut swaps = Vec::new();
                for subcall in &call.data {
                    if let Some(mut sub_swaps) = Self::decode_v3(tx_hash, router, subcall) {
                        swaps.append(&mut sub_swaps);
                    }
                }
                if swaps.is_empty() {
                    None
                } else {
                    Some(swaps)
                }
            }
            Ok(multicall_1(call)) => {
                let mut swaps = Vec::new();
                for subcall in &call.data {
                    if let Some(mut sub_swaps) = Self::decode_v3(tx_hash, router, subcall) {
                        swaps.append(&mut sub_swaps);
                    }
                }
                if swaps.is_empty() {
                    None
                } else {
                    Some(swaps)
                }
            }
            _ => None,
        }
    }

    /// Decodes Uniswap Universal Router `execute(bytes commands, bytes[] inputs)`.
    fn decode_universal_router(
        tx_hash: B256,
        router: Address,
        calldata: &[u8],
    ) -> Option<Vec<DecodedSwap>> {
        use IUniversalRouterABI::IUniversalRouterABICalls::*;

        let (commands, inputs) = match IUniversalRouterABI::IUniversalRouterABICalls::abi_decode(calldata) {
            Ok(execute_0(call)) => (call.commands, call.inputs),
            Ok(execute_1(call)) => (call.commands, call.inputs),
            _ => return None,
        };

        let mut swaps = Vec::new();
        let cmd_bytes = commands.as_ref();

        for (i, &cmd) in cmd_bytes.iter().enumerate() {
            if i >= inputs.len() {
                break;
            }

            let input: &[u8] = inputs[i].as_ref();
            let command_type = cmd & 0x1f; // mask out allow-revert flag (0x20)

            match command_type {
                UR_FLAG_V2_SWAP_EXACT_IN => {
                    if let Ok(decoded) = UniversalRouterV2ExactIn::abi_decode(input) {
                        if decoded.path.len() >= 2 {
                            swaps.push(DecodedSwap {
                                tx_hash,
                                router,
                                protocol: PoolVariant::UniswapV2,
                                token_in: decoded.path[0],
                                token_out: *decoded.path.last().unwrap(),
                                path: decoded.path,
                                amount_in: decoded.amountIn,
                                amount_out_min: decoded.amountOutMin,
                                recipient: decoded.recipient,
                            });
                        }
                    }
                }
                UR_FLAG_V2_SWAP_EXACT_OUT => {
                    if let Ok(decoded) = UniversalRouterV2ExactOut::abi_decode(input) {
                        if decoded.path.len() >= 2 {
                            swaps.push(DecodedSwap {
                                tx_hash,
                                router,
                                protocol: PoolVariant::UniswapV2,
                                token_in: decoded.path[0],
                                token_out: *decoded.path.last().unwrap(),
                                path: decoded.path,
                                amount_in: decoded.amountInMax,
                                amount_out_min: decoded.amountOut,
                                recipient: decoded.recipient,
                            });
                        }
                    }
                }
                UR_FLAG_V3_SWAP_EXACT_IN => {
                    if let Ok(decoded) = UniversalRouterV3ExactIn::abi_decode(input) {
                        if let Some((first, last, path)) = parse_v3_path(&decoded.path) {
                            swaps.push(DecodedSwap {
                                tx_hash,
                                router,
                                protocol: PoolVariant::UniswapV3,
                                token_in: first,
                                token_out: last,
                                path,
                                amount_in: decoded.amountIn,
                                amount_out_min: decoded.amountOutMin,
                                recipient: decoded.recipient,
                            });
                        }
                    }
                }
                UR_FLAG_V3_SWAP_EXACT_OUT => {
                    if let Ok(decoded) = UniversalRouterV3ExactOut::abi_decode(input) {
                        if let Some((first, last, path)) = parse_v3_path(&decoded.path) {
                            swaps.push(DecodedSwap {
                                tx_hash,
                                router,
                                protocol: PoolVariant::UniswapV3,
                                token_in: first,
                                token_out: last,
                                path,
                                amount_in: decoded.amountInMax,
                                amount_out_min: decoded.amountOut,
                                recipient: decoded.recipient,
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        if swaps.is_empty() {
            None
        } else {
            Some(swaps)
        }
    }
}

/// Helper extracting tokens from Uniswap V3 packed path: `tokenIn(20) + fee(3) + tokenNext(20)...`
pub fn parse_v3_path(path: &[u8]) -> Option<(Address, Address, Vec<Address>)> {
    if path.len() < 43 {
        // Minimum single hop: 20 + 3 + 20 = 43 bytes
        return None;
    }

    let first = Address::from_slice(&path[0..20]);
    let last = Address::from_slice(&path[path.len() - 20..]);

    let mut tokens = Vec::new();
    let mut offset = 0;
    while offset + 20 <= path.len() {
        tokens.push(Address::from_slice(&path[offset..offset + 20]));
        offset += 23; // 20 bytes address + 3 bytes fee
    }

    Some((first, last, tokens))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use alloy::sol_types::SolCall;

    #[test]
    fn test_decode_v2_swap_exact_tokens() {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        let to = address!("1111111111111111111111111111111111111111");

        let call = IUniswapV2RouterABI::swapExactTokensForTokensCall {
            amountIn: U256::from(100_000_000),
            amountOutMin: U256::from(50_000_000),
            path: vec![weth, usdc],
            to,
            deadline: U256::from(1999999999),
        };

        let calldata = call.abi_encode();
        let swaps = CalldataDecoder::decode(B256::ZERO, Address::ZERO, U256::ZERO, &calldata);

        assert_eq!(swaps.len(), 1);
        let s = &swaps[0];
        assert_eq!(s.protocol, PoolVariant::UniswapV2);
        assert_eq!(s.token_in, weth);
        assert_eq!(s.token_out, usdc);
        assert_eq!(s.amount_in, U256::from(100_000_000));
        assert_eq!(s.amount_out_min, U256::from(50_000_000));
        assert_eq!(s.recipient, to);
    }

    #[test]
    fn test_decode_v3_exact_input_single() {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        let to = address!("2222222222222222222222222222222222222222");

        let call = ISwapRouter02ABI::exactInputSingleCall {
            params: ISwapRouter02ABI::ExactInputSingleParams {
                tokenIn: weth,
                tokenOut: usdc,
                fee: alloy::primitives::Uint::from(500u32),
                recipient: to,
                amountIn: U256::from(5_000_000),
                amountOutMinimum: U256::from(4_900_000),
                sqrtPriceLimitX96: alloy::primitives::Uint::ZERO,
            },
        };

        let calldata = call.abi_encode();
        let swaps = CalldataDecoder::decode(B256::ZERO, Address::ZERO, U256::ZERO, &calldata);

        assert_eq!(swaps.len(), 1);
        let s = &swaps[0];
        assert_eq!(s.protocol, PoolVariant::UniswapV3);
        assert_eq!(s.token_in, weth);
        assert_eq!(s.token_out, usdc);
        assert_eq!(s.amount_in, U256::from(5_000_000));
        assert_eq!(s.amount_out_min, U256::from(4_900_000));
    }

    #[test]
    fn test_decode_v3_multicall() {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        let dai = address!("6B175474E89094C44Da98b954EedeAC495271d0F");

        let call1 = ISwapRouter02ABI::exactInputSingleCall {
            params: ISwapRouter02ABI::ExactInputSingleParams {
                tokenIn: weth,
                tokenOut: usdc,
                fee: alloy::primitives::Uint::from(500u32),
                recipient: Address::ZERO,
                amountIn: U256::from(100),
                amountOutMinimum: U256::from(90),
                sqrtPriceLimitX96: alloy::primitives::Uint::ZERO,
            },
        };
        let call2 = ISwapRouter02ABI::exactInputSingleCall {
            params: ISwapRouter02ABI::ExactInputSingleParams {
                tokenIn: usdc,
                tokenOut: dai,
                fee: alloy::primitives::Uint::from(100u32),
                recipient: Address::ZERO,
                amountIn: U256::from(90),
                amountOutMinimum: U256::from(89),
                sqrtPriceLimitX96: alloy::primitives::Uint::ZERO,
            },
        };

        let multicall = ISwapRouter02ABI::multicall_0Call {
            data: vec![call1.abi_encode().into(), call2.abi_encode().into()],
        };

        let calldata = multicall.abi_encode();
        let swaps = CalldataDecoder::decode(B256::ZERO, Address::ZERO, U256::ZERO, &calldata);

        assert_eq!(swaps.len(), 2);
        assert_eq!(swaps[0].token_in, weth);
        assert_eq!(swaps[0].token_out, usdc);
        assert_eq!(swaps[1].token_in, usdc);
        assert_eq!(swaps[1].token_out, dai);
    }

    #[test]
    fn test_decode_universal_router_mixed_commands() {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        let recipient = address!("3333333333333333333333333333333333333333");

        // V2 input
        let v2_input = UniversalRouterV2ExactIn {
            recipient,
            amountIn: U256::from(1000),
            amountOutMin: U256::from(950),
            path: vec![weth, usdc],
            payerIsUser: true,
        };

        // V3 input with packed path (weth + fee(3) + usdc)
        let mut v3_path = Vec::new();
        v3_path.extend_from_slice(weth.as_slice());
        v3_path.extend_from_slice(&[0x00, 0x01, 0xf4]); // 500 fee
        v3_path.extend_from_slice(usdc.as_slice());

        let v3_input = UniversalRouterV3ExactIn {
            recipient,
            amountIn: U256::from(2000),
            amountOutMin: U256::from(1900),
            path: v3_path.into(),
            payerIsUser: true,
        };

        let ur_call = IUniversalRouterABI::execute_1Call {
            commands: vec![UR_FLAG_V2_SWAP_EXACT_IN, UR_FLAG_V3_SWAP_EXACT_IN].into(),
            inputs: vec![v2_input.abi_encode().into(), v3_input.abi_encode().into()],
        };

        let calldata = ur_call.abi_encode();
        let swaps = CalldataDecoder::decode(B256::ZERO, Address::ZERO, U256::ZERO, &calldata);

        assert_eq!(swaps.len(), 2);
        assert_eq!(swaps[0].protocol, PoolVariant::UniswapV2);
        assert_eq!(swaps[0].amount_in, U256::from(1000));
        assert_eq!(swaps[1].protocol, PoolVariant::UniswapV3);
        assert_eq!(swaps[1].amount_in, U256::from(2000));
    }
}
