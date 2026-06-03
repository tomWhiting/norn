//! Shared runtime initialisation for CLI and in-process library consumers.
//!
//! This module contains the settings/context/profile pieces that must be
//! identical whether Norn is launched by `norn-cli` or embedded in another
//! process.

#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::config::{HookSettings, NornSettings, load_settings, merge_settings, validate_settings};
use crate::context::{ContextLoader, scan_rule_dirs};
use crate::error::{ConfigError, NornError};
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::{
    Hook, HookContext, HookEventType, HookMatcher, HookRegistry, ShellCommandHook,
    load_hooks_from_settings,
};
use crate::r#loop::config::{AgentLoopConfig, ConversationStateMode, ToolExecutor};
use crate::r#loop::iteration::IterationMonitorConfig;
use crate::r#loop::retry::RetryPolicy;
use crate::profile::Profile;
use crate::provider::request::{ReasoningEffort, ReasoningSummary};
use crate::rules::engine::RuleEngine;
use crate::skill::SkillCatalog;
use crate::tool::context::{SharedWorkingDir, ToolContext};
use crate::tool::registry::ToolRegistry;
use crate::tools::context_paths::ContextSearchPaths;

use crate::tools::skill::SkillSearchPaths;
use crate::tools::task::{DiskTaskStore, SharedTaskStore, TaskStore};
use crate::tools::tool_search::{SharedToolCatalog, ToolCatalogEntry, ToolCatalogExtras};

/// Fully loaded base runtime inputs shared by CLI and embedded agents.
pub struct LoadedRuntimeBase {
    pub settings: NornSettings,
    pub agent_config: AgentLoopConfig,
    pub retry_policy: RetryPolicy,
    pub hooks: Option<Arc<HookRegistry>>,
    pub rules: Option<RuleEngine>,
    pub context_loader: ContextLoader,
    pub skill_paths: Vec<PathBuf>,
    pub skill_catalog: Arc<SkillCatalog>,
    pub shared_task_store: Arc<SharedTaskStore>,
    pub diagnostics: Arc<DiagnosticCollector>,
    pub iteration_monitor: Option<IterationMonitorConfig>,
}

/// Provider settings after merging the same settings layers as the CLI.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProviderSettingsResolved {
    pub base_url: Option<String>,
    pub request_timeout: Option<Duration>,
    pub max_retries: Option<u32>,
    pub provider_options: Option<serde_json::Value>,
    pub debug_dump_dir: Option<PathBuf>,
    pub rate_limit: Option<u32>,
}

/// Load and validate user/project/local settings from the supplied CWD.
pub fn load_merged_settings(cwd: &Path) -> Result<NornSettings, NornError> {
    let mut layers = load_settings(cwd)?;
    let mut cli_layer = NornSettings::default();
    let merged = merge_settings(
        &mut layers.user,
        &mut layers.project,
        &mut layers.local,
        &mut cli_layer,
    );
    validate_settings(&merged)?;
    Ok(merged)
}

/// Apply settings-level agent config onto compiled defaults.
pub fn agent_config_from_settings(settings: &NornSettings) -> Result<AgentLoopConfig, NornError> {
    let mut config = AgentLoopConfig::default();
    apply_settings_to_agent_config(settings, &mut config)?;
    Ok(config)
}

/// Apply settings-level reasoning defaults to a profile when unset.
pub fn apply_settings_reasoning_to_profile(
    settings: &NornSettings,
    profile: &mut Profile,
) -> Result<(), NornError> {
    let Some(agent) = settings.agent.as_ref() else {
        return Ok(());
    };
    if profile.reasoning_effort.is_none()
        && let Some(raw) = agent.reasoning_effort.as_deref()
    {
        profile.reasoning_effort = Some(parse_reasoning_effort(raw)?);
    }
    if profile.reasoning_summary.is_none()
        && let Some(raw) = agent.reasoning_summary.as_deref()
    {
        profile.reasoning_summary = Some(parse_reasoning_summary(raw)?);
    }
    Ok(())
}

