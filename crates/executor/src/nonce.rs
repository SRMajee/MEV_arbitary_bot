//! Lock-free atomic nonce manager with block-scoped peek and commit.

use std::sync::atomic::{AtomicU64, Ordering};

/// Lock-free atomic nonce manager.
///
/// Provides ~1 ns lockless reads for candidate simulations targeting block N+1,
/// and ~3 ns atomic increments when committing a transaction for wire broadcast.
#[derive(Debug)]
pub struct AtomicNonceManager {
    nonce: AtomicU64,
}

impl Default for AtomicNonceManager {
    fn default() -> Self {
        Self::new(0)
    }
}

impl AtomicNonceManager {
    /// Creates a new nonce manager initialized to `initial_nonce`.
    pub fn new(initial_nonce: u64) -> Self {
        Self {
            nonce: AtomicU64::new(initial_nonce),
        }
    }

    /// Returns the current confirmed/base nonce without incrementing (~1 ns).
    ///
    /// Used for candidate simulations and competing bundles targeting the same next block N+1.
    #[inline(always)]
    pub fn current_nonce(&self) -> u64 {
        self.nonce.load(Ordering::Acquire)
    }

    /// Atomically fetches and increments the nonce (~3 ns).
    ///
    /// Used when committing a transaction for wire broadcast.
    #[inline(always)]
    pub fn fetch_and_increment(&self) -> u64 {
        self.nonce.fetch_add(1, Ordering::AcqRel)
    }

    /// Resynchronizes the nonce with an on-chain confirmed count (e.g. on new block tick).
    #[inline(always)]
    pub fn resync(&self, confirmed_nonce: u64) {
        self.nonce.store(confirmed_nonce, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_atomic_nonce_manager_basic_ops() {
        let manager = AtomicNonceManager::new(42);
        assert_eq!(manager.current_nonce(), 42);

        // Peek should not advance nonce
        assert_eq!(manager.current_nonce(), 42);

        // Commit increments nonce
        assert_eq!(manager.fetch_and_increment(), 42);
        assert_eq!(manager.current_nonce(), 43);

        // Resync resets nonce
        manager.resync(100);
        assert_eq!(manager.current_nonce(), 100);
    }

    #[test]
    fn test_atomic_nonce_manager_concurrent_increment() {
        let manager = Arc::new(AtomicNonceManager::new(0));
        let threads: Vec<_> = (0..10)
            .map(|_| {
                let m = Arc::clone(&manager);
                thread::spawn(move || {
                    for _ in 0..1000 {
                        m.fetch_and_increment();
                    }
                })
            })
            .collect();

        for t in threads {
            t.join().unwrap();
        }

        assert_eq!(manager.current_nonce(), 10_000);
    }
}
