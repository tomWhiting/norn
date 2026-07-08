//! Shared delegation plumbing for the spawn/fork launch paths (W3.4):
//! spawner-policy resolution, per-spawn grant computation, hierarchical
//! auto-path generation, and the per-agent child-result channel.
//!
//! One implementation serves both tools so the budget a spawn enforces
//! and the budget a fork enforces can never drift; the registry
//! re-validates the same invariants from ground truth in
//! [`AgentRegistry::reserve`](crate::agent::registry::AgentRegistry::reserve).

use std::sync::Arc;

use parking_lot::RwLock;
use uuid::Uuid;

use super::infra::AgentToolInfra;
use crate::agent::child_policy::{ChildPolicy, CoordinationEnvelope, MessagingScope};
use crate::agent::registry::AgentRegistry;
use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
use crate::error::ToolError;
use crate::tool::context::ToolContext;
use crate::tool::registry::ToolRegistry;

/// Resolve the spawning agent's **own** granted [`ChildPolicy`].
///
/// Children and forks carry the harness-stamped grant on their
/// [`AgentToolInfra`]; a root agent has no granting parent, so its policy
/// is the builder envelope's `child_policy` — the root's own budget
/// (W3.4: "root uses the builder envelope's policy"). The value is never
/// model-controlled.
pub(super) fn resolve_spawner_policy(
    infra: &AgentToolInfra,
    envelope: &CoordinationEnvelope,
) -> ChildPolicy {
    infra.grant.as_ref().map_or_else(
        || envelope.child_policy.clone(),
        |grant| grant.policy.clone(),
    )
}

/// Compute the [`ChildPolicy`] granted to the child being launched:
/// the optional model-supplied `child_policy` argument validated as a
/// strict narrowing of the spawner's own grant, or the design-specified
/// default derivation (inherit-with-decrement) when omitted — see
/// [`ChildPolicy::grant_for_child`].
///
/// # Errors
///
/// Returns [`ToolError::ExecutionFailed`] carrying the typed
/// [`PolicyNarrowingError`](crate::agent::child_policy::PolicyNarrowingError)
/// text — depth exhaustion or the specific widening violation, each
/// naming the caller's own budget. `surface` is the tool name
/// (`"spawn_agent"` / `"fork"`) for message attribution.
pub(super) fn grant_child_policy(
    spawner_policy: &ChildPolicy,
    requested: Option<ChildPolicy>,
    surface: &str,
) -> Result<ChildPolicy, ToolError> {
    spawner_policy
        .grant_for_child(requested)
        .map_err(|violation| ToolError::ExecutionFailed {
            reason: format!("{surface}: {violation}"),
        })
}

/// Generate the auto path for a child, namespaced under the **spawning
/// agent's** registry path so the agents tree reads as a real tree at
/// every depth (W3.4 path namespacing): `{spawner_path}/{kind}/{uuid}`.
///
/// A spawner with no registry entry (an unregistered root) has no path
/// prefix, so its children land at `/{kind}/{uuid}` — exactly the
/// pre-recursion shape.
/// Run [`SessionBinding::branch_child`] off the async executor.
///
/// The mint performs blocking file I/O — an inter-process index-lock
/// wait plus child-file creation (and an fsync under fsyncing
/// durability policies) — which must never run inline on an executor
/// worker ([`SessionManager`](crate::session::SessionManager)'s
/// documented off-executor rule; the same treatment the write-through
/// sink appends and the spool writes get). On a multi-thread runtime
/// the mint runs inside [`tokio::task::block_in_place`] — the
/// borrowed-data form, matching `append_store_event_off_executor` —
/// and elsewhere (current-thread runtime, where `block_in_place`
/// panics by contract, or no runtime) it runs inline, exactly like the
/// sink writes on those flavors.
///
/// # Errors
///
/// Propagates [`SessionBinding::branch_child`]'s typed errors
/// unchanged.
pub(crate) fn branch_child_off_executor(
    binding: &crate::session::SessionBinding,
    parent_store: &crate::session::store::EventStore,
    request: &crate::session::ChildBranchRequest,
) -> Result<crate::session::BranchedChild, crate::session::SessionPersistError> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| binding.branch_child(parent_store, request))
        }
        _ => binding.branch_child(parent_store, request),
    }
}