/// Resolve provider settings using the same settings layer as CLI runtime assembly.
pub fn provider_settings_from_settings(
    settings: &NornSettings,
) -> Result<ProviderSettingsResolved, NornError> {
    let mut resolved = ProviderSettingsResolved::default();
    let Some(provider) = settings.provider.as_ref() else {
        return Ok(resolved);
    };
    if let Some(base_url) = provider.base_url.as_deref() {
        resolved.base_url = Some(base_url.to_owned());
    }
    if let Some(timeout) = provider.timeout.as_deref() {
        resolved.request_timeout = Some(parse_settings_duration("provider.timeout", timeout)?);
    }
    resolved.max_retries = provider.max_retries;
    resolved.provider_options.clone_from(&provider.options);
    resolved.debug_dump_dir = provider.debug_dump_dir.as_deref().map(PathBuf::from);
    resolved.rate_limit = provider.rate_limit;
    Ok(resolved)
}

/// Build all shared base runtime pieces for a profile and working directory.
pub fn load_runtime_base(
    cwd: &Path,
    profile: &mut Profile,
    programmatic_hooks: Option<Arc<HookRegistry>>,
    task_group_slug: Option<&str>,
) -> Result<LoadedRuntimeBase, NornError> {
    let settings = load_merged_settings(cwd)?;
    apply_settings_reasoning_to_profile(&settings, profile)?;
    let agent_config = agent_config_from_settings(&settings)?;
    let retry_policy = retry_policy_from_settings(&settings)?;
    let diagnostics = DiagnosticCollector::shared();
    let shared_wd = SharedWorkingDir::new(cwd.to_path_buf());
    let rules = merge_discovered_rules(None, cwd).map(|r| r.with_working_dir(shared_wd));
    let hook_settings = load_hooks_from_settings(cwd)?;
    let hooks = assemble_hook_registry(programmatic_hooks, &hook_settings, profile, cwd)?;
    let skill_paths = build_skill_search_paths(&settings, cwd);
    let skill_catalog = Arc::new(SkillCatalog::scan(&skill_paths));
    let shared_task_store = build_shared_task_store(task_group_slug);
    let iteration_monitor = iteration_monitor_from_profile(profile)?;

    Ok(LoadedRuntimeBase {
        settings,
        agent_config,
        retry_policy,
        hooks,
        rules,
        context_loader: ContextLoader::load(cwd),
        skill_paths,
        skill_catalog,
        shared_task_store,
        diagnostics,
        iteration_monitor,
    })
}

pub fn install_skill_infra(ctx: &ToolContext, paths: Vec<PathBuf>, catalog: Arc<SkillCatalog>) {
    ctx.insert_extension(Arc::new(SkillSearchPaths(paths)));
    ctx.insert_extension(catalog);
}

pub fn install_context_search_paths(ctx: &ToolContext, settings: &NornSettings, cwd: &Path) {
    let Some(context) = settings.context.as_ref() else {
        return;
    };
    let Some(entries) = context.search_paths.as_ref() else {
        return;
    };
    if entries.is_empty() {
        return;
    }
    let paths = entries
        .iter()
        .map(|entry| resolve_search_path(cwd, entry))
        .collect();
    ctx.insert_extension(Arc::new(ContextSearchPaths(paths)));
}

pub fn install_runtime_extensions(
    ctx: &ToolContext,
    task_store: &Arc<SharedTaskStore>,
    diagnostics: &Arc<DiagnosticCollector>,
    hooks: Option<&Arc<HookRegistry>>,
) {
    ctx.insert_extension(Arc::clone(task_store));
    ctx.insert_extension(Arc::clone(diagnostics));
    if let Some(hooks) = hooks {
        ctx.insert_extension(Arc::clone(hooks));
    }
}

