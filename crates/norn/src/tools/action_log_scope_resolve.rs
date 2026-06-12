//! Scope resolution and labeling for the `action_log` tool.
//!
//! Hoisted from [`crate::tools::action_log`]: parsing the model-supplied
//! `scope` argument, resolving a scope identifier (registry path or UUID)
//! to an agent id with existence checking, enforcing the subtree query
//! boundary, and resolving registry-ground-truth labels for federated
//! output. Query execution stays in `tools::action_log`; the pure
//! federation layer (timeline merging, cross-log look-ups) lives in
//! [`crate::session::action_log_scope`].

use std::sync::Arc;

use uuid::Uuid;

use crate::session::action_log::ActionLog;
use crate::session::action_log_scope::ScopedLog;
use crate::session::action_log_tree::ActionLogTree;
use crate::tool::context::ToolContext;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::traits::ToolOutput;
use crate::tools::agent::AgentToolInfra;

/// Parsed form of the `scope` argument.
pub(crate) enum Scope {
    /// The caller's own log only — the default, identical to the
    /// pre-scope behaviour.
    SelfOnly,
    /// The caller plus its direct children.
    Children,
    /// The caller's entire subtree.
    All,
    /// One specific agent, named by registry path or UUID.
    Agent(String),
}

/// Parse the raw model-supplied `scope` string into a [`Scope`].
pub(crate) fn parse_scope(raw: Option<&str>) -> Scope {
    match raw {
        None | Some("self") => Scope::SelfOnly,
        Some("children") => Scope::Children,
        Some("all") => Scope::All,
        Some(other) => Scope::Agent(other.to_owned()),
    }
}

/// Resolve the model-facing label (and role, when set) for `id`.
///
/// Registry ground truth: `"root"` for the tree's root agent, the registry
/// path for any registered agent (live, terminal, or reclaimed-with-
/// tombstone), and the bare UUID when no record exists. The role is
/// surfaced only when a non-empty one is actually set on the live entry —
/// tombstones do not retain roles.
fn label_for(
    id: Uuid,
    infra: Option<&Arc<AgentToolInfra>>,
    tree: Option<&Arc<ActionLogTree>>,
) -> (String, Option<String>) {
    let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };
    let is_root = match tree {
        Some(tree) => tree.root() == id,
        // Without a tree the only id ever labeled is the caller itself;
        // it is the root exactly when it has no parent (or no sub-agent
        // runtime at all).
        None => match infra {
            Some(infra) => infra.agent_id == id && infra.parent_id.is_none(),
            None => true,
        },
    };
    if is_root {
        let role = infra
            .and_then(|i| i.registry.read().get(id))
            .and_then(|entry| non_empty(entry.role));
        return ("root".to_owned(), role);
    }
    if let Some(infra) = infra {
        let registry = infra.registry.read();
        if let Some(entry) = registry.get(id) {
            return (entry.path, non_empty(entry.role));
        }
        if let Some(tombstone) = registry.tombstone(id) {
            return (tombstone.path, None);
        }
    }
    (id.to_string(), None)
}

/// Resolve a scope identifier (registry path or UUID) to an agent id, or
/// `None` when no such agent has ever existed in this session.
///
/// Probe order matches `resolve_agent` in `tools::agent::infra` — path
/// lookups (live → terminal → tombstone) before the UUID parse — so the
/// two tools can never disagree on a UUID-shaped path. A parseable UUID
/// resolves only when the agent verifiably exists: it is the caller
/// itself, it has a registry record (live, terminal, or tombstoned), or
/// it has a log registered in the [`ActionLogTree`] (covering
/// tree-registered agents with no registry record, e.g. descendants of
/// descendants). Without that existence check any garbage UUID would
/// fall through to the subtree boundary check and be misreported as
/// `permission_denied` — implying an agent that never existed does.
fn resolve_identifier(
    ident: &str,
    own_id: Option<Uuid>,
    infra: Option<&Arc<AgentToolInfra>>,
    tree: Option<&Arc<ActionLogTree>>,
) -> Option<Uuid> {
    if let Some(infra) = infra {
        let registry = infra.registry.read();
        if let Some(entry) = registry.get_by_path(ident) {
            return Some(entry.id);
        }
        if let Some(entry) = registry.get_terminal_by_path(ident) {
            return Some(entry.id);
        }
        if let Some(tombstone) = registry.tombstone_by_path(ident) {
            return Some(tombstone.id);
        }
    }
    let id = Uuid::parse_str(ident).ok()?;
    let exists = own_id == Some(id)
        || infra.is_some_and(|infra| {
            let registry = infra.registry.read();
            registry.get(id).is_some() || registry.tombstone(id).is_some()
        })
        || tree.is_some_and(|tree| tree.log_of(id).is_some());
    exists.then_some(id)
}

