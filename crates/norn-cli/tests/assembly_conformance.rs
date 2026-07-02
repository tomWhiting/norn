//! Assembly golden-snapshot fence (R1.2 / R1.9).
//!
//! Asserts that the single library-owned assembler reached through
//! [`builder_from_cli`](norn_cli::runtime::builder_from_cli) →
//! `AgentBuilder::build` → [`Agent::into_parts`](norn::agent::Agent)
//! produces the expected assembled bundle for a fixed set of resolved
//! [`Cli`] invocations. It began (R1.2) as a dual-path comparison against
//! the legacy `build_runtime`; that path is deleted (R1.9), so this is now
//! a committed golden snapshot of the unified path's assembled fields.
//!
//! The comparison runs under an isolated `HOME` / `NORN_HOME` (via
//! `temp-env`) and working directory so no on-disk settings, rules,
//! skills, or sessions perturb the assertions — assembly alone is under
//! test. The whole check runs in a single `#[test]` because it sets the
//! process working directory (process-global) inside the `temp-env` scope;
//! splitting it would race that under intra-binary test parallelism.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use clap::Parser;

use norn::agent::child_policy::CoordinationEnvelope;
use norn::agent::registry::AgentRegistry;
use norn::agent::{AgentBuilder, AgentParts};
use norn::agent_loop::retry::{
    DEFAULT_BACKOFF_MULTIPLIER, DEFAULT_INITIAL_BACKOFF, DEFAULT_MAX_RETRIES,
};
use norn::agent_loop::runner::ToolExecutor;
use norn::config::NornSettings;
use norn::provider::mock::MockProvider;
use norn::provider::request::{ReasoningEffort, ServiceTier};
use norn::provider::traits::Provider;
use norn::tool::catalog::SharedToolCatalog;
use norn::tools::SharedTaskStore;
use norn::tools::diagnostics::DiagnosticInfra;

use norn_cli::cli::Cli;
use norn_cli::config::{
    apply_cli_profile_overrides, apply_settings_reasoning_to_profile, resolve_model_selection,
    resolve_profile,
};
use norn_cli::runtime::{DEFAULT_DELEGATION_DEPTH, builder_from_cli, cli_coordination_envelope};

fn mock_provider() -> Arc<dyn Provider> {
    Arc::new(MockProvider::new(Vec::new()))
}

/// The merged settings the assembly path consults, loaded exactly as the
/// caller does before `builder_from_cli`. Using the same merged settings
/// keeps the assertions relative to the hermetic environment.
fn merged_settings() -> NornSettings {
    let cwd = std::env::current_dir().expect("cwd");
    let mut layers = norn::config::load_settings(&cwd).expect("load settings");
    let mut cli_layer = NornSettings::default();
    let merged = norn::config::merge_settings(
        &mut layers.user,
        &mut layers.project,
        &mut layers.local,
        &mut cli_layer,
    );
    norn::config::validate_settings(&merged).expect("validate settings");
    merged
}

/// The unified library assembler's builder, resolving the profile through
/// the same helper pipeline the CLI drivers run before `builder_from_cli`.
/// Returned un-built so a caller can chain the driver-specific coordination
/// envelope before `build()`.
fn builder_for(cli: &Cli) -> AgentBuilder {
    let settings = merged_settings();
    let mut profile = resolve_profile(cli.profile.as_deref()).expect("resolve profile");
    apply_settings_reasoning_to_profile(&settings, &mut profile).expect("settings reasoning");
    let applied = apply_cli_profile_overrides(cli, &mut profile).expect("cli overrides");
    let model_selection =
        resolve_model_selection(&profile.model, &settings).expect("model selection");
    profile.model = model_selection.model;

    builder_from_cli(cli, mock_provider(), profile, &settings, &applied)
        .expect("builder_from_cli succeeds")
}

/// The unified library assembler's parts for a fixed CLI invocation.
fn build_parts(cli: &Cli) -> AgentParts {
    builder_for(cli)
        .build()
        .expect("build succeeds")
        .into_parts()
}

/// Sorted registry tool names.
fn sorted_tool_names(registry: &norn::tool::registry::ToolRegistry) -> Vec<String> {
    let mut names: Vec<String> = registry.names().map(str::to_owned).collect();
    names.sort();
    names
}

