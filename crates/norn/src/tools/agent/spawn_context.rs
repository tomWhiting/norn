//! Per-child [`ToolContext`] construction for
//! [`SpawnAgentTool`](super::spawn::SpawnAgentTool).
//!
//! Split from [`super::spawn`] so each file stays inside the per-file
//! 500-line production-code limit; the launch/lifecycle machinery stays
//! in `spawn.rs` while the context-forwarding rules live here.

use std::path::PathBuf;
use std::sync::Arc;

use uuid::Uuid;

use super::handle::{AgentHandles, AgentWakeRegistry};
use super::infra::{AgentCancellation, AgentToolInfra, ParentGrant};
use super::reclaim::ReclaimOnResultDelivery;
use crate::agent::child_policy::{ChildPolicy, CoordinationEnvelope};
use crate::config::permissions::PermissionPolicy;
use crate::error::ToolError;
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::internal::extraction::SharedProvider;
use crate::profile::Profile;
use crate::session::SessionBinding;
use crate::session::action_log::ActionLog;
use crate::session::action_log_tree::ActionLogTree;
use crate::session::store::EventStore;
use crate::tool::catalog::SharedToolCatalog;
use crate::tool::context::{SharedWorkingDir, ToolContext};
use crate::tool::scheduling::ToolEffectIndex;
use crate::tools::diagnostics::{DiagnosticInfra, DiagnosticsPostCheck};
use crate::tools::task::SharedTaskStore;

/// Resolves profile authority independently of the mutable execution CWD.
pub(crate) fn resolve_profile_root(
    ctx: &ToolContext,
    profile_requested: bool,
) -> Result<PathBuf, ToolError> {
    if !profile_requested {
        return Ok(ctx.working_dir());
    }
    Ok(ctx
        .require_extension::<crate::runtime_init::extensions::LaunchWorkingDir>()?
        .0
        .clone())
}

/// Prevents model-selected profiles from invoking ambient user commands.
pub(crate) fn validate_model_selected_profile(profile: &Profile) -> Result<(), ToolError> {
    if profile.prompt_commands.is_empty() {
        return Ok(());
    }
    Err(ToolError::ExecutionFailed {
        reason: "spawn_agent: selected profile declares prompt_commands; model-selected profiles cannot execute ambient commands"
            .to_owned(),
    })
}

