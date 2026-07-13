//! Assembly fences for CLI flags that reach the library `AgentBuilder`
//! through [`resolve_invocation`](norn_cli::runtime::resolve_invocation) and
//! [`builder_from_cli`](norn_cli::runtime::builder_from_cli):
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
use norn_cli::runtime::{
    DEFAULT_DELEGATION_DEPTH, builder_from_cli, cli_coordination_envelope, resolve_invocation,
};

fn mock_provider() -> Arc<dyn Provider> {
    Arc::new(MockProvider::new(Vec::new()))
}

/// The merged settings the assembly path consults, loaded exactly as the
/// CLI drivers do before `builder_from_cli` — from the current working
/// directory and the isolated `NORN_HOME` / `HOME`.
fn merged_settings() -> Result<NornSettings, BuildError> {
    let cwd = std::env::current_dir()?;
    norn::runtime_init::load_merged_settings(&cwd)
        .map_err(|error| BuildError::Argument(error.to_string()))
}

/// Resolve the profile pipeline and return the un-built builder.
fn resolve_builder(cli: &Cli) -> Result<AgentBuilder, BuildError> {
    let settings = merged_settings()?;
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

/// An explicitly empty `--allowed-tools ""` builds a ZERO-tool agent —
/// a pure text-transform step (e.g. a TTS rewrite pipeline piping stdin
/// through the model with no tools at all). Regression for the former
/// "at least one tool" build rejection that the R1 unification surfaced
/// on the CLI path (owner decision 2026-07-02).
#[test]
#[serial_test::serial]
fn empty_allowed_tools_builds_zero_tool_transform_agent() {
    with_isolated_env(|| {
        let cli = Cli::parse_from([
            "norn",
            "-m",
            "gpt-5.5",
            "--no-session",
            "--allowed-tools",
            "",
        ]);
        let parts = build_parts(&cli);
        assert_eq!(
            parts.registry.names().count(),
            0,
            "--allowed-tools \"\" must gate out every tool",
        );
        let prompt = parts
            .loop_context
            .system_sections
            .first()
            .expect("system prompt section assembled");
        assert!(
            !prompt.contains("# Tools"),
            "zero-tool system prompt must omit the # Tools section",
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
// Nested delegation depth (DECISIONS §0.6(d)): default 2, configurable via
// the `[agent] delegation_depth` setting and `-c delegation_depth=<u32>`.
// ---------------------------------------------------------------------------

/// The root delegation depth resolves default-2, honours the `[agent]
/// delegation_depth` setting, and `-c delegation_depth` wins over both —
/// landing on `cli_coordination_envelope`'s root `remaining_depth`. A
/// default-2 root grants a child that can still spawn one level (a
/// grandchild leaf); `-c delegation_depth=1` restores leaf children.
#[test]
#[serial_test::serial]
fn delegation_depth_defaults_to_two_and_is_configurable() {
    with_isolated_env(|| {
        // Default: no flag, no settings → owner-ruled 2.
        let cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session"]);
        let resolved = resolve_invocation(&cli).expect("resolve default");
        assert_eq!(resolved.delegation_depth, DEFAULT_DELEGATION_DEPTH);
        assert_eq!(resolved.delegation_depth, 2, "owner-ruled default is 2");
        let envelope = cli_coordination_envelope(resolved.delegation_depth);
        assert_eq!(envelope.child_policy.delegation.remaining_depth, 2);
        // End-to-end: a child may spawn one level; the grandchild is a leaf.
        let child = envelope
            .child_policy
            .grant_for_child(None)
            .expect("root grants a child");
        assert_eq!(child.delegation.remaining_depth, 1);
        let grandchild = child
            .grant_for_child(None)
            .expect("child grants a grandchild");
        assert_eq!(grandchild.delegation.remaining_depth, 0);
        assert!(
            grandchild.grant_for_child(None).is_err(),
            "the grandchild is a leaf and cannot spawn",
        );

        // `-c delegation_depth=1` restores leaf children.
        let cli = Cli::parse_from([
            "norn",
            "-m",
            "gpt-5.5",
            "--no-session",
            "-c",
            "delegation_depth=1",
        ]);
        let resolved = resolve_invocation(&cli).expect("resolve -c");
        assert_eq!(resolved.delegation_depth, 1);
        let child = cli_coordination_envelope(resolved.delegation_depth)
            .child_policy
            .grant_for_child(None)
            .expect("root grants a child");
        assert_eq!(
            child.delegation.remaining_depth, 0,
            "-c=1 makes children leaves"
        );

        // The `[agent] delegation_depth` setting plumbs when no `-c`.
        let cwd = std::env::current_dir().expect("cwd");
        let norn_dir = cwd.join(".norn");
        std::fs::create_dir_all(&norn_dir).expect("create .norn");
        std::fs::write(
            norn_dir.join("settings.json"),
            r#"{"agent":{"delegation_depth":3}}"#,
        )
        .expect("write settings");
        let cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session"]);
        let resolved = resolve_invocation(&cli).expect("resolve settings");
        assert_eq!(resolved.delegation_depth, 3, "the settings key plumbs");

        // `-c` wins over the settings key.
        let cli = Cli::parse_from([
            "norn",
            "-m",
            "gpt-5.5",
            "--no-session",
            "-c",
            "delegation_depth=1",
        ]);
        let resolved = resolve_invocation(&cli).expect("resolve -c over settings");
        assert_eq!(
            resolved.delegation_depth, 1,
            "explicit -c wins over settings"
        );
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

/// An empty `--extension` URI is a hard error at the shared invocation
/// resolution boundary used by both production drivers.
#[test]
#[serial_test::serial]
fn empty_extension_uri_is_argument_error() {
    with_isolated_env(|| {
        let cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session", "--extension", ""]);
        match resolve_invocation(&cli) {
            Ok(_) => panic!("expected an argument error for an empty --extension URI"),
            Err(BuildError::Argument(_)) => {}
            Err(other) => panic!("expected an argument error, got: {other:?}"),
        }
    });
}

/// A non-empty `--extension` URI passes shared invocation resolution and the
/// resolved profile/settings continue through builder assembly.
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
        let resolved = resolve_invocation(&cli);
        assert!(resolved.is_ok(), "valid extension must resolve");
        let Ok(resolved) = resolved else {
            return;
        };
        let builder = builder_from_cli(
            &cli,
            mock_provider(),
            resolved.profile,
            &resolved.settings,
            &resolved.applied,
        );
        assert!(builder.is_ok(), "resolved extension must reach assembly");
        let Ok(builder) = builder else {
            return;
        };
        assert!(
            builder.build().is_ok(),
            "resolved extension must build successfully"
        );
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

/// `-c auto_compact_reserve_tokens=<u64>` must plumb through the unified
/// assembly path onto the built `AgentLoopConfig` — the reserve knob is the
/// operator's lever for the auto-compaction trigger.
#[test]
#[serial_test::serial]
fn auto_compact_reserve_tokens_override_plumbs_to_built_agent() {
    with_isolated_env(|| {
        let cli = Cli::parse_from([
            "norn",
            "-m",
            "gpt-5.5",
            "--no-session",
            "-c",
            "auto_compact_reserve_tokens=45000",
        ]);
        assert_eq!(
            build_parts(&cli).config.auto_compact_reserve_tokens,
            Some(45_000),
            "-c auto_compact_reserve_tokens must reach the built AgentLoopConfig",
        );
    });
}

/// A plain invocation of a catalogued model must arm auto-compaction by
/// default: the context window is filled from the model catalog and the
/// reserve knob defaults to `Some(30_000)`, so the trigger is live without
/// any explicit configuration.
#[test]
#[serial_test::serial]
fn plain_invocation_arms_catalog_window_and_default_reserve() {
    with_isolated_env(|| {
        let cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session"]);
        let config = build_parts(&cli).config;
        assert_eq!(
            config.context_window_limit,
            norn::model_catalog::smallest_context_window_for_model("gpt-5.5"),
            "a plain invocation must default the window from the model catalog",
        );
        assert!(
            config.context_window_limit.is_some(),
            "gpt-5.5 is catalogued, so the window must be armed",
        );
        assert_eq!(
            config.auto_compact_reserve_tokens,
            Some(30_000),
            "the reserve knob is armed by default on a plain invocation",
        );
    });
}

/// Whether the assembled base system prompt carries the auto-compaction
/// guidance section — the observable manifestation of the builder's
/// `has_auto_compact`. A distinctive fragment of `HARNESS_AUTO_COMPACT`.
fn prompt_has_auto_compact_guidance(parts: &AgentParts) -> bool {
    parts
        .loop_context
        .system_sections
        .first()
        .is_some_and(|p| p.contains("may not survive compaction"))
}

/// The operator's clean disable: `-c auto_compact_reserve_tokens=off` sets
/// the reserve to `None` (beating the catalogued window's default
/// `Some(30_000)`) so the trigger is off and the system prompt does not
/// promise compaction. This is the driven-mode workflow's off switch.
#[test]
#[serial_test::serial]
fn auto_compact_reserve_tokens_off_via_c_flag_disables() {
    with_isolated_env(|| {
        let cli = Cli::parse_from([
            "norn",
            "-m",
            "gpt-5.5",
            "--no-session",
            "-c",
            "auto_compact_reserve_tokens=off",
        ]);
        let parts = build_parts(&cli);
        assert_eq!(
            parts.config.auto_compact_reserve_tokens, None,
            "`-c auto_compact_reserve_tokens=off` must disable the reserve outright",
        );
        assert!(
            parts.config.context_window_limit.is_some(),
            "the catalogued window is still armed — only the reserve is off",
        );
        assert!(
            !prompt_has_auto_compact_guidance(&parts),
            "with the reserve off, the prompt must not claim auto-compaction is active",
        );
    });
}

/// Settings `agent.auto_compact_reserve_tokens = "off"` disables the trigger
/// exactly as the `-c` sentinel does, and beats the builder default.
#[test]
#[serial_test::serial]
fn auto_compact_reserve_tokens_off_via_settings_disables() {
    let home = tempfile::tempdir().expect("home tempdir");
    let workdir = tempfile::tempdir().expect("work tempdir");
    let home_path = home.path().to_path_buf();
    let dotnorn = workdir.path().join(".norn");
    std::fs::create_dir_all(&dotnorn).unwrap();
    std::fs::write(
        dotnorn.join("settings.json"),
        r#"{ "agent": { "auto_compact_reserve_tokens": "off" } }"#,
    )
    .unwrap();

    temp_env::with_vars(
        [
            ("NORN_HOME", Some(home_path.as_os_str())),
            ("HOME", Some(home_path.as_os_str())),
        ],
        || {
            std::env::set_current_dir(workdir.path()).expect("chdir");
            let cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session"]);
            let parts = build_parts(&cli);
            assert_eq!(
                parts.config.auto_compact_reserve_tokens, None,
                "settings \"off\" must disable the reserve, beating the default Some(30_000)",
            );
            assert!(
                !prompt_has_auto_compact_guidance(&parts),
                "settings \"off\" must drop the prompt's auto-compaction guidance",
            );
        },
    );
}

/// A bare integer in settings still arms the reserve — the string form is an
/// addition, not a replacement of the numeric form.
#[test]
#[serial_test::serial]
fn auto_compact_reserve_tokens_integer_via_settings_arms() {
    let home = tempfile::tempdir().expect("home tempdir");
    let workdir = tempfile::tempdir().expect("work tempdir");
    let home_path = home.path().to_path_buf();
    let dotnorn = workdir.path().join(".norn");
    std::fs::create_dir_all(&dotnorn).unwrap();
    std::fs::write(
        dotnorn.join("settings.json"),
        r#"{ "agent": { "auto_compact_reserve_tokens": 12345 } }"#,
    )
    .unwrap();

    temp_env::with_vars(
        [
            ("NORN_HOME", Some(home_path.as_os_str())),
            ("HOME", Some(home_path.as_os_str())),
        ],
        || {
            std::env::set_current_dir(workdir.path()).expect("chdir");
            let cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session"]);
            assert_eq!(
                build_parts(&cli).config.auto_compact_reserve_tokens,
                Some(12_345),
                "a numeric settings reserve must arm the trigger at that value",
            );
        },
    );
}

/// A non-integer, non-`off` reserve in settings is a loud load-time error
/// naming the key and the accepted forms — never a silent fallback.
#[test]
#[serial_test::serial]
fn auto_compact_reserve_tokens_invalid_settings_string_errors()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir().expect("home tempdir");
    let workdir = tempfile::tempdir().expect("work tempdir");
    let home_path = home.path().to_path_buf();
    let dotnorn = workdir.path().join(".norn");
    std::fs::create_dir_all(&dotnorn).unwrap();
    std::fs::write(
        dotnorn.join("settings.json"),
        r#"{ "agent": { "auto_compact_reserve_tokens": "sometimes" } }"#,
    )
    .unwrap();

    temp_env::with_vars(
        [
            ("NORN_HOME", Some(home_path.as_os_str())),
            ("HOME", Some(home_path.as_os_str())),
        ],
        || -> Result<(), Box<dyn std::error::Error>> {
            std::env::set_current_dir(workdir.path())?;
            let cwd = std::env::current_dir()?;
            let result = norn::runtime_init::load_merged_settings(&cwd);
            let err = result.err().ok_or_else(|| {
                std::io::Error::other("an invalid reserve was accepted by settings loading")
            })?;
            let rendered = err.to_string();
            assert!(
                rendered.contains("auto_compact_reserve_tokens"),
                "the load error must name the key: {rendered}",
            );
            assert!(
                rendered.contains("off"),
                "the load error must state the accepted forms (\"off\"): {rendered}",
            );
            Ok(())
        },
    )?;
    Ok(())
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
