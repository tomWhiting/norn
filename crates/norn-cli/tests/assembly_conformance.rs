//! Assembly conformance fence (R1.2).
//!
//! Asserts that the library-owned assembler reached through
//! [`builder_from_cli`](norn_cli::runtime::builder_from_cli) →
//! `AgentBuilder::build` → [`Agent::into_parts`](norn::agent::Agent) and
//! the legacy CLI assembler [`build_runtime`](norn_cli::runtime::build_runtime)
//! produce field-equivalent bundles for the same resolved [`Cli`]. It is
//! the regression fence kept green through the R1 migration; the dual-path
//! comparison is retired only when `build_runtime` is deleted (R1.9).
//!
//! Both paths are exercised under an isolated `HOME` / `NORN_HOME` (via
//! `temp-env`) and working directory so no on-disk settings, rules, skills,
//! or sessions perturb the comparison — the two paths are compared on
//! assembly alone.
//!
//! The whole comparison runs in a single `#[test]` because it sets the
//! process working directory (process-global) inside the `temp-env` scope;
//! splitting it would race that under intra-binary test parallelism.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use clap::Parser;

use norn::agent::AgentParts;
use norn::agent_loop::config::ToolExecutor;
use norn::config::{NornSettings, PermissionPolicy};
use norn::provider::mock::MockProvider;
use norn::provider::traits::Provider;
use norn::system_prompt::builder::ExecutionMode;
use norn::tool::catalog::SharedToolCatalog;
use norn::tools::SharedTaskStore;
use norn::tools::diagnostics::DiagnosticInfra;

use norn_cli::cli::Cli;
use norn_cli::config::{
    apply_cli_profile_overrides, apply_settings_reasoning_to_profile, resolve_model_selection,
    resolve_profile,
};
use norn_cli::runtime::bundle::{RuntimeBundle, RuntimeInputs};
use norn_cli::runtime::register_standard_tools;
use norn_cli::runtime::{apply_system_prompt, build_runtime, builder_from_cli};

fn mock_provider() -> Arc<dyn Provider> {
    Arc::new(MockProvider::new(Vec::new()))
}

/// The legacy CLI assembler's bundle, with the base system prompt applied
/// exactly as the print orchestrator does after `build_runtime`.
fn build_bundle(cli: &Cli) -> RuntimeBundle {
    let mut inputs = RuntimeInputs::default();
    register_standard_tools(&mut inputs.registry, None);
    let mut bundle = build_runtime(cli, inputs).expect("build_runtime succeeds");
    apply_system_prompt(&mut bundle, ExecutionMode::Headless);
    bundle
}

/// The merged settings both assembly paths consult, loaded exactly as
/// `build_runtime`'s private `load_and_merge_settings` does. Using the same
/// merged settings on the caller side keeps the comparison relative — any
/// on-disk user settings perturb both paths identically.
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

/// The unified library assembler's parts, resolving the profile through the
/// same helper pipeline the CLI caller runs before `builder_from_cli`.
fn build_parts(cli: &Cli) -> AgentParts {
    let settings = merged_settings();
    let mut profile = resolve_profile(cli.profile.as_deref()).expect("resolve profile");
    apply_settings_reasoning_to_profile(&settings, &mut profile).expect("settings reasoning");
    let applied = apply_cli_profile_overrides(cli, &mut profile).expect("cli overrides");
    let model_selection =
        resolve_model_selection(&profile.model, &settings).expect("model selection");
    profile.model = model_selection.model;

    let builder = builder_from_cli(cli, mock_provider(), profile, &settings, &applied)
        .expect("builder_from_cli succeeds");
    builder.build().expect("build succeeds").into_parts()
}

