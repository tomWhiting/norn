//! Integration tests for NC-004 R8 end-to-end runtime assembly.
//!
//! Exercises `norn_cli::runtime::build_runtime` through the library
//! surface, covering the full flag-to-runtime pipeline that print mode
//! (NC-003) and the REPL (NC-005+) will rely on. Where a unit test in
//! `runtime.rs` covers a single field, these tests verify that several
//! flags layered together produce the expected combined state.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use norn::integration::DiagnosticCollector;
use norn::r#loop::runner::ToolExecutor;
use norn::profile::Profile;
use norn::tool::traits::Tool;
use norn_cli::cli::{BuildError, Cli, ExitCode};
use norn_cli::config::ConfigOverrides;
use norn_cli::runtime::{
    RuntimeInputs, build_runtime, build_write_tool, length_limit_from_profile,
};

fn cli(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).unwrap()
}

#[test]
fn full_pipeline_with_every_flag_lands_on_the_correct_field() {
    let dir = tempfile::tempdir().unwrap();
    let profile_path = dir.path().join("profile.toml");
    std::fs::write(
        &profile_path,
        r#"name = "p"
model = "profile-model"
system_instructions = ["from profile"]
"#,
    )
    .unwrap();

    let args = vec![
        "norn",
        "--profile",
        profile_path.to_str().unwrap(),
        "-m",
        "cli-model",
        "-S",
        "cli prompt",
        "--append-system-prompt",
        "extra",
        "--allowed-tools",
        "read,edit",
        "--reasoning-effort",
        "high",
        "--max-turns",
        "7",
        "--timeout",
        "45s",
        "-c",
        "schema_budget=8",
        "-c",
        "context_window=100000",
        "-c",
        "compact_threshold=0.5",
        "-c",
        "retry_max=3",
        "-c",
        "base_url=http://local",
        "--variables",
        "project=yggdrasil",
        "--variables",
        "env=staging",
        "-e",
        "stdio://path",
    ];
    let bundle = build_runtime(&cli(&args), RuntimeInputs::default()).unwrap();

    // Model: CLI override wins over profile.
    assert_eq!(bundle.model, "cli-model");

    // System sections: cli prompt + appended snippet.
    let base = &bundle.loop_context.system_sections[0];
    assert!(base.contains("cli prompt"), "base: {base}");
    assert!(base.contains("extra"), "base: {base}");

    // Reasoning effort threaded through from_profile.
    assert_eq!(
        bundle.loop_context.reasoning_effort,
        Some(norn::provider::request::ReasoningEffort::High),
    );

    // AgentLoopConfig.
    assert_eq!(bundle.agent_config.max_iterations, Some(7));
    assert_eq!(
        bundle.agent_config.step_timeout,
        Some(Duration::from_secs(45))
    );
    assert_eq!(bundle.agent_config.schema_attempt_budget, 8);
    assert_eq!(bundle.agent_config.context_window_limit, Some(100_000));
    assert!((bundle.agent_config.auto_compact_threshold_pct.unwrap() - 0.5).abs() < f64::EPSILON);

    // Retry policy via -c retry_max (overrides the always-on default).
    assert_eq!(bundle.loop_context.retry_policy.max_retries, 3);

    // Provider override surface (NC-003 consumes this).
    assert_eq!(
        bundle.provider_overrides.base_url.as_deref(),
        Some("http://local")
    );

    // Variables and extensions and unconditional wiring.
    assert!(bundle.loop_context.variables.is_some());
    assert_eq!(bundle.extension_uris, vec!["stdio://path"]);
    assert!(bundle.loop_context.token_estimator.is_some());
    assert!(bundle.loop_context.context_edits.is_some());
}