/// Assert the runtime-extension set the assembler unconditionally
/// publishes on the shared tool context (a `PermissionPolicy` is
/// installed only when permission rules are configured, so it is not
/// asserted in this hermetic environment).
fn assert_core_extensions(parts: &AgentParts, label: &str) {
    let ctx = parts
        .registry
        .shared_context()
        .expect("parts registry has a shared context");
    assert!(
        ctx.get_extension::<SharedTaskStore>().is_some(),
        "{label}: SharedTaskStore present",
    );
    assert!(
        ctx.get_extension::<SharedToolCatalog>().is_some(),
        "{label}: SharedToolCatalog present",
    );
    assert!(
        ctx.get_extension::<DiagnosticInfra>().is_some(),
        "{label}: DiagnosticInfra present",
    );
}

#[test]
fn builder_from_cli_golden_snapshot() {
    let home = tempfile::tempdir().expect("home tempdir");
    let workdir = tempfile::tempdir().expect("work tempdir");
    // Full hermetic isolation: an empty `HOME` / `NORN_HOME` means no user
    // settings, rules, or skill tiers exist, and an empty working directory
    // carries no project `.norn`. `temp-env` mutates and restores the env
    // safely (no `unsafe` on this side), serialised behind its own lock.
    let home_path = home.path().to_path_buf();
    temp_env::with_vars(
        [
            ("NORN_HOME", Some(home_path.as_os_str())),
            ("HOME", Some(home_path.as_os_str())),
        ],
        || {
            std::env::set_current_dir(workdir.path()).expect("chdir into isolated workdir");
            run_snapshot();
        },
    );
}

