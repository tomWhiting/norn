//! Assembly fences for CLI flags that reach the library `AgentBuilder`
//! through [`builder_from_cli`](norn_cli::runtime::builder_from_cli):
//! `--workspace-root`, `--variables`, `--rules`, `--extension`, and the
//! settings/`-c` agent-config merge. These pin the flags that a Wave-6
//! adversarial review found silently ignored or regressed on the unified
//! assembly path.
//!
//! Every test isolates `HOME` / `NORN_HOME` (so no developer settings
//! leak in) and runs `#[serial]`, because several set the process working
//! directory (process-global) — serialising avoids racing that under
//! intra-binary test parallelism.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use clap::Parser;

use norn::agent::{Agent, AgentBuilder, AgentParts};
use norn::agent_loop::runner::ToolExecutor;
use norn::config::NornSettings;
use norn::error::NornError;
use norn::provider::mock::MockProvider;
use norn::provider::traits::Provider;

use norn_cli::cli::{BuildError, Cli};
use norn_cli::config::{
    apply_cli_profile_overrides, apply_settings_reasoning_to_profile, resolve_model_selection,
    resolve_profile,
};
use norn_cli::runtime::builder_from_cli;

fn mock_provider() -> Arc<dyn Provider> {
    Arc::new(MockProvider::new(Vec::new()))
}

/// The merged settings the assembly path consults, loaded exactly as the
/// CLI drivers do before `builder_from_cli` — from the current working
/// directory and the isolated `NORN_HOME` / `HOME`.
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

/// Resolve the profile pipeline and return the un-built builder (or the
/// `builder_from_cli` argument error, e.g. an empty `--extension` URI).
fn resolve_builder(cli: &Cli) -> Result<AgentBuilder, BuildError> {
    let settings = merged_settings();
    let mut profile = resolve_profile(cli.profile.as_deref()).expect("resolve profile");
    apply_settings_reasoning_to_profile(&settings, &mut profile).expect("settings reasoning");
    let applied = apply_cli_profile_overrides(cli, &mut profile).expect("cli overrides");
    let model_selection =
        resolve_model_selection(&profile.model, &settings).expect("model selection");
    profile.model = model_selection.model;
    builder_from_cli(cli, mock_provider(), profile, &settings, &applied)
}

/// Build to parts (asserting both `builder_from_cli` and `build` succeed).
fn build_parts(cli: &Cli) -> AgentParts {
    resolve_builder(cli)
        .expect("builder_from_cli succeeds")
        .build()
        .expect("build succeeds")
        .into_parts()
}

/// Build, surfacing the `build()` result (for the workspace-root error
/// cases, where `builder_from_cli` succeeds but `build()` validates).
fn build_result(cli: &Cli) -> Result<Agent, NornError> {
    resolve_builder(cli)
        .expect("builder_from_cli succeeds")
        .build()
}

/// Run `body` under an isolated `HOME` / `NORN_HOME` and an empty
/// process working directory (so no project `.norn` perturbs assembly).
fn with_isolated_env(body: impl FnOnce()) {
    let home = tempfile::tempdir().expect("home tempdir");
    let workdir = tempfile::tempdir().expect("work tempdir");
    let home_path = home.path().to_path_buf();
    temp_env::with_vars(
        [
            ("NORN_HOME", Some(home_path.as_os_str())),
            ("HOME", Some(home_path.as_os_str())),
        ],
        || {
            std::env::set_current_dir(workdir.path()).expect("chdir into isolated workdir");
            body();
        },
    );
}

// ---------------------------------------------------------------------------
// Finding 1 (HIGH): `--workspace-root` must confine the shared ToolContext.
// ---------------------------------------------------------------------------

/// The flag lands on `ToolContext::workspace_root` (canonicalised), so the
/// file tools are confined — file-tool path confinement is not silently
/// dropped on the unified CLI assembly path.
#[test]
#[serial_test::serial]
fn workspace_root_flag_confines_shared_tool_context() {
    with_isolated_env(|| {
        let root = tempfile::tempdir().expect("root tempdir");
        let cli = Cli::parse_from([
            "norn",
            "-m",
            "gpt-5.5",
            "--no-session",
            "--workspace-root",
            root.path().to_str().unwrap(),
        ]);
        let parts = build_parts(&cli);
        let shared = parts
            .registry
            .shared_context()
            .expect("shared context present");
        assert_eq!(
            shared.workspace_root(),
            Some(std::fs::canonicalize(root.path()).unwrap().as_path()),
            "--workspace-root must land on ToolContext::workspace_root",
        );
    });
}

