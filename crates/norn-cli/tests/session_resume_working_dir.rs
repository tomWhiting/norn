//! F1 regression: the empty-`--resume` / `--fork` sentinel is scoped to
//! the current working directory, not the globally most-recently-updated
//! session across every directory.
//!
//! Before the R1 assembly unification these no-argument forms resolved
//! through `resume_latest_in_working_dir` / `fork_latest_in_working_dir`
//! (project-scoped). The unified path must preserve that scoping: an
//! empty `--resume` in project A must never resume a newer session that
//! belongs to project B. The whole check runs in one `#[test]` because it
//! mutates the process working directory and `NORN_HOME` (both
//! process-global) inside a `temp-env` scope.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use clap::Parser;

use norn::config::NornSettings;
use norn::provider::mock::MockProvider;
use norn::provider::traits::Provider;
use norn::session::store::DurabilityPolicy;
use norn::session::{CreateSessionOptions, SessionManager};

use norn_cli::cli::Cli;
use norn_cli::config::{
    apply_cli_profile_overrides, apply_settings_reasoning_to_profile, resolve_model_selection,
    resolve_profile, session_data_dir,
};
use norn_cli::runtime::builder_from_cli;

fn mock_provider() -> Arc<dyn Provider> {
    Arc::new(MockProvider::new(Vec::new()))
}

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

/// Build the resolved session id an invocation opens through the unified
/// assembler (`builder_from_cli` → `build` → `into_parts`).
fn resolved_session_id(cli: &Cli) -> String {
    let settings = merged_settings();
    let mut profile = resolve_profile(cli.profile.as_deref()).expect("resolve profile");
    apply_settings_reasoning_to_profile(&settings, &mut profile).expect("settings reasoning");
    let applied = apply_cli_profile_overrides(cli, &mut profile).expect("cli overrides");
    let model_selection =
        resolve_model_selection(&profile.model, &settings).expect("model selection");
    profile.model = model_selection.model;

    let parts = builder_from_cli(cli, mock_provider(), profile, &settings, &applied)
        .expect("builder_from_cli succeeds")
        .build()
        .expect("build succeeds")
        .into_parts();
    parts
        .session_entry
        .expect("a persisted session was opened")
        .id
}

fn seed_session(working_dir: &str) -> String {
    let manager = SessionManager::new(session_data_dir());
    let opened = manager
        .create(
            CreateSessionOptions {
                model: "gpt-5.5".to_owned(),
                working_dir: working_dir.to_owned(),
                name: None,
            },
            DurabilityPolicy::Flush,
        )
        .expect("create seed session");
    opened.entry.id
}

#[test]
fn empty_resume_selects_latest_session_for_current_working_dir() {
    let home = tempfile::tempdir().expect("home tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let home_path = home.path().to_path_buf();
    temp_env::with_vars(
        [
            ("NORN_HOME", Some(home_path.as_os_str())),
            ("HOME", Some(home_path.as_os_str())),
        ],
        || {
            let current_dir = workspace.path().join("current");
            let other_dir = workspace.path().join("other");
            std::fs::create_dir_all(&current_dir).expect("mkdir current");
            std::fs::create_dir_all(&other_dir).expect("mkdir other");

            // Seed the current-project session first, then a globally newer
            // session in another directory.
            std::env::set_current_dir(&current_dir).expect("chdir current");
            let current_cwd = std::env::current_dir().expect("cwd");
            let current_id = seed_session(&current_cwd.display().to_string());

            std::thread::sleep(std::time::Duration::from_millis(5));
            std::env::set_current_dir(&other_dir).expect("chdir other");
            let other_cwd = std::env::current_dir().expect("cwd");
            let other_id = seed_session(&other_cwd.display().to_string());
            assert_ne!(current_id, other_id, "two distinct seed sessions");

            // Back in the current project, an empty `--resume` must resume
            // the current-project session, never the globally newer one.
            std::env::set_current_dir(&current_dir).expect("chdir back");
            let resume_cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--resume", ""]);
            let resumed = resolved_session_id(&resume_cli);
            assert_eq!(
                resumed, current_id,
                "empty --resume must select the current-dir session, \
                 not globally newer session {other_id} from another directory",
            );
        },
    );
}
