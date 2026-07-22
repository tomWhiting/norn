//! Settings loading and runtime-base assembly shared by every launch path.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::config::loader::load_settings_at_launch_root;
use crate::config::{
    NornSettings, ProviderAuthBackend, ProviderAuthConfigError, ProviderAuthMode,
    ResolvedProviderAuth, merge_settings, resolve_provider_auth, validate_settings,
    validate_working_directory_authority,
};
use crate::context::ContextLoader;
use crate::context::scanner::scan_rule_dirs_with_origins;
use crate::error::{ConfigError, NornError};
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::{HookRegistry, load_hooks_from_settings};
use crate::r#loop::config::{AgentLoopConfig, ConversationStateMode};
use crate::r#loop::iteration::IterationMonitorConfig;
use crate::r#loop::retry::RetryPolicy;
use crate::profile::Profile;
use crate::provider::request::{ReasoningEffort, ReasoningSummary, ServiceTier};
use crate::rules::engine::RuleEngine;
use crate::skill::SkillCatalog;
use crate::tool::context::SharedWorkingDir;
use crate::tools::task::{DiskTaskStore, SharedTaskStore, TaskStore};
use crate::util::workspace_relative_path;

use super::hooks::assemble_hook_registry;

/// Fully loaded base runtime inputs shared by CLI and embedded agents.
pub struct LoadedRuntimeBase {
    /// Merged user/project/local settings.
    pub settings: NornSettings,
    /// Agent-loop config with settings-level overrides applied.
    pub agent_config: AgentLoopConfig,
    /// Provider retry policy from the merged settings.
    pub retry_policy: RetryPolicy,
    /// Merged hook registry: programmatic hooks (first, winning conflicts)
    /// plus the settings-declared shell hooks. See [`assemble_hook_registry`].
    pub hooks: Option<Arc<HookRegistry>>,
    /// Rules discovered under `.norn/rules` and the user rules dir.
    pub rules: Option<RuleEngine>,
    /// NORN.md context loader rooted at the working directory.
    pub context_loader: ContextLoader,
    /// Skill search paths (settings-declared plus the standard locations).
    pub skill_paths: Vec<PathBuf>,
    /// Catalog of skills discovered under [`Self::skill_paths`].
    pub skill_catalog: Arc<SkillCatalog>,
    /// Disk-backed task store shared across the agent tree.
    pub shared_task_store: Arc<SharedTaskStore>,
    /// Diagnostic collector created for this runtime base. Embedders that
    /// supply their own collector take precedence over this one.
    pub diagnostics: Arc<DiagnosticCollector>,
    /// Iteration-monitor config parsed from the profile, when declared.
    pub iteration_monitor: Option<IterationMonitorConfig>,
}

/// Provider settings after merging the same settings layers as the CLI.
#[derive(Default, Clone, PartialEq)]
pub struct ProviderSettingsResolved {
    /// Override for the provider's base URL.
    pub base_url: Option<String>,
    /// Per-request timeout.
    pub request_timeout: Option<Duration>,
    /// Maximum provider-level retries.
    pub max_retries: Option<u32>,
    /// Provider-specific options passed through verbatim.
    pub provider_options: Option<serde_json::Value>,
    /// Explicit authentication mode, when configured.
    pub auth: Option<ProviderAuthMode>,
    /// Name of the environment variable holding an API key.
    pub api_key_env: Option<String>,
    /// Directory for provider debug dumps.
    pub debug_dump_dir: Option<PathBuf>,
    /// Requests-per-minute rate limit.
    pub rate_limit: Option<u32>,
    /// Replenishment window over which [`Self::rate_limit`] permits are
    /// granted. `None` defers to the library's owner-approved 60-second
    /// default.
    pub rate_limit_interval: Option<Duration>,
    /// Backoff applied to a `429` response with no parseable
    /// `Retry-After` header. `None` defers to the library's
    /// owner-approved 1-second default.
    pub retry_backoff: Option<Duration>,
    /// Optional ceiling on accepted server-supplied `Retry-After` waits.
    /// `None` honors the header as-is — the library deliberately has no
    /// built-in ceiling.
    pub retry_after_ceiling: Option<Duration>,
}

impl std::fmt::Debug for ProviderSettingsResolved {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderSettingsResolved")
            .field("base_url_present", &self.base_url.is_some())
            .field("request_timeout", &self.request_timeout)
            .field("max_retries", &self.max_retries)
            .field("provider_options_present", &self.provider_options.is_some())
            .field("auth", &self.auth)
            .field("api_key_env_present", &self.api_key_env.is_some())
            .field("debug_dump_dir_present", &self.debug_dump_dir.is_some())
            .field("rate_limit", &self.rate_limit)
            .field("rate_limit_interval", &self.rate_limit_interval)
            .field("retry_backoff", &self.retry_backoff)
            .field("retry_after_ceiling", &self.retry_after_ceiling)
            .finish()
    }
}