/// Without the flag, path resolution stays unconfined.
#[test]
#[serial_test::serial]
fn no_workspace_root_flag_leaves_tools_unconfined() {
    with_isolated_env(|| {
        let cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session"]);
        let parts = build_parts(&cli);
        let shared = parts
            .registry
            .shared_context()
            .expect("shared context present");
        assert!(
            shared.workspace_root().is_none(),
            "no --workspace-root leaves the tools unconfined",
        );
    });
}

/// A nonexistent `--workspace-root` is a hard build error, never a
/// silently-ignored confinement.
#[test]
#[serial_test::serial]
fn workspace_root_missing_directory_fails_build() {
    with_isolated_env(|| {
        let cli = Cli::parse_from([
            "norn",
            "-m",
            "gpt-5.5",
            "--no-session",
            "--workspace-root",
            "/no/such/workspace-root",
        ]);
        match build_result(&cli) {
            Ok(_) => panic!("expected a build error for a missing workspace root"),
            Err(NornError::Config(_)) => {}
            Err(other) => panic!("expected a config error, got: {other}"),
        }
    });
}

/// A `--workspace-root` pointing at a file (not a directory) is rejected.
#[test]
#[serial_test::serial]
fn workspace_root_file_fails_build() {
    with_isolated_env(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("plain-file");
        std::fs::write(&file, "x").unwrap();
        let cli = Cli::parse_from([
            "norn",
            "-m",
            "gpt-5.5",
            "--no-session",
            "--workspace-root",
            file.to_str().unwrap(),
        ]);
        match build_result(&cli) {
            Ok(_) => panic!("expected a build error for a non-directory workspace root"),
            Err(NornError::Config(err)) => {
                assert!(
                    err.to_string().contains("not a directory"),
                    "reason should name the non-directory: {err}",
                );
            }
            Err(other) => panic!("expected a config error, got: {other}"),
        }
    });
}

// ---------------------------------------------------------------------------
// Finding 2 (HIGH): `--variables` must not break persisted-session runs.
// ---------------------------------------------------------------------------

/// `--variables foo=bar` with the DEFAULT (persisted) session must build:
/// the pairs are keyed to the resolved persisted session id, so `build()`
/// never rejects a store carrying an independently-minted id.
#[tokio::test]
#[serial_test::serial]
async fn variables_with_default_persisted_session_builds() {
    let home = tempfile::tempdir().expect("home tempdir");
    let workdir = tempfile::tempdir().expect("work tempdir");
    let home_path = home.path().to_path_buf();
    // `temp_env` is sync; resolve the builder inside its scope, then run the
    // async variable resolution outside it.
    let parts = {
        let mut captured: Option<AgentParts> = None;
        temp_env::with_vars(
            [
                ("NORN_HOME", Some(home_path.as_os_str())),
                ("HOME", Some(home_path.as_os_str())),
            ],
            || {
                std::env::set_current_dir(workdir.path()).expect("chdir");
                // No `--no-session`: the default persisted session is opened,
                // which on the regressed path aborted with a session-id
                // mismatch.
                let cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--variables", "foo=bar"]);
                captured = Some(build_parts(&cli));
            },
        );
        captured.expect("parts captured")
    };

    let store = parts
        .loop_context
        .variables
        .as_ref()
        .expect("variable store installed");
    assert_eq!(
        store.resolve("foo").await.unwrap(),
        "bar",
        "the --variables pair resolves against the persisted-session store",
    );
    assert!(
        !parts.info.session_id.is_empty(),
        "a persisted session id is resolved",
    );
    assert_eq!(
        store.session_id(),
        parts.info.session_id,
        "the variable store shares the resolved persisted session id",
    );
}

// ---------------------------------------------------------------------------
// Finding 6 (LOW): `--extension` URIs must be validated (empty is an error).
// ---------------------------------------------------------------------------

/// An empty `--extension` URI is a hard argument error, matching main and
/// the brief's non-empty-URI requirement (not silently accepted).
#[test]
#[serial_test::serial]
fn empty_extension_uri_is_argument_error() {
    with_isolated_env(|| {
        let cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session", "--extension", ""]);
        match resolve_builder(&cli) {
            Ok(_) => panic!("expected an argument error for an empty --extension URI"),
            Err(BuildError::Argument(_)) => {}
            Err(other) => panic!("expected an argument error, got: {other:?}"),
        }
    });
}

