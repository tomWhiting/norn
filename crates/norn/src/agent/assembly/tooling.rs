//! Shared tool context and agent-coordination wiring.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::agent::message_router::MessageRouter;
use crate::agent::pending_messages::PendingAgentMessages;
use crate::agent::result_channel::{ChildAgentResult, ChildResultSender};
use crate::error::SessionError;
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::internal::extraction::SharedProvider;
use crate::provider::request::ToolDefinition;
use crate::provider::surface::collect_function_definitions;
use crate::provider::traits::Provider;
use crate::session::action_log::ActionLog;
use crate::session::action_log_tree::ActionLogTree;
use crate::tool::catalog::{SharedToolCatalog, ToolCatalogEntry, ToolCatalogExtras};
use crate::tool::context::{SessionId, SharedWorkingDir, ToolContext};
use crate::tool::lifecycle::RuntimePostValidateCheck;
use crate::tool::registry::ToolRegistry;
use crate::tool::traits::Tool;
use crate::tools::agent::AgentToolInfra;
use crate::tools::diagnostics::{DiagnosticInfra, DiagnosticsPostCheck};

use super::runtime::AgentInfraParts;

/// A deferred installer that publishes a typed extension on the agent's
/// shared [`ToolContext`] at build time. Stored by
/// [`AgentBuilder::extension`](crate::agent::builder::AgentBuilder::extension)
/// and run during [`assemble_tool_context`].
pub(crate) type ExtensionInstaller = Box<dyn FnOnce(&ToolContext) + Send>;

/// Parts for [`assemble_tool_context`]; every field is consumed into the
/// assembled context.
pub(crate) struct ToolContextParts {
    /// Shared working-dir handle (same handle as the loop context's).
    pub(crate) shared_wd: SharedWorkingDir,
    /// Validated workspace-confinement root, when configured.
    pub(crate) workspace_root: Option<PathBuf>,
    /// Directories a confined agent may read outside the confinement root.
    pub(crate) read_exempt_roots: Vec<PathBuf>,
    /// Session id minted by the variable store.
    pub(crate) session_id: String,
    /// Resolved diagnostic collector.
    pub(crate) diagnostics: Option<Arc<DiagnosticCollector>>,
    /// Resolved diagnostic infrastructure; installs its post-check.
    pub(crate) diagnostic_infra: Option<Arc<DiagnosticInfra>>,
    /// Final merged hook registry shared with sub-agent tools.
    pub(crate) hooks: Option<Arc<HookRegistry>>,
    /// Caller-supplied post-validation checks.
    pub(crate) post_checks: Vec<Box<dyn RuntimePostValidateCheck>>,
    /// Provider published for internal extraction agents.
    pub(crate) provider: Arc<dyn Provider>,
    /// Shared action log.
    pub(crate) action_log: Arc<ActionLog>,
    /// Effective context window used for tool-output caps.
    pub(crate) context_window_limit: Option<u64>,
    /// Private artifact authority for a managed persisted session.
    pub(crate) artifact_store: Option<Arc<crate::session::SessionArtifactStore>>,
    /// Consumer-supplied extension installers, run last.
    pub(crate) extensions: Vec<ExtensionInstaller>,
}

