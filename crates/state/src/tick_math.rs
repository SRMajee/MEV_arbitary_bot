//! Uniswap V3 tick bitmap and bitwise navigation utilities.
//!
//! Replicates Uniswap V3's `TickBitmap.sol` logic with bitwise operations,
//! mapping active ticks to bitmap words and finding nearest initialized ticks.

use alloy::primitives::U256;
use std::collections::BTreeMap;

/// Minimum allowable tick in Uniswap V3 (-887272).
pub const MIN_TICK: i32 = -887272;
/// Maximum allowable tick in Uniswap V3 (887272).
pub const MAX_TICK: i32 = 887272;

/// Computes the compressed tick for a given tick and tick spacing.
#[inline]
pub fn compress_tick(tick: i32, tick_spacing: i32) -> i32 {
    if tick < 0 && tick % tick_spacing != 0 {
        (tick / tick_spacing) - 1
    } else {
        tick / tick_spacing
    }
}

/// Returns the word position `int16(compressed >> 8)` for a compressed tick.
#[inline]
pub fn tick_to_word_pos(compressed: i32) -> i16 {
    (compressed >> 8) as i16
}

/// Returns the bit position within the word `uint8(compressed & 0xff)` (0..=255).
#[inline]
pub fn tick_to_bit_pos(compressed: i32) -> u8 {
    (compressed & 0xff) as u8
}

