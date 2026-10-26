//! Predefined whitelist of canonical Ethereum mainnet tokens and high-volume pools.

use alloy::primitives::{address, Address};

// Canonical Ethereum mainnet tokens
pub const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
pub const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
pub const USDT: Address = address!("dAC17F958D2ee523a2206206994597C13D831ec7");
pub const DAI: Address = address!("6B175474E89094C44Da98b954EedeAC495271d0F");
pub const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");

// Uniswap V2 canonical pools
pub const UNISWAP_V2_WETH_USDC: Address = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");
pub const UNISWAP_V2_WETH_USDT: Address = address!("0d4a11d5EEaaC28EC3F61d100daF4d40471f1852");
pub const UNISWAP_V2_DAI_WETH: Address = address!("A478c2975Ab1Ea89e8196811F51A7B7Ade33eB11");

// SushiSwap V2 canonical pools
pub const SUSHISWAP_V2_WETH_USDC: Address = address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0");
pub const SUSHISWAP_V2_WETH_USDT: Address = address!("06da0fd433C1A5d7a4faa01111c044910A184553");

// Uniswap V3 canonical pools
pub const UNISWAP_V3_WETH_USDC_500: Address = address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640");
pub const UNISWAP_V3_WETH_USDC_3000: Address = address!("8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8");
pub const UNISWAP_V3_WETH_USDT_500: Address = address!("11b815efB8f581194ae79006d24E0d814B7697F6");
pub const UNISWAP_V3_WETH_USDT_3000: Address = address!("4e68Ccd3E89f51C3074ca5072bbAC773960dFA36");
pub const UNISWAP_V3_WBTC_WETH_3000: Address = address!("CBCdBF3946dCD9B14774376b444558e319796179");

/// Returns the default list of canonical V2 pools (Uniswap V2 + SushiSwap V2).
pub fn default_canonical_v2_pools() -> Vec<Address> {
    vec![
        UNISWAP_V2_WETH_USDC,
        UNISWAP_V2_WETH_USDT,
        UNISWAP_V2_DAI_WETH,
        SUSHISWAP_V2_WETH_USDC,
        SUSHISWAP_V2_WETH_USDT,
    ]
}

/// Returns the default list of canonical V3 pools.
pub fn default_canonical_v3_pools() -> Vec<Address> {
    vec![
        UNISWAP_V3_WETH_USDC_500,
        UNISWAP_V3_WETH_USDC_3000,
        UNISWAP_V3_WETH_USDT_500,
        UNISWAP_V3_WETH_USDT_3000,
        UNISWAP_V3_WBTC_WETH_3000,
    ]
}