impl ProviderSettingsResolved {
    /// Resolve the merged authentication fields for a concrete provider family.
    ///
    /// This is the canonical library path for embedders. It validates the full
    /// mode/companion matrix before the caller reads an environment variable,
    /// opens credential storage, or constructs a provider.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderAuthConfigError`] when the configured mode and API-key
    /// source are incompatible with `backend`.
    pub fn resolve_auth(
        &self,
        backend: ProviderAuthBackend,
    ) -> Result<ResolvedProviderAuth, ProviderAuthConfigError> {
        resolve_provider_auth(backend, self.auth, self.api_key_env.as_deref())
    }
}

/// Load and validate user/project/local settings from the supplied CWD.
///
/// Raw loading and mechanical merging are intentionally not public API. An
/// external embedder cannot obtain unvalidated working-directory layers or
/// merge them while erasing their provenance:
///
/// ```compile_fail
/// use norn::config::loader::load_settings;
/// ```
///
/// ```compile_fail
/// use norn::config::merge_settings;
/// ```
///
/// # Errors
///
/// Returns [`NornError::Config`] when a settings layer fails to load or the
/// merged settings fail validation.
pub fn load_merged_settings(cwd: &Path) -> Result<NornSettings, NornError> {
    let cwd = cwd.canonicalize().map_err(|error| {
        NornError::Config(ConfigError::InvalidConfig {
            reason: format!("failed to resolve the settings workspace trust root: {error}"),
        })
    })?;
    load_merged_settings_at_launch_root(&cwd)
}

pub(crate) fn load_merged_settings_at_launch_root(cwd: &Path) -> Result<NornSettings, NornError> {
    let mut layers = load_settings_at_launch_root(cwd)?;
    validate_working_directory_authority(&layers.user, &layers.project, &layers.local)?;
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
///
/// # Errors
///
/// Returns [`NornError::Config`] for malformed settings values.
pub fn agent_config_from_settings(settings: &NornSettings) -> Result<AgentLoopConfig, NornError> {
    let mut config = AgentLoopConfig::default();
    apply_settings_to_agent_config(settings, &mut config)?;
    Ok(config)
}

/// Apply settings-level reasoning defaults to a profile when unset.
///
/// # Errors
///
/// Returns [`NornError::Config`] for unrecognised reasoning values.
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
    if profile.service_tier.is_none()
        && let Some(raw) = agent.service_tier.as_deref()
    {
        profile.service_tier = Some(parse_service_tier(raw)?);
    }
    Ok(())
}

/// Resolve provider settings from a trusted merged settings value.
///
/// Callers loading settings from disk should obtain `settings` from
/// [`load_merged_settings`], which rejects provider-authority fields from the
/// working-directory layers before merging them.
///
/// # Errors
///
/// Returns [`NornError::Config`] for malformed settings values.
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
    resolved.auth = provider.auth;
    resolved.api_key_env.clone_from(&provider.api_key_env);
    resolved.debug_dump_dir = provider.debug_dump_dir.as_deref().map(PathBuf::from);
    resolved.rate_limit = provider.rate_limit;
    if let Some(interval) = provider.rate_limit_interval.as_deref() {
        resolved.rate_limit_interval = Some(parse_settings_duration(
            "provider.rate_limit_interval",
            interval,
        )?);
    }
    if let Some(backoff) = provider.retry_backoff.as_deref() {
        resolved.retry_backoff = Some(parse_settings_duration("provider.retry_backoff", backoff)?);
    }
    if let Some(ceiling) = provider.retry_after_ceiling.as_deref() {
        resolved.retry_after_ceiling = Some(parse_settings_duration(
            "provider.retry_after_ceiling",
            ceiling,
        )?);
    }
    Ok(resolved)
}

/// Build all shared base runtime pieces for a profile and working directory.
///
/// # Errors
///
/// Returns [`NornError::Config`] when settings fail to load or validate, or
/// when the settings-declared hooks are malformed.
pub fn load_runtime_base(
    cwd: &Path,
    profile: &mut Profile,
    programmatic_hooks: Option<Arc<HookRegistry>>,
    task_group_slug: Option<&str>,
) -> Result<LoadedRuntimeBase, NornError> {
    let cwd = cwd.canonicalize().map_err(|error| {
        NornError::Config(ConfigError::InvalidConfig {
            reason: format!("failed to resolve the runtime workspace trust root: {error}"),
        })
    })?;
    load_runtime_base_at_launch_root(&cwd, profile, programmatic_hooks, task_group_slug)
}

