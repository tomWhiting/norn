//! Forking — context filtering, output-schema construction, and synthetic
//! tool-result injection for a child agent that runs concurrently with its
//! parent.
//!
//! The fork lifecycle (registry reservation, [`tokio::spawn`], status watch
//! channel, status notification, [`SessionEvent::ForkComplete`] append) lives
//! on [`crate::tools::agent::fork_tool::ForkTool`]. This module owns the
//! reusable data types — [`ContextFilter`], [`ForkRequirement`],
//! [`ParentSystemInstruction`] — together with the pure helpers the tool
//! composes:
//!
//! - [`build_fork_output_schema`] derives the child's structured-output JSON
//!   schema from an optional task list (R7).
//! - [`combine_system_instruction`] composes the verbatim fork preamble with
//!   the parent's base system instruction (R5).
//! - [`inject_synthetic_fork_result`] adds the synthetic `fork` tool result
//!   onto the child's seed events so the parent's most-recent assistant turn
//!   leaves no orphan tool calls (R2).
//! - [`verify_no_orphan_tool_calls`] is a defensive check the tool calls
//!   before launch — it surfaces a warning log if any tool call in the latest
//!   `AssistantMessage` lacks a matching `ToolResult`.
//! - [`ParentSystemInstruction`] is the optional
//!   [`ToolContext`](crate::tool::context::ToolContext) extension the fork
//!   tool reads to learn the parent's base system instruction.

use std::fmt::Write as _;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::session::events::{EventBase, SessionEvent};

pub use super::fork_context_filter::ContextFilter;

/// One requirement in a fork's request.
///
/// When requirements are provided, the fork's structured output schema
/// includes a matching array so the child reports completion status and
/// notes for each requirement.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ForkRequirement {
    /// Stable identifier for this requirement.
    pub name: String,
    /// What the requirement entails.
    pub description: String,
}

/// Verbatim intent paragraph opening every fork preamble.
///
/// Kept as a `pub const` so tests can assert byte-for-byte that the child's
/// [`LoopContext::base_system_instruction`](crate::agent_loop::loop_context::LoopContext::base_system_instruction)
/// carries it (R5 acceptance). The full preamble is built by
/// [`build_fork_preamble`], which extends this intent with the fork's
/// structured identity (R4).
pub const FORK_SYSTEM_PREAMBLE: &str = "You are a fork of the parent agent's session, split at this point in time. Complete the requirements in the task and return your structured output. Stay focused on the assigned task — do not pursue work outside the requirements.";

/// The structured identity a fork child is told about itself (brief
/// `agent-variants` R4): who forked it, where it sits, what it owes, and
/// what delegation rights it holds — "a limit an agent doesn't know
/// about is an assassination", so the child is TOLD its budget.
pub struct ForkIdentity<'a> {
    /// The forking agent's id.
    pub parent_agent_id: &'a str,
    /// The fork's own coordination path address
    /// (`BranchedChild::path_address` from the session branch mint).
    pub path_address: &'a str,
    /// The requirement slugs the fork's structured output must report on
    /// (already schema-forced; restated here so the contract is visible
    /// in the child's own instructions).
    pub requirement_slugs: &'a [String],
    /// The [`ChildPolicy`](crate::agent::child_policy::ChildPolicy)
    /// granted to this fork — its delegation and messaging budget.
    pub granted: &'a crate::agent::child_policy::ChildPolicy,
}

