//! JSON wire protocol output parsed from shell hook stdout on exit 0.
//!
//! [`HookOutput`] deserialises whatever a shell hook prints to stdout
//! when it exits cleanly. Three fields are accepted — `decision`,
//! `reason`, and `updated_input` — all optional, all defaulted via
//! `#[serde(default)]` so an empty `{}` payload parses to "no decision"
//! and maps to [`HookOutcome::Proceed`].
//!
//! [`HookOutput::to_hook_outcome`] is the bridge into the dispatcher's
//! native [`HookOutcome`] type. It enforces CO10: the `modify` decision
//! is honoured only when the dispatching event is
//! [`HookEventType::PreTool`]; every other event sees `modify` degrade
//! to [`HookOutcome::Proceed`] with a warning. Unrecognised decision
//! strings degrade the same way — defensive against operator typos and
//! to keep a misconfigured hook from wedging the agent loop.

use serde::Deserialize;

use super::config::HookEventType;
use super::traits::HookOutcome;

/// Parsed JSON output from a shell hook that exited with status 0.
///
/// All three fields are optional and default to [`None`] via
/// `#[serde(default)]`, so an empty `{}` stdout (or any other shape
/// missing some fields) deserialises without error. NH-005 calls
/// [`HookOutput::to_hook_outcome`] to lift this struct into the
/// dispatcher-facing [`HookOutcome`].
#[derive(Deserialize, Debug, Clone, Default)]
pub struct HookOutput {
    /// Decision keyword: `"proceed"`, `"block"`, or `"modify"`.
    ///
    /// [`None`] is treated as proceed. Unrecognised strings degrade to
    /// proceed with a warning so operator typos do not wedge the loop.
    #[serde(default)]
    pub decision: Option<String>,

    /// Human-readable reason shown to the agent when the decision is
    /// `"block"`. [`None`] maps to the empty string.
    #[serde(default)]
    pub reason: Option<String>,

    /// Replacement tool arguments. Required when `decision` is
    /// `"modify"` and the dispatching event is
    /// [`HookEventType::PreTool`]; ignored otherwise.
    #[serde(default)]
    pub updated_input: Option<serde_json::Value>,
}

