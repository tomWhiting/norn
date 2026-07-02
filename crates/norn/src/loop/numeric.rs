//! Lossless token-count conversion from the integer to the `f64` domain.
//!
//! The iteration monitor's usage-fraction math (`used / limit`) mixes
//! integer token counts into a float ratio, which forces a float
//! representation. Plain `as` casts silently lose precision above 2^53, so
//! this module provides an explicit conversion instead:
//!
//! - integer → `f64` is exact for every value up to [`MAX_EXACT_TOKENS`]
//!   (2^53, the largest integer range `f64` represents exactly) and
//!   *saturates* there. 2^53 tokens is roughly nine petabytes of text —
//!   five orders of magnitude beyond any context window — so saturation
//!   is a stated domain bound of the conversion, not a silent data path.

/// Largest integer count these conversions represent exactly: 2^53.
pub(super) const MAX_EXACT_TOKENS: u64 = 1 << 53;

/// 2^53 as an `f64` literal (exactly representable).
const MAX_EXACT_TOKENS_F64: f64 = 9_007_199_254_740_992.0;

/// 2^32 as an `f64` literal (exactly representable).
const TWO_POW_32_F64: f64 = 4_294_967_296.0;

/// Convert a token count to `f64`, exactly below 2^53 and saturating to
/// 2^53 at or above it.
///
/// The value is split into 32-bit halves and recombined through
/// `f64::from(u32)`, which is lossless: for `value < 2^53` the high half
/// has at most 21 significant bits, so both the scaled high half and the
/// final sum are exactly representable.
pub(super) fn token_count_to_f64(value: u64) -> f64 {
    if value >= MAX_EXACT_TOKENS {
        return MAX_EXACT_TOKENS_F64;
    }
    let (Ok(hi), Ok(lo)) = (
        u32::try_from(value >> 32),
        u32::try_from(value & 0xFFFF_FFFF),
    ) else {
        // Unreachable: both halves of a u64 fit in u32 by construction.
        return MAX_EXACT_TOKENS_F64;
    };
    f64::from(hi) * TWO_POW_32_F64 + f64::from(lo)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn token_count_to_f64_is_exact_for_representable_values() {
        // Each pair is an integer token count and its exact `f64` value
        // (every entry is a `< 2^53` integer, so both sides are exactly
        // representable and the conversion must reproduce the literal).
        let cases: [(u64, f64); 8] = [
            (0, 0.0),
            (1, 1.0),
            (4, 4.0),
            (4_096, 4_096.0),
            (1_000_000, 1_000_000.0),
            (u64::from(u32::MAX), 4_294_967_295.0),
            (u64::from(u32::MAX) + 1, 4_294_967_296.0),
            ((1 << 53) - 1, 9_007_199_254_740_991.0),
        ];
        for (value, expected) in cases {
            // Exact integer conversion: any nonzero difference is a
            // precision bug, so a sub-token epsilon proves exactness.
            assert!(
                (token_count_to_f64(value) - expected).abs() < 0.5,
                "conversion must be exact for {value}",
            );
        }
    }

    #[test]
    fn token_count_to_f64_saturates_at_two_pow_53() {
        assert!((token_count_to_f64(MAX_EXACT_TOKENS) - MAX_EXACT_TOKENS_F64).abs() < 1.0);
        assert!((token_count_to_f64(u64::MAX) - MAX_EXACT_TOKENS_F64).abs() < 1.0);
    }
}
