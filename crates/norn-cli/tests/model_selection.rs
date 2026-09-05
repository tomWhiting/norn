//! CLI model transitions retain backend authority and complete context policy.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use norn::model_selection::{CatalogBackend, ModelRuntime};
use norn::profile::Profile;
use norn::provider::request::{ReasoningEffort, ServiceTier};
use norn::session::store::EventStore;
use norn_cli::cli::Cli;
use norn_cli::commands::slash::state::SlashStateSeed;
use norn_cli::commands::slash::{SlashState, build_slash_registry, dispatch_input};
use norn_cli::config::apply_cli_profile_overrides;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn state(window: Option<u64>) -> Result<SlashState, norn::error::ConfigError> {
    Ok(SlashState::new(SlashStateSeed {
        model_selection: ModelRuntime::new(
            Some(CatalogBackend::CODEX),
            "sol",
            window,
            Some(ReasoningEffort::Ultra),
            Some(ServiceTier::Fast),
            BTreeMap::new(),
        )?,
        output_schema: None,
        session_name: None,
        session_id: None,
        data_dir: std::env::temp_dir(),
        no_session: true,
        index_lock_deadline: Duration::from_secs(10),
        variable_pairs: Vec::new(),
        tools: Vec::new(),
        store: Arc::new(EventStore::new()),
    }))
}

#[test]
fn slash_switches_derived_window_and_clears_only_unsupported_policy() -> TestResult {
    let state = state(None)?;
    let registry = build_slash_registry(&state, None);
    dispatch_input("/model codex-spark", &registry)?;
    assert_eq!(state.model_snapshot(), "gpt-5.3-codex-spark");
    assert_eq!(state.model_selection.lock().window(), 128_000);
    assert_eq!(state.reasoning_effort_snapshot(), None);
    assert_eq!(state.service_tier_snapshot(), None);
    dispatch_input("/model astra", &registry)?;
    assert_eq!(state.model_snapshot(), "gpt-6-astra");
    assert_eq!(state.model_selection.lock().window(), 272_000);
    dispatch_input("/effort ultra", &registry)?;
    assert_eq!(
        state.reasoning_effort_snapshot(),
        Some(ReasoningEffort::Ultra)
    );
    dispatch_input("/model luna", &registry)?;
    assert_eq!(state.reasoning_effort_snapshot(), None);
    dispatch_input("/effort ultra", &registry)?;
    assert_eq!(state.reasoning_effort_snapshot(), None);
    Ok(())
}

#[test]
fn failed_slash_switch_retains_all_previous_policy() -> TestResult {
    let state = state(Some(272_000))?;
    let registry = build_slash_registry(&state, None);
    dispatch_input("/model codex-spark", &registry)?;
    assert_eq!(state.model_snapshot(), "gpt-5.6-sol");
    assert_eq!(state.model_selection.lock().window(), 272_000);
    assert_eq!(
        state.model_selection.lock().explicit_window(),
        Some(272_000)
    );
    assert_eq!(
        state.reasoning_effort_snapshot(),
        Some(ReasoningEffort::Ultra)
    );
    assert_eq!(state.service_tier_snapshot(), Some(ServiceTier::Fast));
    Ok(())
}

#[test]
fn ultra_cli_flag_maps_to_the_typed_profile_effort() -> TestResult {
    let cli = Cli::try_parse_from(["norn", "--model", "astra", "--reasoning-effort", "ultra"])?;
    let mut profile = Profile::default();
    apply_cli_profile_overrides(&cli, &mut profile)?;
    assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::Ultra));
    Ok(())
}

/// Run the actual CLI resolution and builder funnel with isolated user settings.
/// Return only the model state so all environment and directory restoration precedes assertions.
fn resolved_alias_fixture(
    second_alias: &serde_json::Value,
) -> Result<(String, ModelRuntime), Box<dyn std::error::Error>> {
    resolved_model_fixture(
        &serde_json::json!({
            "model_aliases": {"work": "local-v1", "local-v1": second_alias},
            "provider_profiles": {"second-provider": {"api_shape": "openai_responses"}},
            "agent": {"context_window": 272_000}
        }),
        "work",
    )
}