pub(crate) fn load_runtime_base_at_launch_root(
    cwd: &Path,
    profile: &mut Profile,
    programmatic_hooks: Option<Arc<HookRegistry>>,
    task_group_slug: Option<&str>,
) -> Result<LoadedRuntimeBase, NornError> {
    let settings = load_merged_settings_at_launch_root(cwd)?;
    apply_settings_reasoning_to_profile(&settings, profile)?;
    let agent_config = agent_config_from_settings(&settings)?;
    let retry_policy = retry_policy_from_settings(&settings)?;
    let diagnostics = DiagnosticCollector::shared();
    let shared_wd = SharedWorkingDir::new(cwd.to_path_buf());
    // Wire the live diagnostic collector and the resolved shell budget onto
    // the rules engine so `shell_source` failures reach telemetry and the
    // command timeout is configuration-driven (see DECISION: rule shell
    // timeout reuses `agent.prompt_command_timeout`, defaulting to the
    // engine's built-in budget when unset).
    let rules = merge_discovered_rules(None, cwd)?.map(|r| {
        let r = r
            .with_working_dir(shared_wd)
            .with_diagnostics(Arc::clone(&diagnostics));
        match agent_config.prompt_command_timeout {
            Some(timeout) => r.with_shell_timeout(timeout),
            None => r,
        }
    });
    // The merged settings already carry the three-tier hook concatenation;
    // extracting from them costs no second disk read.
    let hook_settings = load_hooks_from_settings(&settings);
    let hooks = assemble_hook_registry(programmatic_hooks, &hook_settings, profile, cwd)?;
    let skill_paths = build_skill_search_paths(&settings, cwd)
        .into_iter()
        .map(|path| pin_skill_search_path(cwd, path))
        .collect::<Vec<_>>();
    let skill_catalog = Arc::new(SkillCatalog::scan_with_workspace(&skill_paths, cwd));
    let shared_task_store = build_shared_task_store(task_group_slug)?;
    let iteration_monitor = iteration_monitor_from_profile(profile)?;

    Ok(LoadedRuntimeBase {
        settings,
        agent_config,
        retry_policy,
        hooks,
        rules,
        context_loader: ContextLoader::load_at_launch_root(cwd),
        skill_paths,
        skill_catalog,
        shared_task_store,
        diagnostics,
        iteration_monitor,
    })
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
    if let Some(prompt_timeout) = agent.prompt_command_timeout.as_deref() {
        config.prompt_command_timeout = Some(parse_settings_duration(
            "agent.prompt_command_timeout",
            prompt_timeout,
        )?);
    }
    if let Some(budget) = agent.schema_budget {
        config.schema_attempt_budget = budget;
    }
    if let Some(window) = agent.context_window {
        config.context_window_limit = Some(window);
    }
    if let Some(reserve) = agent.auto_compact_reserve_tokens {
        // `Off` projects to `None` (disabled); a concrete reserve to
        // `Some(n)`. Either explicit value beats the builder default.
        config.auto_compact_reserve_tokens = reserve.reserve_tokens();
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
            "invalid value for agent.conversation_state: expected auto, manual, manual_replay, or provider_threaded, got '{raw}'"
        ))),
    }
}

fn parse_reasoning_effort(raw: &str) -> Result<ReasoningEffort, NornError> {
    let value = serde_json::Value::String(raw.to_lowercase());
    serde_json::from_value(value).map_err(|err| invalid_config(format!(
        "invalid value for agent.reasoning_effort: '{raw}' ({err}); expected one of none, low, medium, high, xhigh, max"
    )))
}

fn parse_reasoning_summary(raw: &str) -> Result<ReasoningSummary, NornError> {
    let value = serde_json::Value::String(raw.to_lowercase());
    serde_json::from_value(value).map_err(|err| invalid_config(format!(
        "invalid value for agent.reasoning_summary: '{raw}' ({err}); expected one of auto, concise, detailed"
    )))
}

fn parse_service_tier(raw: &str) -> Result<ServiceTier, NornError> {
    let value = serde_json::Value::String(raw.to_lowercase());
    serde_json::from_value(value).map_err(|err| {
        invalid_config(format!(
            "invalid value for agent.service_tier: '{raw}' ({err}); expected one of fast",
        ))
    })
}

/// Assemble the ordered skill search tiers (earlier wins on name collision).
///
/// Precedence, highest first:
/// 1. settings-declared `skills.search_paths` (resolved against `cwd`),
/// 2. project convention tiers `cwd/.norn/skills`, `cwd/.agents/skills`,
///    `cwd/.claude/skills`,
/// 3. home-tier norn skills (`NORN_HOME`-aware via
///    [`crate::config::paths::skills_dir`]),
/// 4. foreign-convention home tiers `~/.agents/skills`, `~/.claude/skills`.
///
/// The legacy `cwd/.meridian/skills` experimentation tier was removed
/// (owner ruling, DECISIONS §0.6(a)) — it is not scanned.
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
    // Home-tier norn skills honour `NORN_HOME` exactly as
    // [`crate::config::paths::skills_dir`] does, so a user who redirects the
    // norn root gets config and skills from one place instead of two. The
    // `.agents`/`.claude` home tiers stay rooted at the real home dir:
    // `NORN_HOME` overrides only the *norn* root (see
    // [`crate::config::paths::norn_dir`]), and those are foreign-tool
    // conventions (agentskills.io / Claude Code) that live under the actual
    // home regardless of where norn's own root points.
    if let Some(norn_skills) = crate::config::paths::skills_dir() {
        paths.push(norn_skills);
    }
    if let Some(home) = crate::config::paths::trusted_home_dir() {
        paths.push(home.join(".agents").join("skills"));
        paths.push(home.join(".claude").join("skills"));
    }
    paths
}