/// Construct the per-child [`ToolContext`].
///
/// The child gets a *fresh* [`AgentToolInfra`] carrying its own
/// `agent_id` / `parent_id` and its own [`EventStore`], plus a *fresh*
/// (empty) [`AgentHandles`] so it can spawn grandchildren. The shared
/// infrastructure — [`SharedTaskStore`], [`SharedToolCatalog`],
/// [`DiagnosticCollector`] — is forwarded from the parent context so tasks
/// and tool discovery stay global across the agent tree. The
/// [`crate::agent::message_router::MessageRouter`] is shared by design, so
/// a child's send to its `parent_id` routes through the same router.
///
/// The consent-boundary [`PermissionPolicy`] and the scheduling
/// [`ToolEffectIndex`] are likewise forwarded: the child's agent loop
/// resolves both from *its own* executor's shared context, so omitting
/// them here would let a child evade every deny/ask rule the parent is
/// subject to (and lose effect-based batch scheduling).
///
/// The parent's workspace-confinement root (a plain [`ToolContext`] field,
/// not an extension) is forwarded via
/// [`ToolContext::confine_to_workspace`] for the same reason: the child's
/// file tools check confinement against the *child's* dispatch context, so
/// dropping the root would let a confined parent escape its sandbox simply
/// by spawning a child. The child's working dir is its **own**
/// [`SharedWorkingDir`] handle seeded from the parent's *current* working
/// dir — snapshot semantics, matching [`SharedWorkingDir`]'s documented
/// fork contract: children run concurrently with the parent, so sharing
/// the live handle would let a child's bash `cd` move the parent's (and
/// every sibling's) working dir mid-turn.
///
/// The parent's shared [`HookRegistry`] extension is forwarded so the
/// child's own spawn/fork sites (grandchildren) observe the same operator
/// hooks; the caller separately installs the registry on the child's
/// [`LoopContext`](crate::agent_loop::loop_context::LoopContext) so
/// pre/post-tool hooks fire for the child's own calls.
///
/// `child_session` is the child's own [`SessionBinding`] — minted by the
/// parent's
/// [`SessionBinding::branch_child`](crate::session::SessionBinding::branch_child)
/// alongside the child's store — stamped on the child's
/// [`AgentToolInfra`] so the child's own spawn/fork sites mint
/// grandchildren through the same allocation authority (depth recursion
/// is structural, not per-call).
///
/// `child_policy` is the [`ChildPolicy`] the parent grants this child —
/// computed by the spawn tool from the parent's own grant (narrowed or
/// inherit-with-decrement, W3.4): it is stamped on the child's
/// [`AgentToolInfra`] together with the parent's event store, so
/// `signal_agent` enforces the granted
/// [`MessagingScope`](crate::agent::child_policy::MessagingScope), the
/// dual-store `Sent` audit writes from ground truth, and the child's own
/// spawn/fork sites read *their* budget from the grant. The parent's
/// [`CoordinationEnvelope`] extension is forwarded for the envelope-wide
/// `child_result_capacity` (and the root policy it carries — only a root
/// without a grant ever reads that half).
///
/// The [`ReclaimOnResultDelivery`] marker is forwarded when the parent
/// runs with delivery-anchored reclamation, so grandchild registry
/// entries are reclaimed at every level exactly as depth-1 children are
/// (closing the recorded grandchild-leak gap).
///
/// `child_cancel` is the child's own run-cancellation token — created by
/// the spawn tool as a [`child_token`](tokio_util::sync::CancellationToken::child_token)
/// of the spawner's published [`AgentCancellation`] (or free-standing
/// when the spawner publishes none; see [`AgentCancellation`] for the
/// root boundary). It is published on the child's context here, at
/// construction, so the child's own spawn/fork sites chain grandchild
/// tokens under it — the W3.5 cancellation cascade.
pub(super) fn build_child_context(
    parent_infra: &AgentToolInfra,
    child_id: Uuid,
    child_store: Arc<EventStore>,
    parent_ctx: &ToolContext,
    child_session: Arc<SessionBinding>,
    child_policy: ChildPolicy,
    child_cancel: tokio_util::sync::CancellationToken,
) -> Arc<ToolContext> {
    let child_log_store = Arc::clone(&child_store);
    let child_infra = AgentToolInfra {
        registry: Arc::clone(&parent_infra.registry),
        router: Arc::clone(&parent_infra.router),
        pending_messages: Arc::clone(&parent_infra.pending_messages),
        provider: Arc::clone(&parent_infra.provider),
        event_store: child_store,
        agent_id: child_id,
        parent_id: Some(parent_infra.agent_id),
        grant: Some(ParentGrant {
            policy: child_policy,
            parent_store: Arc::clone(&parent_infra.event_store),
        }),
        tool_registry: parent_infra.tool_registry.as_ref().map(Arc::clone),
        // The child's own branching identity: grandchild mints route
        // through this binding, so depth recursion is structural.
        session: child_session,
    };

    let mut child_ctx =
        ToolContext::with_working_dir(SharedWorkingDir::new(parent_ctx.working_dir()));
    if let Some(root) = parent_ctx.workspace_root() {
        child_ctx.confine_to_workspace(root.to_path_buf());
        // Inherit the parent's read carve-out (already canonicalized) so a
        // confined child can READ the same operator-configured skill /
        // profile / config dirs the parent could — otherwise the skill tool
        // would advertise companion files the child cannot open (DECISIONS
        // §0.6(b)).
        let exempt = parent_ctx.read_exempt_roots().to_vec();
        if !exempt.is_empty() {
            child_ctx.set_read_exempt_roots(exempt);
        }
    }
    child_ctx.insert_extension(Arc::new(child_infra));
    child_ctx.insert_extension(Arc::new(AgentCancellation(child_cancel)));
    child_ctx.insert_extension(Arc::new(AgentHandles::new()));
    forward_shared_extensions(parent_ctx, &mut child_ctx);
    wire_child_action_log(
        parent_infra,
        parent_ctx,
        child_id,
        child_log_store,
        &child_ctx,
    );
    Arc::new(child_ctx)
}

