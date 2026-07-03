//! `agents` — read-only status view over the caller's agent subtree.
//!
//! After `spawn_agent` or `fork` there was no model-facing way to ask "is
//! my child still running?" — operators watched agents guess from
//! action-log spelunking and misdiagnose a completed child as a
//! `close_agent` bug. [`AgentsTool`] answers from the
//! [`AgentRegistry`]'s ground truth: live entries (including terminal
//! entries not yet reclaimed, which keep their full record) and the
//! completion records ([`AgentTombstone`]) left behind at reclamation.
//!
//! **Scope rule** (the same boundary the action-log tree uses): a caller
//! sees itself and its descendant subtree only — never its parent and
//! never its siblings. Descendants are resolved over live entries *and*
//! completion records, so a child that finished and was reclaimed stays
//! visible and attributable instead of silently vanishing from the view.

use std::collections::HashSet;

use async_trait::async_trait;
use norn_macros::ToolArgs;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::agent::registry::{AgentEntry, AgentRegistry, AgentTombstone};
use crate::error::ToolError;
use crate::tool::composite::CompositeTool;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{ToolCategory, ToolOutput};
use crate::tools::agent::AgentToolInfra;

/// Read-only registry status view scoped to the caller and its
/// descendants. See the module docs for the scope rule and data sources.
pub struct AgentsTool;

impl AgentsTool {
    /// Constructs the tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for AgentsTool {
    fn default() -> Self {
        Self::new()
    }
}

/// One `agents` operation, dispatched on `action`.
#[derive(Debug, Deserialize, ToolArgs)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AgentsCommand {
    /// List every agent you can see: yourself plus your descendant
    /// subtree, including completion records of descendants that already
    /// finished and were reclaimed.
    List,
    /// Look up one visible agent by hierarchical registry path or UUID.
    Get {
        /// Target agent identified by hierarchical registry path (e.g.
        /// "/workers/analyzer") or UUID.
        agent_id: String,
    },
    /// Summarize inter-agent messaging as sender → recipient edges,
    /// derived read-only from the message audit events in your own
    /// store: messages you sent, messages your direct children exchanged
    /// (you granted their scope), and deliveries to you. A null count
    /// means "not knowable from your store", never zero.
    Messages,
}

/// Result of resolving an identifier against the registry.
///
/// Mirrors the coordination tools' resolution order so every tool tells
/// the same story about the same agent: live holder of the path →
/// terminal-but-unreclaimed holder of the path → completion record of the
/// most recently reclaimed holder → (for UUIDs) registered entry →
/// completion record.
enum Resolved {
    /// A registered entry — live, or terminal but not yet reclaimed.
    Live(AgentEntry),
    /// A reclaimed agent's retained completion record.
    Reclaimed(AgentTombstone),
}

impl Resolved {
    /// The resolved agent's id, whichever record holds it.
    fn id(&self) -> Uuid {
        match self {
            Self::Live(entry) => entry.id,
            Self::Reclaimed(tombstone) => tombstone.id,
        }
    }
}

/// Resolve `identifier` (path or UUID) against the registry, including
/// agents that already finished. `None` means no agent with this
/// identifier has any record in this session.
fn resolve(reg: &AgentRegistry, identifier: &str) -> Option<Resolved> {
    if let Some(entry) = reg.get_by_path(identifier) {
        return Some(Resolved::Live(entry));
    }
    if let Some(entry) = reg.get_terminal_by_path(identifier) {
        return Some(Resolved::Live(entry));
    }
    if let Some(tombstone) = reg.tombstone_by_path(identifier) {
        return Some(Resolved::Reclaimed(tombstone));
    }
    if let Ok(uuid) = Uuid::parse_str(identifier) {
        if let Some(entry) = reg.get(uuid) {
            return Some(Resolved::Live(entry));
        }
        if let Some(tombstone) = reg.tombstone(uuid) {
            return Some(Resolved::Reclaimed(tombstone));
        }
    }
    None
}

/// The ids visible to `caller`: the caller itself plus every descendant,
/// resolved over both live entries and completion records so reclaimed
/// descendants stay attributable.
///
/// Parent links live on the children, so this runs a fixed-point pass:
/// each iteration adopts every record whose parent is already visible,
/// stopping when a pass adds nothing. The walk is depth-agnostic, so
/// budgeted recursive delegation (W3.4) renders correctly at any depth —
/// grandchildren and deeper descendants are adopted level by level.
fn visible_ids(reg: &AgentRegistry, caller: Uuid) -> HashSet<Uuid> {
    let entries = reg.list();
    let tombstones = reg.tombstones();
    let mut visible: HashSet<Uuid> = HashSet::new();
    visible.insert(caller);
    loop {
        let before = visible.len();
        for entry in &entries {
            if let Some(parent) = entry.parent_id
                && visible.contains(&parent)
            {
                visible.insert(entry.id);
            }
        }
        for tombstone in &tombstones {
            if let Some(parent) = tombstone.parent_id
                && visible.contains(&parent)
            {
                visible.insert(tombstone.id);
            }
        }
        if visible.len() == before {
            return visible;
        }
    }
}