#[test]
fn malformed_config_pair_reports_argument_error_with_exit_code_two() {
    let result = build_runtime(
        &cli(&["norn", "-c", "max_turns=abc"]),
        RuntimeInputs::default(),
    );
    match result {
        Ok(_) => panic!("expected Argument error"),
        Err(BuildError::Argument(reason)) => {
            assert!(reason.contains("max_turns"));
            assert_eq!(
                BuildError::Argument(reason).exit_code(),
                ExitCode::ArgumentError
            );
        }
        Err(other) => panic!("expected Argument, got {other:?}"),
    }
}

#[test]
fn unknown_config_key_does_not_error_or_panic() {
    let bundle = build_runtime(
        &cli(&["norn", "-c", "not_a_real_key=value"]),
        RuntimeInputs::default(),
    )
    .unwrap();
    // The unknown key is dropped; defaults are preserved.
    assert!(bundle.agent_config.step_timeout.is_none());
}

#[test]
fn profile_event_schemas_merge_with_cli_event_schemas_cli_wins() {
    let dir = tempfile::tempdir().unwrap();
    let profile_path = dir.path().join("profile.toml");
    std::fs::write(
        &profile_path,
        r#"name = "p"
model = "gpt-5"
system_instructions = []

[settings]
event_schemas = { text = { type = "object" } }
"#,
    )
    .unwrap();

    let bundle = build_runtime(
        &cli(&[
            "norn",
            "--profile",
            profile_path.to_str().unwrap(),
            "--event-schema",
            r#"text={"type":"string"}"#,
        ]),
        RuntimeInputs::default(),
    )
    .unwrap();
    let set = bundle.loop_context.event_schemas.as_ref().unwrap();
    let schema = set
        .get(norn::r#loop::event_schemas::EventType::Text)
        .unwrap();
    assert_eq!(schema, &serde_json::json!({"type": "string"}));
}

#[test]
fn disallowed_tools_carried_through_bundle_not_into_profile_tools() {
    let bundle = build_runtime(
        &cli(&[
            "norn",
            "--allowed-tools",
            "read",
            "--disallowed-tools",
            "write,edit",
        ]),
        RuntimeInputs::default(),
    )
    .unwrap();
    assert_eq!(
        bundle.disallowed_tools,
        vec!["write".to_owned(), "edit".to_owned()],
    );
}

#[test]
fn diagnostics_collector_is_constructed_and_accessible_via_bundle() {
    // NC-003 R3 acceptance: DiagnosticCollector is constructed and
    // accessible for draining. The bundle field must be a live Arc that
    // the caller can hand to RuntimePostValidateCheck implementations.
    let bundle = build_runtime(&cli(&["norn"]), RuntimeInputs::default()).unwrap();
    assert!(
        bundle.diagnostics.is_empty(),
        "freshly-built collector must be empty",
    );
    assert!(
        Arc::strong_count(&bundle.diagnostics) >= 1,
        "Arc must be live after build_runtime returns",
    );
}

#[test]
fn diagnostics_collector_is_wired_onto_loop_context_and_tool_context() {
    // NC-009 R1 acceptance: the bundle's collector, LoopContext::diagnostics,
    // and the registry's shared ToolContext extension must all be the same
    // Arc instance so push sites at any layer feed the sink the CLI drains.
    let bundle = build_runtime(&cli(&["norn"]), RuntimeInputs::default()).unwrap();

    let loop_arc = bundle
        .loop_context
        .diagnostics
        .as_ref()
        .expect("LoopContext::diagnostics must be Some after NC-009 wiring");
    assert!(
        Arc::ptr_eq(&bundle.diagnostics, loop_arc),
        "bundle.diagnostics and loop_context.diagnostics must share the same Arc",
    );

    let shared = bundle
        .registry
        .shared_context()
        .expect("ToolRegistry must expose its shared ToolContext");
    let registry_arc = shared
        .get_extension::<DiagnosticCollector>()
        .expect("registry ToolContext must carry the collector as a typed extension");
    assert!(
        Arc::ptr_eq(&bundle.diagnostics, &registry_arc),
        "ToolContext extension and bundle.diagnostics must share the same Arc",
    );
}

#[test]
fn write_tool_length_limit_resolves_from_profile_section() {
    // NC-009 R2 acceptance: max_code_lines from the profile becomes
    // LengthLimit.default; length_overrides entries become glob/limit pairs
    // resolvable via limit_for.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile.toml");
    std::fs::write(
        &path,
        r#"name = "p"
model = "gpt-5"
system_instructions = []

[settings.tool_config.write]
max_code_lines = 500

[[settings.tool_config.write.length_overrides]]
pattern = "tests/**"
limit = 800
"#,
    )
    .unwrap();
    let profile = Profile::from_file(&path).unwrap();

    let limit = length_limit_from_profile(&profile, None).unwrap();
    assert_eq!(limit.default, Some(500));
    assert_eq!(
        limit.limit_for(Path::new("tests/foo.rs")),
        Some(800),
        "glob override must apply to matching paths",
    );
    assert_eq!(
        limit.limit_for(Path::new("src/lib.rs")),
        Some(500),
        "default must apply to non-matching paths",
    );
}

#[test]
fn write_tool_length_limit_is_none_without_tool_config() {
    let profile = Profile::default();
    let limit = length_limit_from_profile(&profile, None).unwrap();
    assert!(limit.default.is_none());
    assert!(limit.overrides.is_empty());
}

#[test]
fn write_tool_invalid_glob_pattern_surfaces_argument_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile.toml");
    std::fs::write(
        &path,
        r#"name = "p"
model = "gpt-5"
system_instructions = []

[[settings.tool_config.write.length_overrides]]
pattern = "[unterminated"
limit = 100
"#,
    )
    .unwrap();
    let profile = Profile::from_file(&path).unwrap();
    match length_limit_from_profile(&profile, None) {
        Ok(_) => panic!("expected Argument error for invalid glob pattern"),
        Err(BuildError::Argument(reason)) => {
            assert!(reason.contains("[unterminated"), "reason: {reason}");
            assert_eq!(
                BuildError::Argument(reason).exit_code(),
                ExitCode::ArgumentError,
            );
        }
        Err(other) => panic!("expected Argument, got {other:?}"),
    }
}