pub fn install_tool_catalog(registry: &ToolRegistry) {
    let Some(ctx) = registry.shared_context() else {
        return;
    };
    let mut entries: Vec<ToolCatalogEntry> = registry
        .names()
        .filter_map(|name| {
            registry
                .get(name)
                .map(|tool| ToolCatalogEntry::tool(tool.name(), tool.description()))
        })
        .collect();
    if let Some(extras) = ctx.get_extension::<ToolCatalogExtras>() {
        entries.extend(extras.0.iter().cloned());
    }
    ctx.insert_extension(Arc::new(SharedToolCatalog(Arc::new(entries))));
}

fn apply_settings_to_agent_config(
    settings: &NornSettings,
    config: &mut AgentLoopConfig,
) -> Result<(), NornError> {
    let Some(agent) = settings.agent.as_ref() else {
        return Ok(());
    };
    if let Some(max_turns) = agent.max_turns {
        config.max_iterations = Some(max_turns);
    }
    if let Some(step_timeout) = agent.step_timeout.as_deref() {
        config.step_timeout = Some(parse_settings_duration("agent.step_timeout", step_timeout)?);
    }
    if let Some(budget) = agent.schema_budget {
        config.schema_attempt_budget = budget;
    }
    if let Some(window) = agent.context_window {
        config.context_window_limit = Some(window);
    }
    if let Some(threshold) = agent.compact_threshold {
        config.auto_compact_threshold_pct = Some(threshold);
    }
    if let Some(keep) = agent.compact_keep_turns {
        config.auto_compact_keep_recent_turns = keep;
    }
    if let Some(mode) = agent.conversation_state.as_deref() {
        config.conversation_state = parse_settings_conversation_state(mode)?;
    }
    if let Some(threshold) = agent.server_compaction_threshold_tokens {
        config.server_compaction_threshold_tokens = Some(threshold);
    }
    Ok(())
}

fn retry_policy_from_settings(settings: &NornSettings) -> Result<RetryPolicy, NornError> {
    let mut policy = RetryPolicy::default();
    if let Some(retry) = settings.retry.as_ref() {
        if let Some(max) = retry.max_retries {
            policy.max_retries = max;
        }
        if let Some(base) = retry.base_delay.as_deref() {
            policy.initial_backoff = parse_settings_duration("retry.base_delay", base)?;
        }
        if let Some(mult) = retry.backoff_multiplier {
            policy.backoff_multiplier = mult;
        }
    }
    Ok(policy)
}

fn parse_settings_duration(field: &str, value: &str) -> Result<Duration, NornError> {
    humantime::parse_duration(value).map_err(|err| {
        invalid_config(format!(
            "invalid duration for {field}: '{value}': {err} (examples: 30s, 2m, 1h, 100ms)"
        ))
    })
}

fn parse_settings_conversation_state(raw: &str) -> Result<ConversationStateMode, NornError> {
    match raw {
        "auto" => Ok(ConversationStateMode::Auto),
        "provider_threaded" => Ok(ConversationStateMode::ProviderThreaded),
        "manual" | "manual_replay" => Ok(ConversationStateMode::ManualReplay),
        _ => Err(invalid_config(format!(
            "invalid value for agent.conversation_state: expected auto, manual, or provider_threaded, got '{raw}'"
        ))),
    }
}

fn parse_reasoning_effort(raw: &str) -> Result<ReasoningEffort, NornError> {
    let value = serde_json::Value::String(raw.to_lowercase());
    serde_json::from_value(value).map_err(|err| invalid_config(format!(
        "invalid value for agent.reasoning_effort: '{raw}' ({err}); expected one of none, low, medium, high, xhigh"
    )))
}

fn parse_reasoning_summary(raw: &str) -> Result<ReasoningSummary, NornError> {
    let value = serde_json::Value::String(raw.to_lowercase());
    serde_json::from_value(value).map_err(|err| invalid_config(format!(
        "invalid value for agent.reasoning_summary: '{raw}' ({err}); expected one of auto, concise, detailed"
    )))
}