pub(crate) fn auto_child_path(
    registry: &Arc<RwLock<AgentRegistry>>,
    spawner_id: Uuid,
    kind: &str,
) -> String {
    let prefix = registry
        .read()
        .get(spawner_id)
        .map(|entry| entry.path)
        .unwrap_or_default();
    format!("{prefix}/{kind}/{}", Uuid::new_v4())
}

/// Compute the tool surface a child is actually shown: the base
/// allowlist intersected with the child's granted [`ChildPolicy`]
/// (brief `agent-variants` R6 — policy WINS over any allowlist, computed
/// at assembly, never discovered at call time).
///
/// `base_allowlist` is the caller's resolved base — explicit `tools`
/// argument, else the variant's tool subset, else the profile's tool
/// list; `None` = the parent's full registry surface. The granted policy
/// then subtracts:
///
/// - `signal_agent` when `granted.messaging == MessagingScope::None`
///   (the single implementation of the strip both spawn and fork
///   previously carried inline);
/// - `spawn_agent` AND `fork` when
///   `granted.delegation.remaining_depth == 0` — a leaf must not SEE
///   delegation tools it can never use ("an `explorer` granted spawn in
///   its allowlist at depth 0 simply doesn't see spawn").
///
/// When a subtraction applies to an absent allowlist, the registry's
/// names are materialised first so the removal is explicit and the
/// result is always a concrete list — the child's tool definitions
/// (`collect_function_definitions`) and its
/// [`SubAgentExecutor`](super::infra::SubAgentExecutor) gate agree by
/// construction. With no subtraction to apply the base passes through
/// unchanged (`None` stays "full surface"). The call-rejection paths
/// (registry budget re-validation, `signal_agent`'s scope refusal)
/// remain as defence-in-depth behind this assembly-level filter.
///
/// Allowlist names absent from the parent registry are `tracing::warn!`ed
/// — naming the tool and the child (`child` is the child's role/variant
/// label, or `"fork"`) — never a hard error: legitimately-absent tools
/// exist (`web_fetch`/`web_search` skip registration when their env
/// construction fails, and the built-in `explorer` variant lists both),
/// so a typo silently narrowing a child and a legitimate skip cannot be
/// told apart here. The warn makes the narrowing observable either way.
pub(crate) fn effective_child_tools(
    parent_registry: &ToolRegistry,
    base_allowlist: Option<Vec<String>>,
    granted: &ChildPolicy,
    child: &str,
) -> Option<Vec<String>> {
    if let Some(names) = base_allowlist.as_ref() {
        for name in names {
            if parent_registry.get(name).is_none() {
                tracing::warn!(
                    tool = %name,
                    child = %child,
                    "child allowlist names a tool absent from the parent \
                     registry; the child will not see it — check the name for \
                     a typo (legitimately-absent tools also land here, e.g. \
                     web_fetch/web_search when their environment construction \
                     failed at registration)",
                );
            }
        }
    }
    let strip_messaging = granted.messaging == MessagingScope::None;
    let strip_delegation = granted.delegation.remaining_depth == 0;
    if !strip_messaging && !strip_delegation {
        return base_allowlist;
    }
    let names =
        base_allowlist.unwrap_or_else(|| parent_registry.names().map(str::to_owned).collect());
    Some(
        names
            .into_iter()
            .filter(|name| {
                if strip_messaging && name == crate::tools::agent::coord::SIGNAL_AGENT_TOOL_NAME {
                    return false;
                }
                if strip_delegation
                    && (name == super::spawn::SPAWN_TOOL_NAME
                        || name == super::fork_tool::FORK_TOOL_NAME)
                {
                    return false;
                }
                true
            })
            .collect(),
    )
}

