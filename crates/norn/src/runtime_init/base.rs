//! Settings loading and runtime-base assembly shared by every launch path.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::config::{NornSettings, load_settings, merge_settings, validate_settings};
use crate::context::{ContextLoader, scan_rule_dirs};
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
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProviderSettingsResolved {
    /// Override for the provider's base URL.
    pub base_url: Option<String>,
    /// Per-request timeout.
    pub request_timeout: Option<Duration>,
    /// Maximum provider-level retries.
    pub max_retries: Option<u32>,
    /// Provider-specific options passed through verbatim.
    pub provider_options: Option<serde_json::Value>,
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

/// Load and validate user/project/local settings from the supplied CWD.
///
/// # Errors
///
/// Returns [`NornError::Config`] when a settings layer fails to load or the
/// merged settings fail validation.
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

/// Resolve provider settings using the same settings layer as CLI runtime assembly.
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
    let settings = load_merged_settings(cwd)?;
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
    let rules = merge_discovered_rules(None, cwd).map(|r| {
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
        "invalid value for agent.reasoning_effort: '{raw}' ({err}); expected one of none, low, medium, high, xhigh"
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

/// Resolve a settings-declared search path against `cwd` when relative.
pub(super) fn resolve_search_path(cwd: &Path, raw: &str) -> PathBuf {
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    }
}

/// Discover on-disk rules across the four documented tiers (DESIGN.md §D5
/// / the [`scan_rule_dirs`] ordering contract): project `.norn/rules/`,
/// user `~/.norn/rules/` (honouring `NORN_HOME`), Claude Code
/// `.claude/rules/`, and Meridian `.meridian/rules/`. Earlier tiers win on
/// rule-ID collision.
fn merge_discovered_rules(existing: Option<RuleEngine>, cwd: &Path) -> Option<RuleEngine> {
    let mut dirs = vec![cwd.join(".norn").join("rules")];
    if let Some(user) = crate::config::paths::rules_dir() {
        dirs.push(user);
    }
    dirs.push(cwd.join(".claude").join("rules"));
    dirs.push(cwd.join(".meridian").join("rules"));
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
    use crate::rules::types::{PathOperation, RuntimeEvent};

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
    }

    impl Drop for NornHomeGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(val) => unsafe { std::env::set_var("NORN_HOME", val) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    fn write_rule(path: &Path, glob: &str, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, format!("---\nglobs: \"{glob}\"\n---\n{body}\n")).unwrap();
    }

    async fn injected_content(engine: &RuleEngine, path: &str) -> Option<String> {
        let event = RuntimeEvent::PathChanged {
            path: path.to_owned(),
            operation: PathOperation::Read,
        };
        engine
            .process_event(&event)
            .await
            .into_iter()
            .next()
            .map(|inj| inj.content.trim().to_owned())
    }

    /// All four documented rule tiers are scanned: project `.norn/rules`,
    /// user `~/.norn/rules` (via `NORN_HOME`), Claude Code
    /// `.claude/rules`, and Meridian `.meridian/rules`. The scan formerly
    /// covered only the first two, silently ignoring `.claude` and
    /// `.meridian` rules.
    #[tokio::test]
    #[serial_test::serial]
    async fn merge_discovered_rules_scans_all_four_tiers() {
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

        let engine = merge_discovered_rules(None, cwd.path()).expect("rules discovered");
        for (path, body) in [
            ("src/a.proj", "project rule"),
            ("src/a.user", "user rule"),
            ("src/a.cc", "claude rule"),
            ("src/a.mer", "meridian rule"),
        ] {
            let content = injected_content(&engine, path).await;
            assert_eq!(
                content.as_deref(),
                Some(body),
                "tier rule for {path} must fire"
            );
        }
    }

    /// Earlier tiers shadow later ones on rule-ID collision: a project
    /// `.norn/rules` rule wins over a same-named `.claude/rules` rule.
    #[tokio::test]
    #[serial_test::serial]
    async fn merge_discovered_rules_project_tier_shadows_claude_tier() {
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

        let engine = merge_discovered_rules(None, cwd.path()).expect("rules discovered");
        let content = injected_content(&engine, "src/lib.rs").await;
        assert_eq!(content.as_deref(), Some("from project"));
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

    #[test]
    fn provider_settings_resolve_maps_rate_and_retry_knobs() {
        let settings = NornSettings {
            provider: Some(ProviderSettings {
                rate_limit: Some(120),
                rate_limit_interval: Some("90s".to_owned()),
                retry_backoff: Some("500ms".to_owned()),
                retry_after_ceiling: Some("2m".to_owned()),
                ..ProviderSettings::default()
            }),
            ..NornSettings::default()
        };
        let resolved = provider_settings_from_settings(&settings).expect("valid settings");
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