/// Render the fork preamble: the verbatim [`FORK_SYSTEM_PREAMBLE`] intent
/// followed by the fork's structured identity — parent agent id, path
/// address, requirements contract, and delegation rights (R4).
///
/// Deterministic for fixed inputs (no timestamps, no randomness); the
/// golden-file test pins the exact rendering.
#[must_use]
pub fn build_fork_preamble(identity: &ForkIdentity<'_>) -> String {
    use crate::agent::child_policy::MessagingScope;

    let mut out = String::from(FORK_SYSTEM_PREAMBLE);
    out.push_str("\n\n## Fork identity\n\n");
    let _ = writeln!(out, "- Forked by agent: {}", identity.parent_agent_id);
    let _ = writeln!(out, "- Your address: {}", identity.path_address);
    out.push_str("- Requirements contract (the slugs your structured output must report on):\n");
    if identity.requirement_slugs.is_empty() {
        out.push_str("  - (none declared — only the free-text response is required)\n");
    } else {
        for slug in identity.requirement_slugs {
            let _ = writeln!(out, "  - {slug}");
        }
    }
    out.push_str("\n## Delegation rights\n\n");
    let depth = identity.granted.delegation.remaining_depth;
    let _ = writeln!(
        out,
        "- Remaining delegation depth: {depth}{}",
        if depth == 0 {
            " — you are a leaf: you cannot spawn or fork, and those tools are not on your surface"
        } else {
            " (levels of descendants you may create below yourself)"
        },
    );
    let _ = writeln!(
        out,
        "- Max concurrent children: {}",
        identity.granted.delegation.max_concurrent_children,
    );
    let scope = match identity.granted.messaging {
        MessagingScope::SiblingsAndParent => "siblings and parent",
        MessagingScope::ParentOnly => "parent only",
        MessagingScope::None => "none — signal_agent is not on your surface",
    };
    let _ = write!(out, "- Messaging scope: {scope}");
    out
}

/// Synthetic tool-result content the fork tool injects for its own call.
///
/// Closes the orphan-tool-call hole at the source: the child's seed events
/// retain the parent's last `AssistantMessage` (with the `fork` tool call)
/// and gain a synthetic [`SessionEvent::ToolResult`] whose `tool_name` is
/// `"fork"` so the provider sees a complete tool-call/tool-result pair (R2).
pub const FORK_SYNTHETIC_RESULT_MESSAGE: &str = "fork created successfully";

/// Optional [`ToolContext`](crate::tool::context::ToolContext) extension
/// carrying the **identity-free** base system instruction the fork tool
/// composes with.
///
/// Published when assembling an agent's
/// [`ToolContext`](crate::tool::context::ToolContext) so the fork tool
/// can compose a fork child's base as `fork preamble + parent_base` (R5).
/// The invariant at every level is that this extension never carries a
/// fork-identity preamble:
///
/// - the **root** publishes its own installed base (no preamble by
///   construction);
/// - a **spawned child** publishes its own base (its variant / profile /
///   task instruction — identity-free working instructions);
/// - a **fork** publishes the parent base it composed with — NOT its own
///   combined base, whose leading "Fork identity" block would otherwise
///   stack (stale) under a fork-of-fork's fresh preamble. Every fork
///   level therefore renders fresh preamble + the original base, with
///   exactly one identity block.
///
/// Absent extensions are not an error — the child falls back to just the
/// preamble.
#[derive(Clone, Debug)]
pub struct ParentSystemInstruction(pub Arc<String>);

impl ParentSystemInstruction {
    /// Construct from any string-like input.
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(Arc::new(s.into()))
    }

    /// Borrow the underlying instruction text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Combine a built fork preamble (see [`build_fork_preamble`]) with the
/// parent's base system instruction (R5).
///
/// The preamble comes first so the child's first read frames it as a forked
/// sub-agent before encountering the parent's content. When `parent_base` is
/// empty the result is just the preamble.
#[must_use]
pub fn combine_system_instruction(preamble: &str, parent_base: &str) -> String {
    if parent_base.is_empty() {
        preamble.to_owned()
    } else {
        format!("{preamble}\n\n{parent_base}")
    }
}

/// Build the child's structured-output JSON schema.
///
/// Requirements are keyed by name in an object so the model cannot skip,
/// add extras, or get names wrong. `completed` and `completion_notes` are
/// NOT required within each requirement object — a timed-out fork can
/// produce valid partial output.
#[must_use]
pub fn build_fork_output_schema(requirements: &[ForkRequirement]) -> Value {
    let mut req_properties = serde_json::Map::new();
    let mut req_required = Vec::new();

    for req in requirements {
        let slug = slugify_requirement_name(&req.name);
        req_required.push(Value::String(slug.clone()));
        req_properties.insert(
            slug,
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "completed": { "type": "boolean" },
                    "completion_notes": { "type": "string" }
                }
            }),
        );
    }

    serde_json::json!({
        "type": "object",
        "required": ["response", "requirements"],
        "additionalProperties": false,
        "properties": {
            "response": {
                "type": "string",
                "description": "Free-text summary the fork returns to the parent."
            },
            "requirements": {
                "type": "object",
                "required": req_required,
                "additionalProperties": false,
                "properties": req_properties
            }
        }
    })
}

