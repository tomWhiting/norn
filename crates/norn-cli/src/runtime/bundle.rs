//! Runtime bundle types produced by [`crate::runtime::build_runtime`].
//!
//! Extracted from `runtime.rs` to keep that module within the 500-line
//! code limit (CO5). The structs here are the inputs and outputs of the
//! assembly pipeline — they carry no logic beyond a [`Default`] impl on
//! [`RuntimeInputs`].

use std::sync::Arc;

use norn::integration::DiagnosticCollector;
use norn::integration::hooks::HookRegistry;
use norn::r#loop::config::AgentLoopConfig;
use norn::r#loop::loop_context::LoopContext;
use norn::tool::registry::ToolRegistry;
use norn::tools::SharedTaskStore;
use norn::tools::lsp::{LspBackend, LspWorkspace};

use crate::config::ProviderConfigOverrides;

/// Inputs to [`crate::runtime::build_runtime`] that the caller assembles
/// outside the CLI parser.
///
/// The CLI does not own the [`ToolRegistry`] or [`HookRegistry`] —
/// downstream wiring (NC-003 for tools, future briefs for hooks)
/// constructs them and hands them in. Default behaviour: empty
/// registry, no hooks, no LSP wiring.
pub struct RuntimeInputs {
    /// Tool registry to gate via [`norn::profile::Profile::resolved_tools`].
    /// Caller is responsible for registering every tool that may be
    /// reachable.
    pub registry: ToolRegistry,
    /// Optional hook registry installed onto [`LoopContext::hooks`].
    ///
    /// Wrapped in [`Arc`] so the registry can also be cloned onto the
    /// shared [`norn::tool::context::ToolContext`] as an extension —
    /// sub-agent dispatch sites (e.g. `tools/agent/spawn.rs`) retrieve
    /// it via `ctx.get_extension::<HookRegistry>()` instead of carrying
    /// a [`LoopContext`] reference.
    pub hooks: Option<Arc<HookRegistry>>,
    /// Optional shared [`LspWorkspace`] handed in by the TUI driver
    /// (LD-015 R3). When present, `build_runtime` forwards it to
    /// `build_diagnostic_infra` so the post-check pipeline gets an
    /// `LspBridge` fed by the same diagnostic aggregator the `LspBackend`
    /// publishes into. `None` keeps the bridge unwired — the post-check
    /// still falls back to the LD-003 adapter-subprocess path.
    pub lsp_workspace: Option<Arc<LspWorkspace>>,
    /// Optional shared [`LspBackend`] handed in by the TUI driver
    /// (LD-015 R3). When present, `build_runtime` forwards it to
    /// `build_diagnostic_infra` so the post-check pipeline can dispatch
    /// LSP test execution through the same backend the `lsp` tool uses.
    /// `None` keeps that slot unwired (graceful CO5 degradation).
    pub lsp_backend: Option<Arc<dyn LspBackend>>,
}

impl Default for RuntimeInputs {
    fn default() -> Self {
        Self {
            registry: ToolRegistry::new(),
            hooks: None,
            lsp_workspace: None,
            lsp_backend: None,
        }
    }
}

/// Output of [`crate::runtime::build_runtime`]: everything
/// `run_agent_step` and the downstream provider construction need.
pub struct RuntimeBundle {
    /// Fully populated loop context with system sections, reasoning
    /// effort, prompt commands, rules, hooks, event schemas, variables,
    /// retry policy, token estimator, and context edits.
    pub loop_context: LoopContext,
    /// Tool registry gated by the resolved tool allow-list and the
    /// `--disallowed-tools` deny-list (deny wins).
    ///
    /// Wrapped in [`Arc`] so the same registry can be shared with
    /// [`norn::tools::agent::AgentToolInfra::tool_registry`] — spawned
    /// sub-agents dispatch tool calls through this registry.
    pub registry: Arc<ToolRegistry>,
    /// Agent-loop configuration with CLI and `-c` overrides applied.
    pub agent_config: AgentLoopConfig,
    /// Provider-config overrides (NC-003 consumes these when building
    /// the [`norn::provider::request::ProviderConfig`]).
    pub provider_overrides: ProviderConfigOverrides,
    /// Model identifier to pass to `run_agent_step`. NOT carried on
    /// [`LoopContext`] because the runtime treats it as a per-call
    /// argument.
    pub model: String,
    /// MCP extension URIs collected from `--extension`. Connection lives
    /// in a later brief.
    pub extension_uris: Vec<String>,
    /// Disallowed tool names from `--disallowed-tools` (exact names).
    /// Already applied to [`Self::registry`] via
    /// [`norn::tool::registry::ToolRegistry::set_disallowed`] — deny wins
    /// over the `--allowed-tools` allow-list — and carried here for
    /// downstream audit surfaces.
    pub disallowed_tools: Vec<String>,
    /// Diagnostic collector retained for draining after `run_agent_step`.
    ///
    /// The same `Arc<DiagnosticCollector>` is wired onto
    /// [`LoopContext::diagnostics`] (so the loop's schema-validation,
    /// pre-validate, and post-validate push sites report into it) and
    /// onto the orchestrator [`norn::tool::context::ToolContext`] held
    /// by the [`ToolRegistry`] (so runtime tools can retrieve it via
    /// `ctx.get_extension::<DiagnosticCollector>()` and push directly).
    /// Callers drain via [`DiagnosticCollector::drain`] after the agent
    /// step completes.
    pub diagnostics: Arc<DiagnosticCollector>,
    /// Shared task store handle wrapping the production
    /// [`norn::tools::DiskTaskStore`].
    ///
    /// `build_runtime` constructs the disk-backed store rooted at
    /// `paths::norn_dir()/tasks/` with the session-derived group slug.
    /// The handle is carried on the bundle so the extension wiring
    /// step (NA-005) can install it on the shared [`ToolContext`].
    pub shared_task_store: Arc<SharedTaskStore>,
}