fn resolved_model_fixture(
    settings: &serde_json::Value,
    model: &str,
) -> Result<(String, ModelRuntime), Box<dyn std::error::Error>> {
    use norn::provider::mock::MockProvider;
    use norn_cli::runtime::{builder_from_cli, resolve_invocation};

    let user_config = tempfile::tempdir()?;
    let working_dir = tempfile::tempdir()?;
    let original_dir = std::env::current_dir()?;
    std::fs::write(
        user_config.path().join("settings.json"),
        serde_json::to_vec(settings)?,
    )?;
    temp_env::with_vars(
        [
            ("NORN_HOME", Some(user_config.path().as_os_str())),
            ("HOME", Some(user_config.path().as_os_str())),
        ],
        || -> Result<(String, ModelRuntime), Box<dyn std::error::Error>> {
            std::env::set_current_dir(working_dir.path())?;
            let result = (|| -> Result<(String, ModelRuntime), Box<dyn std::error::Error>> {
                let cli = Cli::try_parse_from(["norn", "--model", model, "--no-session"])?;
                let resolved = resolve_invocation(&cli)?;
                let resolved_model = resolved.profile.model.clone();
                let provider = Arc::new(MockProvider::new(Vec::new()));
                let mut parts = builder_from_cli(
                    &cli,
                    provider,
                    resolved.profile,
                    resolved.profile_source,
                    &resolved.settings,
                    &resolved.applied,
                )?
                .build()?
                .into_parts();
                assert_eq!(parts.model, resolved_model);
                parts
                    .model_selection
                    .bind_provider_profile(resolved.provider_profile);
                Ok((resolved_model, parts.model_selection))
            })();
            std::env::set_current_dir(original_dir)?;
            result
        },
    )
}

#[test]
#[serial_test::serial]
fn cli_resolved_alias_target_is_not_expanded_twice() -> TestResult {
    let (resolved_model, selection) = resolved_alias_fixture(&serde_json::json!("local-v2"))?;
    assert_eq!(resolved_model, "local-v1");
    assert_eq!(selection.model(), "local-v1");
    assert_eq!(selection.explicit_window(), Some(272_000));
    assert_eq!(selection.prepare("work")?.model(), "local-v1");
    assert_eq!(selection.prepare("local-v1")?.model(), "local-v2");
    Ok(())
}

#[test]
#[serial_test::serial]
fn second_alias_provider_profile_cannot_replace_first_alias_target() -> TestResult {
    let (resolved_model, selection) = resolved_alias_fixture(&serde_json::json!({
        "model": "local-v2", "provider_profile": "second-provider"
    }))?;
    assert_eq!(resolved_model, "local-v1");
    assert_eq!(selection.model(), "local-v1");
    assert_eq!(selection.prepare("work")?.model(), "local-v1");
    assert!(selection.prepare("local-v1").is_err());
    assert_eq!(selection.model(), "local-v1");
    Ok(())
}

#[test]
fn resolved_identity_still_checks_actual_backend_and_model_policy() {
    use norn::agent::AgentBuilder;
    use norn::provider::mock::MockProvider;

    let result = AgentBuilder::new(Arc::new(MockProvider::new(Vec::new())))
        .resolved_model("gpt-5.6-luna")
        .reasoning_effort(ReasoningEffort::Ultra)
        .working_dir(std::env::temp_dir())
        .build();
    assert!(matches!(result, Err(norn::error::NornError::Config(_))));
}

#[test]
#[serial_test::serial]
fn cli_user_alias_targeting_bundled_alias_resolves_at_startup_and_live() -> TestResult {
    let (resolved_model, selection) = resolved_model_fixture(
        &serde_json::json!({"model_aliases": {"fast": "codex-spark"}}),
        "fast",
    )?;
    assert_eq!(resolved_model, "gpt-5.3-codex-spark");
    assert_eq!(selection.model(), "gpt-5.3-codex-spark");
    assert_eq!(selection.prepare("fast")?.model(), "gpt-5.3-codex-spark");
    Ok(())
}

fn run_invalid_selection(
    args: &[&str],
    settings: &serde_json::Value,
) -> Result<String, Box<dyn std::error::Error>> {
    let user_config = tempfile::tempdir()?;
    let working_dir = tempfile::tempdir()?;
    std::fs::write(
        user_config.path().join("settings.json"),
        serde_json::to_vec(settings)?,
    )?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_norn"))
        .args(["-p", "-f", "json", "--no-session"])
        .args(args)
        .arg("hello")
        .env("NORN_HOME", user_config.path())
        .env("HOME", user_config.path())
        .env("NORN_SELECTION_FIXTURE_KEY", "fixture-key-not-used")
        .current_dir(working_dir.path())
        .stdin(std::process::Stdio::null())
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "argument refusal emitted an envelope: {:?}",
        output.stdout
    );
    assert_eq!(stderr.matches("norn:").count(), 1, "{stderr}");
    Ok(stderr)
}

#[test]
fn startup_settings_effort_refusal_names_missing_route_metadata() -> TestResult {
    let message = run_invalid_selection(
        &["--model", "gpt-5.5"],
        &serde_json::json!({
            "provider": {"auth": "api_key", "api_key_env": "NORN_SELECTION_FIXTURE_KEY"},
            "agent": {"reasoning_effort": "medium", "context_window": 272_000}
        }),
    )?;
    assert!(message.contains("agent.reasoning_effort"), "{message}");
    assert!(message.contains("openai.responses_api"), "{message}");
    assert!(
        message.contains("declares no capability metadata for model 'gpt-5.5'"),
        "{message}"
    );
    assert!(message.contains("reasoning effort 'medium'"), "{message}");
    assert!(!message.contains("is not supported"), "{message}");
    Ok(())
}