fn run_snapshot() {
    // Base invocation: model + no session. The full standard tool set is
    // present, and the core extensions are published.
    let base = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session"]);
    let base_parts = build_parts(&base);
    assert_core_extensions(&base_parts, "base");
    let base_tools = sorted_tool_names(&base_parts.registry);
    for expected in ["read", "write", "bash", "search"] {
        assert!(
            base_tools.iter().any(|name| name == expected),
            "base: standard tool set must include `{expected}` (got {base_tools:?})",
        );
    }
    // The base system prompt is the Norn runtime prompt with a real tools
    // section listing the assembled tools — not merely non-empty.
    let base_prompt = base_parts
        .loop_context
        .system_sections
        .first()
        .expect("base: a base system prompt section is assembled");
    for marker in ["# Norn Runtime", "# Tools", "**read**"] {
        assert!(
            base_prompt.contains(marker),
            "base: system prompt must contain `{marker}` (a real tools section \
             built from the gated registry), got:\n{base_prompt}",
        );
    }

    // Retry policy resolves to the runtime-base default in the hermetic
    // environment (no settings override the 2-retry / 1s / 2x policy).
    let retry = &base_parts.loop_context.retry_policy;
    assert_eq!(retry.max_retries, DEFAULT_MAX_RETRIES, "base: retry count");
    assert_eq!(
        retry.initial_backoff, DEFAULT_INITIAL_BACKOFF,
        "base: retry backoff",
    );
    assert!(
        (retry.backoff_multiplier - DEFAULT_BACKOFF_MULTIPLIER).abs() < f64::EPSILON,
        "base: retry multiplier",
    );

    // No `--fast` / `--service-tier` and no profile / settings tier: the
    // service tier resolves to `None`.
    assert_eq!(
        base_parts.loop_context.service_tier, None,
        "base: service tier resolves to None absent any tier config",
    );

    // No `[iteration_monitor]` profile section in the hermetic env: the
    // iteration monitor resolves to `None`.
    assert!(
        base_parts.loop_context.iteration_monitor.is_none(),
        "base: iteration monitor resolves to None absent a profile section",
    );

    // --fast resolves the service tier onto the loop context (positive
    // guard that the tier is wired, not merely defaulted to None).
    let fast = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session", "--fast"]);
    assert_eq!(
        build_parts(&fast).loop_context.service_tier,
        Some(ServiceTier::Fast),
        "--fast resolves the service tier onto the loop context",
    );

    // --allowed-tools gates the registry down to the named tools (plus any
    // always-on tools the profile keeps). `bash` is not named, so it is
    // gated out; the named tools survive.
    let allowed = Cli::parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "--no-session",
        "--allowed-tools",
        "read,search",
    ]);
    let allowed_tools = sorted_tool_names(&build_parts(&allowed).registry);
    assert!(
        allowed_tools.iter().any(|name| name == "read"),
        "--allowed-tools: `read` stays available (got {allowed_tools:?})",
    );
    assert!(
        allowed_tools.iter().any(|name| name == "search"),
        "--allowed-tools: `search` stays available (got {allowed_tools:?})",
    );
    assert!(
        !allowed_tools.iter().any(|name| name == "bash"),
        "--allowed-tools: `bash` is gated out (got {allowed_tools:?})",
    );

    // --disallowed-tools removes the named tool even from the default set
    // (deny wins).
    let disallowed = Cli::parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "--no-session",
        "--disallowed-tools",
        "bash",
    ]);
    let disallowed_tools = sorted_tool_names(&build_parts(&disallowed).registry);
    assert!(
        !disallowed_tools.iter().any(|name| name == "bash"),
        "--disallowed-tools: `bash` is denied (got {disallowed_tools:?})",
    );
    assert!(
        disallowed_tools.iter().any(|name| name == "read"),
        "--disallowed-tools: unrelated tools stay available (got {disallowed_tools:?})",
    );

    // -c max_turns=7 overlays the agent-loop config.
    let max_turns = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session", "-c", "max_turns=7"]);
    let max_turns_config = build_parts(&max_turns).config;
    let config_json = serde_json::to_value(&max_turns_config).expect("serialize config");
    assert_eq!(
        config_json.get("max_iterations"),
        Some(&serde_json::json!(7)),
        "-c max_turns=7 sets the agent-loop max_iterations (config: {config_json})",
    );

    // --reasoning-effort high lands on the loop context.
    let reasoning = Cli::parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "--no-session",
        "--reasoning-effort",
        "high",
    ]);
    assert_eq!(
        build_parts(&reasoning).loop_context.reasoning_effort,
        Some(ReasoningEffort::High),
        "--reasoning-effort high resolves onto the loop context",
    );

    // --variables is carried through assembly (the variable store is minted
    // and the run still assembles).
    let variables = Cli::parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "--no-session",
        "--variables",
        "foo=bar",
    ]);
    assert_core_extensions(&build_parts(&variables), "--variables");

    // --session-name assembles (the task-group slug derives from it); the
    // core extensions still publish.
    let session_name = Cli::parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "--no-session",
        "--session-name",
        "my-work",
    ]);
    assert_core_extensions(&build_parts(&session_name), "--session-name");

    // --event-schema installs the schema set on the loop context.
    let event_schema = Cli::parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "--no-session",
        "--event-schema",
        "text={\"type\":\"string\"}",
    ]);
    assert!(
        build_parts(&event_schema)
            .loop_context
            .event_schemas
            .is_some(),
        "--event-schema installs the event-schema set on the loop context",
    );

    // Coordination envelope + session-derived cache key. A session-backed
    // invocation with the CLI's coordination envelope chained (exactly as
    // the print driver does) publishes the envelope on the shared tool
    // context, wires the child-result receiver and root agent id, and keys
    // the prompt cache on the resolved session id.
    let coord_cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--session-name", "conf-coord"]);
    let envelope = cli_coordination_envelope(DEFAULT_DELEGATION_DEPTH);
    let coord_parts = builder_for(&coord_cli)
        .agent_registry(AgentRegistry::shared())
        .child_policy(envelope.child_policy.clone())
        .child_result_capacity(envelope.child_result_capacity)
        .register_root("/root".to_string(), "lead".to_string())
        .build()
        .expect("coordination build succeeds")
        .into_parts();

    let published = coord_parts
        .registry
        .shared_context()
        .expect("coord: shared tool context")
        .get_extension::<CoordinationEnvelope>()
        .expect("coord: CoordinationEnvelope published on the shared context");
    assert_eq!(
        *published, envelope,
        "coord: the published envelope carries the CLI's child policy and capacities",
    );
    assert!(
        coord_parts.loop_context.child_result_rx.is_some(),
        "coord: the child-result receiver is wired alongside the envelope",
    );
    assert!(
        coord_parts.loop_context.agent_id.is_some(),
        "coord: the root agent id is wired onto the loop context",
    );

    let session_id = coord_parts.info.session_id.clone();
    assert!(!session_id.is_empty(), "coord: a session id is resolved");
    assert_eq!(
        coord_parts.config.cache_key.as_deref(),
        Some(session_id.as_str()),
        "coord: open_session wires the resolved session id as the prompt cache_key",
    );
}
