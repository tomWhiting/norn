//! Forking — context filtering, output-schema construction, and synthetic
//! tool-result injection for a child agent that runs concurrently with its
//! parent.
//!
//! The fork lifecycle (registry reservation, [`tokio::spawn`], status watch
//! channel, status notification, [`SessionEvent::ForkComplete`] append) lives
//! on [`crate::tools::agent::fork_tool::ForkTool`]. This module owns the
//! reusable data types — [`ContextFilter`], [`ForkRequirement`],
//! [`ParentPromptPlan`], and the legacy [`ParentSystemInstruction`] bridge —
//! together with the pure helpers the tool composes:
//!
//! - [`build_fork_output_schema`] derives the child's structured-output JSON
//!   schema from an optional task list (R7).
//! - [`build_fork_preamble`] renders compiled fork identity and delegation
//!   policy without human-authored task content.
//! - [`inject_synthetic_fork_result`] adds the synthetic `fork` tool result
//!   onto the child's seed events so the parent's most-recent assistant turn
//!   leaves no orphan tool calls (R2).
//! - [`verify_no_orphan_tool_calls`] is a defensive check the tool calls
//!   before launch — it surfaces a warning log if any tool call in the latest
//!   `AssistantMessage` lacks a matching `ToolResult`.
//! - [`ParentPromptPlan`] preserves each inherited source and authority.
//!   [`ParentSystemInstruction`] remains an input-only fallback for legacy
//!   embedders and is mapped explicitly to `EmbedderPolicy` System authority.

use std::fmt::Write as _;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::r#loop::loop_context::LoopContext;
use crate::session::events::{EventBase, SessionEvent};
use crate::system_prompt::{PromptPlan, PromptSource};

pub use super::fork_context_filter::ContextFilter;
pub use super::fork_context_filter_error::ContextFilterError;

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

/// The compiled identity and delegation policy a fork child is told about
/// itself (brief `agent-variants` R4): who forked it, where it sits, and what
/// delegation rights it holds. Human-authored task and requirement content is
/// deliberately absent and travels once as the fork's User prompt.
pub struct ForkIdentity<'a> {
    /// The forking agent's id.
    pub parent_agent_id: &'a str,
    /// The fork's own coordination path address
    /// (`BranchedChild::path_address` from the session branch mint).
    pub path_address: &'a str,
    /// The [`ChildPolicy`](crate::agent::child_policy::ChildPolicy)
    /// granted to this fork — its delegation and messaging budget.
    pub granted: &'a crate::agent::child_policy::ChildPolicy,
}

/// Render the fork preamble: the verbatim [`FORK_SYSTEM_PREAMBLE`] intent
/// followed by the fork's structured identity: parent agent id, path address,
/// and delegation rights (R4).
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

/// Legacy input-only [`ToolContext`](crate::tool::context::ToolContext)
/// extension carrying an embedder's untyped base instruction.
///
/// D8-assembled root and child contexts never publish this type. The fork
/// tool reads it only when [`ParentPromptPlan`] is absent, maps the exact text
/// to [`PromptSource::EmbedderPolicy`] System authority, and immediately
/// continues on the typed plan path. Keeping the bridge input-only prevents a
/// flattened Developer/User plan from being re-armed as System authority.
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

/// Source-aware stable prompt inherited by child launch surfaces.
///
/// Typed loop contexts are cloned byte-for-byte. Legacy
/// [`LoopContext::new`] callers contribute only their explicit base under
/// [`PromptSource::EmbedderPolicy`], preventing an untyped string from being
/// mistaken for compiled Norn policy.
#[derive(Clone, Debug)]
pub struct ParentPromptPlan(pub Arc<PromptPlan>);

impl ParentPromptPlan {
    /// Construct from an already source-aware stable plan.
    #[must_use]
    pub fn new(plan: PromptPlan) -> Self {
        Self(Arc::new(plan))
    }

    /// Capture the exact stable plan a loop would send to its provider.
    #[must_use]
    pub fn from_loop_context(context: &LoopContext) -> Self {
        if let Some(plan) = context.stable_prompt_plan() {
            return Self::new(plan.clone());
        }
        let mut plan = PromptPlan::new();
        plan.set(
            PromptSource::EmbedderPolicy,
            context.base_system_instruction(),
        );
        Self::new(plan)
    }

    /// Borrow the captured stable prompt plan.
    #[must_use]
    pub fn plan(&self) -> &PromptPlan {
        self.0.as_ref()
    }
}

/// Legacy string composition helper retained for embedder compatibility.
///
/// The live fork path uses [`ParentPromptPlan`] and never calls this helper.
/// Existing embedders that still build a flat instruction get the preamble
/// first; an empty `parent_base` yields only the preamble.
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
/// The envelope uses neutral harness framing so the receiving model can
/// distinguish runtime-delivered child output without inventing a wire role.
#[must_use]
pub fn format_fork_result(fork_id: Uuid, response: &str, requirements: &Value) -> String {
    let short_id = &fork_id.to_string()[..8];
    let mut out = format!(
        "[Norn agent result: automatically delivered by fork {short_id} on completion.]\n\n\
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
        "[Norn agent failure: automatically delivered by fork {short_id} on completion.]\n\n\
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
        "[Norn agent result: automatically delivered by {agent_role} {short_id} on completion.]\n\n\
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
        "[Norn agent failure: automatically delivered by {agent_role} {short_id} on completion.]\n\n\
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
mod context_schema_tests;
#[cfg(test)]
mod parent_prompt_tests;
#[cfg(test)]
mod result_tests;
#[cfg(test)]
mod test_support;