fn build_skill_search_paths(settings: &NornSettings, cwd: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(skills) = settings.skills.as_ref()
        && let Some(entries) = skills.search_paths.as_ref()
    {
        for entry in entries {
            paths.push(resolve_search_path(cwd, entry));
        }
    }

    paths.push(cwd.join(".norn").join("skills"));
    paths.push(cwd.join(".agents").join("skills"));
    paths.push(cwd.join(".claude").join("skills"));
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".norn").join("skills"));
        paths.push(home.join(".agents").join("skills"));
        paths.push(home.join(".claude").join("skills"));
    }
    paths.push(cwd.join(".meridian").join("skills"));
    paths
}

fn resolve_search_path(cwd: &Path, raw: &str) -> PathBuf {
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    }
}

fn merge_discovered_rules(existing: Option<RuleEngine>, cwd: &Path) -> Option<RuleEngine> {
    let mut dirs = vec![cwd.join(".norn").join("rules")];
    if let Some(user) = crate::config::paths::rules_dir() {
        dirs.push(user);
    }
    let discovered = scan_rule_dirs(&dirs);
    if discovered.is_empty() {
        return existing;
    }
    match existing {
        Some(mut engine) => {
            for rule in discovered {
                engine.add_rule(rule);
            }
            Some(engine)
        }
        None => Some(RuleEngine::new(discovered)),
    }
}

fn build_shared_task_store(group_slug: Option<&str>) -> Arc<SharedTaskStore> {
    let root = crate::config::paths::norn_dir()
        .unwrap_or_else(|| PathBuf::from(".norn"))
        .join("tasks");
    let slug = group_slug.map_or_else(|| "default".to_string(), sanitise_slug);
    let disk = DiskTaskStore::new(root, slug);
    let store: Arc<dyn TaskStore> = Arc::new(disk);
    Arc::new(SharedTaskStore(store))
}

fn sanitise_slug(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("default");
    }
    out
}

