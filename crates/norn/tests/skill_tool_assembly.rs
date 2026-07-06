//! F2 / D5 regression: the `skill` tool is registered on the
//! `load_runtime_base` assembly path exactly when a non-empty skill
//! catalog is discovered, and absent otherwise.
//!
//! Before R1.8 the embedded / library assembly path published skill
//! *infra* but never the skill *tool* (only the deleted CLI `build_runtime`
//! registered it), so every `AgentBuilder`-assembled agent — including all
//! Meridian paths — could not invoke skills. These tests pin the gate at
//! the source.
//!
//! Both tests mutate `HOME` / `NORN_HOME` (process-global) so no on-disk
//! user skills perturb the catalog; they are serialised behind
//! `#[serial_test::serial]` and never observe each other's env.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use norn::agent::AgentBuilder;
use norn::provider::mock::MockProvider;
use norn::provider::traits::Provider;

/// Swaps `HOME` and `NORN_HOME` to `dir` for the test's duration and
/// restores the prior values on drop. Paired with `#[serial_test::serial]`.
struct HomeEnvGuard {
    prior_home: Option<OsString>,
    prior_norn_home: Option<OsString>,
}

impl HomeEnvGuard {
    fn set(dir: &Path) -> Self {
        let prior_home = std::env::var_os("HOME");
        let prior_norn_home = std::env::var_os("NORN_HOME");
        // SAFETY: paired with `#[serial_test::serial]`; no concurrent
        // reader observes the mutated env.
        unsafe {
            std::env::set_var("HOME", dir);
            std::env::set_var("NORN_HOME", dir);
        }
        Self {
            prior_home,
            prior_norn_home,
        }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prior_home {
                Some(val) => std::env::set_var("HOME", val),
                None => std::env::remove_var("HOME"),
            }
            match &self.prior_norn_home {
                Some(val) => std::env::set_var("NORN_HOME", val),
                None => std::env::remove_var("NORN_HOME"),
            }
        }
    }
}

fn mock_provider() -> Arc<dyn Provider> {
    Arc::new(MockProvider::new(Vec::new()))
}

fn write_skill(cwd: &Path, name: &str) {
    let dir = cwd.join(".norn").join("skills").join(name);
    std::fs::create_dir_all(&dir).expect("mkdir skills");
    std::fs::write(
        dir.join("SKILL.md"),
        "---\ndescription: a demo skill\n---\nbody\n",
    )
    .expect("write SKILL.md");
}

/// Explicit window for the fixture: "test-model" is deliberately
/// uncatalogued, and `build` hard-errors on an unarmed window
/// (2026-07-05 incident guard). `272_000` is gpt-5.5's catalogued
/// standard window (assets/models.json) — factual, not invented.
const TEST_CONTEXT_WINDOW: u64 = 272_000;

fn skill_tool_present(cwd: &Path) -> bool {
    let parts = AgentBuilder::new(mock_provider())
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(cwd)
        .load_runtime_base()
        .build()
        .expect("build succeeds")
        .into_parts();
    parts.registry.get("skill").is_some()
}

#[test]
#[serial_test::serial]
fn skill_tool_registered_when_catalog_non_empty() {
    let home = tempfile::tempdir().expect("home tempdir");
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let _guard = HomeEnvGuard::set(home.path());
    write_skill(cwd.path(), "demo");

    assert!(
        skill_tool_present(cwd.path()),
        "a non-empty skill catalog must register the `skill` tool on the \
         load_runtime_base path",
    );
}

#[test]
#[serial_test::serial]
fn skill_tool_absent_when_catalog_empty() {
    let home = tempfile::tempdir().expect("home tempdir");
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let _guard = HomeEnvGuard::set(home.path());

    assert!(
        !skill_tool_present(cwd.path()),
        "with no skills discoverable in any search path, the `skill` tool \
         must not be registered",
    );
}
