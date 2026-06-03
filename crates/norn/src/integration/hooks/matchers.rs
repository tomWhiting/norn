//! Hook matchers — regex patterns compiled once at settings load time.
//!
//! Each shell hook entry in [`crate::config::types::HookSettings`] carries
//! an optional `matcher` string. [`HookMatcher`] turns that string into a
//! match-all marker or a compiled [`regex::Regex`], rejecting invalid
//! patterns as [`crate::error::ConfigError::InvalidConfig`] at startup
//! (CO7 — settings captured at startup, no hot-reload).
//!
//! Three forms collapse to match-all:
//!
//! - `None` — the operator omitted the matcher field.
//! - `Some("")` — the operator wrote `"matcher": ""`.
//! - `Some("*")` — Claude-Code-compatible wildcard.
//!
//! All other patterns compile as anchored full-match regexes: `Write`
//! matches `Write` but **not** `WriteFile`. Anchoring is implemented by
//! wrapping the user's pattern in `^(?:…)$` before handing it to
//! [`regex::Regex::new`]. That keeps the surface intuitive — `Edit|Write`
//! matches either tool name in full — and avoids the substring-match
//! surprise.
//!
//! Per-event-type matcher inputs are documented on
//! [`crate::integration::hooks::HookEventType`]; NH-003 only enforces
//! pattern compilation, not what each event type passes in.

use regex::Regex;

use crate::error::ConfigError;

/// Compiled hook matcher.
///
/// Either a match-all marker (no pattern, or the wildcard `"*"`) or a
/// concrete anchored regex. Cheap to clone — the inner [`Regex`] is
/// `Arc`-backed internally.
#[derive(Clone, Debug)]
pub enum HookMatcher {
    /// Matches every input.
    All,
    /// Matches inputs whose full string is accepted by the regex.
    Pattern(Regex),
}

impl HookMatcher {
    /// Build a matcher from the operator's `matcher` string.
    ///
    /// Returns [`HookMatcher::All`] when `pattern` is one of the three
    /// match-all forms (`None`, empty string, or `"*"`). Otherwise the
    /// pattern is anchored as `^(?:…)$` and compiled by [`regex::Regex`].
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidConfig`] when `pattern` is not a
    /// valid regex. The error reason quotes the offending pattern and
    /// the underlying parser message so operators can locate the bad
    /// entry in their settings file.
    pub fn new(pattern: Option<&str>) -> Result<Self, ConfigError> {
        match pattern {
            None => Ok(Self::All),
            Some(s) if s.is_empty() || s == "*" => Ok(Self::All),
            Some(s) => {
                let anchored = format!("^(?:{s})$");
                Regex::new(&anchored)
                    .map(Self::Pattern)
                    .map_err(|err| ConfigError::InvalidConfig {
                        reason: format!("invalid hook matcher regex {s:?}: {err}"),
                    })
            }
        }
    }

    /// Test whether `input` matches this matcher.
    ///
    /// [`HookMatcher::All`] returns `true` unconditionally;
    /// [`HookMatcher::Pattern`] uses [`Regex::is_match`] against the
    /// anchored pattern, which is full-match by construction.
    #[must_use]
    pub fn matches(&self, input: &str) -> bool {
        match self {
            Self::All => true,
            Self::Pattern(re) => re.is_match(input),
        }
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
    fn none_pattern_becomes_match_all() {
        let m = HookMatcher::new(None).unwrap();
        assert!(matches!(m, HookMatcher::All));
        assert!(m.matches("anything"));
        assert!(m.matches(""));
    }

    #[test]
    fn empty_string_becomes_match_all() {
        let m = HookMatcher::new(Some("")).unwrap();
        assert!(matches!(m, HookMatcher::All));
        assert!(m.matches("Write"));
    }

    #[test]
    fn wildcard_star_becomes_match_all() {
        let m = HookMatcher::new(Some("*")).unwrap();
        assert!(matches!(m, HookMatcher::All));
        assert!(m.matches("Edit"));
        assert!(m.matches("bash"));
    }

    #[test]
    fn exact_pattern_matches_only_full_string() {
        let m = HookMatcher::new(Some("Write")).unwrap();
        assert!(m.matches("Write"));
        // Full-match anchoring rejects substrings on either side.
        assert!(!m.matches("WriteFile"));
        assert!(!m.matches("OverWrite"));
        assert!(!m.matches("write"), "regex is case-sensitive by default");
    }

    #[test]
    fn alternation_pattern_matches_either_alternative() {
        let m = HookMatcher::new(Some("Edit|Write")).unwrap();
        assert!(m.matches("Edit"));
        assert!(m.matches("Write"));
        assert!(!m.matches("Read"));
        // Anchoring still applies to either alternative.
        assert!(!m.matches("WriteFile"));
        assert!(!m.matches("OverEdit"));
    }

    #[test]
    fn char_class_pattern_matches_any_listed_char() {
        let m = HookMatcher::new(Some("[ab]")).unwrap();
        assert!(m.matches("a"));
        assert!(m.matches("b"));
        assert!(!m.matches("c"));
        assert!(!m.matches("ab"), "char class is one-char only");
    }

    #[test]
    fn invalid_regex_is_rejected_at_construction() {
        let err = HookMatcher::new(Some("[unclosed")).expect_err("unclosed class must error");
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig, got {err:?}");
        };
        assert!(
            reason.contains("[unclosed"),
            "reason should quote the bad pattern: {reason}"
        );
    }

    #[test]
    fn invalid_repeat_metachar_is_rejected() {
        // `+` at the start has nothing to repeat — regex crate refuses it.
        let err = HookMatcher::new(Some("+abc")).expect_err("dangling repeat must error");
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig, got {err:?}");
        };
        assert!(
            reason.contains("+abc"),
            "reason should quote the bad pattern: {reason}"
        );
    }

    #[test]
    fn pattern_can_match_session_event_variant_names() {
        // C51: session events are matched against the SessionEvent variant
        // name. Verify regex semantics line up with the example variants.
        let m = HookMatcher::new(Some("UserMessage|ToolResult")).unwrap();
        assert!(m.matches("UserMessage"));
        assert!(m.matches("ToolResult"));
        assert!(!m.matches("AssistantMessage"));
    }
}