/// Assemble the agent's shared [`ToolContext`].
pub(crate) fn assemble_tool_context(parts: ToolContextParts) -> ToolContext {
    let launch_working_dir = parts.shared_wd.get();
    let mut ctx = ToolContext::with_working_dir(parts.shared_wd);
    let artifact_store = parts.artifact_store;
    let mut read_exempt_roots = parts.read_exempt_roots;
    if let Some(store) = artifact_store.as_ref() {
        read_exempt_roots.push(store.readable_root());
    }
    ctx.insert_extension(Arc::new(crate::runtime_init::extensions::LaunchWorkingDir(
        launch_working_dir,
    )));
    if let Some(root) = parts.workspace_root {
        ctx.confine_to_workspace(root);
        if !read_exempt_roots.is_empty() {
            ctx.set_read_exempt_roots(read_exempt_roots);
        }
    }
    ctx.insert_extension(Arc::new(SessionId(parts.session_id)));
    if let Some(diagnostics) = parts.diagnostics {
        ctx.insert_extension(diagnostics);
    }
    if let Some(infra) = parts.diagnostic_infra {
        ctx.insert_extension(infra);
        ctx.post_checks.push(Box::new(DiagnosticsPostCheck));
    }
    if let Some(hooks) = parts.hooks {
        ctx.insert_extension(hooks);
    }
    ctx.post_checks.extend(parts.post_checks);
    crate::runtime_init::install_tool_output_budget(&ctx, parts.context_window_limit);
    if let Some(store) = artifact_store {
        ctx.insert_extension(store);
    }
    ctx.insert_extension(Arc::new(SharedProvider(parts.provider)));
    ctx.insert_extension(parts.action_log);
    for install in parts.extensions {
        install(&ctx);
    }
    ctx
}

/// Publish the registry tools plus consumer extras on `ctx`.
pub(crate) fn install_tool_catalog(registry: &ToolRegistry, ctx: &ToolContext) {
    let mut entries: Vec<ToolCatalogEntry> = registry
        .names()
        .filter_map(|name| registry.get(name))
        .flat_map(Tool::catalog_entries)
        .collect();

    if let Some(extras) = ctx.get_extension::<ToolCatalogExtras>() {
        entries.extend(extras.0.iter().cloned());
    }

    ctx.insert_extension(Arc::new(SharedToolCatalog(Arc::new(entries))));
}

/// Build provider-facing definitions from the fully gated registry.
pub(crate) fn collect_tool_definitions(registry: &ToolRegistry) -> Vec<ToolDefinition> {
    collect_function_definitions(registry, None)
}

/// Install complete fork/spawn coordination on the shared context.
pub(crate) fn install_agent_infra(
    tool_registry: &Arc<ToolRegistry>,
    shared: &ToolContext,
    parts: AgentInfraParts,
) -> Result<mpsc::Receiver<ChildAgentResult>, SessionError> {
    let router = Arc::new(MessageRouter::new());
    if let Some(root_inbound) = parts.root_inbound {
        router.register(parts.id, root_inbound);
    }
    let mailbox_id = parts.session.mailbox_id();
    let pending_messages = Arc::new(PendingAgentMessages::from_events(
        parts.id,
        mailbox_id,
        &parts.event_store.events(),
    )?);
    pending_messages.register_root_mailbox(
        parts.id,
        mailbox_id,
        &parts.event_store,
        &parts.mailbox_lease,
    )?;
    let infra = AgentToolInfra {
        registry: parts.registry,
        router,
        pending_messages,
        provider: parts.provider,
        event_store: parts.event_store,
        agent_id: parts.id,
        parent_id: None,
        grant: None,
        tool_registry: Some(Arc::clone(tool_registry)),
        session: parts.session,
    };
    shared.insert_extension(Arc::new(infra));
    shared.insert_extension(Arc::new(crate::tools::agent::AgentCancellation(
        parts.cancel,
    )));
    crate::runtime_init::install_agent_handles(shared);
    if parts.terminal_reclamation {
        crate::runtime_init::install_terminal_reclamation(shared);
    }

    let log_tree = Arc::new(ActionLogTree::new(parts.id));
    if let Some(root_log) = shared.get_extension::<ActionLog>() {
        log_tree.register(parts.id, None, root_log);
    } else {
        tracing::warn!(
            agent_id = %parts.id,
            "install_agent_infra: no ActionLog extension on the shared context; \
             the action-log tree is anchored at the root with no root log",
        );
    }
    shared.insert_extension(log_tree);

    let child_result_capacity = parts.envelope.child_result_capacity;
    shared.insert_extension(Arc::new(parts.envelope));

    let (child_tx, child_rx) = mpsc::channel::<ChildAgentResult>(child_result_capacity);
    shared.insert_extension(Arc::new(ChildResultSender(Arc::new(child_tx))));
    Ok(child_rx)
}