#[test]
fn startup_tier_and_window_refusals_are_argument_errors_without_envelopes() -> TestResult {
    for flag in [vec!["--service-tier", "fast"], vec!["--fast"]] {
        let mut args = vec!["--model", "gpt-5.4-mini"];
        args.extend_from_slice(&flag);
        let message =
            run_invalid_selection(&args, &serde_json::json!({"provider": {"auth": "oauth"}}))?;
        assert!(message.contains(flag[0]), "{message}");
        assert!(
            message.contains("service tier 'fast' is not supported"),
            "{message}"
        );
        assert!(message.contains("openai.codex_subscription"), "{message}");
    }
    for provider in ["openai", "openai-compatible"] {
        let message = run_invalid_selection(
            &["--provider", provider, "--model", "gpt-5.5"],
            &serde_json::json!({"provider": {"auth": "api_key", "api_key_env": "NORN_SELECTION_FIXTURE_KEY"}}),
        )?;
        assert!(
            message.contains("no context window is configured"),
            "{message}"
        );
        assert!(
            message.contains("declares no capability metadata"),
            "{message}"
        );
        assert!(!message.contains("typo"), "{message}");
    }
    Ok(())
}

#[test]
fn startup_known_model_effort_refusal_lists_declared_values_once() -> TestResult {
    let message = run_invalid_selection(
        &["--model", "gpt-5.6-luna", "--reasoning-effort", "ultra"],
        &serde_json::json!({"provider": {"auth": "oauth"}}),
    )?;
    assert!(message.contains("--reasoning-effort"));
    assert!(message.contains("declared values: low, medium, high, xhigh, max"));
    assert_eq!(message.matches("openai.codex_subscription").count(), 1);
    assert_eq!(message.matches("'ultra'").count(), 1);
    Ok(())
}

#[test]
fn refused_compaction_changing_slash_switch_keeps_previous_model_policy() -> TestResult {
    let state = state(None)?;
    state
        .model_selection
        .lock()
        .bind_compaction_reserve(Some(150_000));
    let registry = build_slash_registry(&state, None);
    dispatch_input("/model codex-spark", &registry)?;
    assert_eq!(state.model_snapshot(), "gpt-5.6-sol");
    assert_eq!(state.model_selection.lock().window(), 272_000);
    assert_eq!(
        state.reasoning_effort_snapshot(),
        Some(ReasoningEffort::Ultra)
    );
    assert_eq!(state.service_tier_snapshot(), Some(ServiceTier::Fast));
    dispatch_input("/model astra", &registry)?;
    assert_eq!(state.model_snapshot(), "gpt-6-astra");
    Ok(())
}

#[test]
fn actual_cli_help_only_advertises_available_efforts_and_rejects_none() -> TestResult {
    let user_config = tempfile::tempdir()?;
    let working_dir = tempfile::tempdir()?;
    let command = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_norn"))
            .args(args)
            .env("NORN_HOME", user_config.path())
            .env("HOME", user_config.path())
            .current_dir(working_dir.path())
            .stdin(std::process::Stdio::null())
            .output()
    };
    let help = command(&["--help"])?;
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout)?;
    let effort_help = text
        .split_once("--reasoning-effort")
        .and_then(|(_, suffix)| suffix.split_once("--service-tier"))
        .map(|(section, _)| section)
        .ok_or("CLI help omitted the reasoning-effort section")?;
    assert!(!effort_help.contains("none"), "{effort_help}");
    for value in ["low", "medium", "high", "xhigh", "max", "ultra"] {
        assert!(effort_help.contains(value), "{effort_help}");
        assert!(
            norn::model_catalog::catalog()
                .providers
                .iter()
                .any(|provider| {
                    provider.backends.iter().any(|backend| {
                        backend
                            .models
                            .iter()
                            .any(|model| model.supported_reasoning_efforts.contains(&value))
                    })
                }),
            "advertised effort '{value}' has no supporting route/model"
        );
    }
    let rejected = command(&["--reasoning-effort", "none", "-p", "-f", "json", "hello"])?;
    assert_eq!(rejected.status.code(), Some(2));
    assert!(rejected.stdout.is_empty());
    let error = String::from_utf8(rejected.stderr)?;
    assert!(error.contains("invalid value 'none'"), "{error}");
    assert!(error.contains("--reasoning-effort"), "{error}");
    Ok(())
}

#[test]
#[serial_test::serial]
fn cli_padded_operator_alias_matches_live_resolution() -> TestResult {
    let (resolved_model, selection) = resolved_model_fixture(
        &serde_json::json!({"model_aliases": {"fast": "codex-spark"}}),
        "  fast\t",
    )?;
    assert_eq!(resolved_model, "gpt-5.3-codex-spark");
    assert_eq!(selection.model(), "gpt-5.3-codex-spark");
    assert_eq!(
        selection.prepare("  fast\t")?.model(),
        "gpt-5.3-codex-spark"
    );
    Ok(())
}