#[test]
fn write_tool_cli_override_replaces_profile_default_and_preserves_glob_overrides() {
    // NC-009 R3 acceptance: -c write.max_code_lines=N overrides the
    // profile's default while leaving glob overrides untouched.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile.toml");
    std::fs::write(
        &path,
        r#"name = "p"
model = "gpt-5"
system_instructions = []

[settings.tool_config.write]
max_code_lines = 500

[[settings.tool_config.write.length_overrides]]
pattern = "tests/**"
limit = 1500
"#,
    )
    .unwrap();
    let profile = Profile::from_file(&path).unwrap();
    let limit = length_limit_from_profile(&profile, Some(800)).unwrap();
    assert_eq!(limit.default, Some(800));
    assert_eq!(limit.limit_for(Path::new("tests/foo.rs")), Some(1500));
    assert_eq!(limit.limit_for(Path::new("src/lib.rs")), Some(800));
}

#[test]
fn write_tool_cli_override_alone_yields_default_and_no_overrides() {
    let profile = Profile::default();
    let limit = length_limit_from_profile(&profile, Some(800)).unwrap();
    assert_eq!(limit.default, Some(800));
    assert!(limit.overrides.is_empty());
}

#[test]
fn build_write_tool_threads_resolved_limit_via_config_overrides() {
    // Verification item: WriteTool::with_length_limit is called with the
    // resolved LengthLimit. We can't inspect the tool's private field
    // directly, but we can prove the helper is wired end-to-end by
    // verifying it constructs successfully under each branch.
    let profile = Profile::default();
    let overrides = ConfigOverrides::parse(&["write.max_code_lines=800".to_owned()]).unwrap();
    assert_eq!(overrides.write_max_code_lines, Some(800));
    let tool = build_write_tool(&profile, &overrides).expect("WriteTool must build");
    assert_eq!(tool.name(), "write");
}