/// Give a child that can itself delegate its own child-result channel.
///
/// Created iff the child's granted `delegation.remaining_depth >= 1` — a
/// leaf cannot spawn, so it gets no channel (and its spawn/fork calls are
/// refused by the registry budget anyway). The sender is installed as an
/// extension on the **child's** [`ToolContext`] (exactly what
/// `install_agent_infra` does for the root), so the child's own
/// spawn/fork sites deliver grandchild results to *the child*; the
/// returned receiver must be wired onto the child's
/// [`LoopContext::child_result_rx`](crate::agent_loop::loop_context::LoopContext::child_result_rx)
/// so its loop drains them at the existing step boundaries. Results
/// bubble exactly one hop per level — never skipping levels (Wave 3
/// §"Recursive result delivery").
///
/// `capacity` comes from the builder envelope's `child_result_capacity`
/// (DECISION R3) — the same deliberate value at every depth, never a
/// library default.
pub(super) fn install_child_result_channel(
    child_ctx: &ToolContext,
    child_policy: &ChildPolicy,
    capacity: usize,
) -> Option<tokio::sync::mpsc::Receiver<ChildAgentResult>> {
    if child_policy.delegation.remaining_depth == 0 {
        return None;
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<ChildAgentResult>(capacity);
    child_ctx.insert_extension(Arc::new(ChildResultSender(Arc::new(tx))));
    Some(rx)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::agent::child_policy::{DelegationBudget, MessagingScope};

    fn policy(depth: u32) -> ChildPolicy {
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: depth,
                max_concurrent_children: 4,
            },
            inbound_capacity: 8,
            loop_config: None,
        }
    }

    /// A delegating child gets a channel pair: sender installed on its
    /// context, receiver returned for its loop. A leaf gets neither.
    #[test]
    fn result_channel_exists_iff_child_can_delegate() {
        let ctx = ToolContext::empty();
        let rx = install_child_result_channel(&ctx, &policy(1), 16);
        assert!(rx.is_some(), "a depth-1 child drains its own children");
        assert!(
            ctx.get_extension::<ChildResultSender>().is_some(),
            "the sender rides the child's context for its spawn sites",
        );

        let leaf_ctx = ToolContext::empty();
        let leaf_rx = install_child_result_channel(&leaf_ctx, &policy(0), 16);
        assert!(leaf_rx.is_none(), "a leaf cannot spawn — no channel");
        assert!(
            leaf_ctx.get_extension::<ChildResultSender>().is_none(),
            "no sender is installed where nothing may ever send",
        );
    }

    fn policy_with(depth: u32, messaging: MessagingScope) -> ChildPolicy {
        ChildPolicy {
            messaging,
            ..policy(depth)
        }
    }

    fn registry_with(names: &[&'static str]) -> ToolRegistry {
        struct Named(&'static str);
        #[async_trait::async_trait]
        impl crate::tool::traits::Tool for Named {
            fn name(&self) -> &'static str {
                self.0
            }
            fn description(&self) -> &'static str {
                "stub"
            }
            fn input_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            fn effect(&self) -> crate::tool::scheduling::ToolEffect {
                crate::tool::scheduling::ToolEffect::ReadOnly
            }
            async fn execute(
                &self,
                _envelope: &crate::tool::envelope::ToolEnvelope,
                _ctx: &ToolContext,
            ) -> Result<crate::tool::traits::ToolOutput, ToolError> {
                Ok(crate::tool::traits::ToolOutput::success(serde_json::json!(
                    {}
                )))
            }
        }
        let mut registry = ToolRegistry::new();
        for name in names {
            registry.register(Box::new(Named(name)));
        }
        registry
    }

    /// R6: an unrestricted grant passes the base allowlist through
    /// untouched — `None` stays "full parent surface".
    #[test]
    fn effective_child_tools_unrestricted_grant_passes_base_through() {
        let registry = registry_with(&["read", "spawn_agent", "fork", "signal_agent"]);
        let granted = policy(1);
        assert_eq!(
            effective_child_tools(&registry, None, &granted, "worker"),
            None
        );
        assert_eq!(
            effective_child_tools(&registry, Some(vec!["read".to_owned()]), &granted, "worker"),
            Some(vec!["read".to_owned()]),
        );
    }

    /// R6: a leaf grant (`remaining_depth == 0`) strips BOTH delegation
    /// tools — from an explicit allowlist and from a materialised full
    /// surface alike — so the leaf never sees tools it cannot use.
    #[test]
    fn effective_child_tools_leaf_strips_spawn_and_fork() {
        let registry = registry_with(&["read", "spawn_agent", "fork", "signal_agent"]);
        let granted = policy(0);

        let explicit = effective_child_tools(
            &registry,
            Some(vec![
                "read".to_owned(),
                "spawn_agent".to_owned(),
                "fork".to_owned(),
            ]),
            &granted,
            "worker",
        )
        .expect("a restricted grant always yields a concrete list");
        assert_eq!(explicit, vec!["read".to_owned()]);

        let materialised =
            effective_child_tools(&registry, None, &granted, "worker").expect("materialised list");
        assert!(materialised.contains(&"read".to_owned()));
        assert!(materialised.contains(&"signal_agent".to_owned()));
        assert!(!materialised.contains(&"spawn_agent".to_owned()));
        assert!(!materialised.contains(&"fork".to_owned()));
    }

    /// R6 + the centralised messaging strip: `MessagingScope::None`
    /// removes `signal_agent`; combined with a leaf grant all three
    /// gated tools disappear in one pass.
    #[test]
    fn effective_child_tools_messaging_none_strips_signal_agent() {
        let registry = registry_with(&["read", "spawn_agent", "fork", "signal_agent"]);

        let messaging_only = effective_child_tools(
            &registry,
            None,
            &policy_with(1, MessagingScope::None),
            "worker",
        )
        .expect("materialised list");
        assert!(!messaging_only.contains(&"signal_agent".to_owned()));
        assert!(messaging_only.contains(&"spawn_agent".to_owned()));
        assert!(messaging_only.contains(&"fork".to_owned()));

        let both = effective_child_tools(
            &registry,
            None,
            &policy_with(0, MessagingScope::None),
            "worker",
        )
        .expect("materialised list");
        assert_eq!(both, vec!["read".to_owned()]);
    }

    /// F7: an allowlist name absent from the parent registry emits a
    /// `tracing::warn!` naming the tool and the child — never a hard
    /// error — and the surviving set passes through untouched (the
    /// absent name simply matches nothing downstream). Registered names
    /// stay silent.
    #[test]
    fn effective_child_tools_warns_on_absent_allowlist_names() {
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct SharedBuf(Arc<Mutex<Vec<u8>>>);

        impl std::io::Write for SharedBuf {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("buffer lock").write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for SharedBuf {
            type Writer = SharedBuf;
            fn make_writer(&'writer self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = SharedBuf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_writer(buf.clone())
            .with_ansi(false)
            .finish();

        let registry = registry_with(&["read", "search"]);
        let allowlist = vec![
            "read".to_owned(),
            "web_fetch".to_owned(), // legitimately absent (env-gated tool)
            "raed".to_owned(),      // the typo case
        ];
        let out = tracing::subscriber::with_default(subscriber, || {
            effective_child_tools(&registry, Some(allowlist.clone()), &policy(1), "explorer")
        });

        // No error, no silent filtering: the base list survives verbatim.
        assert_eq!(
            out,
            Some(allowlist),
            "absent names must not be dropped or fail"
        );

        let output = String::from_utf8(buf.0.lock().expect("buffer lock").clone())
            .expect("log output is UTF-8");
        for absent in ["web_fetch", "raed"] {
            let line = output
                .lines()
                .find(|line| line.contains(&format!("tool={absent}")))
                .unwrap_or_else(|| panic!("expected a warn for '{absent}', got: {output}"));
            assert!(line.contains("WARN"), "must log at warn: {line}");
            assert!(
                line.contains("child=explorer"),
                "the warn names the child: {line}",
            );
            assert!(
                line.contains("absent from the parent registry"),
                "the warn states the narrowing: {line}",
            );
        }
        assert!(
            !output.contains("tool=read"),
            "registered names must not be warned about: {output}",
        );
    }

    /// The auto path nests under the spawner's registered path; an
    /// unregistered spawner (root) has no prefix.
    #[test]
    fn auto_child_path_namespaces_under_spawner() {
        let registry = AgentRegistry::shared();
        let unregistered = Uuid::new_v4();
        let root_path = auto_child_path(&registry, unregistered, "spawn");
        assert!(
            root_path.starts_with("/spawn/"),
            "unregistered root keeps the flat prefix: {root_path}",
        );

        let guard = AgentRegistry::reserve(
            &registry,
            "/root/worker".to_string(),
            "dev".to_string(),
            "claude".to_string(),
            None,
            policy(2),
            None,
        )
        .expect("register spawner");
        let spawner = guard.id();
        guard.confirm().expect("confirm");
        let nested = auto_child_path(&registry, spawner, "fork");
        assert!(
            nested.starts_with("/root/worker/fork/"),
            "child paths nest under the spawner: {nested}",
        );
    }
}
