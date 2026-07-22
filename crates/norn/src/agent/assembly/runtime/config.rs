//! Effective loop-configuration and loop-context population.

use std::sync::Arc;

use crate::integration::DiagnosticCollector;
use crate::integration::variables::VariableStore;
use crate::r#loop::config::AgentLoopConfig;
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::retry::RetryPolicy;
use crate::runtime_init::LoadedRuntimeBase;
use crate::system_prompt::environment::EnvironmentConfig;
use crate::tool::context::SharedWorkingDir;

/// Which non-`Option` [`AgentLoopConfig`] fields the caller explicitly set.
///
/// `Option` fields carry their own presence (`Some` = explicitly set), while
/// the fields below are indistinguishable from an unset default by value
/// alone. A caller that explicitly restores a library default must still win
/// over a settings-derived runtime base, so presence is tracked structurally.
#[derive(Clone, Copy, Default)]
pub(crate) struct AgentConfigPresence {
    /// `schema_attempt_budget` was explicitly set.
    pub(crate) schema_attempt_budget: bool,
    /// `auto_compact_keep_recent_turns` was explicitly set.
    pub(crate) auto_compact_keep_recent_turns: bool,
    /// `auto_compact_reserve_tokens` was explicitly set. Its default is a
    /// meaningful `Some(30_000)`, not an unset sentinel.
    pub(crate) auto_compact_reserve_tokens: bool,
    /// `schema_tool_name` was explicitly set.
    pub(crate) schema_tool_name: bool,
    /// `conversation_state` was explicitly set.
    pub(crate) conversation_state: bool,
}

impl AgentConfigPresence {
    /// Mark every structurally tracked field present for a complete explicit
    /// [`AgentLoopConfig`].
    pub(crate) fn all() -> Self {
        Self {
            schema_attempt_budget: true,
            auto_compact_keep_recent_turns: true,
            auto_compact_reserve_tokens: true,
            schema_tool_name: true,
            conversation_state: true,
        }
    }
}

/// The effective agent-loop config: the runtime base's config with
/// explicitly-set builder fields overlaid, or the explicit config alone
/// when no base was loaded. This single value drives both the loop config
/// and the system prompt's compaction guidance.
pub(crate) fn effective_agent_config(
    runtime_base: Option<&LoadedRuntimeBase>,
    explicit: AgentLoopConfig,
    present: AgentConfigPresence,
) -> AgentLoopConfig {
    match runtime_base {
        Some(base) => merge_agent_config(base.agent_config.clone(), explicit, present),
        None => explicit,
    }
}

/// Populate retry policy, diagnostics, working directory, variables, and
/// environment on the loop context, returning the resolved session id.
///
/// The returned id, the `{{session_id}}` variable, and the system-prompt
/// environment always agree. Auto-compaction is armed separately by the
/// shared root/child arming path after the effective config is resolved.
pub(crate) fn populate_loop_context(
    loop_context: &mut LoopContext,
    retry_policy: Option<RetryPolicy>,
    runtime_base: Option<&LoadedRuntimeBase>,
    diagnostics: Option<&Arc<DiagnosticCollector>>,
    shared_wd: &SharedWorkingDir,
    model: &str,
    session_id_override: Option<&str>,
) -> String {
    loop_context.retry_policy = retry_policy.unwrap_or_else(|| {
        runtime_base.map_or_else(RetryPolicy::default, |base| base.retry_policy.clone())
    });
    loop_context.diagnostics = diagnostics.map(Arc::clone);
    loop_context.working_dir = shared_wd.clone();
    loop_context.nested_scanner = loop_context
        .rules
        .as_ref()
        .map(|_| crate::context::scanner::NestedScanner::new_at_launch_root(shared_wd.get()));
    let mut variables = VariableStore::with_builtins().with_working_dir(shared_wd.clone());
    if let Some(id) = session_id_override {
        variables = variables.with_session_id(id);
    }
    let variables = Arc::new(variables);
    let session_id = variables.session_id().to_owned();
    loop_context.variables = Some(variables);
    loop_context.environment = Some(EnvironmentConfig {
        session_id: Some(session_id.clone()),
        model: model.to_owned(),
    });
    session_id
}

/// Overlay every explicitly-set builder field onto the runtime-base config.
///
/// Fields with meaningful non-`Option` defaults overlay only when `present`
/// marks them explicit; ordinary optional fields overlay when they are
/// `Some`. Every [`AgentLoopConfig`] field is covered here.
pub(super) fn merge_agent_config(
    mut base: AgentLoopConfig,
    explicit: AgentLoopConfig,
    present: AgentConfigPresence,
) -> AgentLoopConfig {
    if present.schema_attempt_budget {
        base.schema_attempt_budget = explicit.schema_attempt_budget;
    }
    if present.auto_compact_keep_recent_turns {
        base.auto_compact_keep_recent_turns = explicit.auto_compact_keep_recent_turns;
    }
    if present.auto_compact_reserve_tokens {
        base.auto_compact_reserve_tokens = explicit.auto_compact_reserve_tokens;
    }
    if present.schema_tool_name {
        base.schema_tool_name = explicit.schema_tool_name;
    }
    if present.conversation_state {
        base.conversation_state = explicit.conversation_state;
    }
    if explicit.max_iterations.is_some() {
        base.max_iterations = explicit.max_iterations;
    }
    if explicit.step_timeout.is_some() {
        base.step_timeout = explicit.step_timeout;
    }
    if explicit.context_window_limit.is_some() {
        base.context_window_limit = explicit.context_window_limit;
    }
    if explicit.cache_key.is_some() {
        base.cache_key = explicit.cache_key;
    }
    if explicit.server_compaction_threshold_tokens.is_some() {
        base.server_compaction_threshold_tokens = explicit.server_compaction_threshold_tokens;
    }
    if explicit.output_schema.is_some() {
        base.output_schema = explicit.output_schema;
    }
    if explicit.prompt_command_timeout.is_some() {
        base.prompt_command_timeout = explicit.prompt_command_timeout;
    }
    if explicit.linger.is_some() {
        base.linger = explicit.linger;
    }
    base
}
