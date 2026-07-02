//! Driver executor semantics: the print/TUI drivers must hand the loop an
//! executor that exposes an owned handle (`ToolExecutor::owned_handle`), so
//! concurrent tool batches spawn each member on its own tokio task exactly
//! as `Agent::run` does — and a local `/exit`/`/quit` prompt is still
//! answered on the plain (`-f json`) path rather than silently swallowed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;

use norn::agent_loop::runner::{ToolExecutor, driver_executor};
use norn::tool::registry::ToolRegistry;
use serde_json::Value;

/// The drivers hand `run_agent_step` the value returned by the shared
/// `driver_executor` helper — the registry coerced to `Arc<dyn
/// ToolExecutor>` — passed by reference so the blanket impl's
/// `owned_handle` hands each concurrent batch member its own spawnable
/// task. Testing the *production helper* (not an inline re-implementation
/// of the coercion) is what guards the fix: every driver
/// (`print/orchestrator.rs`, `tui/driver.rs`) routes through it, so a
/// revert to the borrowed `&*registry` form cannot happen without
/// abandoning the helper the drivers call.
#[test]
fn driver_executor_exposes_owned_handle() {
    let registry = Arc::new(ToolRegistry::new());

    // Exactly what orchestrate()/the TUI driver produce and then pass by
    // reference into `run_agent_step`.
    let executor = driver_executor(&registry);
    let as_driver_supplies: &dyn ToolExecutor = &executor;
    assert!(
        as_driver_supplies.owned_handle().is_some(),
        "the driver-supplied &Arc<dyn ToolExecutor> MUST expose an owned handle \
         so concurrent tool batches spawn each member on its own task",
    );

    // The pre-fix borrowed form reaches ToolRegistry's own impl, whose
    // default owned_handle is None — proving the coercion the helper performs
    // actually changes execution semantics, so the helper is load-bearing.
    let borrowed: &dyn ToolExecutor = &*registry;
    assert!(
        borrowed.owned_handle().is_none(),
        "the borrowed &*registry form must NOT expose an owned handle \
         (the join_all fallback) — this is the behavior the helper moves away from",
    );
}

/// Path to the built `norn` binary for this test run.
fn norn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_norn")
}

/// Plain print mode (`-p -f json`): a prompt that is entirely the local
/// `/exit` builtin must still emit the JSON completion envelope on stdout
/// and exit 0. Pre-fix, `exit_requested` short-circuited before
/// `write_handled_locally`, so no envelope was written. The prompt is fed
/// via stdin (not positional) so `compose_prompt` yields exactly `/exit`.
#[test]
fn plain_json_exit_prompt_still_emits_completion_envelope() {
    let home = tempfile::tempdir().expect("temp NORN_HOME");
    let mut child = Command::new(norn_bin())
        .arg("-p")
        .arg("-f")
        .arg("json")
        .arg("--provider")
        .arg("openai-compatible")
        .arg("-c")
        // Never contacted: /exit short-circuits before any completion call.
        .arg("base_url=http://127.0.0.1:9/v1")
        .arg("--no-session")
        .env("NORN_HOME", home.path())
        .env("NORN_OPENAI_COMPAT_API_KEY", "test-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn norn -p -f json");

    {
        let mut stdin = child.stdin.take().expect("child stdin");
        stdin.write_all(b"/exit").expect("write prompt");
        // Drop closes stdin (EOF) so the piped-stdin read completes.
    }

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("child stdout")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    let status = child.wait().expect("norn exits");

    assert!(
        status.success(),
        "a local /exit prompt exits 0 (stdout: {stdout:?})",
    );
    let envelope: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout must be one JSON envelope ({err}): {stdout:?}"));
    assert_eq!(
        envelope["envelope_version"], 1,
        "the versioned completion envelope is emitted: {envelope}",
    );
    assert_eq!(
        envelope["stop"]["reason"], "completed",
        "a locally-handled prompt reports the completed stop reason: {envelope}",
    );
    assert!(
        envelope["output"].is_null(),
        "no agent output for a local slash command: {envelope}",
    );
}