/// Finds the next initialized tick within a single 256-bit word of the tick bitmap.
///
/// Returns `(next_tick, initialized)`.
/// - If `lte = true` (price decreasing / moving left), searches for initialized ticks $\le$ current tick.
/// - If `lte = false` (price increasing / moving right), searches for initialized ticks $>$ current tick.
pub fn next_initialized_tick_within_one_word(
    tick_bitmap: &BTreeMap<i16, U256>,
    tick: i32,
    tick_spacing: i32,
    lte: bool,
) -> (i32, bool) {
    let compressed = compress_tick(tick, tick_spacing);

    if lte {
        let word_pos = tick_to_word_pos(compressed);
        let bit_pos = tick_to_bit_pos(compressed);

        // Mask all bits at or below bit_pos: (1 << (bit_pos + 1)) - 1
        let mask = if bit_pos == 255 {
            U256::MAX
        } else {
            (U256::from(1) << (bit_pos + 1)) - U256::from(1)
        };

        let word = tick_bitmap.get(&word_pos).copied().unwrap_or(U256::ZERO);
        let masked = word & mask;

        if masked != U256::ZERO {
            // Most significant bit of masked
            let next_bit = 255 - (masked.leading_zeros() as u8);
            let next_compressed = ((word_pos as i32) << 8) + (next_bit as i32);
            let next_tick = next_compressed * tick_spacing;
            (next_tick, true)
        } else {
            // No initialized tick in this word <= bit_pos; return the bottom of this word
            let next_compressed = (word_pos as i32) << 8;
            let next_tick = next_compressed * tick_spacing;
            (next_tick, false)
        }
    } else {
        // Price increasing: search > current tick
        let next_compressed = compressed + 1;
        let word_pos = tick_to_word_pos(next_compressed);
        let bit_pos = tick_to_bit_pos(next_compressed);

        // Mask all bits at or above bit_pos: !((1 << bit_pos) - 1)
        let mask = if bit_pos == 0 {
            U256::MAX
        } else {
            !((U256::from(1) << bit_pos) - U256::from(1))
        };

        let word = tick_bitmap.get(&word_pos).copied().unwrap_or(U256::ZERO);
        let masked = word & mask;

        if masked != U256::ZERO {
            // Least significant bit of masked
            let next_bit = masked.trailing_zeros() as u8;
            let next_compressed = ((word_pos as i32) << 8) + (next_bit as i32);
            let next_tick = next_compressed * tick_spacing;
            (next_tick, true)
        } else {
            // No initialized tick in this word >= bit_pos; return the top of this word
            let next_compressed = ((word_pos as i32) << 8) + 255;
            let next_tick = next_compressed * tick_spacing;
            (next_tick, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tick_compression() {
        assert_eq!(compress_tick(0, 60), 0);
        assert_eq!(compress_tick(59, 60), 0);
        assert_eq!(compress_tick(60, 60), 1);
        assert_eq!(compress_tick(-1, 60), -1);
        assert_eq!(compress_tick(-60, 60), -1);
        assert_eq!(compress_tick(-61, 60), -2);
    }

    #[test]
    fn test_word_and_bit_positions() {
        assert_eq!(tick_to_word_pos(0), 0);
        assert_eq!(tick_to_bit_pos(0), 0);

        assert_eq!(tick_to_word_pos(255), 0);
        assert_eq!(tick_to_bit_pos(255), 255);

        assert_eq!(tick_to_word_pos(256), 1);
        assert_eq!(tick_to_bit_pos(256), 0);

        assert_eq!(tick_to_word_pos(-1), -1);
        assert_eq!(tick_to_bit_pos(-1), 255);

        assert_eq!(tick_to_word_pos(-256), -1);
        assert_eq!(tick_to_bit_pos(-256), 0);

        assert_eq!(tick_to_word_pos(-257), -2);
        assert_eq!(tick_to_bit_pos(-257), 255);
    }

    #[test]
    fn test_next_initialized_tick_lte() {
        let mut bitmap = BTreeMap::new();
        // Set bit 5 and bit 10 in word 0
        let word_0 = (U256::from(1) << 5) | (U256::from(1) << 10);
        bitmap.insert(0, word_0);

        // From tick corresponding to compressed = 12 (12 * 60 = 720), search lte:
        // Should find bit 10 (10 * 60 = 600)
        let (next_tick, init) = next_initialized_tick_within_one_word(&bitmap, 720, 60, true);
        assert!(init);
        assert_eq!(next_tick, 600);

        // From tick corresponding to compressed = 9 (9 * 60 = 540), search lte:
        // Should find bit 5 (5 * 60 = 300)
        let (next_tick, init) = next_initialized_tick_within_one_word(&bitmap, 540, 60, true);
        assert!(init);
        assert_eq!(next_tick, 300);

        // From tick corresponding to compressed = 4 (4 * 60 = 240), search lte:
        // No initialized bit <= 4 in word 0, so should return bottom of word (bit 0 * 60 = 0)
        let (next_tick, init) = next_initialized_tick_within_one_word(&bitmap, 240, 60, true);
        assert!(!init);
        assert_eq!(next_tick, 0);
    }

    #[test]
    fn test_next_initialized_tick_gt() {
        let mut bitmap = BTreeMap::new();
        // Set bit 5 and bit 10 in word 0
        let word_0 = (U256::from(1) << 5) | (U256::from(1) << 10);
        bitmap.insert(0, word_0);

        // From tick corresponding to compressed = 3 (3 * 60 = 180), search gt:
        // Should find bit 5 (5 * 60 = 300)
        let (next_tick, init) = next_initialized_tick_within_one_word(&bitmap, 180, 60, false);
        assert!(init);
        assert_eq!(next_tick, 300);

        // From tick corresponding to compressed = 5 (5 * 60 = 300), search gt:
        // Should find bit 10 (10 * 60 = 600)
        let (next_tick, init) = next_initialized_tick_within_one_word(&bitmap, 300, 60, false);
        assert!(init);
        assert_eq!(next_tick, 600);

        // From tick corresponding to compressed = 15 (15 * 60 = 900), search gt:
        // No initialized bit > 15 in word 0, so should return top of word (bit 255 * 60 = 15300)
        let (next_tick, init) = next_initialized_tick_within_one_word(&bitmap, 900, 60, false);
        assert!(!init);
        assert_eq!(next_tick, 255 * 60);
    }
}