/// Render a registered entry (live or terminal-unreclaimed). Every field
/// is ground truth from the [`AgentEntry`]; `"self"` marks the caller's
/// own record and `"reclaimed": false` distinguishes it from a completion
/// record.
fn entry_json(entry: &AgentEntry, caller: Uuid) -> Value {
    serde_json::json!({
        "id": entry.id.to_string(),
        "path": entry.path,
        "role": entry.role,
        "model": entry.model,
        "status": entry.status,
        "parent_id": entry.parent_id.map(|id| id.to_string()),
        "spawned_at": entry.spawned_at.to_rfc3339(),
        "completed_at": entry.completed_at.map(|at| at.to_rfc3339()),
        "reclaimed": false,
        "self": entry.id == caller,
        // The granted ChildPolicy is registry ground truth (W3.4): the
        // caller reads its descendants' real budgets — and its own —
        // instead of guessing what a child may delegate.
        "policy": entry.policy,
    })
}

/// Render a reclaimed agent's completion record. Role, model, and spawn
/// time are deliberately absent: the registry does not retain them after
/// reclamation, and inventing them would falsify the record.
fn tombstone_json(tombstone: &AgentTombstone) -> Value {
    serde_json::json!({
        "id": tombstone.id.to_string(),
        "path": tombstone.path,
        "status": tombstone.status,
        "parent_id": tombstone.parent_id.map(|id| id.to_string()),
        "completed_at": tombstone.completed_at.to_rfc3339(),
        "reclaimed": true,
    })
}

/// The typed soft failure for an identifier no agent in this session ever
/// had — reserved strictly for never-existed identifiers; finished agents
/// resolve to their entry or completion record instead.
fn not_found_output(action: &str, agent_id: &str) -> ToolOutput {
    ToolOutput::failure_with_content(
        serde_json::json!({ "action": action, "agent_id": agent_id }),
        ToolErrorPayload::new(
            ToolErrorKind::NotFound,
            format!(
                "agent '{agent_id}' is not registered and has no completion record — \
                 no agent with this identifier has run in this session"
            ),
        )
        .with_detail(serde_json::json!({ "agent_id": agent_id })),
    )
}

/// The typed soft failure for an agent that exists but is outside the
/// caller's scope (its parent, a sibling, or any other subtree).
fn out_of_scope_output(action: &str, agent_id: &str) -> ToolOutput {
    ToolOutput::failure_with_content(
        serde_json::json!({ "action": action, "agent_id": agent_id }),
        ToolErrorPayload::new(
            ToolErrorKind::PermissionDenied,
            format!(
                "agent '{agent_id}' is outside your scope: agents reports only \
                 yourself and your descendant subtree — not your parent or siblings"
            ),
        )
        .with_detail(serde_json::json!({ "agent_id": agent_id })),
    )
}

#[async_trait]
impl CompositeTool for AgentsTool {
    type Command = AgentsCommand;