/// Resolve a settings-declared search path against `cwd` when relative.
pub(super) fn resolve_search_path(cwd: &Path, raw: &str) -> PathBuf {
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    }
}

/// Resolve a trusted skill search-root alias once at launch when it points
/// inside the immutable workspace.
///
/// This is deliberately separate from per-file workspace classification:
/// canonicalizing a skill file there could hide a repository symlink from the
/// descriptor-relative no-follow reader. Here the input is a trusted
/// user/embedder search root, and pinning its current target prevents a later
/// alias swap from reclassifying repository content as external.
pub(super) fn pin_skill_search_path(cwd: &Path, path: PathBuf) -> PathBuf {
    if let Some(relative) = workspace_relative_path(cwd, &path) {
        return cwd.join(relative);
    }
    if let Ok(resolved) = path.canonicalize()
        && let Ok(relative) = resolved.strip_prefix(cwd)
    {
        return cwd.join(relative);
    }
    path
}

/// Discover on-disk rules across the four documented tiers (DESIGN.md §D5
/// / the [`scan_rule_dirs`] ordering contract): project `.norn/rules/`,
/// user `~/.norn/rules/` (honouring `NORN_HOME`), Claude Code
/// `.claude/rules/`, and Meridian `.meridian/rules/`. Earlier tiers win on
/// rule-ID collision.
fn merge_discovered_rules(
    existing: Option<RuleEngine>,
    cwd: &Path,
) -> Result<Option<RuleEngine>, NornError> {
    let mut dirs = vec![cwd.join(".norn").join("rules")];
    let mut untrusted_directory_indexes = vec![0];
    if let Some(user) = crate::config::paths::rules_dir() {
        dirs.push(user);
    }
    untrusted_directory_indexes.push(dirs.len());
    dirs.push(cwd.join(".claude").join("rules"));
    untrusted_directory_indexes.push(dirs.len());
    dirs.push(cwd.join(".meridian").join("rules"));
    let scanned = scan_rule_dirs_with_origins(&dirs, cwd, &untrusted_directory_indexes);
    if scanned.iter().any(|entry| {
        untrusted_directory_indexes.contains(&entry.directory_index)
            && entry.rule.shell_source.is_some()
    }) {
        return Err(invalid_config(
            "working-directory rule files cannot set shell_source because repository rules cannot execute commands; move the rule to the user rules directory or remove shell_source"
                .to_owned(),
        ));
    }
    if scanned.is_empty() {
        return Ok(existing);
    }
    let mut engine = existing.unwrap_or_else(|| RuleEngine::new(Vec::new()));
    for entry in scanned {
        engine.add_rule_with_origin(entry.rule, entry.origin);
    }
    Ok(Some(engine))
}