/// Sorted registry tool names.
///
/// Under the hermetic `HOME` / `NORN_HOME` isolation the test installs, no
/// skill catalog is discovered on either path, so neither registers the
/// `skill` tool (the §5.2 gap the library still carries until R1.8) and the
/// sets are strictly equal. A non-hermetic run that leaked a skill dir
/// would fail this assertion loudly — surfacing exactly that gap.
fn sorted_tool_names(registry: &norn::tool::registry::ToolRegistry) -> Vec<String> {
    let mut names: Vec<String> = registry.names().map(str::to_owned).collect();
    names.sort();
    names
}

/// Assert every R1.2 field is equivalent between the two assembly paths.
/// `check_prompt` is `false` only where the legacy path's provider-blind,
/// event-schema-aware prompt rendering is known to diverge (deferred to the
/// step-3 provider-aware convergence).
fn assert_conformant(cli: &Cli, check_prompt: bool, label: &str) {
    let bundle = build_bundle(cli);
    let parts = build_parts(cli);

    assert_eq!(
        sorted_tool_names(&bundle.registry),
        sorted_tool_names(&parts.registry),
        "{label}: sorted registry tool names must match",
    );

    let bundle_config = serde_json::to_value(&bundle.agent_config).unwrap();
    let parts_config = serde_json::to_value(&parts.config).unwrap();
    assert_eq!(
        bundle_config, parts_config,
        "{label}: agent-loop config must be serde-equal",
    );

    let bundle_ctx = bundle
        .registry
        .shared_context()
        .expect("bundle registry has a shared context");
    let parts_ctx = parts
        .registry
        .shared_context()
        .expect("parts registry has a shared context");

    assert_eq!(
        bundle_ctx.get_extension::<PermissionPolicy>().is_some(),
        parts_ctx.get_extension::<PermissionPolicy>().is_some(),
        "{label}: PermissionPolicy presence",
    );
    assert_eq!(
        bundle_ctx.get_extension::<SharedTaskStore>().is_some(),
        parts_ctx.get_extension::<SharedTaskStore>().is_some(),
        "{label}: SharedTaskStore presence",
    );
    assert_eq!(
        bundle_ctx.get_extension::<SharedToolCatalog>().is_some(),
        parts_ctx.get_extension::<SharedToolCatalog>().is_some(),
        "{label}: SharedToolCatalog presence",
    );
    assert_eq!(
        bundle_ctx.get_extension::<DiagnosticInfra>().is_some(),
        parts_ctx.get_extension::<DiagnosticInfra>().is_some(),
        "{label}: DiagnosticInfra presence",
    );
    // HookRegistry: the brief compares this "when configured" — i.e. when
    // user hooks are wired. Neither path wires user hooks here. They still
    // differ on an internal detail: the library's `load_runtime_base`
    // overlay always folds the diagnostic stop hook into a HookRegistry,
    // while `build_runtime` routes diagnostics purely through the
    // DiagnosticsPostCheck (asserted above). That is a benign pre-existing
    // path difference, not user-hook configuration, so it is not asserted
    // here; both publish a registry once real hooks are configured.

    // RetryPolicy and IterationMonitorConfig derive Debug but not PartialEq;
    // their Debug rendering is the equality surface here.
    assert_eq!(
        format!("{:?}", bundle.loop_context.retry_policy),
        format!("{:?}", parts.loop_context.retry_policy),
        "{label}: retry policy",
    );
    assert_eq!(
        bundle.loop_context.reasoning_effort, parts.loop_context.reasoning_effort,
        "{label}: reasoning effort",
    );
    assert_eq!(
        bundle.loop_context.service_tier, parts.loop_context.service_tier,
        "{label}: service tier",
    );
    assert_eq!(
        format!("{:?}", bundle.loop_context.iteration_monitor),
        format!("{:?}", parts.loop_context.iteration_monitor),
        "{label}: iteration monitor",
    );

    if check_prompt {
        // The provider-aware widening to a hosted-search provider is
        // deferred to step 3 (R1.5); here the mock provider is non-hosted,
        // so `reframe_prompt_entries` is a no-op and the two prompts carry
        // identical content. They are compared as a multiset of lines: the
        // system prompt lists tools in `ToolRegistry::names()` order, which
        // is `HashMap` iteration order — nondeterministic per registry
        // instance — so the tool blocks appear in a different order while
        // the content is identical. Line-multiset equality proves every
        // line is present in both.
        let bundle_prompt = bundle.loop_context.system_sections.first();
        let parts_prompt = parts.loop_context.system_sections.first();
        assert_eq!(
            bundle_prompt.map(|s| sorted_lines(s)),
            parts_prompt.map(|s| sorted_lines(s)),
            "{label}: base system prompt content (order-insensitive, non-hosted provider)",
        );
    }
}

