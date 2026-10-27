//! Deterministic, non-cryptographic two-way registry mapping 20-byte `Address` to contiguous `u16` IDs.

use alloy::primitives::Address;
use rustc_hash::FxHashMap;
use std::fmt::Debug;
use std::hash::Hash;

/// A contiguous two-way registry mapping 20-byte `Address` (cold ingestion path)
/// to compact `u16` IDs (hot routing/pricing path).
///
/// Features:
/// - $O(1)$ deterministic Address -> Id lookup using `FxHashMap` (avoiding cryptographic SipHash overhead).
/// - $O(1)$ direct array indexing Id -> Address using `Vec<Address>`.
/// - Contiguous integer allocation starting at 0 up to `u16::MAX`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdRegistry<Id> {
    addr_to_id: FxHashMap<Address, Id>,
    id_to_addr: Vec<Address>,
}

impl<Id> Default for IdRegistry<Id> {
    fn default() -> Self {
        Self {
            addr_to_id: FxHashMap::default(),
            id_to_addr: Vec::new(),
        }
    }
}

impl<Id> IdRegistry<Id>
where
    Id: Copy + Eq + Hash + From<u16> + Into<u16> + Debug,
{
    /// Creates a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an address if not already present, returning its assigned `Id`.
    /// Panics if more than `u16::MAX` items are registered.
    pub fn get_or_register(&mut self, address: Address) -> Id {
        if let Some(&id) = self.addr_to_id.get(&address) {
            return id;
        }

        let next_idx = self.id_to_addr.len();
        assert!(next_idx <= u16::MAX as usize, "IdRegistry overflow: exceeded u16 capacity");
        let id = Id::from(next_idx as u16);
        self.addr_to_id.insert(address, id);
        self.id_to_addr.push(address);
        id
    }

    /// Gets the compact `Id` for an address if registered.
    #[inline(always)]
    pub fn get_id(&self, address: &Address) -> Option<Id> {
        self.addr_to_id.get(address).copied()
    }

    /// Gets the original 20-byte `Address` for an `Id` via direct slice indexing.
    #[inline(always)]
    pub fn get_address(&self, id: Id) -> Option<Address> {
        let idx = id.into() as usize;
        self.id_to_addr.get(idx).copied()
    }

    /// Returns the number of registered items.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.id_to_addr.len()
    }

    /// Returns `true` if the registry is empty.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.id_to_addr.is_empty()
    }

    /// Returns an immutable slice of all registered addresses, indexed by `Id`.
    #[inline(always)]
    pub fn addresses(&self) -> &[Address] {
        &self.id_to_addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use mev_types::{PoolId, TokenId};

    #[test]
    fn test_token_id_registry_two_way_mapping() {
        let mut registry = IdRegistry::<TokenId>::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);

        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
        let usdt = address!("dAC17F958D2ee523a2206206994597C13D831ec7");

        let id_weth = registry.get_or_register(weth);
        let id_usdc = registry.get_or_register(usdc);
        let id_usdt = registry.get_or_register(usdt);

        assert_eq!(id_weth, TokenId(0));
        assert_eq!(id_usdc, TokenId(1));
        assert_eq!(id_usdt, TokenId(2));

        // Idempotency
        assert_eq!(registry.get_or_register(weth), TokenId(0));
        assert_eq!(registry.get_or_register(usdc), TokenId(1));
        assert_eq!(registry.len(), 3);

        // Forward lookup
        assert_eq!(registry.get_id(&weth), Some(TokenId(0)));
        assert_eq!(registry.get_id(&usdc), Some(TokenId(1)));
        assert_eq!(registry.get_id(&usdt), Some(TokenId(2)));

        let unknown = address!("1111111111111111111111111111111111111111");
        assert_eq!(registry.get_id(&unknown), None);

        // Reverse lookup
        assert_eq!(registry.get_address(TokenId(0)), Some(weth));
        assert_eq!(registry.get_address(TokenId(1)), Some(usdc));
        assert_eq!(registry.get_address(TokenId(2)), Some(usdt));
        assert_eq!(registry.get_address(TokenId(99)), None);
    }

    #[test]
    fn test_pool_id_registry_order() {
        let mut registry = IdRegistry::<PoolId>::new();
        let pool0 = address!("0000000000000000000000000000000000000001");
        let pool1 = address!("0000000000000000000000000000000000000002");

        let p0_id = registry.get_or_register(pool0);
        let p1_id = registry.get_or_register(pool1);

        assert_eq!(p0_id, PoolId(0));
        assert_eq!(p1_id, PoolId(1));
        assert_eq!(registry.addresses(), &[pool0, pool1]);
    }
}