#[test]
fn unknown_write_subkey_warns_and_leaves_resolution_unchanged() {
    // NC-009 R3 acceptance: -c write.unknown_key=foo is accepted by the
    // parser, emits a warning (tested at config.rs unit level), and does
    // not affect the resolved LengthLimit.
    let overrides = ConfigOverrides::parse(&["write.unknown_key=foo".to_owned()]).unwrap();
    assert!(overrides.write_max_code_lines.is_none());
    let profile = Profile::default();
    let limit = length_limit_from_profile(&profile, overrides.write_max_code_lines).unwrap();
    assert!(limit.default.is_none());
    assert!(limit.overrides.is_empty());
}

#[test]
fn iteration_monitor_profile_section_threads_into_loop_context() {
    // NC-003 R3 acceptance: with a profile [iteration_monitor] section,
    // LoopContext::iteration_monitor is Some and matches the supplied
    // values byte-for-byte.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile.toml");
    std::fs::write(
        &path,
        r#"name = "p"
model = "gpt-5"
system_instructions = []

[settings.iteration_monitor]
context_window_tokens = 200000
warn_threshold_pct = 0.75
handoff_threshold_pct = 0.90
handoff_guidance = "wrap up"
failure_repeat_window = 3
hedging_patterns = ["I cannot", "I'm unable"]
"#,
    )
    .unwrap();
    let bundle = build_runtime(
        &cli(&["norn", "--profile", path.to_str().unwrap()]),
        RuntimeInputs::default(),
    )
    .unwrap();
    let cfg = bundle
        .loop_context
        .iteration_monitor
        .as_ref()
        .expect("iteration_monitor must be wired from profile");
    assert_eq!(cfg.context_window_tokens, 200_000);
    assert!((cfg.warn_threshold_pct - 0.75).abs() < f64::EPSILON);
    assert!((cfg.handoff_threshold_pct - 0.90).abs() < f64::EPSILON);
    assert_eq!(cfg.handoff_guidance, "wrap up");
    assert_eq!(cfg.failure_repeat_window, 3);
    assert_eq!(cfg.hedging_patterns.len(), 2);
}

#[test]
fn iteration_monitor_absent_yields_none_on_loop_context() {
    let bundle = build_runtime(&cli(&["norn"]), RuntimeInputs::default()).unwrap();
    assert!(bundle.loop_context.iteration_monitor.is_none());
}

#[test]
fn slash_state_builder_seeds_loop_context_with_all_eleven_builtins() {
    use norn::session::store::EventStore;
    use norn_cli::commands::slash::CLI_BUILTIN_NAMES;
    use norn_cli::runtime::build_slash_state_from_bundle;

    let cli = cli(&["norn"]);
    let mut bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
    let store = Arc::new(EventStore::new());
    let (_state, registry) = build_slash_state_from_bundle(&cli, &bundle, store, None);
    bundle.loop_context.slash_commands = Some(registry);

    let installed = bundle
        .loop_context
        .slash_commands
        .as_ref()
        .expect("slash_commands must be installed after wiring");
    for name in CLI_BUILTIN_NAMES {
        assert!(installed.get(name).is_some(), "missing /{name}");
    }
    assert_eq!(installed.len(), CLI_BUILTIN_NAMES.len());
}

#[test]
fn rules_flag_with_valid_yaml_installs_rule_engine() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coding.yaml");
    std::fs::write(
        &path,
        "---\ntriggers:\n  - type: path_glob\n    pattern: \"**/*.rs\"\ndelivery: context_injection\n---\nbody",
    )
    .unwrap();
    let bundle = build_runtime(
        &cli(&["norn", "--rules", path.to_str().unwrap()]),
        RuntimeInputs::default(),
    )
    .unwrap();
    assert!(bundle.loop_context.rules.is_some());
}
