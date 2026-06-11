//! Lossless token-count conversions between integer and `f64` domains.
//!
//! Threshold math (`estimated > limit * pct`) mixes integer token counts
//! with fractional config values, which forces a float representation.
//! Plain `as` casts silently lose precision above 2^53 and silently
//! truncate/wrap on the way back, so this module provides explicit
//! conversions instead:
//!
//! - integer → `f64` is exact for every value up to [`MAX_EXACT_TOKENS`]
//!   (2^53, the largest integer range `f64` represents exactly) and
//!   *saturates* there. 2^53 tokens is roughly nine petabytes of text —
//!   five orders of magnitude beyond any context window — so saturation
//!   is a stated domain bound of the conversion, not a silent data path.
//! - `f64` → integer is a floor conversion built from the IEEE 754 bit
//!   layout, with non-finite and negative inputs mapping to zero and
//!   values at or above 2^53 saturating to [`MAX_EXACT_TOKENS`].

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

/// [`token_count_to_f64`] for `usize` token counts.
pub(super) fn usize_token_count_to_f64(value: usize) -> f64 {
    match u64::try_from(value) {
        Ok(v) => token_count_to_f64(v),
        // Only reachable on a target where usize is wider than 64 bits.
        Err(_) => MAX_EXACT_TOKENS_F64,
    }
}

/// Floor a non-negative `f64` to a token count.
///
/// `NaN` and negative inputs (including `-∞`) map to `0`; values at or
/// above 2^53 (including `+∞`) saturate to [`MAX_EXACT_TOKENS`]. Within
/// `[1, 2^53)` the result is the exact mathematical floor, reconstructed
/// from the IEEE 754 mantissa and exponent so no lossy cast is involved.
pub(super) fn f64_to_token_count(value: f64) -> u64 {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    if value >= MAX_EXACT_TOKENS_F64 {
        return MAX_EXACT_TOKENS;
    }
    let truncated = value.trunc();
    if truncated < 1.0 {
        return 0;
    }
    // truncated is in [1, 2^53): a normal IEEE 754 double whose biased
    // exponent lies in [1023, 1075], so the integer value is
    // (implicit-one | mantissa) >> (52 - unbiased exponent).
    let bits = truncated.to_bits();
    let Ok(biased) = i64::try_from((bits >> 52) & 0x7FF) else {
        // Unreachable: an 11-bit field always fits in i64.
        return MAX_EXACT_TOKENS;
    };
    let exponent = biased - 1023;
    let Ok(shift) = u32::try_from(52 - exponent) else {
        // Unreachable for inputs in [1, 2^53): exponent is in [0, 52].
        return MAX_EXACT_TOKENS;
    };
    let mantissa = (bits & ((1_u64 << 52) - 1)) | (1_u64 << 52);
    mantissa >> shift
}

/// Floor a non-negative `f64` to a `usize` token count, saturating at
/// `usize::MAX` on targets narrower than 64 bits.
pub(super) fn f64_to_usize_token_count(value: f64) -> usize {
    usize::try_from(f64_to_token_count(value)).unwrap_or(usize::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn token_count_to_f64_is_exact_for_representable_values() {
        for value in [
            0_u64,
            1,
            4,
            4_096,
            1_000_000,
            u64::from(u32::MAX),
            u64::from(u32::MAX) + 1,
            (1 << 53) - 1,
        ] {
            let converted = token_count_to_f64(value);
            assert_eq!(
                f64_to_token_count(converted),
                value,
                "round trip must be exact for {value}",
            );
        }
    }

    #[test]
    fn token_count_to_f64_saturates_at_two_pow_53() {
        assert!((token_count_to_f64(MAX_EXACT_TOKENS) - MAX_EXACT_TOKENS_F64).abs() < 1.0);
        assert!((token_count_to_f64(u64::MAX) - MAX_EXACT_TOKENS_F64).abs() < 1.0);
    }

    #[test]
    fn f64_to_token_count_floors_fractions() {
        assert_eq!(f64_to_token_count(0.0), 0);
        assert_eq!(f64_to_token_count(0.99), 0);
        assert_eq!(f64_to_token_count(1.0), 1);
        assert_eq!(f64_to_token_count(2.5), 2);
        assert_eq!(f64_to_token_count(6_000.0), 6_000);
        assert_eq!(f64_to_token_count(6_000.9), 6_000);
    }

    #[test]
    fn f64_to_token_count_rejects_non_finite_and_negative() {
        assert_eq!(f64_to_token_count(f64::NAN), 0);
        assert_eq!(f64_to_token_count(f64::NEG_INFINITY), 0);
        assert_eq!(f64_to_token_count(-1.5), 0);
        assert_eq!(f64_to_token_count(-0.0), 0);
        assert_eq!(f64_to_token_count(f64::INFINITY), MAX_EXACT_TOKENS);
        assert_eq!(f64_to_token_count(1e300), MAX_EXACT_TOKENS);
    }

    #[test]
    fn usize_helpers_round_trip() {
        assert!((usize_token_count_to_f64(123_456) - 123_456.0).abs() < f64::EPSILON);
        assert_eq!(f64_to_usize_token_count(123_456.7), 123_456);
    }
}
