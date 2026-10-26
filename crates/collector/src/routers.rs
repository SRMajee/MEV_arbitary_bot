//! Target DEX router addresses and O(1) recipient matching.

use alloy::primitives::{address, Address};

/// Uniswap V2 Router02 address on Ethereum Mainnet.
pub const UNISWAP_V2_ROUTER_02: Address = address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D");

/// SushiSwap V2 Router address on Ethereum Mainnet.
pub const SUSHISWAP_V2_ROUTER: Address = address!("d9e1cE17f2641f24aE83637ab66a2cca9C378B9F");

/// Uniswap V3 SwapRouter02 address on Ethereum Mainnet.
pub const UNISWAP_V3_SWAP_ROUTER_02: Address = address!("68b3465833fb72A70ecDF485E0e4C7bD8665Fc45");

/// Uniswap Universal Router (current) on Ethereum Mainnet.
pub const UNISWAP_UNIVERSAL_ROUTER: Address = address!("3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD");

/// Uniswap Universal Router (legacy v1.1) on Ethereum Mainnet.
pub const UNISWAP_UNIVERSAL_ROUTER_LEGACY: Address = address!("ef1c6e67703c7bd7107eed8303fbe6ec2554bf6b");

/// Fast O(1) check if a target address is one of our watched DEX routers.
#[inline(always)]
pub fn is_target_router(to: &Address) -> bool {
    *to == UNISWAP_V2_ROUTER_02
        || *to == SUSHISWAP_V2_ROUTER
        || *to == UNISWAP_V3_SWAP_ROUTER_02
        || *to == UNISWAP_UNIVERSAL_ROUTER
        || *to == UNISWAP_UNIVERSAL_ROUTER_LEGACY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_target_router() {
        assert!(is_target_router(&UNISWAP_V2_ROUTER_02));
        assert!(is_target_router(&SUSHISWAP_V2_ROUTER));
        assert!(is_target_router(&UNISWAP_V3_SWAP_ROUTER_02));
        assert!(is_target_router(&UNISWAP_UNIVERSAL_ROUTER));
        assert!(is_target_router(&UNISWAP_UNIVERSAL_ROUTER_LEGACY));

        let random_address = address!("1111111111111111111111111111111111111111");
        assert!(!is_target_router(&random_address));
    }
}
