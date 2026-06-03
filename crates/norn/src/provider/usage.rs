//! Token usage tracking for provider calls.

use serde::{Deserialize, Serialize};

/// Tracks token consumption for a single provider call.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Number of input tokens consumed.
    pub input_tokens: u64,
    /// Number of output tokens produced.
    pub output_tokens: u64,
    /// Number of tokens read from cache.
    pub cache_read_tokens: u64,
    /// Number of tokens written to cache.
    pub cache_write_tokens: u64,
    /// Estimated cost in USD, if the provider reports it.
    pub cost_usd: Option<f64>,
}

impl std::ops::Add for Usage {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            input_tokens: self.input_tokens + rhs.input_tokens,
            output_tokens: self.output_tokens + rhs.output_tokens,
            cache_read_tokens: self.cache_read_tokens + rhs.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens + rhs.cache_write_tokens,
            cost_usd: match (self.cost_usd, rhs.cost_usd) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) | (None, Some(a)) => Some(a),
                (None, None) => None,
            },
        }
    }
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, rhs: Self) {
        *self = std::mem::take(self) + rhs;
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_zeros() {
        let u = Usage::default();
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 0);
        assert_eq!(u.cache_read_tokens, 0);
        assert_eq!(u.cache_write_tokens, 0);
        assert!(u.cost_usd.is_none());
    }

    #[test]
    fn add_accumulates_tokens() {
        let a = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 10,
            cache_write_tokens: 5,
            cost_usd: Some(0.01),
        };
        let b = Usage {
            input_tokens: 200,
            output_tokens: 80,
            cache_read_tokens: 20,
            cache_write_tokens: 15,
            cost_usd: Some(0.02),
        };
        let sum = a + b;
        assert_eq!(sum.input_tokens, 300);
        assert_eq!(sum.output_tokens, 130);
        assert_eq!(sum.cache_read_tokens, 30);
        assert_eq!(sum.cache_write_tokens, 20);
        assert!((sum.cost_usd.unwrap_or(0.0) - 0.03).abs() < f64::EPSILON);
    }

    #[test]
    fn add_with_none_cost() {
        let a = Usage {
            cost_usd: Some(0.05),
            ..Usage::default()
        };
        let b = Usage::default();
        let sum = a + b;
        assert!((sum.cost_usd.unwrap_or(0.0) - 0.05).abs() < f64::EPSILON);
    }
}
