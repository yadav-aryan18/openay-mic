//! Deterministic payload filler shared with the Android side.

/// Fill `buf` with the xorshift32 stream specified by `shared/protocol.md`:
///
/// ```text
/// state = seed
/// for each output byte:
///     state ^= state << 13
///     state ^= state >> 17
///     state ^= state << 5        # all arithmetic mod 2^32
///     emit (state & 0xFF)
/// ```
///
/// The interop convention seeds with the packet's sequence number, so both
/// sides can verify byte-exact content without sharing code.
pub fn fill_xorshift(buf: &mut [u8], seed: u32) {
    let mut state = seed;
    for b in buf.iter_mut() {
        state ^= state.wrapping_shl(13);
        state ^= state.wrapping_shr(17);
        state ^= state.wrapping_shl(5);
        *b = (state & 0xFF) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-computed and cross-checked with Python: seed = 1, first four bytes.
    ///
    /// Iteration trace:
    ///   state=1 → 0x2001→0x2001→0x42021 → emit 0x21
    ///   → 0x84000021→0x84004221→0x04080601 → emit 0x01
    ///   → 0x9DCCA8C5→0x08D408C5→0x08D40CAF→0x1255994F → emit 0x4F
    #[test]
    fn seed_one_first_four_bytes() {
        let mut buf = [0u8; 4];
        fill_xorshift(&mut buf, 1);
        assert_eq!(buf, [0x21, 0x01, 0xC5, 0x4F]);
    }

    #[test]
    fn seed_zero_is_zeroes() {
        let mut buf = vec![0xFFu8; 16];
        fill_xorshift(&mut buf, 0);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn deterministic_repeat() {
        let mut a = vec![0u8; 960];
        let mut b = vec![0u8; 960];
        fill_xorshift(&mut a, 12345);
        fill_xorshift(&mut b, 12345);
        assert_eq!(a, b);
        // Different seed, different stream.
        let mut c = vec![0u8; 960];
        fill_xorshift(&mut c, 12346);
        assert_ne!(a, c);
    }
}