impl HookOutput {
    /// Map this parsed output onto a dispatcher [`HookOutcome`].
    ///
    /// Decision rules (mirrored from `DESIGN.md` D13):
    ///
    /// - `None` or `"proceed"` → [`HookOutcome::Proceed`].
    /// - `"block"` → [`HookOutcome::Block`] with `reason` defaulting to
    ///   the empty string when absent.
    /// - `"modify"` on [`HookEventType::PreTool`] **with**
    ///   `updated_input` set → [`HookOutcome::Modify`].
    /// - `"modify"` on [`HookEventType::PreTool`] **without**
    ///   `updated_input` → [`HookOutcome::Proceed`], warning logged.
    /// - `"modify"` on any other event type → [`HookOutcome::Proceed`],
    ///   warning logged (CO10).
    /// - Any other decision string → [`HookOutcome::Proceed`], warning
    ///   logged (defensive against typos).
    ///
    /// `event_type` is taken by value because [`HookEventType`] is
    /// [`Copy`] (declared on `config.rs`). The method does not mutate
    /// `self`; clones are taken only of `reason` / `updated_input` so
    /// the original output remains available to callers that record it.
    #[must_use]
    pub fn to_hook_outcome(&self, event_type: HookEventType) -> HookOutcome {
        match self.decision.as_deref() {
            None | Some("proceed") => HookOutcome::Proceed,
            Some("block") => HookOutcome::Block {
                reason: self.reason.clone().unwrap_or_default(),
            },
            Some("modify") => {
                if event_type != HookEventType::PreTool {
                    tracing::warn!(
                        event = ?event_type,
                        "hook returned modify decision for non-pre_tool event; treating as proceed"
                    );
                    return HookOutcome::Proceed;
                }
                if let Some(updated_input) = self.updated_input.clone() {
                    HookOutcome::Modify { updated_input }
                } else {
                    tracing::warn!(
                        event = ?event_type,
                        "hook returned modify decision without updated_input; treating as proceed"
                    );
                    HookOutcome::Proceed
                }
            }
            Some(other) => {
                tracing::warn!(
                    decision = %other,
                    event = ?event_type,
                    "unrecognised hook decision string; treating as proceed"
                );
                HookOutcome::Proceed
            }
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

    #[test]
    fn empty_object_deserialises_to_all_none() {
        let out: HookOutput = serde_json::from_str("{}").unwrap();
        assert!(out.decision.is_none());
        assert!(out.reason.is_none());
        assert!(out.updated_input.is_none());
    }

    #[test]
    fn empty_object_maps_to_proceed_for_pre_tool() {
        let out: HookOutput = serde_json::from_str("{}").unwrap();
        assert!(matches!(
            out.to_hook_outcome(HookEventType::PreTool),
            HookOutcome::Proceed
        ));
    }

    #[test]
    fn decision_proceed_maps_to_proceed() {
        let out: HookOutput = serde_json::from_str(r#"{"decision": "proceed"}"#).unwrap();
        assert!(matches!(
            out.to_hook_outcome(HookEventType::PreTool),
            HookOutcome::Proceed
        ));
    }

    #[test]
    fn decision_block_carries_reason() {
        let out: HookOutput =
            serde_json::from_str(r#"{"decision": "block", "reason": "denied"}"#).unwrap();
        match out.to_hook_outcome(HookEventType::PreTool) {
            HookOutcome::Block { reason } => assert_eq!(reason, "denied"),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn decision_block_without_reason_uses_empty_string() {
        let out: HookOutput = serde_json::from_str(r#"{"decision": "block"}"#).unwrap();
        match out.to_hook_outcome(HookEventType::PreLlm) {
            HookOutcome::Block { reason } => assert_eq!(reason, ""),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn modify_on_pre_tool_with_updated_input_yields_modify() {
        let raw = r#"{"decision": "modify", "updated_input": {"x": 1}}"#;
        let out: HookOutput = serde_json::from_str(raw).unwrap();
        match out.to_hook_outcome(HookEventType::PreTool) {
            HookOutcome::Modify { updated_input } => {
                assert_eq!(updated_input, serde_json::json!({"x": 1}));
            }
            other => panic!("expected Modify, got {other:?}"),
        }
    }

    #[test]
    fn modify_on_pre_tool_without_updated_input_degrades_to_proceed() {
        let out: HookOutput = serde_json::from_str(r#"{"decision": "modify"}"#).unwrap();
        assert!(matches!(
            out.to_hook_outcome(HookEventType::PreTool),
            HookOutcome::Proceed
        ));
    }

    #[test]
    fn modify_on_non_pre_tool_event_degrades_to_proceed() {
        let raw = r#"{"decision": "modify", "updated_input": {"x": 1}}"#;
        let out: HookOutput = serde_json::from_str(raw).unwrap();
        // Every non-PreTool variant must coerce modify → proceed.
        for ev in [
            HookEventType::PreLlm,
            HookEventType::UserPrompt,
            HookEventType::Stop,
            HookEventType::SubagentStop,
            HookEventType::PreCompaction,
        ] {
            assert!(
                matches!(out.to_hook_outcome(ev), HookOutcome::Proceed),
                "modify on {ev:?} should degrade to Proceed"
            );
        }
    }

    #[test]
    fn unknown_decision_string_degrades_to_proceed() {
        let out: HookOutput = serde_json::from_str(r#"{"decision": "totally-bogus"}"#).unwrap();
        assert!(matches!(
            out.to_hook_outcome(HookEventType::PreTool),
            HookOutcome::Proceed
        ));
    }

    #[test]
    fn extra_unknown_fields_are_ignored() {
        let raw = r#"{"decision": "block", "reason": "no", "extra": 42}"#;
        let out: HookOutput = serde_json::from_str(raw).unwrap();
        match out.to_hook_outcome(HookEventType::Stop) {
            HookOutcome::Block { reason } => assert_eq!(reason, "no"),
            other => panic!("expected Block, got {other:?}"),
        }
    }
}