/// Forward the parent's curated shared-infrastructure extensions onto a
/// child context — the SINGLE list every child-context assembly site
/// (spawn's [`build_child_context`], fork's
/// [`build_fork_context`](super::fork_context::build_fork_context), and
/// the rhai script surface's per-child context) uses, so the forwarded
/// set cannot drift between launch paths.
///
/// Covers: the wake registry, shared task store, searchable tool catalog,
/// skill infrastructure (search paths + catalog + immutable workspace root —
/// the child shares the
/// parent's registry, so the `skill` tool is offered to it subject to its
/// allow-list; without both extensions the tool would be offered but
/// always fail `MissingExtension` at execute), the diagnostic collector
/// and convention-diagnostics infra (+ post-check), the shared extraction
/// provider, the private root-session artifact authority, the consent-boundary
/// [`PermissionPolicy`], the scheduling
/// [`ToolEffectIndex`], the operator [`HookRegistry`], the shared
/// agent-event channel, the [`CoordinationEnvelope`], the
/// [`ReclaimOnResultDelivery`] marker, and the agent-variants catalog
/// (forwarded so the child's own spawn/fork sites resolve variants —
/// built-ins included — at every depth; without it a depth-2 spawn naming
/// even a built-in variant fails with the no-catalog error despite the
/// root having a perfectly good catalog).
///
/// Deliberately NOT forwarded here: identity-bearing extensions
/// ([`AgentToolInfra`], [`AgentCancellation`], [`AgentHandles`],
/// `ParentSystemInstruction`,
/// [`AgentModel`](super::infra::AgentModel)) — each launch path
/// publishes its child's OWN values for those.
pub(crate) fn forward_shared_extensions(parent_ctx: &ToolContext, child_ctx: &mut ToolContext) {
    if let Some(wake_registry) = parent_ctx.get_extension::<AgentWakeRegistry>() {
        child_ctx.insert_extension(wake_registry);
    }
    if let Some(task_store) = parent_ctx.get_extension::<SharedTaskStore>() {
        child_ctx.insert_extension(task_store);
    }
    if let Some(catalog) = parent_ctx.get_extension::<SharedToolCatalog>() {
        child_ctx.insert_extension(catalog);
    }
    if let Some(skill_paths) = parent_ctx.get_extension::<crate::tools::skill::SkillSearchPaths>() {
        child_ctx.insert_extension(skill_paths);
    }
    if let Some(skill_catalog) = parent_ctx.get_extension::<crate::skill::SkillCatalog>() {
        child_ctx.insert_extension(skill_catalog);
    }
    if let Some(workspace_root) =
        parent_ctx.get_extension::<crate::tools::skill::WorkspaceSkillRoot>()
    {
        child_ctx.insert_extension(workspace_root);
    }
    if let Some(launch_root) =
        parent_ctx.get_extension::<crate::runtime_init::extensions::LaunchWorkingDir>()
    {
        child_ctx.insert_extension(launch_root);
    }
    if let Some(diagnostics) = parent_ctx.get_extension::<DiagnosticCollector>() {
        child_ctx.insert_extension(diagnostics);
    }
    forward_diagnostic_infra(parent_ctx, child_ctx);
    if let Some(sp) = parent_ctx.get_extension::<SharedProvider>() {
        child_ctx.insert_extension(sp);
    }
    if let Some(artifacts) = parent_ctx.get_extension::<crate::session::SessionArtifactStore>() {
        child_ctx.insert_extension(artifacts);
    }
    if let Some(policy) = parent_ctx.get_extension::<PermissionPolicy>() {
        child_ctx.insert_extension(policy);
    }
    if let Some(effects) = parent_ctx.get_extension::<ToolEffectIndex>() {
        child_ctx.insert_extension(effects);
    }
    if let Some(hooks) = parent_ctx.get_extension::<HookRegistry>() {
        child_ctx.insert_extension(hooks);
    }
    if let Some(ch) =
        parent_ctx.get_extension::<crate::provider::agent_event::SharedAgentEventChannel>()
    {
        child_ctx.insert_extension(ch);
    }
    if let Some(envelope) = parent_ctx.get_extension::<CoordinationEnvelope>() {
        child_ctx.insert_extension(envelope);
    }
    if let Some(marker) = parent_ctx.get_extension::<ReclaimOnResultDelivery>() {
        child_ctx.insert_extension(marker);
    }
    if let Some(variants) = parent_ctx.get_extension::<crate::agent::variants::VariantCatalog>() {
        child_ctx.insert_extension(variants);
    }
    if let Some(runtime) = parent_ctx.get_extension::<crate::integration::McpRuntime>() {
        child_ctx.insert_extension(runtime);
    }
    if let Some(runtimes) = parent_ctx.get_extension::<crate::integration::McpRuntimeStore>() {
        child_ctx.insert_extension(runtimes);
    }
    if let Some(generations) = parent_ctx.get_extension::<crate::tool::ToolGenerationStore>() {
        child_ctx.insert_extension(generations);
    }
    if let Some(view) = parent_ctx.get_extension::<super::live_tools::McpServerView>() {
        child_ctx.insert_extension(view);
    }
}

/// Forward convention diagnostics into a spawned/forked child context.
///
/// [`DiagnosticInfra`] carries the parsed `CONVENTIONS.toml`; the stateless
/// [`DiagnosticsPostCheck`] is installed alongside it so child mutations run
/// the same post-validation path as root mutations.
pub(super) fn forward_diagnostic_infra(parent_ctx: &ToolContext, child_ctx: &mut ToolContext) {
    if let Some(infra) = parent_ctx.get_extension::<DiagnosticInfra>() {
        child_ctx.insert_extension(infra);
        child_ctx.post_checks.push(Box::new(DiagnosticsPostCheck));
    }
}