/// A non-empty `--extension` URI passes validation and assembly still
/// succeeds (the URI is carried past validation, not rejected).
#[test]
#[serial_test::serial]
fn valid_extension_uri_passes_validation() {
    with_isolated_env(|| {
        let cli = Cli::parse_from([
            "norn",
            "-m",
            "gpt-5.5",
            "--no-session",
            "--extension",
            "stdio://server",
        ]);
        // Assembles without error — the flag is validated, not a hard stop.
        let _ = build_parts(&cli);
    });
}

// ---------------------------------------------------------------------------
// Finding 4 (MEDIUM): an explicit `-c` value equal to the library default
// must still win over a differing settings value.
// ---------------------------------------------------------------------------

/// Project settings set `agent.schema_budget = 5`; `-c schema_budget=3`
/// (3 is the library default) must produce an assembled
/// `schema_attempt_budget = 3` — the explicit override is not reverted to
/// the settings value by a value-vs-default sentinel.
#[test]
#[serial_test::serial]
fn explicit_config_value_equal_to_default_beats_settings() {
    let home = tempfile::tempdir().expect("home tempdir");
    let workdir = tempfile::tempdir().expect("work tempdir");
    let home_path = home.path().to_path_buf();
    // Project settings with a non-default schema budget.
    let dotnorn = workdir.path().join(".norn");
    std::fs::create_dir_all(&dotnorn).unwrap();
    std::fs::write(
        dotnorn.join("settings.json"),
        r#"{ "agent": { "schema_budget": 5 } }"#,
    )
    .unwrap();

    temp_env::with_vars(
        [
            ("NORN_HOME", Some(home_path.as_os_str())),
            ("HOME", Some(home_path.as_os_str())),
        ],
        || {
            std::env::set_current_dir(workdir.path()).expect("chdir");
            // Sanity: without the override, settings win (budget = 5).
            let baseline = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session"]);
            assert_eq!(
                build_parts(&baseline).config.schema_attempt_budget,
                5,
                "settings agent.schema_budget=5 is the baseline",
            );
            // With `-c schema_budget=3` (the library default), the explicit
            // override must win — the regression reverted it back to 5.
            let overridden = Cli::parse_from([
                "norn",
                "-m",
                "gpt-5.5",
                "--no-session",
                "-c",
                "schema_budget=3",
            ]);
            assert_eq!(
                build_parts(&overridden).config.schema_attempt_budget,
                3,
                "-c schema_budget=3 (== default) must win over settings=5",
            );
        },
    );
}

// ---------------------------------------------------------------------------
// Finding 3 (HIGH): `--rules <file>` must load an explicit rule engine onto
// the loop context (not be silently ignored).
// ---------------------------------------------------------------------------

/// The rules YAML at `--rules` is parsed and lands on
/// `loop_context.rules`; a run with only auto-discovered rules would leave
/// it `None` in this hermetic environment, so a non-`None` engine proves
/// the explicit file was loaded.
#[test]
#[serial_test::serial]
fn rules_flag_loads_engine_onto_loop_context() {
    with_isolated_env(|| {
        let dir = tempfile::tempdir().expect("rules tempdir");
        let path = dir.path().join("coding.yaml");
        std::fs::write(
            &path,
            "---\nname: Rust\ntriggers:\n  - type: path_glob\n    pattern: \"**/*.rs\"\ndelivery: context_injection\n---\nUse yg diagnostics.",
        )
        .unwrap();
        let cli = Cli::parse_from([
            "norn",
            "-m",
            "gpt-5.5",
            "--no-session",
            "--rules",
            path.to_str().unwrap(),
        ]);
        assert!(
            build_parts(&cli).loop_context.rules.is_some(),
            "--rules must load a RuleEngine onto the loop context",
        );
    });
}

/// A nonexistent `--rules` path is a hard argument error at assembly, not a
/// silently unenforced guardrail.
#[test]
#[serial_test::serial]
fn missing_rules_file_is_argument_error() {
    with_isolated_env(|| {
        let cli = Cli::parse_from([
            "norn",
            "-m",
            "gpt-5.5",
            "--no-session",
            "--rules",
            "/no/such/rules.yaml",
        ]);
        match resolve_builder(&cli) {
            Ok(_) => panic!("expected an argument error for a missing --rules file"),
            Err(BuildError::Argument(reason)) => {
                assert!(reason.contains("rules.yaml"), "reason: {reason}");
            }
            Err(other) => panic!("expected an argument error, got: {other:?}"),
        }
    });
}