/// Convert a requirement name to a valid JSON property key.
///
/// Replaces whitespace and non-alphanumeric characters with underscores,
/// lowercases, and deduplicates consecutive underscores.
#[must_use]
pub fn slugify_requirement_name(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            slug.extend(ch.to_lowercase());
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    slug.trim_matches('_').to_owned()
}

/// Format a successful fork result as the markdown envelope delivered to
/// the parent agent.
///
/// The envelope is framed with a system-delivery notice so the receiving
/// model can distinguish auto-delivered fork results from user input.
#[must_use]
pub fn format_fork_result(fork_id: Uuid, response: &str, requirements: &Value) -> String {
    let short_id = &fork_id.to_string()[..8];
    let mut out = format!(
        "[System: the following result was automatically delivered by fork {short_id} \
         on completion. This is not user input.]\n\n\
         --- FORK RESULT ({short_id}) ---\n\n\
         {response}\n",
    );

    if let Some(obj) = requirements.as_object() {
        for (name, value) in obj {
            let completed = value
                .get("completed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let status = if completed {
                "completed"
            } else {
                "not completed"
            };
            let notes = value
                .get("completion_notes")
                .and_then(Value::as_str)
                .unwrap_or("");
            let _ = write!(out, "\n**{name}** _{status}_\n");
            if !notes.is_empty() {
                let _ = writeln!(out, "{notes}");
            }
        }
    }
    out.push_str("\n--- END FORK RESULT ---\n");
    out
}

/// Format a failed fork result as the markdown envelope.
#[must_use]
pub fn format_fork_failure(fork_id: Uuid, error: &str, requirement_names: &[String]) -> String {
    let short_id = &fork_id.to_string()[..8];
    let mut out = format!(
        "[System: the following failure was automatically delivered by fork {short_id} \
         on completion. This is not user input.]\n\n\
         --- FORK FAILED ({short_id}) ---\n\n\
         {error}\n",
    );
    if !requirement_names.is_empty() {
        let _ = write!(
            out,
            "\nRequirements were: {}\n",
            requirement_names.join(", "),
        );
    }
    out.push_str("\n--- END FORK RESULT ---\n");
    out
}

/// Format a successful spawn result as the markdown envelope.
#[must_use]
pub fn format_spawn_result(agent_id: Uuid, agent_role: &str, output: &str) -> String {
    let short_id = &agent_id.to_string()[..8];
    format!(
        "[System: the following result was automatically delivered by {agent_role} {short_id} \
         on completion. This is not user input.]\n\n\
         --- AGENT RESULT ({agent_role} {short_id}) ---\n\n\
         {output}\n\n\
         --- END AGENT RESULT ---\n",
    )
}

/// Format a failed spawn result as the markdown envelope.
#[must_use]
pub fn format_spawn_failure(agent_id: Uuid, agent_role: &str, error: &str) -> String {
    let short_id = &agent_id.to_string()[..8];
    format!(
        "[System: the following failure was automatically delivered by {agent_role} {short_id} \
         on completion. This is not user input.]\n\n\
         --- AGENT FAILED ({agent_role} {short_id}) ---\n\n\
         {error}\n\n\
         --- END AGENT RESULT ---\n",
    )
}

/// Format a [`ForkOutcome`](crate::tools::agent::fork_outcome::ForkOutcome)
/// into the `(succeeded, formatted_message, error)` triple for the child
/// result channel. Lives here alongside the other format functions so
/// `fork_outcome.rs` stays under the 500-line production code limit.
#[must_use]
pub(crate) fn format_fork_outcome(
    fork_id: Uuid,
    outcome: &crate::tools::agent::ForkOutcome,
    requirement_names: &[String],
) -> (bool, String, Option<String>) {
    use crate::agent::registry::AgentStatus;

    if let Some(ref err) = outcome.error_message {
        let msg = format_fork_failure(fork_id, err, requirement_names);
        return (false, msg, Some(err.clone()));
    }
    if outcome.status == AgentStatus::Failed {
        let err = "fork failed with no error message";
        let msg = format_fork_failure(fork_id, err, requirement_names);
        return (false, msg, Some(err.to_owned()));
    }

    let response = outcome
        .result_summary
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or("");
    let requirements = outcome
        .result_summary
        .get("requirements")
        .cloned()
        .unwrap_or(Value::Null);
    let msg = format_fork_result(fork_id, response, &requirements);
    (true, msg, None)
}