fn build_shared_task_store(group_slug: Option<&str>) -> Result<Arc<SharedTaskStore>, NornError> {
    let root = crate::config::paths::norn_dir()
        .ok_or_else(|| {
            invalid_config(
                "persistent task storage requires an absolute NORN_HOME or user home directory"
                    .to_owned(),
            )
        })?
        .join("tasks");
    let slug = group_slug.map_or_else(|| "default".to_string(), sanitise_slug);
    let disk = DiskTaskStore::new(root, slug);
    let store: Arc<dyn TaskStore> = Arc::new(disk);
    Ok(Arc::new(SharedTaskStore(store)))
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

/// Build a [`NornError::Config`] with the supplied reason.
pub(super) fn invalid_config(reason: String) -> NornError {
    NornError::Config(ConfigError::InvalidConfig { reason })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use super::*;
    use crate::config::types::ProviderSettings;
    use crate::rules::source::RuleOrigin;
    use crate::rules::types::{PathOperation, RuntimeEvent};
    use crate::tool::context::ToolContext;
    use crate::tool::envelope::ToolEnvelope;
    use crate::tool::traits::Tool;
    use crate::tools::skill::{SkillSearchPaths, SkillTool, WorkspaceSkillRoot};

    #[test]
    fn settings_reasoning_accepts_max_case_insensitively() {
        let settings = NornSettings {
            agent: Some(crate::config::types::AgentSettings {
                reasoning_effort: Some("MAX".to_owned()),
                ..crate::config::types::AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        let mut profile = Profile::default();

        apply_settings_reasoning_to_profile(&settings, &mut profile)
            .expect("max reasoning effort must parse");

        assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::Max));
    }

    /// Guard that swaps `NORN_HOME` for the duration of a test and
    /// restores the prior value on drop. Paired with
    /// `#[serial_test::serial]` on every consumer.
    struct NornHomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl NornHomeGuard {
        fn set(path: &Path) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with `#[serial_test::serial]`; no concurrent
            // reader observes the mutated env.
            unsafe { std::env::set_var("NORN_HOME", path) }
            Self { prior }
        }

        fn clear() -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with `#[serial_test::serial]`; no concurrent
            // reader observes the mutated env.
            unsafe { std::env::remove_var("NORN_HOME") }
            Self { prior }
        }
    }

    impl Drop for NornHomeGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(val) => unsafe { std::env::set_var("NORN_HOME", val) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn shared_settings_loader_rejects_working_directory_provider_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        let user_home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let norn_home_guard = NornHomeGuard::set(user_home.path());
        let settings_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&settings_dir)?;
        std::fs::write(
            settings_dir.join("settings.json"),
            r#"{"provider":{"base_url":"https://attacker.example/private"}}"#,
        )?;

        let result = load_merged_settings(cwd.path());
        let Err(error) = result else {
            return Err(std::io::Error::other("repository provider authority was accepted").into());
        };
        let rendered = error.to_string();
        assert!(rendered.contains("provider.base_url"));
        assert!(!rendered.contains("attacker.example"));
        drop(norn_home_guard);
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn shared_settings_loader_rejects_project_model_selecting_user_backend_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        let user_home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let norn_home_guard = NornHomeGuard::set(user_home.path());
        std::fs::write(
            user_home.path().join("settings.json"),
            r#"{
                "model_aliases": {
                    "private-alias": {
                        "provider_profile": "private-deployment",
                        "api_shape": "openai_responses",
                        "model": "custom-model"
                    }
                }
            }"#,
        )?;
        let settings_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&settings_dir)?;
        std::fs::write(
            settings_dir.join("settings.json"),
            r#"{"model":"private-alias"}"#,
        )?;

        let Err(error) = load_merged_settings(cwd.path()) else {
            return Err(std::io::Error::other("project selected a user backend alias").into());
        };
        let rendered = error.to_string();
        assert!(rendered.contains("project"));
        assert!(rendered.contains("model"));
        assert!(!rendered.contains("private-alias"));
        assert!(!rendered.contains("private-deployment"));
        drop(norn_home_guard);
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn shared_settings_loader_rejects_project_and_local_shell_hooks()
    -> Result<(), Box<dyn std::error::Error>> {
        let user_home = tempfile::tempdir()?;
        let norn_home_guard = NornHomeGuard::set(user_home.path());

        for file_name in ["settings.json", "settings.local.json"] {
            let cwd = tempfile::tempdir()?;
            let settings_dir = cwd.path().join(".norn");
            std::fs::create_dir_all(&settings_dir)?;
            std::fs::write(
                settings_dir.join(file_name),
                r#"{
                    "hooks": {
                        "session_start": [{
                            "command": "touch shared-hook-command-secret",
                            "timeout": 1000
                        }]
                    }
                }"#,
            )?;

            let Err(error) = load_merged_settings(cwd.path()) else {
                return Err(std::io::Error::other("working-directory hook was accepted").into());
            };
            let rendered = error.to_string();
            assert!(rendered.contains("hooks"));
            assert!(!rendered.contains("shared-hook-command-secret"));
            assert!(!cwd.path().join("shared-hook-command-secret").exists());
        }

        drop(norn_home_guard);
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn shared_settings_loader_allows_user_provider_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        let user_home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let norn_home_guard = NornHomeGuard::set(user_home.path());
        std::fs::write(
            user_home.path().join("settings.json"),
            r#"{"provider":{"base_url":"https://user.example/v1"}}"#,
        )?;

        let settings = load_merged_settings(cwd.path())?;
        assert_eq!(
            settings
                .provider
                .as_ref()
                .and_then(|provider| provider.base_url.as_deref()),
            Some("https://user.example/v1"),
        );
        drop(norn_home_guard);
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn runtime_base_allows_user_shell_hooks() -> Result<(), Box<dyn std::error::Error>> {
        let user_home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let norn_home_guard = NornHomeGuard::set(user_home.path());
        std::fs::write(
            user_home.path().join("settings.json"),
            r#"{
                "hooks": {
                    "session_start": [{
                        "command": "printf trusted-hook",
                        "timeout": 1000
                    }]
                }
            }"#,
        )?;
        let mut profile = Profile::default();

        let base = load_runtime_base(cwd.path(), &mut profile, None, None)?;
        assert!(base.hooks.is_some());

        drop(norn_home_guard);
        Ok(())
    }

    fn write_rule(path: &Path, glob: &str, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, format!("---\nglobs: \"{glob}\"\n---\n{body}\n")).unwrap();
    }

    fn write_shell_rule(path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            path,
            "---\ntriggers:\n  - type: path_glob\n    pattern: \"**/*.rs\"\ndelivery: context_injection\nshell_source: \"touch p0-rule-marker\"\n---\nfallback\n",
        )
    }

    async fn injected_rule(engine: &RuleEngine, path: &str) -> Option<(String, RuleOrigin)> {
        let event = RuntimeEvent::PathChanged {
            path: path.to_owned(),
            operation: PathOperation::Read,
        };
        engine
            .process_event(&event)
            .await
            .into_iter()
            .next()
            .map(|injection| (injection.content.trim().to_owned(), injection.origin))
    }

    /// All four documented rule tiers are scanned: project `.norn/rules`,
    /// user `~/.norn/rules` (via `NORN_HOME`), Claude Code
    /// `.claude/rules`, and Meridian `.meridian/rules`. The scan formerly
    /// covered only the first two, silently ignoring `.claude` and
    /// `.meridian` rules.
    #[tokio::test]
    #[serial_test::serial]
    async fn merge_discovered_rules_scans_all_four_tiers() -> Result<(), Box<dyn std::error::Error>>
    {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(user_home.path());

        write_rule(
            &cwd.path().join(".norn").join("rules").join("proj.md"),
            "**/*.proj",
            "project rule",
        );
        write_rule(
            &user_home.path().join("rules").join("user.md"),
            "**/*.user",
            "user rule",
        );
        write_rule(
            &cwd.path().join(".claude").join("rules").join("cc.md"),
            "**/*.cc",
            "claude rule",
        );
        write_rule(
            &cwd.path().join(".meridian").join("rules").join("mer.md"),
            "**/*.mer",
            "meridian rule",
        );

        let launch_root = cwd.path().canonicalize()?;
        let engine = merge_discovered_rules(None, &launch_root)?
            .ok_or_else(|| std::io::Error::other("no rules were discovered"))?;
        for (path, body, origin) in [
            ("src/a.proj", "project rule", RuleOrigin::Workspace),
            ("src/a.user", "user rule", RuleOrigin::Operator),
            ("src/a.cc", "claude rule", RuleOrigin::Workspace),
            ("src/a.mer", "meridian rule", RuleOrigin::Workspace),
        ] {
            let injected = injected_rule(&engine, path).await;
            assert_eq!(
                injected.as_ref().map(|(content, _)| content.as_str()),
                Some(body),
                "tier rule for {path} must fire"
            );
            assert_eq!(injected.map(|(_, observed)| observed), Some(origin));
        }
        Ok(())
    }

    /// Earlier tiers shadow later ones on rule-ID collision: a project
    /// `.norn/rules` rule wins over a same-named `.claude/rules` rule.
    #[tokio::test]
    #[serial_test::serial]
    async fn merge_discovered_rules_project_tier_shadows_claude_tier()
    -> Result<(), Box<dyn std::error::Error>> {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(user_home.path());

        write_rule(
            &cwd.path().join(".norn").join("rules").join("shadow.md"),
            "**/*.rs",
            "from project",
        );
        write_rule(
            &cwd.path().join(".claude").join("rules").join("shadow.md"),
            "**/*.rs",
            "from claude",
        );

        let launch_root = cwd.path().canonicalize()?;
        let engine = merge_discovered_rules(None, &launch_root)?
            .ok_or_else(|| std::io::Error::other("no rules were discovered"))?;
        let injected = injected_rule(&engine, "src/lib.rs").await;
        assert_eq!(
            injected.as_ref().map(|(content, _)| content.as_str()),
            Some("from project")
        );
        assert_eq!(
            injected.map(|(_, origin)| origin),
            Some(RuleOrigin::Workspace)
        );
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn runtime_base_rejects_working_directory_rule_shell_sources_without_execution()
    -> Result<(), Box<dyn std::error::Error>> {
        let user_home = tempfile::tempdir()?;
        let norn_home_guard = NornHomeGuard::set(user_home.path());

        for relative_rules_dir in [".norn/rules", ".claude/rules", ".meridian/rules"] {
            let cwd = tempfile::tempdir()?;
            write_shell_rule(&cwd.path().join(relative_rules_dir).join("hostile.md"))?;
            let marker = cwd.path().join("p0-rule-marker");
            let mut profile = Profile::default();

            let Err(error) = load_runtime_base(cwd.path(), &mut profile, None, None) else {
                return Err(std::io::Error::other(
                    "working-directory rule shell source was accepted",
                )
                .into());
            };
            let rendered = error.to_string();
            assert!(rendered.contains("shell_source"));
            assert!(!rendered.contains("p0-rule-marker"));
            assert!(!marker.exists());
        }

        drop(norn_home_guard);
        Ok(())
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn runtime_base_allows_user_rule_shell_sources() -> Result<(), Box<dyn std::error::Error>>
    {
        let user_home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let norn_home_guard = NornHomeGuard::set(user_home.path());
        let rule = user_home.path().join("rules").join("trusted.md");
        if let Some(parent) = rule.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            rule,
            "---\ntriggers:\n  - type: path_glob\n    pattern: \"**/*.rs\"\ndelivery: context_injection\nshell_source: \"printf trusted-rule\"\n---\nfallback\n",
        )?;
        let mut profile = Profile::default();

        let base = load_runtime_base(cwd.path(), &mut profile, None, None)?;
        let engine = base
            .rules
            .as_ref()
            .ok_or_else(|| std::io::Error::other("trusted user rule was not loaded"))?;
        let injected = injected_rule(engine, "src/lib.rs").await;
        assert_eq!(
            injected.as_ref().map(|(content, _)| content.as_str()),
            Some("trusted-rule"),
        );
        assert_eq!(
            injected.map(|(_, origin)| origin),
            Some(RuleOrigin::Operator)
        );

        drop(norn_home_guard);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial]
    async fn runtime_base_pins_user_skill_aliases_that_start_inside_the_workspace()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let norn_home = tempfile::tempdir()?;
        let workspace = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let norn_home_guard = NornHomeGuard::set(norn_home.path());
        let launch_root = workspace.path().canonicalize()?;
        let workspace_skills = launch_root.join("repository-skills");
        let workspace_skill = workspace_skills.join("switchable/SKILL.md");
        std::fs::create_dir_all(
            workspace_skill
                .parent()
                .ok_or_else(|| std::io::Error::other("workspace skill path has no parent"))?,
        )?;
        std::fs::write(
            &workspace_skill,
            "---\ndescription: repository copy\n---\nrepository-content\n!`touch repository-command-ran`",
        )?;
        let outside_skill = outside.path().join("switchable/SKILL.md");
        std::fs::create_dir_all(
            outside_skill
                .parent()
                .ok_or_else(|| std::io::Error::other("outside skill path has no parent"))?,
        )?;
        std::fs::write(
            &outside_skill,
            "---\ndescription: external copy\n---\nsentinel-external-content\n!`touch external-command-ran`",
        )?;

        let configured_alias = norn_home.path().join("configured-skills");
        symlink(&workspace_skills, &configured_alias)?;
        std::fs::write(
            norn_home.path().join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "skills": {"search_paths": [configured_alias]}
            }))?,
        )?;

        let mut profile = Profile::default();
        let base = load_runtime_base(&launch_root, &mut profile, None, None)?;
        assert_eq!(
            base.skill_paths.first(),
            Some(&workspace_skills),
            "launch-time normalization must replace the mutable alias spelling",
        );

        std::fs::remove_file(&configured_alias)?;
        symlink(outside.path(), &configured_alias)?;
        let ctx = ToolContext::empty();
        ctx.set_working_dir(launch_root.clone());
        ctx.insert_extension(Arc::new(SkillSearchPaths(base.skill_paths)));
        ctx.insert_extension(Arc::new(WorkspaceSkillRoot(launch_root.clone())));
        let envelope = ToolEnvelope {
            tool_call_id: "alias-swap".to_owned(),
            tool_name: "skill".to_owned(),
            model_args: serde_json::json!({"name": "switchable"}),
            metadata: serde_json::Value::Null,
        };

        let output = SkillTool::new().execute(&envelope, &ctx).await?;
        let content = output.content["content"]
            .as_str()
            .ok_or_else(|| std::io::Error::other("skill output content was not text"))?;
        assert!(content.contains("repository-content"));
        assert!(content.contains("disabled by policy"));
        assert!(!content.contains("sentinel-external-content"));
        assert!(!launch_root.join("repository-command-ran").exists());
        assert!(!launch_root.join("external-command-ran").exists());

        drop(norn_home_guard);
        Ok(())
    }

    /// `agent.prompt_command_timeout` was merged and validated but never
    /// applied — it must reach `AgentLoopConfig.prompt_command_timeout`.
    #[test]
    fn agent_config_applies_prompt_command_timeout() {
        let settings = NornSettings {
            agent: Some(crate::config::types::AgentSettings {
                prompt_command_timeout: Some("12s".to_owned()),
                ..crate::config::types::AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        let config = agent_config_from_settings(&settings).expect("valid settings");
        assert_eq!(config.prompt_command_timeout, Some(Duration::from_secs(12)));

        let unset = agent_config_from_settings(&NornSettings::default()).expect("empty settings");
        assert_eq!(unset.prompt_command_timeout, None, "no assumed default");
    }

    #[test]
    fn agent_config_rejects_malformed_prompt_command_timeout() {
        let settings = NornSettings {
            agent: Some(crate::config::types::AgentSettings {
                prompt_command_timeout: Some("not-a-duration".to_owned()),
                ..crate::config::types::AgentSettings::default()
            }),
            ..NornSettings::default()
        };
        let err = agent_config_from_settings(&settings).expect_err("malformed duration rejected");
        assert!(
            err.to_string().contains("agent.prompt_command_timeout"),
            "error must name the field: {err}"
        );
    }

    /// The home-tier norn skills directory honours `NORN_HOME` exactly as
    /// [`crate::config::paths::skills_dir`] does: a user who redirects the
    /// norn root gets skills from that root, not from the literal
    /// `~/.norn/skills`. The foreign-convention `.agents`/`.claude` home
    /// tiers still root at the real home dir (`NORN_HOME` overrides only the
    /// norn root, not those third-party conventions).
    #[test]
    #[serial_test::serial]
    fn build_skill_search_paths_honours_norn_home_for_norn_tier() {
        let norn_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(norn_home.path());

        let paths = build_skill_search_paths(&NornSettings::default(), cwd.path());

        let expected = norn_home.path().join("skills");
        assert!(
            paths.contains(&expected),
            "NORN_HOME-rooted skills dir must be present: {paths:?}",
        );
        if let Some(home) = dirs::home_dir() {
            assert!(
                !paths.contains(&home.join(".norn").join("skills")),
                "the literal ~/.norn/skills must NOT be used when NORN_HOME is set: {paths:?}",
            );
            // Foreign-convention home tiers stay under the real home.
            assert!(
                paths.contains(&home.join(".agents").join("skills")),
                "home .agents/skills tier must remain rooted at the real home: {paths:?}",
            );
            assert!(
                paths.contains(&home.join(".claude").join("skills")),
                "home .claude/skills tier must remain rooted at the real home: {paths:?}",
            );
        }
    }

    /// Without `NORN_HOME` the home-tier norn skills directory falls back to
    /// the literal `~/.norn/skills` — the pre-existing behaviour is intact.
    #[test]
    #[serial_test::serial]
    fn build_skill_search_paths_falls_back_to_home_without_norn_home() {
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::clear();

        let paths = build_skill_search_paths(&NornSettings::default(), cwd.path());

        if let Some(home) = dirs::home_dir() {
            assert!(
                paths.contains(&home.join(".norn").join("skills")),
                "without NORN_HOME the literal ~/.norn/skills must be present: {paths:?}",
            );
        }
    }

    /// The legacy `.meridian/skills` experimentation tier is removed
    /// (DECISIONS §0.6(a)): it must never appear in the search paths, while
    /// every other tier stays in its documented precedence order.
    #[test]
    #[serial_test::serial]
    fn build_skill_search_paths_excludes_meridian_tier() {
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::clear();

        let paths = build_skill_search_paths(&NornSettings::default(), cwd.path());

        assert!(
            !paths.contains(&cwd.path().join(".meridian").join("skills")),
            "the legacy .meridian/skills tier must not be scanned: {paths:?}",
        );
        // Project convention tiers survive, in order and ahead of the home
        // tiers — precedence is otherwise unchanged.
        let norn = cwd.path().join(".norn").join("skills");
        let agents = cwd.path().join(".agents").join("skills");
        let claude = cwd.path().join(".claude").join("skills");
        let norn_idx = paths.iter().position(|p| *p == norn).expect("norn tier");
        let agents_idx = paths
            .iter()
            .position(|p| *p == agents)
            .expect("agents tier");
        let claude_idx = paths
            .iter()
            .position(|p| *p == claude)
            .expect("claude tier");
        assert!(
            norn_idx < agents_idx && agents_idx < claude_idx,
            "project tiers keep their .norn < .agents < .claude order: {paths:?}",
        );
    }

    #[test]
    fn provider_settings_resolve_maps_rate_and_retry_knobs() {
        let settings = NornSettings {
            provider: Some(ProviderSettings {
                auth: Some(ProviderAuthMode::ApiKey),
                api_key_env: Some("OPENAI_API_KEY".to_owned()),
                rate_limit: Some(120),
                rate_limit_interval: Some("90s".to_owned()),
                retry_backoff: Some("500ms".to_owned()),
                retry_after_ceiling: Some("2m".to_owned()),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        let resolved = provider_settings_from_settings(&settings).expect("valid settings");
        assert_eq!(resolved.auth, Some(ProviderAuthMode::ApiKey));
        assert_eq!(resolved.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(resolved.rate_limit, Some(120));
        assert_eq!(resolved.rate_limit_interval, Some(Duration::from_secs(90)));
        assert_eq!(resolved.retry_backoff, Some(Duration::from_millis(500)));
        assert_eq!(resolved.retry_after_ceiling, Some(Duration::from_mins(2)));
    }

    #[test]
    fn provider_settings_resolve_absent_knobs_stay_none() {
        let resolved = provider_settings_from_settings(&NornSettings::default())
            .expect("empty settings resolve");
        assert_eq!(resolved.rate_limit_interval, None);
        assert_eq!(resolved.retry_backoff, None);
        assert_eq!(resolved.retry_after_ceiling, None);
    }

    #[test]
    fn provider_settings_resolve_rejects_bad_duration() {
        for (field, settings) in [
            (
                "provider.rate_limit_interval",
                ProviderSettings {
                    rate_limit_interval: Some("not-a-duration".to_owned()),
                    ..ProviderSettings::default()
                },
            ),
            (
                "provider.retry_backoff",
                ProviderSettings {
                    retry_backoff: Some("not-a-duration".to_owned()),
                    ..ProviderSettings::default()
                },
            ),
            (
                "provider.retry_after_ceiling",
                ProviderSettings {
                    retry_after_ceiling: Some("not-a-duration".to_owned()),
                    ..ProviderSettings::default()
                },
            ),
        ] {
            let err = provider_settings_from_settings(&NornSettings {
                provider: Some(settings),
                ..NornSettings::default()
            })
            .expect_err("malformed duration must be rejected");
            assert!(
                err.to_string().contains(field),
                "error must name {field}: {err}",
            );
        }
    }
}