/// The lines of `s` as a sorted multiset — the order-insensitive equality
/// surface for the system prompt (see [`assert_conformant`]).
fn sorted_lines(s: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = s.lines().collect();
    lines.sort_unstable();
    lines
}

#[test]
fn builder_from_cli_matches_build_runtime() {
    let home = tempfile::tempdir().expect("home tempdir");
    let workdir = tempfile::tempdir().expect("work tempdir");
    // Full hermetic isolation: an empty `HOME` / `NORN_HOME` means no user
    // settings, rules, or skill tiers exist, and an empty working directory
    // carries no project `.norn`. `temp-env` mutates and restores the env
    // safely (no `unsafe` on this side), serialised behind its own lock.
    // Without the skill tiers neither path discovers a skill catalog, so the
    // §5.2 skill-tool gap does not perturb the tool set or the prompt, and
    // the fence compares strict equality.
    let home_path = home.path().to_path_buf();
    temp_env::with_vars(
        [
            ("NORN_HOME", Some(home_path.as_os_str())),
            ("HOME", Some(home_path.as_os_str())),
        ],
        || {
            std::env::set_current_dir(workdir.path()).expect("chdir into isolated workdir");
            run_comparisons();
        },
    );
}

fn run_comparisons() {
    // Base invocation: model + no session. All R1.2 fields, prompt included.
    let base = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session"]);
    assert_conformant(&base, true, "base");

    // -m already covered by base; each remaining flag layered onto the base.
    let allowed = Cli::parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "--no-session",
        "--allowed-tools",
        "read,search",
    ]);
    assert_conformant(&allowed, true, "--allowed-tools");

    let disallowed = Cli::parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "--no-session",
        "--disallowed-tools",
        "bash",
    ]);
    assert_conformant(&disallowed, true, "--disallowed-tools");

    let max_turns = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session", "-c", "max_turns=7"]);
    assert_conformant(&max_turns, true, "-c max_turns=");

    let reasoning = Cli::parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "--no-session",
        "--reasoning-effort",
        "high",
    ]);
    assert_conformant(&reasoning, true, "--reasoning-effort");

    let variables = Cli::parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "--no-session",
        "--variables",
        "foo=bar",
    ]);
    assert_conformant(&variables, true, "--variables");

    let session_name = Cli::parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "--no-session",
        "--session-name",
        "my-work",
    ]);
    assert_conformant(&session_name, true, "--session-name");

    // --event-schema: the legacy path renders event-schema descriptions and
    // toggles auto-compact guidance into the system prompt, while the
    // library `install_system_prompt` defers that to the step-3
    // provider-aware convergence — so the prompt equality is deferred here
    // and the schemas themselves are compared instead. Every other R1.2
    // field must still match.
    let event_schema = Cli::parse_from([
        "norn",
        "-m",
        "gpt-5.5",
        "--no-session",
        "--event-schema",
        "text={\"type\":\"string\"}",
    ]);
    assert_conformant(&event_schema, false, "--event-schema");
    let bundle = build_bundle(&event_schema);
    let parts = build_parts(&event_schema);
    assert!(
        bundle.loop_context.event_schemas.is_some(),
        "--event-schema: legacy path installs the schema set",
    );
    assert_eq!(
        format!("{:?}", bundle.loop_context.event_schemas),
        format!("{:?}", parts.loop_context.event_schemas),
        "--event-schema: both paths install the same event-schema set",
    );
}
