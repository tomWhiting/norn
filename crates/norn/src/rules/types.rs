//! Core rules engine types: Rule, triggers, delivery modes, runtime events.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// R1: RuleId, TriggerTiming, Rule
// ---------------------------------------------------------------------------

/// Unique identifier for a rule.
#[derive(Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuleId(String);

impl RuleId {
    /// Return the inner string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RuleId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl<S: Into<String>> From<S> for RuleId {
    fn from(s: S) -> Self {
        Self(s.into())
    }
}

/// Whether a rule fires before or after the matched action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerTiming {
    /// Fire before the matched action executes.
    Before,
    /// Fire after the matched action executes.
    After,
}

/// A rule: contextual guidance that fires based on trigger conditions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    /// Unique identifier for this rule.
    pub id: RuleId,
    /// Human-readable name.
    pub name: String,
    /// Conditions that cause this rule to fire.
    pub triggers: Vec<TriggerCondition>,
    /// How the rule content is delivered to the model.
    pub delivery: DeliveryMode,
    /// Whether the rule fires before or after the matched action.
    pub timing: TriggerTiming,
    /// The rule body content to deliver.
    pub body: String,
    /// Optional shell command executed at injection time. When `Some`, the
    /// command's stdout (trimmed) replaces the static `body` for that
    /// injection. On timeout or non-zero exit the engine falls back to
    /// `body` and emits a diagnostic (when a collector is attached).
    #[serde(default)]
    pub shell_source: Option<String>,
}

// ---------------------------------------------------------------------------
// R2: TriggerCondition
// ---------------------------------------------------------------------------

/// A condition that determines when a rule fires.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "pattern")]
pub enum TriggerCondition {
    /// Fire when a file matching the glob pattern is read or written.
    PathGlob {
        /// Glob pattern (supports `*`, `**`, `?`).
        pattern: String,
    },
    /// Fire when a bash command containing the substring is run.
    BashCommand {
        /// Substring to match against command strings.
        pattern: String,
    },
    /// Fire when a specific tool is invoked (exact name match).
    ToolInvocation {
        /// Exact tool name to match.
        tool_name: String,
    },
}

// ---------------------------------------------------------------------------
// R3: DeliveryMode
// ---------------------------------------------------------------------------

/// How a rule's content is delivered to the model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    /// Add the rule to the system prompt for the remainder of the session.
    SystemContextAppend,
    /// Deliver the rule at the next input boundary.
    ContextInjection,
    /// Send the rule as a conversation message.
    MessageDelivery,
}

impl DeliveryMode {
    /// Format a fired rule's raw content for delivery as a conversation
    /// message.
    ///
    /// Returns [`None`] for [`DeliveryMode::SystemContextAppend`], which is
    /// delivered through the system prompt (re-materialized into the
    /// dynamic system sections each prompt-construction pass) rather than
    /// as a message. [`DeliveryMode::ContextInjection`] and
    /// [`DeliveryMode::MessageDelivery`] each carry a distinguishing prefix
    /// so the model can tell rule-sourced content apart from ordinary user
    /// input.
    ///
    /// The same formatting is applied both when a rule fires live and when
    /// a persisted [`RuleInjection`](crate::session::events::SessionEvent::RuleInjection)
    /// event is replayed on resume, so the provider-facing text is byte-for-byte
    /// identical across a restart.
    #[must_use]
    pub fn format_conversation_content(&self, rule_id: &str, content: &str) -> Option<String> {
        match self {
            Self::SystemContextAppend => None,
            Self::ContextInjection => Some(format!("[Context: {rule_id}] {content}")),
            Self::MessageDelivery => Some(format!("[Rule: {rule_id}] {content}")),
        }
    }
}

// ---------------------------------------------------------------------------
// R4: RuntimeEvent, PathOperation
// ---------------------------------------------------------------------------