/// Give a spawn/fork child its own per-agent [`ActionLog`] and register it
/// in the session-wide [`ActionLogTree`].
///
/// The child's log is built over the **child's** event store and the
/// **child's** [`SharedWorkingDir`] handle (so its mutation ledger
/// resolves relative paths against the child's live working dir), then
/// inserted on the child context — fixing the inherited-tool /
/// missing-extension failure where a child's `action_log` calls errored
/// with `MissingExtension`. A fork's log starts empty at the fork point:
/// its seeded conversation is its memory; its action log records what
/// *it* did.
///
/// The [`ActionLogTree`] is fetched from the parent context and forwarded
/// to the child, so the child's own spawn/fork sites register
/// grandchildren into the same tree (and the child can federate over its
/// own subtree — never upward). When the parent context carries no tree —
/// a runtime assembled outside `AgentBuilder`, e.g. `norn-cli`'s
/// `build_runtime` — the tree is installed on the parent now, rooted at
/// the parent agent, with the parent's own log registered when one is
/// published. Spawn and fork are `Process`-effect tools and therefore run
/// serialized within the parent's dispatch loop, so this get-or-install
/// step never races with itself.
pub(super) fn wire_child_action_log(
    parent_infra: &AgentToolInfra,
    parent_ctx: &ToolContext,
    child_id: Uuid,
    child_store: Arc<EventStore>,
    child_ctx: &ToolContext,
) {
    let child_log = Arc::new(ActionLog::with_working_dir(
        child_store,
        child_ctx.shared_working_dir(),
    ));
    child_ctx.insert_extension(Arc::clone(&child_log));

    let log_tree = parent_ctx
        .get_extension::<ActionLogTree>()
        .unwrap_or_else(|| {
            let tree = Arc::new(ActionLogTree::new(parent_infra.agent_id));
            if let Some(parent_log) = parent_ctx.get_extension::<ActionLog>() {
                tree.register(parent_infra.agent_id, None, parent_log);
            }
            parent_ctx.insert_extension(Arc::clone(&tree));
            tree
        });
    log_tree.register(child_id, Some(parent_infra.agent_id), child_log);
    child_ctx.insert_extension(log_tree);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::agent::message_router::MessageRouter;
    use crate::agent::registry::AgentRegistry;
    use crate::agent::variants::VariantCatalog;
    use crate::config::types::VariantSettings;
    use crate::provider::events::{ProviderEvent, StopReason};
    use crate::provider::mock::MockProvider;
    use crate::provider::request::{MessageRole, ProviderRequest};
    use crate::provider::traits::{Provider, ProviderStream};
    use crate::provider::usage::Usage;
    use crate::tool::envelope::ToolEnvelope;
    use crate::tool::registry::ToolRegistry;
    use crate::tool::traits::Tool as _;
    use crate::tools::agent::{AgentHandles, AgentWakeRegistry, ForkTool, SpawnAgentTool};
    use crate::tools::diagnostics::build_diagnostic_infra;
    use futures_util::stream;
    use tempfile::tempdir;

    fn parent_infra(agent_id: Uuid) -> AgentToolInfra {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        AgentToolInfra {
            registry: AgentRegistry::shared(),
            router: Arc::new(MessageRouter::new()),
            pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
            provider,
            event_store: Arc::new(EventStore::new()),
            agent_id,
            parent_id: None,
            grant: None,
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            session: Arc::new(SessionBinding::ephemeral_root()),
        }
    }

    /// Documented-proposal policy used by tests — a deliberate test-caller
    /// choice, never a library default.
    fn test_policy() -> ChildPolicy {
        use crate::agent::child_policy::{DelegationBudget, MessagingScope};
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
            loop_config: None,
        }
    }

    /// Provider-authority sentinel: neither child construction path may
    /// rebuild a provider from a child-selected model or alias. Spawn and fork
    /// must carry the exact parent-owned provider allocation, so only the
    /// trusted root can choose credentials, endpoint, or backend identity.
    #[test]
    fn spawn_and_fork_inherit_exact_parent_provider_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        let parent_id = Uuid::new_v4();
        let infra = parent_infra(parent_id);
        let parent_ctx = ToolContext::empty();
        let artifact_dir = tempdir()?;
        let artifacts = Arc::new(crate::session::SessionArtifactStore::for_session(
            artifact_dir.path(),
            "root-session",
            crate::session::DurabilityPolicy::Flush,
        )?);
        parent_ctx.insert_extension(Arc::clone(&artifacts));

        let spawn_ctx = build_child_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );
        let fork_ctx = super::super::fork_context::build_fork_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );

        let spawn_infra = spawn_ctx
            .get_extension::<AgentToolInfra>()
            .ok_or_else(|| std::io::Error::other("spawn child is missing AgentToolInfra"))?;
        let fork_infra = fork_ctx
            .get_extension::<AgentToolInfra>()
            .ok_or_else(|| std::io::Error::other("fork child is missing AgentToolInfra"))?;
        assert!(Arc::ptr_eq(&spawn_infra.provider, &infra.provider));
        assert!(Arc::ptr_eq(&fork_infra.provider, &infra.provider));
        let spawn_artifacts = spawn_ctx
            .get_extension::<crate::session::SessionArtifactStore>()
            .ok_or_else(|| std::io::Error::other("spawn child artifact authority missing"))?;
        let fork_artifacts = fork_ctx
            .get_extension::<crate::session::SessionArtifactStore>()
            .ok_or_else(|| std::io::Error::other("fork child artifact authority missing"))?;
        assert!(Arc::ptr_eq(&spawn_artifacts, &artifacts));
        assert!(Arc::ptr_eq(&fork_artifacts, &artifacts));
        Ok(())
    }

    struct AuthoritySentinelProvider {
        requests: Arc<parking_lot::Mutex<Vec<ProviderRequest>>>,
    }

    impl Provider for AuthoritySentinelProvider {
        fn stream(
            &self,
            request: ProviderRequest,
        ) -> Result<ProviderStream, crate::error::ProviderError> {
            self.requests.lock().push(request);
            Ok(Box::pin(stream::iter([Ok(ProviderEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                response_id: None,
            })])))
        }
    }

    fn request_contains_prompt(request: &ProviderRequest, prompt: &str) -> bool {
        request.messages.iter().any(|message| {
            message.role == MessageRole::User
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains(prompt))
        })
    }

    /// Real-entry authority fence for SEC-08A: a user-tier alias carrying a
    /// provider profile, hostile endpoint, and API-key environment variable is
    /// inert inside variant, profile, and explicit-model child entry paths.
    /// Every dispatched request must use the exact root-owned provider, and the
    /// hostile endpoint must remain untouched.
    #[tokio::test]
    #[serial_test::serial]
    async fn child_entry_paths_cannot_reinterpret_model_as_backend_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        let hostile = wiremock::MockServer::start().await;
        let norn_home = tempfile::tempdir()?;
        let settings = serde_json::json!({
            "model_aliases": {
                "sol": {
                    "provider_profile": "hostile-child-backend",
                    "api_shape": "openai_chat_completions",
                    "model": "gpt-5.6-sol"
                }
            },
            "provider_profiles": {
                "hostile-child-backend": {
                    "api_shape": "openai_chat_completions",
                    "base_url": hostile.uri(),
                    "api_key_env": "NORN_CHILD_AUTHORITY_SENTINEL_KEY"
                }
            }
        });
        std::fs::write(
            norn_home.path().join("settings.json"),
            serde_json::to_vec(&settings)?,
        )?;
        let launch_root = tempfile::tempdir()?;
        let profile_directory = launch_root.path().join(".norn/profiles");
        std::fs::create_dir_all(&profile_directory)?;
        std::fs::write(
            profile_directory.join("authority-sentinel.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": "authority-sentinel",
                "model": "sol",
                "system_instructions": ["Use only the inherited provider."]
            }))?,
        )?;
        let sol_window = crate::model_catalog::smallest_context_window_for_model("gpt-5.6-sol")
            .ok_or_else(|| std::io::Error::other("gpt-5.6-sol is missing from the catalog"))?;
        let gpt_55_window = crate::model_catalog::smallest_context_window_for_model("gpt-5.5")
            .ok_or_else(|| std::io::Error::other("gpt-5.5 is missing from the catalog"))?;
        let child_context_window = sol_window.min(gpt_55_window);

        temp_env::async_with_vars(
            [
                ("NORN_HOME", Some(norn_home.path().as_os_str())),
                (
                    "NORN_CHILD_AUTHORITY_SENTINEL_KEY",
                    Some(std::ffi::OsStr::new("must-never-authenticate-child")),
                ),
            ],
            async {
                let requests = Arc::new(parking_lot::Mutex::new(Vec::new()));
                let provider: Arc<dyn Provider> = Arc::new(AuthoritySentinelProvider {
                    requests: Arc::clone(&requests),
                });
                let parent_id = Uuid::new_v4();
                let infra = Arc::new(AgentToolInfra {
                    registry: AgentRegistry::shared(),
                    router: Arc::new(MessageRouter::new()),
                    pending_messages: Arc::new(crate::agent::PendingAgentMessages::new()),
                    provider,
                    event_store: Arc::new(EventStore::new()),
                    agent_id: parent_id,
                    parent_id: None,
                    grant: None,
                    tool_registry: Some(Arc::new(ToolRegistry::new())),
                    session: Arc::new(SessionBinding::ephemeral_root()),
                });
                let parent_ctx = ToolContext::empty();
                parent_ctx.insert_extension(infra);
                parent_ctx.insert_extension(Arc::new(AgentHandles::new()));
                parent_ctx.insert_extension(Arc::new(AgentWakeRegistry::new()));
                parent_ctx.insert_extension(Arc::new(
                    crate::runtime_init::extensions::LaunchWorkingDir(
                        launch_root.path().canonicalize()?,
                    ),
                ));
                let mut authority_policy = test_policy();
                authority_policy.loop_config = Some(crate::agent::child_policy::ChildLoopConfig {
                    step_timeout_secs: None,
                    linger_secs: None,
                    context_window: Some(child_context_window),
                });
                parent_ctx.insert_extension(Arc::new(CoordinationEnvelope {
                    child_policy: authority_policy,
                    child_result_capacity: 256,
                }));

                let mut variants = BTreeMap::new();
                variants.insert(
                    "authority-sentinel".to_owned(),
                    VariantSettings {
                        prompt: Some("Use only the inherited provider.".to_owned()),
                        model: Some("sol".to_owned()),
                        ..VariantSettings::default()
                    },
                );
                let catalog = VariantCatalog::build(Some(&variants), norn_home.path())?;
                parent_ctx.insert_extension(Arc::new(catalog));

                SpawnAgentTool::new()
                    .execute(
                        &ToolEnvelope {
                            tool_call_id: "spawn-authority-sentinel".to_owned(),
                            tool_name: "spawn_agent".to_owned(),
                            model_args: serde_json::json!({
                                "task": "spawn authority sentinel",
                                "variant": "authority-sentinel"
                            }),
                            metadata: serde_json::Value::Null,
                        },
                        &parent_ctx,
                    )
                    .await?;
                SpawnAgentTool::new()
                    .execute(
                        &ToolEnvelope {
                            tool_call_id: "profile-authority-sentinel".to_owned(),
                            tool_name: "spawn_agent".to_owned(),
                            model_args: serde_json::json!({
                                "task": "profile authority sentinel",
                                "profile": "authority-sentinel",
                                "role": "profile-authority-sentinel",
                                "model": "gpt-5.5"
                            }),
                            metadata: serde_json::Value::Null,
                        },
                        &parent_ctx,
                    )
                    .await?;
                ForkTool::new()
                    .execute(
                        &ToolEnvelope {
                            tool_call_id: "fork-authority-sentinel".to_owned(),
                            tool_name: "fork".to_owned(),
                            model_args: serde_json::json!({
                                "request": "fork authority sentinel",
                                "model": "sol",
                                "requirements": []
                            }),
                            metadata: serde_json::Value::Null,
                        },
                        &parent_ctx,
                    )
                    .await?;

                let expected_paths = [
                    ("spawn authority sentinel", "sol"),
                    ("profile authority sentinel", "gpt-5.5"),
                    ("fork authority sentinel", "sol"),
                ];
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    while {
                        let captured = requests.lock();
                        expected_paths.iter().any(|(prompt, _)| {
                            !captured
                                .iter()
                                .any(|request| request_contains_prompt(request, prompt))
                        })
                    } {
                        tokio::task::yield_now().await;
                    }
                })
                .await?;
                {
                    let captured = requests.lock();
                    for (prompt, expected_model) in expected_paths {
                        assert!(captured.iter().any(|request| {
                            request.model == expected_model
                                && request_contains_prompt(request, prompt)
                        }));
                    }
                    for request in captured.iter() {
                        let expected_model = expected_paths.iter().find_map(|(prompt, model)| {
                            request_contains_prompt(request, prompt).then_some(*model)
                        });
                        assert_eq!(expected_model, Some(request.model.as_str()));
                    }
                }

                let hostile_requests = hostile.received_requests().await.ok_or_else(|| {
                    std::io::Error::other("wiremock request recording is unavailable")
                })?;
                assert!(
                    hostile_requests.is_empty(),
                    "child authority escaped to the user-tier provider endpoint",
                );
                Ok::<(), Box<dyn std::error::Error>>(())
            },
        )
        .await
    }

    /// The child context carries its own [`ActionLog`] (the production
    /// regression: children previously had none and every `action_log`
    /// call failed `MissingExtension`), registered in the shared
    /// [`ActionLogTree`] under the parent, with the tree forwarded to the
    /// child.
    #[test]
    fn build_child_context_installs_child_log_and_registers_in_tree() {
        let parent_id = Uuid::new_v4();
        let infra = parent_infra(parent_id);
        let parent_ctx = ToolContext::empty();
        let parent_log = Arc::new(crate::session::action_log::ActionLog::new(Arc::new(
            EventStore::new(),
        )));
        parent_ctx.insert_extension(Arc::clone(&parent_log));

        let child_id = Uuid::new_v4();
        let child_ctx = build_child_context(
            &infra,
            child_id,
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );

        let child_log = child_ctx
            .get_extension::<crate::session::action_log::ActionLog>()
            .expect("the child must carry its own ActionLog extension");
        assert!(
            !Arc::ptr_eq(&child_log, &parent_log),
            "the child's log is per-agent, never the parent's instance",
        );

        // The tree was lazily installed on the parent, rooted at the
        // parent, with both logs registered and the parent→child edge.
        let tree = parent_ctx
            .get_extension::<ActionLogTree>()
            .expect("tree installed on the parent context");
        assert_eq!(tree.root(), parent_id);
        assert!(Arc::ptr_eq(
            &tree.log_of(parent_id).expect("root log"),
            &parent_log
        ));
        assert!(Arc::ptr_eq(
            &tree.log_of(child_id).expect("child log"),
            &child_log
        ));
        assert_eq!(tree.children_of(parent_id), vec![child_id]);

        // Forwarded: the child shares the same tree instance, so
        // grandchildren register into the same session-wide tree.
        let child_tree = child_ctx
            .get_extension::<ActionLogTree>()
            .expect("tree forwarded to the child context");
        assert!(Arc::ptr_eq(&child_tree, &tree));
    }

    /// DECISIONS §0.6(b): a spawned child inherits the parent's read
    /// carve-out, so under inherited confinement it can READ a file inside a
    /// parent-declared exempt dir that lies outside the workspace root —
    /// end-to-end through the read tool, not merely by field inspection.
    #[tokio::test]
    async fn child_inherits_read_exemption_and_reads_exempt_file() {
        use crate::tool::envelope::ToolEnvelope;
        use crate::tool::traits::Tool;
        use crate::tools::read::ReadTool;

        let outer = tempdir().expect("outer");
        let root = outer.path().join("ws");
        let skills = outer.path().join("home-skills");
        std::fs::create_dir(&root).expect("mkdir ws");
        std::fs::create_dir(&skills).expect("mkdir skills");
        let companion = skills.join("SKILL.md");
        std::fs::write(&companion, "name: demo\n").expect("write companion");

        // Confined parent that carries the exempt root.
        let mut parent_ctx = ToolContext::with_working_dir(SharedWorkingDir::new(root.clone()));
        parent_ctx.confine_to_workspace(root.clone());
        parent_ctx.set_read_exempt_roots(vec![skills.clone()]);

        let parent_id = Uuid::new_v4();
        let infra = parent_infra(parent_id);
        let child_ctx = build_child_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );

        // The child inherited the (canonicalized) exempt root.
        assert_eq!(
            child_ctx.read_exempt_roots(),
            parent_ctx.read_exempt_roots(),
            "child must inherit the parent's canonicalized exempt roots",
        );

        // End-to-end: the child reads the exempt companion despite being
        // confined to `root`.
        let tool = ReadTool::new();
        let env = ToolEnvelope {
            tool_call_id: "call-1".to_owned(),
            tool_name: "read".to_owned(),
            model_args: serde_json::json!({ "path": companion.to_string_lossy() }),
            metadata: serde_json::Value::Null,
        };
        let out = tool.execute(&env, &child_ctx).await.expect("read output");
        assert!(
            !out.is_error(),
            "child must read the inherited-exempt file: {:?}",
            out.content
        );
        assert_eq!(out.content["kind"], "text");

        // A non-exempt sibling outside the root stays refused for the child.
        let secret = outer.path().join("secret.txt");
        std::fs::write(&secret, "s").expect("write secret");
        let refused_env = ToolEnvelope {
            tool_call_id: "call-2".to_owned(),
            tool_name: "read".to_owned(),
            model_args: serde_json::json!({ "path": secret.to_string_lossy() }),
            metadata: serde_json::Value::Null,
        };
        let refused = tool
            .execute(&refused_env, &child_ctx)
            .await
            .expect("refusal output");
        assert!(
            refused.is_error(),
            "non-exempt outside path must be refused"
        );
        assert_eq!(refused.content["kind"], "confinement_refused");
    }

    /// A second child reuses the already-installed tree — both children
    /// hang off the same root.
    #[test]
    fn second_child_registers_into_the_same_tree() {
        let parent_id = Uuid::new_v4();
        let infra = parent_infra(parent_id);
        let parent_ctx = ToolContext::empty();
        parent_ctx.insert_extension(Arc::new(crate::session::action_log::ActionLog::new(
            Arc::new(EventStore::new()),
        )));

        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let _c1 = build_child_context(
            &infra,
            first,
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );
        let tree_after_first = parent_ctx.get_extension::<ActionLogTree>().expect("tree");
        let _c2 = build_child_context(
            &infra,
            second,
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );
        let tree_after_second = parent_ctx.get_extension::<ActionLogTree>().expect("tree");

        assert!(
            Arc::ptr_eq(&tree_after_first, &tree_after_second),
            "the second child must reuse the installed tree, not replace it",
        );
        assert_eq!(
            tree_after_second.children_of(parent_id),
            vec![first, second]
        );
    }

    /// A parent context with no [`ActionLog`] of its own (assembled
    /// outside `AgentBuilder`) still anchors the tree at the parent: the
    /// child registers and is reachable; the root simply has no log.
    #[test]
    fn child_registers_even_when_parent_has_no_log() {
        let parent_id = Uuid::new_v4();
        let infra = parent_infra(parent_id);
        let parent_ctx = ToolContext::empty();

        let child_id = Uuid::new_v4();
        let _child_ctx = build_child_context(
            &infra,
            child_id,
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );

        let tree = parent_ctx.get_extension::<ActionLogTree>().expect("tree");
        assert_eq!(tree.root(), parent_id);
        assert!(
            tree.log_of(parent_id).is_none(),
            "no parent log to register"
        );
        assert!(tree.log_of(child_id).is_some(), "child log registered");
        assert_eq!(tree.children_of(parent_id), vec![child_id]);
    }

    /// W3.5: the child's run-cancellation token is published on the
    /// child context as an [`AgentCancellation`] extension *at
    /// construction* — even when the parent context publishes none
    /// (token-less embedder roots) — so the child's own spawn/fork
    /// sites always have a token to chain grandchild tokens under.
    #[test]
    fn child_context_publishes_the_passed_cancellation_token() {
        let infra = parent_infra(Uuid::new_v4());
        let parent_ctx = ToolContext::empty();
        assert!(
            parent_ctx.get_extension::<AgentCancellation>().is_none(),
            "this parent deliberately publishes no token (root boundary)",
        );

        let child_cancel = tokio_util::sync::CancellationToken::new();
        let child_ctx = build_child_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            child_cancel.clone(),
        );

        let published = child_ctx
            .get_extension::<AgentCancellation>()
            .expect("the child context must publish its own AgentCancellation");
        assert!(!published.0.is_cancelled());
        child_cancel.cancel();
        assert!(
            published.0.is_cancelled(),
            "the published extension must be the same token the launch path uses",
        );
    }

    /// Agent-variants forwarding regression (the depth-2 case): the
    /// parent's [`crate::agent::variants::VariantCatalog`] extension is
    /// forwarded to the child context, so a built-in variant resolves
    /// FROM THE CHILD's ctx with zero `variants` settings — a grandchild
    /// spawn naming `explorer` must not fail with the no-catalog error.
    #[test]
    fn child_context_forwards_variant_catalog_and_resolves_builtins() {
        use crate::agent::variants::VariantCatalog;

        let infra = parent_infra(Uuid::new_v4());
        let parent_ctx = ToolContext::empty();
        let catalog = Arc::new(
            VariantCatalog::build(None, &std::env::temp_dir())
                .expect("built-in catalog builds with zero settings"),
        );
        parent_ctx.insert_extension(Arc::clone(&catalog));

        let child_ctx = build_child_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );

        let forwarded = child_ctx
            .get_extension::<VariantCatalog>()
            .expect("the child context must inherit the parent's variant catalog");
        assert!(
            Arc::ptr_eq(&forwarded, &catalog),
            "the child shares the parent's catalog instance, never a rebuild",
        );

        // Resolve a built-in variant through the real spawn-resolution
        // path, exactly as the child's own spawn site would.
        let resolved = super::super::variant_resolve::resolve_spawn(
            super::super::variant_resolve::SpawnIdentityArgs {
                variant: Some("explorer".to_owned()),
                profile: None,
                role: None,
                model: None,
            },
            child_ctx.get_extension::<VariantCatalog>().as_deref(),
            "spawn_agent",
            || Ok("parent-model".to_owned()),
        )
        .expect("a built-in variant must resolve from the child context");
        assert_eq!(resolved.role, "explorer");
        assert_eq!(resolved.variant_name.as_deref(), Some("explorer"));
    }

    #[test]
    fn child_context_forwards_immutable_workspace_skill_root() -> Result<(), String> {
        let infra = parent_infra(Uuid::new_v4());
        let parent_ctx = ToolContext::empty();
        let root = Arc::new(crate::tools::skill::WorkspaceSkillRoot(
            std::path::PathBuf::from("/workspace-authority-root"),
        ));
        parent_ctx.insert_extension(Arc::clone(&root));

        let child_ctx = build_child_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );

        let forwarded = child_ctx
            .get_extension::<crate::tools::skill::WorkspaceSkillRoot>()
            .ok_or_else(|| "workspace skill authority root was not forwarded".to_owned())?;
        assert!(Arc::ptr_eq(&forwarded, &root));
        Ok(())
    }

    #[test]
    fn child_context_forwards_diagnostic_infra_and_post_check() {
        let dir = tempdir().expect("temp dir");
        let diagnostic_infra = Arc::new(build_diagnostic_infra(dir.path(), None, None));
        let infra = parent_infra(Uuid::new_v4());
        let parent_ctx = ToolContext::empty();
        parent_ctx.insert_extension(Arc::clone(&diagnostic_infra));

        let child_ctx = build_child_context(
            &infra,
            Uuid::new_v4(),
            Arc::new(EventStore::new()),
            &parent_ctx,
            Arc::new(SessionBinding::ephemeral_root()),
            test_policy(),
            tokio_util::sync::CancellationToken::new(),
        );

        let forwarded = child_ctx
            .get_extension::<DiagnosticInfra>()
            .expect("child must inherit DiagnosticInfra");
        assert!(
            Arc::ptr_eq(&forwarded, &diagnostic_infra),
            "spawned agents must share the parent's diagnostic infrastructure",
        );
        assert_eq!(
            child_ctx.post_checks.len(),
            1,
            "spawned agents must install the diagnostics post-check",
        );
    }
}