/// Inject a synthetic [`SessionEvent::ToolResult`] for the fork tool call
/// itself onto `events` (R2).
///
/// `fork_call_id` is the tool-call id the provider assigned to the parent's
/// `fork` call (read off the parent's most-recent `AssistantMessage`'s
/// `tool_calls`). `fork_id` is the registry id the fork tool reserved — it
/// flows into the synthetic result's `agent_id` field so the child sees how
/// it was created. The field name matches the real fork tool output on the
/// parent side (`{"agent_id": ...}`), so both replay sides of a
/// `tool_name: "fork"` result share one vocabulary.
///
/// The synthetic event is appended at the end of `events`. The caller is
/// responsible for applying any [`ContextFilter`] *before* calling this
/// helper so the filter does not strip the freshly-injected result.
#[must_use]
pub fn inject_synthetic_fork_result(
    events: Vec<SessionEvent>,
    fork_call_id: &str,
    fork_id: Uuid,
) -> Vec<SessionEvent> {
    let parent_id = events.last().map(|e| e.base().id.clone());
    let mut out = events;
    out.push(SessionEvent::ToolResult {
        base: EventBase::new(parent_id),
        tool_call_id: fork_call_id.to_owned(),
        tool_name: "fork".to_owned(),
        output: serde_json::json!({
            "agent_id": fork_id.to_string(),
            "status": "active",
            "message": FORK_SYNTHETIC_RESULT_MESSAGE,
        }),
        spool_ref: None,
        duration_ms: 0,
    });
    out
}

/// An orphan `tool_call` in the parent history that has no legacy `ToolResult`
/// or canonical function/custom call-output item.
pub struct OrphanToolCall {
    /// Provider-assigned tool call ID.
    pub id: String,
    /// Tool name (used when injecting a synthetic placeholder result).
    pub name: String,
}

/// Defensive completeness check (R2).
///
/// Returns every tool call in `events` that lacks either supported local
/// result representation, *excluding* `fork_call_id` (which the synthetic
/// injection covers). An empty or complete history returns an empty `Vec`.
#[must_use]
pub fn verify_no_orphan_tool_calls(
    events: &[SessionEvent],
    fork_call_id: &str,
) -> Vec<OrphanToolCall> {
    crate::session::unresolved_local_tool_calls(events)
        .into_iter()
        .filter(|tc| tc.call_id != fork_call_id)
        .map(|tc| OrphanToolCall {
            id: tc.call_id,
            name: tc.name,
        })
        .collect()
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
    clippy::match_wildcard_for_single_variants,
    clippy::unreachable
)]
mod tests {
    use super::*;
    use crate::session::events::{EventBase, EventUsage, ToolCallEvent};