/// A generic runtime event consumed by the rules engine.
///
/// The rules engine does not define its own event types — it passively
/// consumes these generic events per D7's dependency inversion principle.
#[derive(Clone, Debug)]
pub enum RuntimeEvent {
    /// A file was read or written.
    PathChanged {
        /// File path that changed.
        path: String,
        /// Whether the file was read or written.
        operation: PathOperation,
    },
    /// A tool was invoked.
    ToolInvoked {
        /// Name of the invoked tool.
        tool_name: String,
        /// Optional tool call arguments.
        arguments: Option<serde_json::Value>,
    },
    /// A bash command was executed.
    BashCommandRun {
        /// The command string that was run.
        command: String,
    },
}

/// Whether a path event represents a read or a write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathOperation {
    /// The file was read.
    Read,
    /// The file was written.
    Write,
}

// ---------------------------------------------------------------------------
// R8: RuleInjection (output type from the engine)
// ---------------------------------------------------------------------------

/// A rule ready for delivery, produced by the engine when a trigger matches
/// and the rule is not already in context.
#[derive(Clone, Debug)]
pub struct RuleInjection {
    /// ID of the rule that matched.
    pub rule_id: RuleId,
    /// How to deliver the rule content.
    pub delivery: DeliveryMode,
    /// Whether this fires before or after the matched action.
    pub timing: TriggerTiming,
    /// The rule body content to deliver.
    pub content: String,
}

/// Result of evaluating a rule's triggers against a runtime event.
#[derive(Clone, Debug)]
pub struct TriggerMatch {
    /// ID of the rule that matched.
    pub rule_id: RuleId,
    /// Timing of the matched rule.
    pub timing: TriggerTiming,
    /// Delivery mode of the matched rule.
    pub delivery: DeliveryMode,
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
    fn rule_id_display_fromstr_roundtrip() {
        let id: RuleId = "rust-conventions".parse().expect("infallible");
        let s = id.to_string();
        let parsed: RuleId = s.parse().expect("infallible");
        assert_eq!(id, parsed);
    }

    #[test]
    fn rule_id_from_string() {
        let id = RuleId::from("test-rule");
        assert_eq!(id.as_str(), "test-rule");
    }

    #[test]
    fn trigger_timing_serde() {
        let before = TriggerTiming::Before;
        let json = serde_json::to_string(&before).expect("serialize");
        assert_eq!(json, "\"before\"");
        let after: TriggerTiming = serde_json::from_str("\"after\"").expect("deserialize");
        assert_eq!(after, TriggerTiming::After);
    }

    #[test]
    fn delivery_mode_serde() {
        let mode = DeliveryMode::SystemContextAppend;
        let json = serde_json::to_string(&mode).expect("serialize");
        assert_eq!(json, "\"system_context_append\"");

        let parsed: DeliveryMode =
            serde_json::from_str("\"context_injection\"").expect("deserialize");
        assert_eq!(parsed, DeliveryMode::ContextInjection);

        let parsed: DeliveryMode =
            serde_json::from_str("\"message_delivery\"").expect("deserialize");
        assert_eq!(parsed, DeliveryMode::MessageDelivery);
    }

    #[test]
    fn rule_serde_roundtrip() {
        let rule = Rule {
            id: RuleId::from("test"),
            name: "Test Rule".to_owned(),
            triggers: vec![TriggerCondition::PathGlob {
                pattern: "**/*.rs".to_owned(),
            }],
            delivery: DeliveryMode::ContextInjection,
            timing: TriggerTiming::Before,
            body: "Follow Rust conventions.".to_owned(),
            shell_source: None,
        };
        let json = serde_json::to_string(&rule).expect("serialize");
        let _: Rule = serde_json::from_str(&json).expect("deserialize");
    }

    #[test]
    fn rule_serde_omits_default_shell_source() {
        let json = r#"{"id":"test","name":"Test","triggers":[{"type":"PathGlob","pattern":{"pattern":"**/*.rs"}}],"delivery":"context_injection","timing":"before","body":"x"}"#;
        let rule: Rule = serde_json::from_str(json).expect("deserialize");
        assert!(rule.shell_source.is_none());
    }
}