fn assemble_hook_registry(
    programmatic: Option<Arc<HookRegistry>>,
    settings: &HookSettings,
    profile: &Profile,
    cwd: &Path,
) -> Result<Option<Arc<HookRegistry>>, NornError> {
    let shell_total = settings_total_entries(settings);
    if programmatic.is_none() && shell_total == 0 {
        return Ok(None);
    }
    if shell_total == 0 {
        return Ok(programmatic);
    }

    let mut registry = match programmatic {
        Some(arc) => Arc::try_unwrap(arc).unwrap_or_else(|_| HookRegistry::new()),
        None => HookRegistry::new(),
    };
    let context = HookContext {
        session_id: String::new(),
        cwd: cwd.display().to_string(),
        agent_id: String::new(),
        profile_name: profile.name.clone(),
    };

    register_shell_hooks(
        &mut registry,
        settings.pre_tool.as_ref(),
        HookEventType::PreTool,
        &context,
        |h| Hook::PreTool(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.post_tool.as_ref(),
        HookEventType::PostTool,
        &context,
        |h| Hook::PostTool(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.post_tool_failure.as_ref(),
        HookEventType::PostToolFailure,
        &context,
        |h| Hook::PostToolFailure(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.pre_llm.as_ref(),
        HookEventType::PreLlm,
        &context,
        |h| Hook::PreLlm(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.post_llm.as_ref(),
        HookEventType::PostLlm,
        &context,
        |h| Hook::PostLlm(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.session_event.as_ref(),
        HookEventType::SessionEvent,
        &context,
        |h| Hook::SessionEvent(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.user_prompt.as_ref(),
        HookEventType::UserPrompt,
        &context,
        |h| Hook::UserPrompt(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.stop.as_ref(),
        HookEventType::Stop,
        &context,
        |h| Hook::Stop(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.subagent_start.as_ref(),
        HookEventType::SubagentStart,
        &context,
        |h| Hook::Subagent(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.subagent_stop.as_ref(),
        HookEventType::SubagentStop,
        &context,
        |h| Hook::Subagent(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.session_start.as_ref(),
        HookEventType::SessionStart,
        &context,
        |h| Hook::SessionLifecycle(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.session_end.as_ref(),
        HookEventType::SessionEnd,
        &context,
        |h| Hook::SessionLifecycle(Box::new(h)),
    )?;
    register_shell_hooks(
        &mut registry,
        settings.pre_compaction.as_ref(),
        HookEventType::PreCompaction,
        &context,
        |h| Hook::Compaction(Box::new(h)),
    )?;

    Ok(Some(Arc::new(registry)))
}

fn register_shell_hooks<F>(
    registry: &mut HookRegistry,
    entries: Option<&Vec<crate::config::HookEntry>>,
    event_type: HookEventType,
    context: &HookContext,
    wrap: F,
) -> Result<(), NornError>
where
    F: Fn(ShellCommandHook) -> Hook,
{
    let Some(entries) = entries else {
        return Ok(());
    };
    for entry in entries {
        let timeout_ms = entry.timeout.ok_or_else(|| {
            invalid_config(format!(
                "hook {:?} command '{}' is missing required timeout",
                event_type, entry.command
            ))
        })?;
        let timeout = Duration::from_millis(timeout_ms);
        let matcher = HookMatcher::new(entry.matcher.as_deref())?;
        registry.register(wrap(ShellCommandHook::new(
            entry.command.clone(),
            matcher,
            timeout,
            event_type,
            context.clone(),
        )));
    }
    Ok(())
}

fn settings_total_entries(settings: &HookSettings) -> usize {
    settings.pre_tool.as_ref().map_or(0, Vec::len)
        + settings.post_tool.as_ref().map_or(0, Vec::len)
        + settings.post_tool_failure.as_ref().map_or(0, Vec::len)
        + settings.pre_llm.as_ref().map_or(0, Vec::len)
        + settings.post_llm.as_ref().map_or(0, Vec::len)
        + settings.session_event.as_ref().map_or(0, Vec::len)
        + settings.user_prompt.as_ref().map_or(0, Vec::len)
        + settings.stop.as_ref().map_or(0, Vec::len)
        + settings.subagent_start.as_ref().map_or(0, Vec::len)
        + settings.subagent_stop.as_ref().map_or(0, Vec::len)
        + settings.session_start.as_ref().map_or(0, Vec::len)
        + settings.session_end.as_ref().map_or(0, Vec::len)
        + settings.pre_compaction.as_ref().map_or(0, Vec::len)
}

fn iteration_monitor_from_profile(
    profile: &Profile,
) -> Result<Option<IterationMonitorConfig>, NornError> {
    let Some(raw) = profile.settings.get("iteration_monitor") else {
        return Ok(None);
    };
    let spec: IterationMonitorSpec = serde_json::from_value(raw.clone()).map_err(|err| {
        invalid_config(format!(
            "invalid [iteration_monitor] profile section: {err} (expected fields: context_window_tokens, warn_threshold_pct, handoff_threshold_pct, handoff_guidance, failure_repeat_window, hedging_patterns)"
        ))
    })?;
    Ok(Some(spec.into_config()))
}

#[derive(Debug, Deserialize)]
struct IterationMonitorSpec {
    context_window_tokens: u64,
    warn_threshold_pct: f64,
    handoff_threshold_pct: f64,
    handoff_guidance: String,
    failure_repeat_window: usize,
    #[serde(default)]
    hedging_patterns: Vec<String>,
}

impl IterationMonitorSpec {
    fn into_config(self) -> IterationMonitorConfig {
        IterationMonitorConfig {
            context_window_tokens: self.context_window_tokens,
            warn_threshold_pct: self.warn_threshold_pct,
            handoff_threshold_pct: self.handoff_threshold_pct,
            handoff_guidance: self.handoff_guidance,
            failure_repeat_window: self.failure_repeat_window,
            hedging_patterns: self.hedging_patterns,
        }
    }
}

fn invalid_config(reason: String) -> NornError {
    NornError::Config(ConfigError::InvalidConfig { reason })
}
