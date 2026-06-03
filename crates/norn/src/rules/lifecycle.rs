//! Rule presence tracking and re-injection logic.

use std::collections::HashSet;

use crate::r#loop::context::ContentTag;
use crate::rules::types::RuleId;

/// Tracks which rules are currently present in the active context.
///
/// Rebuilt synchronously from [`ContentTag`] slices during each prompt
/// construction pass. When a rule was previously present but is absent
/// after a rebuild, the rules engine knows to re-inject it on the next
/// trigger match.
#[derive(Debug, Default)]
pub struct RulePresenceSet {
    present: HashSet<RuleId>,
}

impl RulePresenceSet {
    /// Create an empty presence set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild the presence set from a slice of content tags.
    ///
    /// Replaces the entire set with rule IDs extracted from
    /// [`ContentTag::Rule`] variants.
    pub fn rebuild(&mut self, tags: &[ContentTag]) {
        self.present.clear();
        for tag in tags {
            if let ContentTag::Rule(id_str) = tag {
                self.present.insert(RuleId::from(id_str.as_str()));
            }
        }
    }

    /// Check whether a rule is currently present in the active context.
    #[must_use]
    pub fn is_present(&self, id: &RuleId) -> bool {
        self.present.contains(id)
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
    fn empty_presence_set() {
        let set = RulePresenceSet::new();
        assert!(!set.is_present(&RuleId::from("any-rule")));
    }

    #[test]
    fn rebuild_marks_rules_present() {
        let mut set = RulePresenceSet::new();
        let tags = vec![
            ContentTag::Message,
            ContentTag::Rule("rust-conventions".to_owned()),
            ContentTag::ToolResult,
            ContentTag::Rule("mod-rs-check".to_owned()),
        ];
        set.rebuild(&tags);

        assert!(set.is_present(&RuleId::from("rust-conventions")));
        assert!(set.is_present(&RuleId::from("mod-rs-check")));
        assert!(!set.is_present(&RuleId::from("not-present")));
    }

    #[test]
    fn rebuild_clears_previously_present() {
        let mut set = RulePresenceSet::new();

        let tags_with_rule = vec![ContentTag::Rule("rule-a".to_owned())];
        set.rebuild(&tags_with_rule);
        assert!(set.is_present(&RuleId::from("rule-a")));

        let tags_without_rule = vec![ContentTag::Message];
        set.rebuild(&tags_without_rule);
        assert!(!set.is_present(&RuleId::from("rule-a")));
    }

    #[test]
    fn rebuild_replaces_entire_set() {
        let mut set = RulePresenceSet::new();

        set.rebuild(&[ContentTag::Rule("alpha".to_owned())]);
        assert!(set.is_present(&RuleId::from("alpha")));

        set.rebuild(&[ContentTag::Rule("beta".to_owned())]);
        assert!(!set.is_present(&RuleId::from("alpha")));
        assert!(set.is_present(&RuleId::from("beta")));
    }
}