/// Resolve the queried scope into the ordered list of logs (the caller
/// first, then descendants in tree preorder), or a structured error
/// output when the identifier is unknown or outside the caller's subtree.
///
/// A context without an [`ActionLogTree`] (or without agent infrastructure
/// at all) has no descendants by construction — `children` / `all` then
/// truthfully resolve to the caller alone.
pub(crate) fn resolve_scoped_logs(
    scope: &Scope,
    own_log: Arc<ActionLog>,
    ctx: &ToolContext,
) -> Result<Vec<ScopedLog>, Box<ToolOutput>> {
    let infra = ctx.get_extension::<AgentToolInfra>();
    let tree = ctx.get_extension::<ActionLogTree>();
    let own_id = infra.as_ref().map(|i| i.agent_id);

    let scoped_for = |id: Uuid, log: Arc<ActionLog>| {
        let (label, role) = label_for(id, infra.as_ref(), tree.as_ref());
        ScopedLog {
            agent_id: Some(id),
            label,
            role,
            log,
        }
    };
    let self_scoped = |log: Arc<ActionLog>| match own_id {
        Some(id) => scoped_for(id, log),
        None => ScopedLog {
            agent_id: None,
            label: "root".to_owned(),
            role: None,
            log,
        },
    };

    match scope {
        Scope::SelfOnly => Ok(vec![self_scoped(own_log)]),
        Scope::Children | Scope::All => {
            let mut scoped = vec![self_scoped(own_log)];
            if let (Some(own), Some(tree)) = (own_id, tree.as_ref()) {
                let ids = match scope {
                    Scope::Children => tree.children_of(own),
                    _ => tree.descendants_of(own),
                };
                for id in ids {
                    if let Some(log) = tree.log_of(id) {
                        scoped.push(scoped_for(id, log));
                    } else {
                        // Edges are only ever created alongside a log
                        // (ActionLogTree::register), so this indicates a
                        // wiring bug; the rest of the scope still answers.
                        tracing::warn!(
                            agent_id = %id,
                            "action_log scope: tree edge without a registered log",
                        );
                    }
                }
            }
            Ok(scoped)
        }
        Scope::Agent(ident) => {
            let Some(target) = resolve_identifier(ident, own_id, infra.as_ref(), tree.as_ref())
            else {
                return Err(Box::new(ToolOutput::failure(
                    ToolErrorPayload::new(
                        ToolErrorKind::NotFound,
                        format!(
                            "agent '{ident}' could not be resolved by path or UUID in this session"
                        ),
                    )
                    .with_detail(serde_json::json!({ "scope": ident })),
                )));
            };
            if own_id == Some(target) {
                return Ok(vec![self_scoped(own_log)]);
            }
            let in_subtree = match (own_id, tree.as_ref()) {
                (Some(own), Some(tree)) => tree.is_in_subtree(own, target),
                // No identity or no tree: the caller has no descendants,
                // so any non-self target is outside its subtree.
                _ => false,
            };
            if !in_subtree {
                return Err(Box::new(ToolOutput::failure(
                    ToolErrorPayload::new(
                        ToolErrorKind::PermissionDenied,
                        format!(
                            "agent '{ident}' is not this agent or one of its descendants; \
                             action_log scope is limited to the caller's own subtree"
                        ),
                    )
                    .with_detail(serde_json::json!({ "scope": ident })),
                )));
            }
            let Some(tree) = tree.as_ref() else {
                // Unreachable: in_subtree above is false without a tree.
                return Err(Box::new(ToolOutput::failure(ToolErrorPayload::new(
                    ToolErrorKind::NotFound,
                    "no action-log tree is installed in this context",
                ))));
            };
            match tree.log_of(target) {
                Some(log) => Ok(vec![scoped_for(target, log)]),
                None => Err(Box::new(ToolOutput::failure(
                    ToolErrorPayload::new(
                        ToolErrorKind::NotFound,
                        format!("no action log is recorded for agent '{ident}'"),
                    )
                    .with_detail(serde_json::json!({ "scope": ident })),
                ))),
            }
        }
    }
}

/// The per-agent legend on federated responses: label, id (when known),
/// and role (only when actually set).
pub(crate) fn agents_legend(scoped: &[ScopedLog]) -> Vec<serde_json::Value> {
    scoped
        .iter()
        .map(|s| {
            let mut value = serde_json::json!({ "agent": s.label });
            if let Some(id) = s.agent_id {
                value["id"] = serde_json::json!(id.to_string());
            }
            if let Some(role) = &s.role {
                value["role"] = serde_json::json!(role);
            }
            value
        })
        .collect()
}