    fn user_msg(text: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: text.to_string(),
        }
    }

    fn assistant_with_tool_calls(calls: Vec<(&str, &str)>) -> SessionEvent {
        SessionEvent::AssistantMessage {
            response_items: Vec::new(),
            base: EventBase::new(None),
            content: "calling tool".to_string(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: calls
                .into_iter()
                .map(|(call_id, name)| ToolCallEvent {
                    call_id: call_id.to_string(),
                    name: name.to_string(),
                    arguments: serde_json::json!({}),
                    kind: crate::provider::request::ToolCallKind::Function,
                    caller: crate::provider::request::ToolCallCaller::Absent,
                })
                .collect(),
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }
    }

    fn tool_result(call_id: &str, name: &str) -> SessionEvent {
        SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: call_id.to_string(),
            tool_name: name.to_string(),
            output: serde_json::json!({"content":"hi"}),
            spool_ref: None,
            duration_ms: 5,
        }
    }

    fn label() -> SessionEvent {
        SessionEvent::Label {
            base: EventBase::new(None),
            label: "checkpoint".to_string(),
            description: None,
        }
    }

    #[test]
    fn context_filter_default_keeps_everything() {
        let events = vec![
            user_msg("hi"),
            assistant_with_tool_calls(vec![("tc1", "read")]),
            tool_result("tc1", "read"),
        ];
        let filter = ContextFilter::default();
        let out = filter.apply(&events);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn context_filter_exclude_tool_calls_drops_tool_results_and_strips_tool_calls() {
        let events = vec![
            user_msg("hi"),
            assistant_with_tool_calls(vec![("tc1", "read")]),
            tool_result("tc1", "read"),
        ];
        let filter = ContextFilter {
            include_system: true,
            include_recent_n: None,
            exclude_tool_calls: true,
        };
        let out = filter.apply(&events);
        assert_eq!(out.len(), 2, "tool result should be dropped");
        let SessionEvent::AssistantMessage { tool_calls, .. } = &out[1] else {
            panic!("expected assistant message");
        };
        assert!(tool_calls.is_empty(), "tool_calls should be stripped");
    }

    #[test]
    fn context_filter_include_recent_n_truncates_to_last_n() {
        let events: Vec<SessionEvent> = (0..10).map(|i| user_msg(&format!("msg {i}"))).collect();
        let filter = ContextFilter {
            include_system: true,
            include_recent_n: Some(3),
            exclude_tool_calls: false,
        };
        let out = filter.apply(&events);
        assert_eq!(out.len(), 3);
        let SessionEvent::UserMessage { content, .. } = &out[0] else {
            panic!("expected user message");
        };
        assert_eq!(content, "msg 7");
    }

    #[test]
    fn context_filter_include_recent_n_trims_leading_orphan_tool_results() {
        let events = vec![
            user_msg("hi"),
            assistant_with_tool_calls(vec![("tc1", "bash")]),
            tool_result("tc1", "bash"),
            user_msg("next"),
            assistant_with_tool_calls(vec![("tc2", "read")]),
            tool_result("tc2", "read"),
        ];
        let filter = ContextFilter {
            include_system: true,
            include_recent_n: Some(4),
            exclude_tool_calls: false,
        };
        let out = filter.apply(&events);
        assert!(
            !matches!(out.first(), Some(SessionEvent::ToolResult { .. })),
            "leading ToolResult must be trimmed to avoid orphan tool results",
        );
        let SessionEvent::UserMessage { content, .. } = &out[0] else {
            panic!("expected UserMessage after trim, got {:?}", out[0]);
        };
        assert_eq!(content, "next");
    }

    #[test]
    fn context_filter_exclude_system_drops_label_events() {
        let events = vec![user_msg("hi"), label(), user_msg("bye")];
        let filter = ContextFilter {
            include_system: false,
            include_recent_n: None,
            exclude_tool_calls: false,
        };
        let out = filter.apply(&events);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|e| !matches!(e, SessionEvent::Label { .. })));
    }

    fn golden_identity_policy() -> crate::agent::child_policy::ChildPolicy {
        use crate::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 4,
            },
            inbound_capacity: 8,
            loop_config: None,
        }
    }

    #[test]
    fn combine_system_instruction_empty_parent_uses_preamble_only() {
        let combined = combine_system_instruction(FORK_SYSTEM_PREAMBLE, "");
        assert_eq!(combined, FORK_SYSTEM_PREAMBLE);
    }

    #[test]
    fn combine_system_instruction_prepends_preamble_to_parent_base() {
        let parent = "You are the parent agent. Be terse.";
        let slugs = vec!["check_code".to_owned()];
        let policy = golden_identity_policy();
        let preamble = build_fork_preamble(&ForkIdentity {
            parent_agent_id: "5e1f0000-0000-0000-0000-000000000001",
            path_address: "root/fork-kestrel",
            requirement_slugs: &slugs,
            granted: &policy,
        });
        let combined = combine_system_instruction(&preamble, parent);
        assert!(
            combined.starts_with(FORK_SYSTEM_PREAMBLE),
            "fork preamble must come first: {combined}",
        );
        assert!(
            combined.contains(parent),
            "parent base must be retained verbatim: {combined}",
        );
        assert!(
            combined.find(&preamble).expect("preamble present")
                < combined.find(parent).expect("parent present"),
            "the whole structured preamble precedes the parent base: {combined}",
        );
    }

    /// R4 golden-file test: fixed inputs render byte-for-byte to the
    /// checked-in `testdata/fork_preamble.golden.md` — contract slugs,
    /// path address, parent id, and the delegation budget all present.
    /// A plain file plus `assert_eq!`, no snapshot crate.
    #[test]
    fn fork_preamble_matches_golden_file() {
        let slugs = vec!["check_code".to_owned(), "run_tests".to_owned()];
        let policy = golden_identity_policy();
        let rendered = build_fork_preamble(&ForkIdentity {
            parent_agent_id: "5e1f0000-0000-0000-0000-000000000001",
            path_address: "root/fork-kestrel",
            requirement_slugs: &slugs,
            granted: &policy,
        });
        assert_eq!(
            rendered,
            include_str!("testdata/fork_preamble.golden.md"),
            "the fork preamble rendering is pinned by the golden file; \
             update testdata/fork_preamble.golden.md deliberately when the \
             preamble changes",
        );
    }

    /// R4: a leaf fork (depth 0, messaging none) is told plainly that it
    /// cannot delegate and cannot message — the budget is never a
    /// surprise discovered at call-rejection time.
    #[test]
    fn fork_preamble_tells_a_leaf_it_is_a_leaf() {
        use crate::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
        let policy = ChildPolicy {
            messaging: MessagingScope::None,
            delegation: DelegationBudget {
                remaining_depth: 0,
                max_concurrent_children: 0,
            },
            inbound_capacity: 1,
            loop_config: None,
        };
        let rendered = build_fork_preamble(&ForkIdentity {
            parent_agent_id: "parent-id",
            path_address: "root/fork-a",
            requirement_slugs: &[],
            granted: &policy,
        });
        assert!(
            rendered.contains("Remaining delegation depth: 0"),
            "{rendered}"
        );
        assert!(rendered.contains("you are a leaf"), "{rendered}");
        assert!(rendered.contains("Messaging scope: none"), "{rendered}");
        assert!(
            rendered.contains("none declared"),
            "an empty contract is stated honestly: {rendered}",
        );
    }

    #[test]
    fn build_fork_output_schema_uses_object_keyed_requirements() {
        let reqs = vec![
            ForkRequirement {
                name: "Summarise the diff".to_string(),
                description: "summarise the diff".to_string(),
            },
            ForkRequirement {
                name: "Check for bugs".to_string(),
                description: "look for common bugs".to_string(),
            },
        ];
        let schema = build_fork_output_schema(&reqs);

        // Top-level required
        let required: Vec<&str> = schema["required"]
            .as_array()
            .expect("required array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"response"));
        assert!(required.contains(&"requirements"));

        // Requirements is an object, not an array
        assert_eq!(
            schema["properties"]["requirements"]["type"], "object",
            "requirements must be an object schema",
        );

        // Each requirement name is slugified and present as a required key
        let req_required: Vec<&str> = schema["properties"]["requirements"]["required"]
            .as_array()
            .expect("requirements.required array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(req_required.contains(&"summarise_the_diff"));
        assert!(req_required.contains(&"check_for_bugs"));

        // Each requirement has completed and completion_notes properties
        let props = schema["properties"]["requirements"]["properties"]
            .as_object()
            .expect("requirements properties");
        assert!(props.contains_key("summarise_the_diff"));
        assert!(props.contains_key("check_for_bugs"));

        let item = &props["summarise_the_diff"];
        assert_eq!(item["type"], "object");
        assert!(item["properties"]["completed"]["type"] == "boolean");
        assert!(item["properties"]["completion_notes"]["type"] == "string");

        // completed and completion_notes are NOT required within each requirement
        assert!(
            item.get("required").is_none() || item["required"].as_array().is_none_or(Vec::is_empty),
            "inner fields must not be required — partial output is valid",
        );
    }

    #[test]
    fn build_fork_output_schema_validates_well_formed_object_output() {
        let reqs = vec![
            ForkRequirement {
                name: "task a".to_string(),
                description: "first".to_string(),
            },
            ForkRequirement {
                name: "task b".to_string(),
                description: "second".to_string(),
            },
        ];
        let schema = build_fork_output_schema(&reqs);
        let compiled = jsonschema::validator_for(&schema).expect("schema compiles");

        // Valid: all requirements present with both fields
        let valid_full = serde_json::json!({
            "response": "done",
            "requirements": {
                "task_a": { "completed": true, "completion_notes": "ok" },
                "task_b": { "completed": false, "completion_notes": "skipped" },
            },
        });
        assert!(compiled.is_valid(&valid_full));

        // Valid: partial output (empty requirement objects — fields not required)
        let valid_partial = serde_json::json!({
            "response": "timed out",
            "requirements": {
                "task_a": {},
                "task_b": { "completed": true },
            },
        });
        assert!(compiled.is_valid(&valid_partial));

        // Invalid: missing requirements entirely
        let invalid_no_reqs = serde_json::json!({"response": "missing requirements"});
        assert!(
            !compiled.is_valid(&invalid_no_reqs),
            "missing required requirements must fail validation",
        );

        // Invalid: missing a required requirement key
        let invalid_missing_key = serde_json::json!({
            "response": "done",
            "requirements": {
                "task_a": { "completed": true },
            },
        });
        assert!(
            !compiled.is_valid(&invalid_missing_key),
            "missing requirement key must fail validation",
        );

        // Invalid: extra requirement key not in schema
        let invalid_extra_key = serde_json::json!({
            "response": "done",
            "requirements": {
                "task_a": { "completed": true },
                "task_b": { "completed": true },
                "task_c": { "completed": true },
            },
        });
        assert!(
            !compiled.is_valid(&invalid_extra_key),
            "extra requirement key must fail validation",
        );
    }

    #[test]
    fn slugify_requirement_name_basic_cases() {
        assert_eq!(
            slugify_requirement_name("Summarise the diff"),
            "summarise_the_diff"
        );
        assert_eq!(slugify_requirement_name("Check for bugs"), "check_for_bugs");
        assert_eq!(slugify_requirement_name("simple"), "simple");
        assert_eq!(slugify_requirement_name("ALL CAPS"), "all_caps");
    }

    #[test]
    fn slugify_requirement_name_special_characters() {
        assert_eq!(slugify_requirement_name("foo--bar"), "foo_bar");
        assert_eq!(
            slugify_requirement_name("  leading spaces  "),
            "leading_spaces"
        );
        assert_eq!(slugify_requirement_name("a!@#$%b"), "a_b");
        assert_eq!(slugify_requirement_name("CamelCase123"), "camelcase123");
    }

    #[test]
    fn format_fork_result_includes_system_notice_and_envelope() {
        let fork_id = Uuid::new_v4();
        let reqs = serde_json::json!({
            "check_code": { "completed": true, "completion_notes": "all good" },
            "run_tests": { "completed": false, "completion_notes": "timed out" },
        });
        let result = format_fork_result(fork_id, "Summary text", &reqs);
        let short_id = &fork_id.to_string()[..8];

        assert!(
            result.contains("automatically delivered by fork"),
            "system notice: {result:?}"
        );
        assert!(
            result.contains("This is not user input"),
            "user-input disclaimer: {result:?}"
        );
        assert!(result.contains(&format!("FORK RESULT ({short_id})")));
        assert!(result.contains("Summary text"));
        assert!(result.contains("**check_code** _completed_"));
        assert!(result.contains("all good"));
        assert!(result.contains("**run_tests** _not completed_"));
        assert!(result.contains("timed out"));
        assert!(result.contains("END FORK RESULT"));
    }

    #[test]
    fn format_fork_result_handles_missing_fields() {
        let fork_id = Uuid::new_v4();
        let reqs = serde_json::json!({
            "partial": {},
        });
        let result = format_fork_result(fork_id, "partial output", &reqs);
        assert!(result.contains("**partial** _not completed_"));
        assert!(!result.contains('\0'));
    }

    #[test]
    fn format_fork_result_handles_non_object_requirements() {
        let fork_id = Uuid::new_v4();
        let reqs = serde_json::json!("not an object");
        let result = format_fork_result(fork_id, "response", &reqs);
        assert!(result.contains("response"));
        assert!(result.contains("END FORK RESULT"));
        assert!(
            !result.contains("_completed_"),
            "no requirement lines for non-object"
        );
    }

    #[test]
    fn format_fork_failure_includes_system_notice_and_error() {
        let fork_id = Uuid::new_v4();
        let names = vec!["check code".to_string(), "run tests".to_string()];
        let result = format_fork_failure(fork_id, "context window exceeded", &names);
        let short_id = &fork_id.to_string()[..8];

        assert!(
            result.contains("automatically delivered by fork"),
            "system notice: {result:?}"
        );
        assert!(result.contains(&format!("FORK FAILED ({short_id})")));
        assert!(result.contains("context window exceeded"));
        assert!(result.contains("Requirements were: check code, run tests"));
        assert!(result.contains("END FORK RESULT"));
    }

    #[test]
    fn format_fork_failure_empty_requirements() {
        let fork_id = Uuid::new_v4();
        let result = format_fork_failure(fork_id, "error", &[]);
        assert!(!result.contains("Requirements were"));
        assert!(result.contains("END FORK RESULT"));
    }

    #[test]
    fn format_spawn_result_includes_system_notice_and_output() {
        let agent_id = Uuid::new_v4();
        let result = format_spawn_result(agent_id, "reviewer", "Looks good");
        let short_id = &agent_id.to_string()[..8];

        assert!(
            result.contains("automatically delivered by reviewer"),
            "system notice: {result:?}"
        );
        assert!(result.contains(&format!("AGENT RESULT (reviewer {short_id})")));
        assert!(result.contains("Looks good"));
        assert!(result.contains("END AGENT RESULT"));
    }

    #[test]
    fn format_spawn_failure_includes_system_notice_and_error() {
        let agent_id = Uuid::new_v4();
        let result = format_spawn_failure(agent_id, "reviewer", "timed out");
        let short_id = &agent_id.to_string()[..8];

        assert!(
            result.contains("automatically delivered by reviewer"),
            "system notice: {result:?}"
        );
        assert!(result.contains(&format!("AGENT FAILED (reviewer {short_id})")));
        assert!(result.contains("timed out"));
        assert!(result.contains("END AGENT RESULT"));
    }

    #[test]
    fn inject_synthetic_fork_result_appends_tool_result_with_fork_name() {
        let fork_id = Uuid::new_v4();
        let events = vec![
            user_msg("go"),
            assistant_with_tool_calls(vec![("tc-fork", "fork")]),
        ];
        let out = inject_synthetic_fork_result(events, "tc-fork", fork_id);
        let injected = out.last().expect("at least one event");
        match injected {
            SessionEvent::ToolResult {
                tool_call_id,
                tool_name,
                output,
                ..
            } => {
                assert_eq!(tool_call_id, "tc-fork");
                assert_eq!(tool_name, "fork");
                // Pinned vocabulary: the synthetic child-side result uses
                // the same `agent_id` field as the parent-side fork tool
                // output — never `fork_id`.
                assert_eq!(output["agent_id"], fork_id.to_string());
                assert!(
                    output.get("fork_id").is_none(),
                    "legacy fork_id field must not reappear",
                );
                assert_eq!(output["status"], "active");
                assert_eq!(output["message"], FORK_SYNTHETIC_RESULT_MESSAGE);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn verify_no_orphan_tool_calls_returns_empty_when_all_results_present() {
        let events = vec![
            user_msg("go"),
            assistant_with_tool_calls(vec![("tc-read", "read"), ("tc-fork", "fork")]),
            tool_result("tc-read", "read"),
        ];
        let orphans = verify_no_orphan_tool_calls(&events, "tc-fork");
        assert!(orphans.is_empty());
    }

    #[test]
    fn verify_no_orphan_tool_calls_flags_missing_results() {
        let events = vec![
            user_msg("go"),
            assistant_with_tool_calls(vec![
                ("tc-read", "read"),
                ("tc-search", "search"),
                ("tc-fork", "fork"),
            ]),
            // Only the `read` result has been appended so far.
            tool_result("tc-read", "read"),
        ];
        let orphans = verify_no_orphan_tool_calls(&events, "tc-fork");
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].id, "tc-search");
        assert_eq!(orphans[0].name, "search");
    }

    #[test]
    fn verify_no_orphan_tool_calls_empty_events_returns_empty() {
        let orphans = verify_no_orphan_tool_calls(&[], "tc-fork");
        assert!(orphans.is_empty());
    }

    #[test]
    fn parent_system_instruction_roundtrips() {
        let ext = ParentSystemInstruction::new("be brief");
        assert_eq!(ext.as_str(), "be brief");
    }
}