    fn name(&self) -> &'static str {
        "agents"
    }

    fn description(&self) -> &'static str {
        include_str!("guidance/agents.description.md")
    }

    fn command_field(&self) -> &'static str {
        "action"
    }

    fn input_schema(&self) -> Value {
        AgentsCommand::json_schema()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("guidance/agents.usage.md"))
    }

    fn command_effect(&self, command: &AgentsCommand) -> ToolEffect {
        match command {
            AgentsCommand::List | AgentsCommand::Get { .. } | AgentsCommand::Messages => {
                ToolEffect::ReadOnly
            }
        }
    }

    fn conservative_effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn run(
        &self,
        command: AgentsCommand,
        _envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let infra = ctx.require_extension::<AgentToolInfra>()?;
        let caller = infra.agent_id;

        match command {
            AgentsCommand::List => {
                // One read lock for the whole snapshot so the view is
                // internally consistent (an entry can never appear both
                // live and reclaimed).
                let reg = infra.registry.read();
                let visible = visible_ids(&reg, caller);
                let mut live: Vec<AgentEntry> = reg
                    .list()
                    .into_iter()
                    .filter(|entry| visible.contains(&entry.id))
                    .collect();
                let mut reclaimed: Vec<AgentTombstone> = reg
                    .tombstones()
                    .into_iter()
                    .filter(|tombstone| visible.contains(&tombstone.id))
                    .collect();
                drop(reg);

                // Deterministic ordering: registered entries oldest-spawn
                // first, then completion records oldest-completion first
                // (ids break timestamp ties).
                live.sort_by(|a, b| {
                    a.spawned_at
                        .cmp(&b.spawned_at)
                        .then_with(|| a.id.cmp(&b.id))
                });
                reclaimed.sort_by(|a, b| {
                    a.completed_at
                        .cmp(&b.completed_at)
                        .then_with(|| a.id.cmp(&b.id))
                });

                let agents: Vec<Value> = live
                    .iter()
                    .map(|entry| entry_json(entry, caller))
                    .chain(reclaimed.iter().map(tombstone_json))
                    .collect();
                Ok(ToolOutput::success(serde_json::json!({
                    "action": "list",
                    "caller_id": caller.to_string(),
                    "count": agents.len(),
                    "agents": agents,
                })))
            }
            AgentsCommand::Get { agent_id } => {
                let reg = infra.registry.read();
                let resolved = resolve(&reg, &agent_id);
                let visible = visible_ids(&reg, caller);
                drop(reg);

                let Some(resolved) = resolved else {
                    return Ok(not_found_output("get", &agent_id));
                };
                if !visible.contains(&resolved.id()) {
                    return Ok(out_of_scope_output("get", &agent_id));
                }
                let agent = match &resolved {
                    Resolved::Live(entry) => entry_json(entry, caller),
                    Resolved::Reclaimed(tombstone) => tombstone_json(tombstone),
                };
                Ok(ToolOutput::success(serde_json::json!({
                    "action": "get",
                    "agent": agent,
                })))
            }
            AgentsCommand::Messages => {
                // The audit events are read from the caller's own store
                // (sender copies, scope-granted child copies, and
                // deliveries to the caller) — see the module docs of
                // `agents_messages` for exactly what one store attests.
                let events = infra.event_store.events();
                let derived = {
                    let reg = infra.registry.read();
                    crate::tools::agents_messages::derive_message_edges(&events, caller, &reg)
                };
                let mut output = serde_json::json!({
                    "action": "messages",
                    "caller_id": caller.to_string(),
                    "count": derived.edges.len(),
                    "edges": derived.edges,
                });
                if derived.malformed > 0 {
                    output["malformed"] = serde_json::json!(derived.malformed);
                }
                Ok(ToolOutput::success(output))
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
    clippy::doc_markdown,
    clippy::too_many_arguments
)]
mod tests {
    use std::sync::Arc;

    use parking_lot::RwLock;
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::agent::message_router::MessageRouter;
    use crate::provider::mock::MockProvider;
    use crate::provider::traits::Provider;
    use crate::session::store::EventStore;
    use crate::tool::traits::Tool;
    use crate::tools::agent::coord::test_support::{build_infra, envelope_for, register_agent};

    fn as_tool(tool: &AgentsTool) -> &dyn Tool {
        tool
    }

    /// Build an [`AgentToolInfra`] keyed to `agent_id` over an existing
    /// shared registry (unlike `build_infra`, which creates a fresh one).
    fn infra_keyed(
        registry: &Arc<RwLock<AgentRegistry>>,
        agent_id: Uuid,
        parent_id: Option<Uuid>,
    ) -> Arc<AgentToolInfra> {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(vec![]));
        Arc::new(AgentToolInfra {
            registry: Arc::clone(registry),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id,
            parent_id,
            grant: None,
            tool_registry: None,
        })
    }

    async fn execute(tool: &AgentsTool, args: Value, ctx: &ToolContext) -> ToolOutput {
        as_tool(tool)
            .execute(&envelope_for("agents", args), ctx)
            .await
            .expect("agents never hard-errors on resolvable input")
    }

    fn ctx_for(infra: Arc<AgentToolInfra>) -> ToolContext {
        let ctx = ToolContext::empty();
        ctx.insert_extension(infra);
        ctx
    }

    /// Find the entry for `id` in a list output's `agents` array.
    fn find(agents: &[Value], id: Uuid) -> Option<&Value> {
        agents.iter().find(|a| a["id"] == id.to_string())
    }

    // -- Composite derivation --------------------------------------------

    #[test]
    fn schema_is_derived_one_of_with_root_object() {
        let tool = AgentsTool::new();
        let schema = as_tool(&tool).input_schema();
        assert_eq!(schema["type"], "object");
        let variants = schema["oneOf"].as_array().expect("oneOf array");
        assert_eq!(variants.len(), 3);

        let get = variants
            .iter()
            .find(|v| v["properties"]["action"]["const"] == "get")
            .expect("get variant");
        assert_eq!(get["required"], json!(["action", "agent_id"]));

        let list = variants
            .iter()
            .find(|v| v["properties"]["action"]["const"] == "list")
            .expect("list variant");
        assert_eq!(list["required"], json!(["action"]));

        let messages = variants
            .iter()
            .find(|v| v["properties"]["action"]["const"] == "messages")
            .expect("messages variant");
        assert_eq!(messages["required"], json!(["action"]));
    }

    /// Contract pin (doc-mandated for every `CompositeTool` impl): the
    /// conservative effect covers every command's effect, one constructed
    /// value per `AgentsCommand` variant.
    #[test]
    fn conservative_effect_covers_every_command() {
        crate::tool::composite::assert_conservative_effect_covers_all_commands(
            &AgentsTool::new(),
            [
                AgentsCommand::List,
                AgentsCommand::Get {
                    agent_id: "/a".to_owned(),
                },
                AgentsCommand::Messages,
            ],
        );
    }

    #[test]
    fn every_command_classifies_read_only() {
        let tool = AgentsTool::new();
        let dyn_tool = as_tool(&tool);
        assert_eq!(dyn_tool.effect(), ToolEffect::ReadOnly);
        assert_eq!(
            dyn_tool.effect_for_args(&json!({"action": "list"})),
            ToolEffect::ReadOnly,
        );
        assert_eq!(
            dyn_tool.effect_for_args(&json!({"action": "get", "agent_id": "/a"})),
            ToolEffect::ReadOnly,
        );
        assert_eq!(
            dyn_tool.effect_for_args(&json!({"action": "messages"})),
            ToolEffect::ReadOnly,
        );
        // Malformed args fall back to the conservative effect — still
        // read-only, because no command mutates anything.
        assert_eq!(
            dyn_tool.effect_for_args(&json!({"action": "explode"})),
            ToolEffect::ReadOnly,
        );
    }

    // -- list -------------------------------------------------------------

    #[tokio::test]
    async fn list_shows_self_and_live_children_with_honest_fields() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let caller = infra.agent_id;
        let child = register_agent(&registry, "/lead/worker", Some(caller));

        let ctx = ctx_for(infra);
        let out = execute(&AgentsTool::new(), json!({"action": "list"}), &ctx).await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["action"], "list");
        assert_eq!(out.content["caller_id"], caller.to_string());
        let agents = out.content["agents"].as_array().expect("agents array");

        let entry = find(agents, child).expect("live child listed");
        assert_eq!(entry["path"], "/lead/worker");
        assert_eq!(entry["role"], "worker");
        assert_eq!(entry["model"], "claude");
        assert_eq!(entry["status"], "active");
        assert_eq!(entry["parent_id"], caller.to_string());
        assert!(
            entry["spawned_at"].as_str().is_some(),
            "spawned_at present: {entry:?}"
        );
        assert_eq!(entry["completed_at"], Value::Null);
        assert_eq!(entry["reclaimed"], false);
        assert_eq!(entry["self"], false);
    }

    #[tokio::test]
    async fn list_marks_the_caller_entry_as_self() {
        let registry = AgentRegistry::shared();
        let registered = register_agent(&registry, "/self", None);

        let ctx = ctx_for(infra_keyed(&registry, registered, None));
        let out = execute(&AgentsTool::new(), json!({"action": "list"}), &ctx).await;
        let agents = out.content["agents"].as_array().expect("agents array");
        let me = find(agents, registered).expect("caller's own entry listed");
        assert_eq!(me["self"], true);
        assert_eq!(me["path"], "/self");
    }

    /// A reclaimed child stays in the list via its completion record,
    /// marked distinctly: `reclaimed: true`, terminal status,
    /// completion timestamp — and no role/model/`spawned_at`, which the
    /// registry does not retain after reclamation.
    #[tokio::test]
    async fn list_includes_reclaimed_children_marked_distinctly() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let caller = infra.agent_id;
        let live = register_agent(&registry, "/c/live", Some(caller));
        let done = register_agent(&registry, "/c/done", Some(caller));
        registry.write().mark_completed(done).expect("complete");
        assert!(registry.write().remove_terminal(done), "reclaim");

        let ctx = ctx_for(infra);
        let out = execute(&AgentsTool::new(), json!({"action": "list"}), &ctx).await;
        let agents = out.content["agents"].as_array().expect("agents array");
        assert_eq!(out.content["count"], 2);

        let live_entry = find(agents, live).expect("live child listed");
        assert_eq!(live_entry["reclaimed"], false);

        let reclaimed = find(agents, done).expect("reclaimed child listed");
        assert_eq!(reclaimed["reclaimed"], true);
        assert_eq!(reclaimed["status"], "completed");
        assert_eq!(reclaimed["path"], "/c/done");
        assert_eq!(reclaimed["parent_id"], caller.to_string());
        assert!(
            reclaimed["completed_at"].as_str().is_some(),
            "completion time present: {reclaimed:?}"
        );
        // Honesty: fields the completion record does not carry are
        // absent, never invented.
        assert!(reclaimed.get("role").is_none());
        assert!(reclaimed.get("model").is_none());
        assert!(reclaimed.get("spawned_at").is_none());
    }

    /// A child that finished but was NOT yet reclaimed is still a
    /// registered entry: full fields, terminal status, `completed_at`
    /// stamped, `reclaimed: false`.
    #[tokio::test]
    async fn list_shows_terminal_unreclaimed_child_with_full_record() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let caller = infra.agent_id;
        let failed = register_agent(&registry, "/c/failed", Some(caller));
        registry.write().mark_failed(failed).expect("fail");

        let ctx = ctx_for(infra);
        let out = execute(&AgentsTool::new(), json!({"action": "list"}), &ctx).await;
        let agents = out.content["agents"].as_array().expect("agents array");
        let entry = find(agents, failed).expect("terminal child listed");
        assert_eq!(entry["status"], "failed");
        assert_eq!(entry["reclaimed"], false);
        assert_eq!(entry["role"], "worker");
        assert!(
            entry["completed_at"].as_str().is_some(),
            "terminal mark stamps completed_at: {entry:?}"
        );
    }

    /// Scope boundary: the caller sees itself and its descendants only —
    /// its parent and its siblings are invisible, live or reclaimed.
    #[tokio::test]
    async fn list_excludes_parent_and_siblings() {
        let registry = AgentRegistry::shared();
        let parent = register_agent(&registry, "/parent", None);
        let caller = register_agent(&registry, "/parent/me", Some(parent));
        let sibling = register_agent(&registry, "/parent/sibling", Some(parent));
        let reclaimed_sibling = register_agent(&registry, "/parent/gone", Some(parent));
        registry
            .write()
            .mark_completed(reclaimed_sibling)
            .expect("complete");
        assert!(registry.write().remove_terminal(reclaimed_sibling));

        let ctx = ctx_for(infra_keyed(&registry, caller, Some(parent)));
        let out = execute(&AgentsTool::new(), json!({"action": "list"}), &ctx).await;
        let agents = out.content["agents"].as_array().expect("agents array");

        assert!(find(agents, caller).is_some(), "caller visible");
        assert!(find(agents, parent).is_none(), "parent must be invisible");
        assert!(find(agents, sibling).is_none(), "sibling must be invisible");
        assert!(
            find(agents, reclaimed_sibling).is_none(),
            "reclaimed sibling must be invisible too",
        );
        assert_eq!(out.content["count"], 1);
    }

    /// Depth ≥ 2 (W3.4): the fixed-point walk adopts the whole descendant
    /// subtree — child, live grandchild, and a reclaimed grandchild —
    /// with nested paths, real parent links, and the granted budget
    /// surfaced from ground truth; a grandchild caller still sees only
    /// itself, and a mid-tree caller sees its subtree but never the root.
    #[tokio::test]
    async fn list_renders_deeper_tree_and_scopes_each_level() {
        let registry = AgentRegistry::shared();
        let root = register_agent(&registry, "/root", None);
        let child = register_agent(&registry, "/root/spawn/c", Some(root));
        let grandchild = register_agent(&registry, "/root/spawn/c/spawn/g1", Some(child));
        let reclaimed_grandchild = register_agent(&registry, "/root/spawn/c/spawn/g2", Some(child));
        registry
            .write()
            .mark_completed(reclaimed_grandchild)
            .expect("complete");
        assert!(registry.write().remove_terminal(reclaimed_grandchild));

        // Root view: the whole tree, including the reclaimed grandchild.
        let ctx = ctx_for(infra_keyed(&registry, root, None));
        let out = execute(&AgentsTool::new(), json!({"action": "list"}), &ctx).await;
        assert_eq!(out.content["count"], 4);
        let agents = out.content["agents"].as_array().expect("agents array");
        assert!(find(agents, child).is_some(), "child visible");
        let g = find(agents, grandchild).expect("grandchild visible from root");
        assert_eq!(g["parent_id"], child.to_string());
        assert_eq!(g["path"], "/root/spawn/c/spawn/g1");
        // Granted budgets decrement per level (root test policy is depth 5).
        assert_eq!(g["policy"]["delegation"]["remaining_depth"], 3);
        // R5: the rendered policy carries loop_config explicitly — null
        // when the agent runs library defaults, never silently omitted.
        assert!(
            g["policy"]
                .as_object()
                .expect("policy object")
                .contains_key("loop_config"),
            "the policy rendering must surface loop_config: {:?}",
            g["policy"],
        );
        assert_eq!(g["policy"]["loop_config"], serde_json::Value::Null);
        let reclaimed = find(agents, reclaimed_grandchild).expect("reclaimed grandchild visible");
        assert_eq!(reclaimed["reclaimed"], true);
        assert_eq!(reclaimed["parent_id"], child.to_string());

        // Grandchild view: only itself.
        let gctx = ctx_for(infra_keyed(&registry, grandchild, Some(child)));
        let out = execute(&AgentsTool::new(), json!({"action": "list"}), &gctx).await;
        assert_eq!(out.content["count"], 1);

        // Mid-tree view: itself + its own children; root stays invisible.
        let cctx = ctx_for(infra_keyed(&registry, child, Some(root)));
        let out = execute(&AgentsTool::new(), json!({"action": "list"}), &cctx).await;
        assert_eq!(out.content["count"], 3);
        let agents = out.content["agents"].as_array().expect("agents array");
        assert!(
            find(agents, root).is_none(),
            "root must be invisible from a mid-tree caller"
        );
    }

    /// An unregistered caller (e.g. a root agent with no registry entry)
    /// still sees its children — the walk keys on parent links, not on
    /// the caller having an entry of its own.
    #[tokio::test]
    async fn list_works_when_caller_has_no_registry_entry() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let caller = infra.agent_id;
        let child = register_agent(&registry, "/spawn/kid", Some(caller));

        let ctx = ctx_for(infra);
        let out = execute(&AgentsTool::new(), json!({"action": "list"}), &ctx).await;
        let agents = out.content["agents"].as_array().expect("agents array");
        assert_eq!(agents.len(), 1);
        assert!(find(agents, child).is_some());
    }

    // -- get ----------------------------------------------------------------

    #[tokio::test]
    async fn get_resolves_live_child_by_path_and_uuid() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let caller = infra.agent_id;
        let child = register_agent(&registry, "/me/kid", Some(caller));

        let ctx = ctx_for(infra);
        let tool = AgentsTool::new();
        for identifier in ["/me/kid".to_string(), child.to_string()] {
            let out = execute(
                &tool,
                json!({"action": "get", "agent_id": identifier}),
                &ctx,
            )
            .await;
            assert!(!out.is_error(), "{:?}", out.content);
            let agent = &out.content["agent"];
            assert_eq!(agent["id"], child.to_string());
            assert_eq!(agent["path"], "/me/kid");
            assert_eq!(agent["status"], "active");
            assert_eq!(agent["reclaimed"], false);
        }
    }

    /// A finished-but-unreclaimed child reports its real terminal
    /// outcome (path resolution goes through the terminal-by-path scan
    /// because terminal entries free their live path index slot).
    #[tokio::test]
    async fn get_reports_terminal_unreclaimed_child() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let caller = infra.agent_id;
        let child = register_agent(&registry, "/me/done", Some(caller));
        registry.write().mark_failed(child).expect("fail");

        let ctx = ctx_for(infra);
        let tool = AgentsTool::new();
        for identifier in ["/me/done".to_string(), child.to_string()] {
            let out = execute(
                &tool,
                json!({"action": "get", "agent_id": identifier}),
                &ctx,
            )
            .await;
            assert!(!out.is_error(), "{:?}", out.content);
            let agent = &out.content["agent"];
            assert_eq!(agent["status"], "failed");
            assert_eq!(agent["reclaimed"], false);
            assert!(
                agent["completed_at"].as_str().is_some(),
                "terminal mark stamps completed_at: {agent:?}"
            );
        }
    }

    /// A reclaimed child resolves to its completion record — honest
    /// terminal status and timestamp, never "not found".
    #[tokio::test]
    async fn get_reports_reclaimed_child_completion_record() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let caller = infra.agent_id;
        let child = register_agent(&registry, "/me/gone", Some(caller));
        registry.write().mark_completed(child).expect("complete");
        assert!(registry.write().remove_terminal(child));

        let ctx = ctx_for(infra);
        let tool = AgentsTool::new();
        for identifier in ["/me/gone".to_string(), child.to_string()] {
            let out = execute(
                &tool,
                json!({"action": "get", "agent_id": identifier}),
                &ctx,
            )
            .await;
            assert!(!out.is_error(), "{:?}", out.content);
            let agent = &out.content["agent"];
            assert_eq!(agent["id"], child.to_string());
            assert_eq!(agent["status"], "completed");
            assert_eq!(agent["reclaimed"], true);
            assert!(agent["completed_at"].as_str().is_some());
        }
    }

    /// `not_found` is reserved for identifiers that never existed: unknown
    /// UUID and unknown path both produce the typed soft failure.
    #[tokio::test]
    async fn get_never_existed_is_typed_not_found() {
        let (infra, _registry, _router) = build_infra(Uuid::new_v4());
        let ctx = ctx_for(infra);
        let tool = AgentsTool::new();
        for identifier in [Uuid::new_v4().to_string(), "/never/existed".to_string()] {
            let out = execute(
                &tool,
                json!({"action": "get", "agent_id": identifier}),
                &ctx,
            )
            .await;
            assert!(out.is_error());
            let payload = out.error().expect("typed payload");
            assert_eq!(payload.kind, ToolErrorKind::NotFound);
            assert_eq!(payload.detail["agent_id"], identifier);
            assert!(
                payload.message.contains("no completion record"),
                "message states the truth: {}",
                payload.message
            );
        }
    }

    /// Scope boundary on get: the caller's parent and sibling exist but
    /// are out of scope — typed `permission_denied`, not `not_found` (they
    /// did exist) and not their record (they are not the caller's to see).
    #[tokio::test]
    async fn get_parent_or_sibling_is_permission_denied() {
        let registry = AgentRegistry::shared();
        let parent = register_agent(&registry, "/parent", None);
        let caller = register_agent(&registry, "/parent/me", Some(parent));
        let sibling = register_agent(&registry, "/parent/sibling", Some(parent));

        let ctx = ctx_for(infra_keyed(&registry, caller, Some(parent)));
        let tool = AgentsTool::new();
        for identifier in [
            "/parent".to_string(),
            parent.to_string(),
            "/parent/sibling".to_string(),
            sibling.to_string(),
        ] {
            let out = execute(
                &tool,
                json!({"action": "get", "agent_id": identifier}),
                &ctx,
            )
            .await;
            assert!(out.is_error(), "out-of-scope must fail: {identifier}");
            let payload = out.error().expect("typed payload");
            assert_eq!(payload.kind, ToolErrorKind::PermissionDenied);
            assert!(
                payload.message.contains("outside your scope"),
                "message names the boundary: {}",
                payload.message
            );
        }
    }

    #[tokio::test]
    async fn get_self_is_visible() {
        let registry = AgentRegistry::shared();
        let me = register_agent(&registry, "/me", None);

        let ctx = ctx_for(infra_keyed(&registry, me, None));
        let out = execute(
            &AgentsTool::new(),
            json!({"action": "get", "agent_id": "/me"}),
            &ctx,
        )
        .await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["agent"]["self"], true);
    }

    // -- wiring -------------------------------------------------------------

    #[tokio::test]
    async fn missing_infra_is_missing_extension() {
        let tool = AgentsTool::new();
        let ctx = ToolContext::empty();
        let err = as_tool(&tool)
            .execute(&envelope_for("agents", json!({"action": "list"})), &ctx)
            .await
            .expect_err("no infra configured");
        match err {
            ToolError::MissingExtension { extension } => {
                assert!(extension.contains("AgentToolInfra"), "{extension}");
            }
            other => panic!("expected MissingExtension, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_action_is_typed_invalid_arguments() {
        let (infra, _registry, _router) = build_infra(Uuid::new_v4());
        let ctx = ctx_for(infra);
        let out = execute(&AgentsTool::new(), json!({"action": "explode"}), &ctx).await;
        assert!(out.is_error());
        let payload = out.error().expect("typed payload");
        assert_eq!(payload.kind, ToolErrorKind::InvalidArguments);
        assert_eq!(
            payload.detail["valid_commands"],
            json!(["list", "get", "messages"])
        );
    }

    // -- messages -----------------------------------------------------------

    use chrono::Utc;

    use crate::r#loop::inbound::MessageKind;
    use crate::provider::agent_event::{
        AGENT_MESSAGE_DELIVERED_EVENT_TYPE, AGENT_MESSAGE_SENT_EVENT_TYPE, AgentMessageLifecycle,
    };
    use crate::session::events::{EventBase, SessionEvent};

    fn append_sent(
        store: &EventStore,
        message_id: Uuid,
        from_id: Uuid,
        from: &str,
        to_id: Uuid,
        to: &str,
        kind: MessageKind,
        seq: u64,
    ) {
        let lifecycle = AgentMessageLifecycle::Sent {
            message_id,
            from_id,
            from: from.to_owned(),
            to_id,
            to: to.to_owned(),
            kind,
            seq,
            content: "status".to_owned(),
            sent_at: Utc::now(),
        };
        store
            .append(SessionEvent::Custom {
                base: EventBase::new(None),
                event_type: AGENT_MESSAGE_SENT_EVENT_TYPE.to_owned(),
                data: serde_json::to_value(&lifecycle).expect("serializable"),
            })
            .expect("append sent");
    }

    fn append_delivered(
        store: &EventStore,
        message_id: Uuid,
        from_id: Uuid,
        to_id: Uuid,
        seq: u64,
    ) {
        let lifecycle = AgentMessageLifecycle::Delivered {
            message_id,
            from_id,
            from: String::new(),
            to_id,
            seq: Some(seq),
            delivered_at: Utc::now(),
        };
        store
            .append(SessionEvent::Custom {
                base: EventBase::new(None),
                event_type: AGENT_MESSAGE_DELIVERED_EVENT_TYPE.to_owned(),
                data: serde_json::to_value(&lifecycle).expect("serializable"),
            })
            .expect("append delivered");
    }

    /// End-to-end through the tool: a child→caller message (granted sent
    /// copy + recipient delivered copy) and a child→child message
    /// (granted sent copy only) render as two honest edges.
    #[tokio::test]
    async fn messages_renders_edges_from_own_audit_store() {
        let (infra, registry, _router) = build_infra(Uuid::new_v4());
        let caller = infra.agent_id;
        let a = register_agent(&registry, "/me/a", Some(caller));
        let b = register_agent(&registry, "/me/b", Some(caller));

        let m1 = Uuid::new_v4();
        append_sent(
            &infra.event_store,
            m1,
            a,
            "/me/a",
            caller,
            "root",
            MessageKind::Steer,
            1,
        );
        append_delivered(&infra.event_store, m1, a, caller, 1);
        append_sent(
            &infra.event_store,
            Uuid::new_v4(),
            a,
            "/me/a",
            b,
            "/me/b",
            MessageKind::Update,
            1,
        );

        let ctx = ctx_for(infra);
        let out = execute(&AgentsTool::new(), json!({"action": "messages"}), &ctx).await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["action"], "messages");
        assert_eq!(out.content["caller_id"], caller.to_string());
        assert_eq!(out.content["count"], 2);
        assert!(
            out.content.get("malformed").is_none(),
            "no malformed key when every event parses: {:?}",
            out.content,
        );

        let edges = out.content["edges"].as_array().expect("edges array");
        let to_caller = edges
            .iter()
            .find(|e| e["to_id"] == caller.to_string())
            .expect("child→caller edge");
        assert_eq!(to_caller["from"], "/me/a");
        assert_eq!(to_caller["sent"], 1);
        assert_eq!(to_caller["kinds"]["steer"], 1);
        assert_eq!(to_caller["delivered"], 1);

        let sibling = edges
            .iter()
            .find(|e| e["to_id"] == b.to_string())
            .expect("child→child edge");
        assert_eq!(sibling["sent"], 1);
        assert_eq!(sibling["kinds"]["update"], 1);
        assert!(
            sibling["delivered"].is_null(),
            "sibling delivery is recorded in the recipient's store, not ours: {sibling}",
        );
    }

    /// An empty audit store answers honestly with zero edges.
    #[tokio::test]
    async fn messages_with_no_audit_events_is_empty() {
        let (infra, _registry, _router) = build_infra(Uuid::new_v4());
        let ctx = ctx_for(infra);
        let out = execute(&AgentsTool::new(), json!({"action": "messages"}), &ctx).await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["count"], 0);
        assert_eq!(out.content["edges"], json!([]));
    }

    /// Unparseable agent_message payloads surface a malformed count in
    /// the output instead of vanishing.
    #[tokio::test]
    async fn messages_surfaces_malformed_count() {
        let (infra, _registry, _router) = build_infra(Uuid::new_v4());
        infra
            .event_store
            .append(SessionEvent::Custom {
                base: EventBase::new(None),
                event_type: AGENT_MESSAGE_SENT_EVENT_TYPE.to_owned(),
                data: json!({ "phase": "bogus" }),
            })
            .expect("append");
        let ctx = ctx_for(infra);
        let out = execute(&AgentsTool::new(), json!({"action": "messages"}), &ctx).await;
        assert!(!out.is_error(), "{:?}", out.content);
        assert_eq!(out.content["count"], 0);
        assert_eq!(out.content["malformed"], 1);
    }
}
