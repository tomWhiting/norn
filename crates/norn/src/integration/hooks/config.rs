//! Config-side types for the hook system.
//!
//! [`HookEventType`] is the closed taxonomy of hook events that map config
//! event names (`snake_case`) to the corresponding trait dispatch. The
//! settings schema lives in [`crate::config::types::HookSettings`]; this
//! module provides the typed lifting from those config field names into
//! enum variants the dispatcher can branch on.
//!
//! The 13 variants match the `DESIGN.md` D6 taxonomy table:
//!
//! | Config name          | Trait dispatched           | Matcher input        |
//! |----------------------|----------------------------|----------------------|
//! | `pre_tool`           | `PreToolHook`              | Tool name            |
//! | `post_tool`          | `PostToolHook`             | Tool name            |
//! | `post_tool_failure`  | `PostToolFailureHook`      | Tool name            |
//! | `pre_llm`            | `PreLlmHook`               | Model name           |
//! | `post_llm`           | `PostLlmHook`              | Model name           |
//! | `session_event`      | `SessionEventHook`         | Event variant name   |
//! | `user_prompt`        | `UserPromptHook`           | N/A                  |
//! | `stop`               | `StopHook`                 | N/A                  |
//! | `subagent_start`     | `SubagentHook` (start)     | Agent type / profile |
//! | `subagent_stop`      | `SubagentHook` (stop)      | Agent type / profile |
//! | `session_start`      | `SessionLifecycleHook`     | N/A                  |
//! | `session_end`        | `SessionLifecycleHook`     | N/A                  |
//! | `pre_compaction`     | `CompactionHook`           | N/A                  |
//!
//! Variants without a matcher input always fire when registered for that
//! event type; [`HookEventType::supports_matcher`] reflects this.

use serde::{Deserialize, Serialize};

/// Closed taxonomy of hook events.
///
/// Variant order matches `DESIGN.md` D6 and the field order of
/// [`crate::config::types::HookSettings`]. Serde renames every variant to
/// `snake_case` so the JSON wire form is `"pre_tool"`, `"post_tool"`, … —
/// the same names operators use in their settings files.
///
/// Derives [`Hash`] because downstream dispatchers (NH-005 onwards) key
/// shell-hook routing tables by event type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEventType {
    /// Fires before a tool executes. Can block or modify (NH-002).
    PreTool,
    /// Fires after a tool executes successfully. Observational.
    PostTool,
    /// Fires after a tool execution reports an error. Observational.
    PostToolFailure,
    /// Fires before a provider call. Can block.
    PreLlm,
    /// Fires after a provider call. Observational.
    PostLlm,
    /// Fires on every session-event append. Observational.
    SessionEvent,
    /// Fires when a user/orchestrator prompt enters the agent loop. Can block.
    UserPrompt,
    /// Fires when the model would stop. Can block to force continue.
    Stop,
    /// Fires when a sub-agent is launched. Observational.
    SubagentStart,
    /// Fires when a sub-agent would complete. Can block.
    SubagentStop,
    /// Fires at session construction. Observational.
    SessionStart,
    /// Fires at session teardown. Observational.
    SessionEnd,
    /// Fires before auto-compaction runs. Can block.
    PreCompaction,
}

impl HookEventType {
    /// Whether this event type accepts a matcher input.
    ///
    /// The eight events that take a matcher (`pre_tool`, `post_tool`,
    /// `post_tool_failure`, `pre_llm`, `post_llm`, `session_event`,
    /// `subagent_start`, `subagent_stop`) return `true`. The five
    /// always-fire events (`user_prompt`, `stop`, `session_start`,
    /// `session_end`, `pre_compaction`) return `false`.
    ///
    /// See `DESIGN.md` D17 for the matcher-input table.
    #[must_use]
    pub const fn supports_matcher(self) -> bool {
        match self {
            Self::PreTool
            | Self::PostTool
            | Self::PostToolFailure
            | Self::PreLlm
            | Self::PostLlm
            | Self::SessionEvent
            | Self::SubagentStart
            | Self::SubagentStop => true,
            Self::UserPrompt
            | Self::Stop
            | Self::SessionStart
            | Self::SessionEnd
            | Self::PreCompaction => false,
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
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;

    /// The 13 variants paired with their canonical snake_case wire form.
    /// Ordering matches `DESIGN.md` D6 and the variant declaration order.
    const ALL_VARIANTS: &[(HookEventType, &str)] = &[
        (HookEventType::PreTool, "pre_tool"),
        (HookEventType::PostTool, "post_tool"),
        (HookEventType::PostToolFailure, "post_tool_failure"),
        (HookEventType::PreLlm, "pre_llm"),
        (HookEventType::PostLlm, "post_llm"),
        (HookEventType::SessionEvent, "session_event"),
        (HookEventType::UserPrompt, "user_prompt"),
        (HookEventType::Stop, "stop"),
        (HookEventType::SubagentStart, "subagent_start"),
        (HookEventType::SubagentStop, "subagent_stop"),
        (HookEventType::SessionStart, "session_start"),
        (HookEventType::SessionEnd, "session_end"),
        (HookEventType::PreCompaction, "pre_compaction"),
    ];

    #[test]
    fn round_trip_all_variants_through_serde_json() {
        for (variant, expected) in ALL_VARIANTS {
            let encoded = serde_json::to_string(variant).unwrap();
            assert_eq!(
                encoded,
                format!("\"{expected}\""),
                "variant {variant:?} did not serialise to \"{expected}\""
            );
            let decoded: HookEventType = serde_json::from_str(&encoded).unwrap();
            assert_eq!(
                decoded, *variant,
                "round-trip mismatch for {variant:?} via {encoded}"
            );
        }
    }

    #[test]
    fn supports_matcher_returns_true_for_eight_events() {
        let with_matcher = [
            HookEventType::PreTool,
            HookEventType::PostTool,
            HookEventType::PostToolFailure,
            HookEventType::PreLlm,
            HookEventType::PostLlm,
            HookEventType::SessionEvent,
            HookEventType::SubagentStart,
            HookEventType::SubagentStop,
        ];
        for ev in with_matcher {
            assert!(
                ev.supports_matcher(),
                "{ev:?} should support a matcher input"
            );
        }
    }

    #[test]
    fn supports_matcher_returns_false_for_five_events() {
        let always_fires = [
            HookEventType::UserPrompt,
            HookEventType::Stop,
            HookEventType::SessionStart,
            HookEventType::SessionEnd,
            HookEventType::PreCompaction,
        ];
        for ev in always_fires {
            assert!(
                !ev.supports_matcher(),
                "{ev:?} should always fire (no matcher)"
            );
        }
    }

    #[test]
    fn unknown_snake_case_string_is_rejected() {
        let err = serde_json::from_str::<HookEventType>("\"not_an_event\"")
            .expect_err("unknown variant must error");
        let msg = err.to_string();
        assert!(
            msg.contains("not_an_event") || msg.contains("variant"),
            "expected serde error to mention the bad variant; got: {msg}"
        );
    }

    #[test]
    fn variants_are_hash_and_eq() {
        use std::collections::HashSet;
        let mut set: HashSet<HookEventType> = HashSet::new();
        for (variant, _) in ALL_VARIANTS {
            assert!(set.insert(*variant), "duplicate variant {variant:?}");
        }
        assert_eq!(set.len(), 13);
    }
}
