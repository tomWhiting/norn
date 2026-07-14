#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
use super::cd_track::strip_surrounding_quotes;
use super::tool::BashArgs;
use super::*;
use crate::error::ToolError;
use crate::tool::context::{SessionId, ToolContext};
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::ToolErrorKind;
use crate::tool::lifecycle::PreValidateOutcome;
use crate::tool::risk::{BashRiskTier, classify_risk};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::Tool;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

fn envelope(args: Value) -> ToolEnvelope {
    ToolEnvelope {
        tool_call_id: "call-bash".to_owned(),
        tool_name: "bash".to_owned(),
        model_args: args,
        metadata: Value::Null,
    }
}

fn expand_tilde_for_test(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir().expect("home dir").join(rest)
    } else {
        std::path::PathBuf::from(path)
    }
}

/// Guard that points `NORN_HOME` at a temp dir so migrated/backgrounded spools
/// land there, restoring the prior value on drop. Paired with `#[serial]`.
struct HomeGuard {
    prior: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let prior = std::env::var_os("NORN_HOME");
        // SAFETY: paired with `#[serial]`; no concurrent reader observes it.
        unsafe { std::env::set_var("NORN_HOME", path) };
        Self { prior }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => unsafe { std::env::set_var("NORN_HOME", v) },
            None => unsafe { std::env::remove_var("NORN_HOME") },
        }
    }
}

/// A context carrying a `SessionId` and a (sinkless) `ProcessManager`, so the
/// bash tool can background and migrate through it.
fn ctx_with_manager(session: &str) -> (ToolContext, Arc<crate::process::ProcessManager>) {
    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(SessionId(session.to_owned())));
    let manager = Arc::new(crate::process::ProcessManager::new(
        Some(session.to_owned()),
        None,
    ));
    ctx.insert_extension(Arc::clone(&manager));
    (ctx, manager)
}

async fn wait_terminal(handle: &crate::process::ProcessHandle) {
    for _ in 0..600 {
        if !handle.is_running() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("process did not terminate in time");
}

/// Wait until the spool's on-disk content contains `needle` (the drains flush
/// asynchronously after the direct child exits).
async fn wait_spool_contains(handle: &crate::process::ProcessHandle, needle: &str) {
    for _ in 0..600 {
        let (bytes, _) = handle.spool().read_from(0).await.expect("spool read");
        if String::from_utf8_lossy(&bytes).contains(needle) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("spool never contained {needle:?}");
}

#[test]
fn bash_args_schema_matches_previous_hand_written_schema() {
    let expected_schema = json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": "Shell command line. Executed via `sh -c`."
            },
            "timeout": {
                "type": "integer",
                "minimum": 0,
                "description": "Timeout in seconds. 0 means wait forever. Defaults to 120."
            },
            "working_dir": {
                "type": "string",
                "description": "Working directory for the subprocess. Resolved like file-tool paths: `~` expands to the home directory and relative paths resolve against the agent's working directory."
            },
            "run_in_background": {
                "type": "boolean",
                "description": "Run detached in the background instead of waiting. Returns immediately with a process id and spool path; the process runs with no timeout until it exits or you kill it. Cannot be combined with `timeout`. Check on it with the `process` tool."
            },
            "watch": {
                "type": "object",
                "required": ["brief", "filter"],
                "properties": {
                    "brief": {
                        "type": "string",
                        "description": "A human-readable statement of what to watch for (e.g. \"a compile error\")."
                    },
                    "filter": {
                        "type": "string",
                        "description": "A shell filter script run via `sh -c` over each new spool region on stdin. Exit 0 means match (its stdout is the excerpt that wakes you); any other exit means no match, except exits 126 and 127, which the shell reserves (command not executable, and command not found) and which are reported as watch errors rather than no-matches."
                    }
                },
                "additionalProperties": false,
                "description": "Optional deterministic watch to attach at spawn (only with `run_in_background`). A `{brief, filter}` object: the filter runs via `sh -c` over each new spool region and a match (exit 0) wakes you with the matching excerpt. Attach more later with the `process` tool (op=watch)."
            }
        },
        "required": ["command"],
        "additionalProperties": false
    });
    assert_eq!(BashArgs::json_schema(), expected_schema);
}

#[test]
fn object_safe() {
    let _: Box<dyn Tool + Send + Sync> = Box::new(BashTool::new());
}

#[test]
fn effect_is_process() {
    assert_eq!(BashTool::new().effect(), ToolEffect::Process);
}

#[test]
fn name_is_bash() {
    assert_eq!(BashTool::new().name(), "bash");
}

#[tokio::test]
async fn echo_hello_succeeds() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "echo hello" }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");

    assert!(!out.is_error());
    assert_eq!(out.content["exit_code"].as_i64(), Some(0));
    let stdout = out.content["stdout"].as_str().unwrap_or_default();
    assert!(stdout.contains("hello"), "stdout was {stdout:?}");
    assert_eq!(
        out.content["metadata"]["risk_tier"].as_str(),
        Some("Harmless"),
    );
}

#[tokio::test]
async fn output_under_threshold_returns_inline() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "printf small-output" }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");

    assert_eq!(out.content["stdout"].as_str(), Some("small-output\n"));
    assert_eq!(out.content["stderr"].as_str(), Some(""));
    assert!(out.content.get("output_redirected").is_none());
}

#[tokio::test]
async fn output_over_threshold_redirects_to_file_with_shape_and_content() {
    let ctx = ToolContext::empty();
    let session_id = format!("bash-test-{}", Uuid::new_v4());
    ctx.insert_extension(Arc::new(SessionId(session_id.clone())));
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "yes O | head -c 23000; printf '\\n'; printf 'ERR\\n' 1>&2",
    }));

    let out = tool.execute(&env, &ctx).await.expect("bash ok");

    assert_eq!(out.content["output_redirected"].as_bool(), Some(true));
    assert_eq!(out.content["output_chars"].as_u64(), Some(23_005));
    assert!(out.content.get("stdout_chars").is_none());
    assert!(out.content.get("stderr_chars").is_none());
    let path = out.content["output_path"].as_str().expect("output_path");
    assert!(path.contains(&session_id));
    assert!(path.ends_with("call-bash.log"));
    assert!(
        out.content["hint"]
            .as_str()
            .expect("hint")
            .contains("22000-character inline threshold")
    );

    let absolute = expand_tilde_for_test(path);
    let content = tokio::fs::read_to_string(&absolute)
        .await
        .expect("read log");
    assert!(content.contains("O\nO\nO\n"));
    assert!(content.len() > 23_000);
    assert!(content.contains("ERR\n"));
    assert!(!content.contains("=== STDOUT ==="));
    assert!(!content.contains("=== STDERR ==="));
    assert!(absolute.parent().expect("session dir").is_dir());
}

/// R4: a foreground command that redirected before its timeout migrates to the
/// background; the pre-migration (redirected) output is seeded into the spool.
#[tokio::test]
#[serial_test::serial]
async fn timeout_with_partial_output_migrates_and_seeds_spool() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::set(dir.path());
    let (ctx, manager) = ctx_with_manager("bash-migrate-redirect");
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "yes P | head -c 23000; printf '\\n'; sleep 5",
        "timeout": 1_u64,
    }));

    let out = tool.execute(&env, &ctx).await.expect("bash ok");

    assert!(!out.is_error(), "migration is a success: {:?}", out.content);
    assert_eq!(out.content["migrated"].as_bool(), Some(true));
    // The pre-migration snapshot was over the inline threshold, so it is
    // reported as redirected; the spool holds the full pre-migration output.
    assert_eq!(out.content["output_redirected"].as_bool(), Some(true));
    let process_id = out.content["process_id"].as_str().expect("process_id");
    let handle = manager.get(process_id).expect("registered");
    let (bytes, _) = handle.spool().read_from(0).await.expect("spool read");
    assert!(String::from_utf8_lossy(&bytes).contains("P\nP\nP\n"));
    handle.kill().await;
}

#[tokio::test]
async fn non_zero_exit_marks_is_error() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "exit 7" }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    assert!(out.is_error());
    assert_eq!(out.content["exit_code"].as_i64(), Some(7));
}

#[tokio::test]
async fn stderr_captured_separately() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "echo oops 1>&2" }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    assert_eq!(out.content["exit_code"].as_i64(), Some(0));
    assert!(
        out.content["stderr"]
            .as_str()
            .unwrap_or("")
            .contains("oops")
    );
}

/// R4 headline: a foreground command that outruns its timeout is migrated to
/// the background instead of killed — it keeps running under the manager.
#[tokio::test]
#[serial_test::serial]
async fn timeout_migrates_long_running_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::set(dir.path());
    let (ctx, manager) = ctx_with_manager("bash-migrate-running");
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "sleep 3; echo finished",
        "timeout": 1_u64,
    }));

    let started = std::time::Instant::now();
    let out = tool.execute(&env, &ctx).await.expect("bash ok");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "migration returns promptly at the timeout boundary",
    );
    assert!(!out.is_error(), "migration is a success: {:?}", out.content);
    assert_eq!(out.content["migrated"].as_bool(), Some(true));
    let process_id = out.content["process_id"].as_str().expect("process_id");
    let handle = manager.get(process_id).expect("registered");
    assert!(handle.is_running(), "the migrated process keeps running");

    // ~2s later it exits on its own and the spool holds its post-migration output.
    wait_terminal(&handle).await;
    assert_eq!(
        handle.status(),
        crate::process::ProcessStatus::Exited { code: 0 }
    );
    wait_spool_contains(&handle, "finished").await;
}

/// R4: `ToolErrorKind::Timeout` is no longer produced by the bash tool. On the
/// degenerate path where no manager is wired, a timeout still kills the tree,
/// but the honest failure is the missing infrastructure — never `Timeout`.
#[tokio::test]
async fn timeout_without_manager_is_missing_extension_never_timeout() {
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "sleep 5",
        "timeout": 1_u64,
    }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    assert!(out.is_error());
    assert_eq!(out.content["timed_out"].as_bool(), Some(true));
    assert_eq!(
        out.error().expect("error").kind,
        crate::tool::failure::ToolErrorKind::MissingExtension,
        "the removed Timeout failure is never produced",
    );
}

#[tokio::test]
async fn working_dir_is_applied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "pwd",
        "working_dir": dir.path().to_string_lossy(),
    }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    let stdout = out.content["stdout"].as_str().unwrap_or_default().trim();
    // On macOS, /tmp resolves to /private/tmp; compare suffix.
    let expected = dir.path().to_string_lossy();
    assert!(
        stdout.ends_with(expected.as_ref()),
        "expected pwd {stdout:?} to end with {expected:?}",
    );
}

/// Track B finding 8: `with_drain_grace` overrides the 2s default drain
/// grace — a backgrounded child holding the output pipes is cut off after
/// the configured grace and the result is annotated accordingly.
#[tokio::test]
async fn with_drain_grace_bounds_background_pipe_holders() {
    let tool = BashTool::new().with_drain_grace(std::time::Duration::from_millis(200));
    let started = std::time::Instant::now();
    let out = tool
        .execute(
            &envelope(json!({ "command": "sleep 5 & echo started" })),
            &ToolContext::empty(),
        )
        .await
        .expect("bash ok");
    let elapsed = started.elapsed();

    assert!(!out.is_error());
    assert_eq!(out.content["streams_still_open"].as_bool(), Some(true));
    assert!(
        out.content["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("started"),
        "buffered output before the cutoff is preserved: {}",
        out.content,
    );
    assert!(
        elapsed < std::time::Duration::from_millis(1500),
        "a 200ms grace must return well before the 2s default (elapsed: {elapsed:?})",
    );
}

/// Concurrent-drain regression: a background child holding BOTH stdout and
/// stderr open must cost one drain grace period, not two — the drains
/// settle concurrently. With a 1s grace, the sequential worst case is
/// >= 2s; the concurrent implementation returns well under that.
#[tokio::test]
async fn held_open_stdout_and_stderr_cost_one_grace_period() {
    let tool = BashTool::new().with_drain_grace(std::time::Duration::from_secs(1));
    let started = std::time::Instant::now();
    let out = tool
        .execute(
            // The backgrounded sleep inherits both pipes and outlives the
            // shell, so stdout AND stderr each hit the drain grace.
            &envelope(json!({ "command": "sleep 5 & echo held" })),
            &ToolContext::empty(),
        )
        .await
        .expect("bash ok");
    let elapsed = started.elapsed();

    assert!(!out.is_error());
    assert_eq!(out.content["streams_still_open"].as_bool(), Some(true));
    assert!(
        elapsed < std::time::Duration::from_millis(1900),
        "two held pipes must settle within ~one 1s grace, not two (elapsed: {elapsed:?})",
    );
}

/// Track B finding 7 regression: a relative `working_dir` argument must
/// resolve against the agent's context working directory, not the process
/// CWD.
#[tokio::test]
async fn relative_working_dir_resolves_against_context_working_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("sub")).expect("mkdir sub");
    let ctx = ToolContext::with_working_dir(crate::tool::context::SharedWorkingDir::new(
        dir.path().to_path_buf(),
    ));
    let out = BashTool::new()
        .execute(
            &envelope(json!({ "command": "pwd", "working_dir": "sub" })),
            &ctx,
        )
        .await
        .expect("bash ok");
    let stdout = out.content["stdout"].as_str().unwrap_or_default().trim();
    let expected = dir
        .path()
        .join("sub")
        .canonicalize()
        .expect("canonicalize sub");
    assert_eq!(
        stdout,
        expected.to_string_lossy(),
        "relative working_dir must resolve against the context working dir",
    );
}

/// Track B finding 7 regression: a `~`-prefixed `working_dir` argument
/// expands to the home directory, matching the file tools' path semantics.
#[tokio::test]
async fn tilde_working_dir_expands_to_home() {
    let out = BashTool::new()
        .execute(
            &envelope(json!({ "command": "pwd", "working_dir": "~" })),
            &ToolContext::empty(),
        )
        .await
        .expect("bash ok");
    let stdout = out.content["stdout"].as_str().unwrap_or_default().trim();
    let home = dirs::home_dir()
        .expect("home dir")
        .canonicalize()
        .expect("canonicalize home");
    assert_eq!(stdout, home.to_string_lossy());
}

/// Track B finding 7 regression: a `working_dir` that does not resolve to
/// an existing directory is refused with a structured pre-validation error
/// instead of an opaque spawn failure.
#[tokio::test]
async fn nonexistent_working_dir_is_rejected_before_spawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ToolContext::with_working_dir(crate::tool::context::SharedWorkingDir::new(
        dir.path().to_path_buf(),
    ));
    let err = BashTool::new()
        .execute(
            &envelope(json!({ "command": "pwd", "working_dir": "no-such-dir" })),
            &ctx,
        )
        .await
        .expect_err("missing working_dir must fail");
    assert!(
        matches!(err, ToolError::PreValidationFailed { .. }),
        "expected PreValidationFailed, got: {err}",
    );
    assert!(
        err.to_string().contains("not an existing directory"),
        "{err}",
    );
}

/// Track B finding 7 regression: a workspace-confined context refuses a
/// model-supplied `working_dir` outside the confinement root.
#[tokio::test]
async fn confined_context_rejects_out_of_root_working_dir() {
    let outer = tempfile::tempdir().expect("tempdir");
    let root = outer.path().join("ws");
    let elsewhere = outer.path().join("elsewhere");
    std::fs::create_dir(&root).expect("mkdir ws");
    std::fs::create_dir(&elsewhere).expect("mkdir elsewhere");
    let mut ctx =
        ToolContext::with_working_dir(crate::tool::context::SharedWorkingDir::new(root.clone()));
    ctx.confine_to_workspace(root);

    let err = BashTool::new()
        .execute(
            &envelope(json!({
                "command": "pwd",
                "working_dir": elsewhere.to_string_lossy(),
            })),
            &ctx,
        )
        .await
        .expect_err("out-of-root working_dir must be refused");
    assert!(
        matches!(err, ToolError::PreValidationFailed { .. }),
        "expected PreValidationFailed, got: {err}",
    );
    assert!(err.to_string().contains("outside the workspace"), "{err}");
}

/// Companion to the confinement regression: an in-root `working_dir` still
/// works on a confined context.
#[tokio::test]
async fn confined_context_allows_in_root_working_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("inner")).expect("mkdir inner");
    let mut ctx = ToolContext::with_working_dir(crate::tool::context::SharedWorkingDir::new(
        dir.path().to_path_buf(),
    ));
    ctx.confine_to_workspace(dir.path().to_path_buf());

    let out = BashTool::new()
        .execute(
            &envelope(json!({ "command": "pwd", "working_dir": "inner" })),
            &ctx,
        )
        .await
        .expect("in-root working_dir runs");
    let stdout = out.content["stdout"].as_str().unwrap_or_default().trim();
    let expected = dir
        .path()
        .join("inner")
        .canonicalize()
        .expect("canonicalize inner");
    assert_eq!(stdout, expected.to_string_lossy());
}

#[tokio::test]
async fn critical_risk_tier_appears_in_metadata() {
    // `classify_risk` flags any command line containing the literal
    // substring `chmod 777` as Critical. The executed shell only echoes
    // the string to /dev/null — no actual permission change occurs — so
    // the tier assertion is meaningful and the test is side-effect-free.
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "echo 'chmod 777 nothing' > /dev/null",
    }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    assert_eq!(
        out.content["metadata"]["risk_tier"].as_str(),
        Some("Critical"),
    );
}

#[tokio::test]
async fn pre_validate_proceeds_on_critical_command() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "sudo rm -rf /" }));
    let outcome = tool.pre_validate(&env, &ToolContext::empty()).await;
    assert!(matches!(outcome, PreValidateOutcome::Proceed));
    // And classify_risk confirms the tier independently.
    assert_eq!(classify_risk("sudo rm -rf /"), BashRiskTier::Critical);
    assert_eq!(classify_risk("ls"), BashRiskTier::Harmless);
}

#[tokio::test]
async fn invalid_args_pre_validation_failure() {
    let tool = BashTool::new();
    let env = envelope(json!({ "not_command": "oops" }));
    let err = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect_err("must fail");
    assert!(matches!(err, ToolError::PreValidationFailed { .. }));
}

#[tokio::test]
async fn ctx_working_dir_used_when_args_working_dir_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize");
    let ctx = ToolContext::with_working_dir(crate::tool::context::SharedWorkingDir::new(
        canonical.clone(),
    ));
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "pwd" }));
    let out = tool.execute(&env, &ctx).await.expect("bash ok");
    let stdout = out.content["stdout"].as_str().unwrap_or_default().trim();
    assert_eq!(stdout, canonical.to_string_lossy());
}

#[tokio::test]
async fn cd_absolute_updates_working_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize");
    let ctx = ToolContext::empty();
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": format!("cd {}", canonical.display()),
    }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), canonical);
}

#[tokio::test]
async fn cd_relative_joins_against_working_dir() {
    let root = tempfile::tempdir().expect("tempdir");
    let sub = root.path().join("sub");
    std::fs::create_dir(&sub).expect("mkdir sub");
    let root_canonical = root.path().canonicalize().expect("canonicalize root");
    let sub_canonical = sub.canonicalize().expect("canonicalize sub");
    let ctx =
        ToolContext::with_working_dir(crate::tool::context::SharedWorkingDir::new(root_canonical));
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "cd sub" }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), sub_canonical);
}

#[tokio::test]
async fn cd_parent_moves_up_one_level() {
    let root = tempfile::tempdir().expect("tempdir");
    let sub = root.path().join("sub");
    std::fs::create_dir(&sub).expect("mkdir sub");
    let root_canonical = root.path().canonicalize().expect("canonicalize root");
    let sub_canonical = sub.canonicalize().expect("canonicalize sub");
    let ctx =
        ToolContext::with_working_dir(crate::tool::context::SharedWorkingDir::new(sub_canonical));
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "cd .." }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), root_canonical);
}

#[tokio::test]
async fn cd_compound_command_detected() {
    // `ls && cd <dir>` — cd is at the end of a chain; must still be detected.
    let dir = tempfile::tempdir().expect("tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize");
    let ctx = ToolContext::empty();
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": format!("ls > /dev/null && cd {}", canonical.display()),
    }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), canonical);
}

#[tokio::test]
async fn cd_prefix_in_chain_detected() {
    // `cd <dir> && ls` — cd is at the start with `&&` separator.
    let dir = tempfile::tempdir().expect("tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize");
    let ctx = ToolContext::empty();
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": format!("cd {} && ls > /dev/null", canonical.display()),
    }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), canonical);
}

#[tokio::test]
async fn cd_to_nonexistent_dir_does_not_update() {
    let ctx = ToolContext::with_working_dir(crate::tool::context::SharedWorkingDir::new(
        std::path::PathBuf::from("/tmp"),
    ));
    let original = ctx.working_dir();
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "cd /no/such/path/should/exist || true",
    }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), original);
}

#[tokio::test]
async fn cd_with_tilde_expands_to_home() {
    let home = dirs::home_dir().expect("home dir");
    let ctx = ToolContext::empty();
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "cd ~" }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    let expected = home.canonicalize().unwrap_or(home);
    assert_eq!(ctx.working_dir(), expected);
}

#[tokio::test]
async fn multiple_cds_apply_in_order() {
    // The shell would execute them sequentially; the parser applies each
    // in source order, so the last successful one is the final state.
    let root = tempfile::tempdir().expect("tempdir");
    let a = root.path().join("a");
    let b = root.path().join("b");
    std::fs::create_dir(&a).expect("mkdir a");
    std::fs::create_dir(&b).expect("mkdir b");
    let a_canonical = a.canonicalize().expect("canonicalize a");
    let b_canonical = b.canonicalize().expect("canonicalize b");
    let ctx = ToolContext::empty();
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": format!("cd {} && cd {}", a_canonical.display(), b_canonical.display()),
    }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), b_canonical);
}

#[test]
fn strip_surrounding_quotes_handles_double_quotes() {
    assert_eq!(strip_surrounding_quotes("\"foo bar\""), "foo bar");
}

#[test]
fn strip_surrounding_quotes_handles_single_quotes() {
    assert_eq!(strip_surrounding_quotes("'foo bar'"), "foo bar");
}

#[test]
fn strip_surrounding_quotes_leaves_unquoted_alone() {
    assert_eq!(strip_surrounding_quotes("foo"), "foo");
    assert_eq!(strip_surrounding_quotes("\"unmatched"), "\"unmatched");
    assert_eq!(strip_surrounding_quotes("'mismatched\""), "'mismatched\"");
}

// --- H12 regressions: process groups and bounded draining -------------------

/// A command that backgrounds a child (which inherits the stdout/stderr
/// pipes) must return promptly with the buffered output and the
/// streams-still-open annotation, not block until the grandchild exits.
#[tokio::test]
async fn backgrounded_child_returns_promptly_with_buffered_output() {
    let tool = BashTool::new();
    let started = std::time::Instant::now();
    let env = envelope(json!({
        "command": "echo started; sleep 30 &",
        "timeout": 60_u64,
    }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "tool blocked on the backgrounded child's pipe: {:?}",
        started.elapsed()
    );
    assert!(!out.is_error(), "{:?}", out.content);
    assert_eq!(out.content["exit_code"].as_i64(), Some(0));
    assert!(
        out.content["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("started"),
        "buffered output preserved: {:?}",
        out.content
    );
    assert_eq!(out.content["streams_still_open"].as_bool(), Some(true));
    assert!(
        out.content["streams_still_open_note"].as_str().is_some(),
        "annotation present: {:?}",
        out.content
    );
}

/// A command that finishes cleanly (pipes closed at exit) must not carry
/// the streams-still-open annotation.
#[tokio::test]
async fn clean_exit_reports_streams_closed() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "echo done" }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    assert_eq!(out.content["streams_still_open"].as_bool(), Some(false));
    assert!(out.content.get("streams_still_open_note").is_none());
}

/// Returns whether `pid` is still alive, probed via `kill -0` (works on
/// macOS and Linux without unsafe libc calls).
#[cfg(unix)]
fn process_alive(pid: i64) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|s| s.success())
}

/// On the degenerate no-manager path a timeout must still kill the entire
/// process tree, not just the `sh` wrapper: the grandchild `sleep` recorded in
/// the pid file has to be gone shortly after the tool returns. (With a manager
/// wired the command would migrate instead — see the migration tests.)
#[cfg(unix)]
#[tokio::test]
async fn timeout_kills_the_whole_process_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pid_file = dir.path().join("grandchild.pid");
    let tool = BashTool::new();
    let started = std::time::Instant::now();
    let env = envelope(json!({
        "command": format!("sleep 30 & echo $! > '{}'; wait", pid_file.display()),
        "timeout": 1_u64,
    }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    assert!(out.is_error());
    assert_eq!(out.content["timed_out"].as_bool(), Some(true));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "timed-out command did not return promptly: {:?}",
        started.elapsed()
    );

    let pid: i64 = std::fs::read_to_string(&pid_file)
        .expect("grandchild pid recorded")
        .trim()
        .parse()
        .expect("pid parses");
    // SIGKILL delivery is immediate but reaping by init is asynchronous;
    // poll briefly before declaring the grandchild a survivor.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while process_alive(pid) {
        assert!(
            std::time::Instant::now() < deadline,
            "grandchild sleep (pid {pid}) survived the group kill"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// R3: `run_in_background` returns immediately with a process id, spool path,
/// and check guidance; the process later shows Exited(0) with its output
/// spooled.
#[tokio::test]
#[serial_test::serial]
async fn run_in_background_returns_immediately_and_spools() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::set(dir.path());
    let (ctx, manager) = ctx_with_manager("bash-bg");
    let tool = BashTool::new();

    let started = std::time::Instant::now();
    let out = tool
        .execute(
            &envelope(json!({ "command": "sleep 1 && echo done", "run_in_background": true })),
            &ctx,
        )
        .await
        .expect("bash ok");
    assert!(
        started.elapsed() < std::time::Duration::from_millis(800),
        "run_in_background returns well under a second",
    );
    assert!(!out.is_error());
    assert_eq!(out.content["background"].as_bool(), Some(true));
    let process_id = out.content["process_id"].as_str().expect("process_id");
    assert!(out.content["spool_path"].as_str().is_some());
    assert!(
        out.content["hint"]
            .as_str()
            .expect("hint")
            .contains("process tool")
    );

    let handle = manager.get(process_id).expect("registered");
    wait_terminal(&handle).await;
    assert_eq!(
        handle.status(),
        crate::process::ProcessStatus::Exited { code: 0 }
    );
    wait_spool_contains(&handle, "done").await;
}

/// NP-002 R1a: `run_in_background` with a `watch: {brief, filter}` argument
/// spawns the process with exactly one active watch visible in `list` output,
/// and echoes the assigned watch id in the tool result.
#[tokio::test]
#[serial_test::serial]
async fn run_in_background_with_a_watch_attaches_it_at_spawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::set(dir.path());
    let (ctx, manager) = ctx_with_manager("bash-watch");
    let tool = BashTool::new();

    let out = tool
        .execute(
            &envelope(json!({
                "command": "sleep 30",
                "run_in_background": true,
                "watch": { "brief": "any error", "filter": "grep -i error" },
            })),
            &ctx,
        )
        .await
        .expect("bash ok");
    assert!(!out.is_error());
    let process_id = out.content["process_id"].as_str().expect("process_id");
    let watch_id = out.content["watch_id"].as_str().expect("watch_id echoed");
    assert!(watch_id.starts_with('w'));

    let watches = manager.watches_for(process_id);
    assert_eq!(watches.len(), 1, "exactly one watch active at spawn");
    assert_eq!(watches[0].watch_id, watch_id);
    assert_eq!(watches[0].brief, "any error");
    manager.shutdown();
}

/// NP-002 R1a: a `watch` argument without `run_in_background` is a structured
/// `InvalidArguments` failure — a watch attaches to a managed background
/// process, and there is none in the foreground path.
#[tokio::test]
#[serial_test::serial]
async fn watch_without_run_in_background_is_invalid_arguments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::set(dir.path());
    let (ctx, _manager) = ctx_with_manager("bash-watch-bad");
    let tool = BashTool::new();

    let err = tool
        .execute(
            &envelope(json!({
                "command": "echo hi",
                "watch": { "brief": "b", "filter": "cat" },
            })),
            &ctx,
        )
        .await
        .expect_err("watch without background is rejected");
    match err {
        ToolError::PreValidationFailed { payload } => {
            assert_eq!(payload.kind, ToolErrorKind::InvalidArguments);
        }
        other => panic!("expected InvalidArguments pre-validation, got {other:?}"),
    }
}

/// R3: `run_in_background` combined with `timeout` is a structured
/// `InvalidArguments` failure naming the conflict — a background process has no
/// timeout (owner ruling).
#[tokio::test]
#[serial_test::serial]
async fn run_in_background_with_timeout_is_invalid_arguments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::set(dir.path());
    let (ctx, _manager) = ctx_with_manager("bash-bg-conflict");
    let tool = BashTool::new();
    let err = tool
        .execute(
            &envelope(json!({
                "command": "sleep 1",
                "run_in_background": true,
                "timeout": 30_u64,
            })),
            &ctx,
        )
        .await
        .expect_err("conflict is a hard pre-validation error");
    match err {
        ToolError::PreValidationFailed { payload } => {
            assert_eq!(
                payload.kind,
                crate::tool::failure::ToolErrorKind::InvalidArguments
            );
            assert!(payload.message.contains("timeout"));
        }
        other => panic!("expected InvalidArguments, got {other:?}"),
    }
}

/// R3: a manager-owned background process keeps spooling a backgrounded
/// grandchild's output after the direct child exits — no drain grace cuts it
/// off (contrast with the foreground `streams_still_open` path).
#[tokio::test]
#[serial_test::serial]
async fn run_in_background_captures_grandchild_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::set(dir.path());
    let (ctx, manager) = ctx_with_manager("bash-bg-grandchild");
    let tool = BashTool::new();
    let out = tool
        .execute(
            &envelope(json!({
                "command": "(sleep 1; echo late) & echo early",
                "run_in_background": true,
            })),
            &ctx,
        )
        .await
        .expect("bash ok");
    let process_id = out.content["process_id"].as_str().expect("process_id");
    let handle = manager.get(process_id).expect("registered");
    wait_terminal(&handle).await; // direct child exits after printing "early"
    wait_spool_contains(&handle, "late").await; // grandchild keeps spooling
    let (bytes, _) = handle.spool().read_from(0).await.expect("spool read");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("early"), "direct output: {text}");
    assert!(
        text.contains("late"),
        "grandchild output kept spooling: {text}"
    );
    manager.shutdown();
}

/// R4 / F3 (a): `timeout: 0` waits forever and never migrates — even with a
/// manager wired, a fast command run with `timeout: 0` completes inline and the
/// migration machinery never engages (no `migrated` flag, no process id, no
/// adopted registry entry).
#[tokio::test]
#[serial_test::serial]
async fn timeout_zero_completes_without_migrating() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::set(dir.path());
    let (ctx, manager) = ctx_with_manager("bash-timeout-zero");
    let tool = BashTool::new();
    let out = tool
        .execute(
            &envelope(json!({ "command": "echo done", "timeout": 0_u64 })),
            &ctx,
        )
        .await
        .expect("bash ok");
    assert!(!out.is_error());
    assert!(
        out.content.get("migrated").is_none(),
        "timeout:0 never migrates: {:?}",
        out.content,
    );
    assert!(
        out.content.get("process_id").is_none(),
        "no background process is created",
    );
    assert_eq!(out.content["exit_code"].as_i64(), Some(0));
    assert!(
        out.content["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("done"),
    );
    assert!(
        manager.list().is_empty(),
        "the migration machinery never adopted anything",
    );
}

/// Poll a file into existence and parse its trimmed contents. The migrating
/// shell writes these marker files near t=0, well before migration at the 1s
/// timeout, but poll defensively rather than racing the filesystem.
#[cfg(unix)]
async fn read_marker(path: &std::path::Path) -> String {
    for _ in 0..600 {
        if let Ok(contents) = std::fs::read_to_string(path) {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("marker file {path:?} never appeared");
}

/// R4 / F3 (b): the process group survives migration intact. The adopted handle
/// references the same pgid the shell was spawned into (on Unix the group
/// leader's pid == its `$$`), and a kill *after* migration reaches the whole
/// group — a backgrounded grandchild sharing the group is reaped.
#[cfg(unix)]
#[tokio::test]
#[serial_test::serial]
async fn pgid_survives_migration_and_post_migration_kill_reaches_the_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::set(dir.path());
    let pgid_file = dir.path().join("pgid.txt");
    let gc_file = dir.path().join("grandchild.pid");
    let (ctx, manager) = ctx_with_manager("bash-migrate-pgid");
    let tool = BashTool::new();
    // The shell records its own pid ($$, which equals the pgid under
    // process_group(0)) and a backgrounded grandchild's pid, then sleeps past
    // the 1s timeout so it migrates rather than completing.
    let env = envelope(json!({
        "command": format!(
            "echo $$ > '{}'; sleep 30 & echo $! > '{}'; sleep 5",
            pgid_file.display(),
            gc_file.display(),
        ),
        "timeout": 1_u64,
    }));
    let out = tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(out.content["migrated"].as_bool(), Some(true));
    let process_id = out.content["process_id"].as_str().expect("process_id");
    let handle = manager.get(process_id).expect("registered");

    // Same pgid before/after adopt: the shell's recorded group id equals the
    // adopted handle's pid.
    let shell_pgid: u32 = read_marker(&pgid_file).await.parse().expect("pgid parses");
    assert_eq!(
        handle.pid(),
        Some(shell_pgid),
        "the adopted handle references the original process group, unchanged by migration",
    );

    // A grandchild sharing the group is alive before the kill.
    let gc_pid: i64 = read_marker(&gc_file).await.parse().expect("gc pid parses");
    assert!(process_alive(gc_pid), "grandchild alive before the kill");

    // Kill after migration reaches the whole group: the grandchild dies.
    handle.kill().await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while process_alive(gc_pid) {
        assert!(
            std::time::Instant::now() < deadline,
            "grandchild (pid {gc_pid}) survived the post-migration group kill",
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// R4 / F5: when the pre-migration seed is delivered INLINE the model has
/// already seen it, so the process's model-output cursor is advanced past the
/// seed at adopt time — the first op=output returns only new post-migration
/// output, never a verbatim re-delivery of the seed.
#[tokio::test]
#[serial_test::serial]
async fn inline_migration_seed_is_not_re_delivered_by_first_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::set(dir.path());
    let (ctx, manager) = ctx_with_manager("bash-migrate-inline-seed");
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "echo seed-line; sleep 3; echo after-line",
        "timeout": 1_u64,
    }));
    let out = tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(out.content["migrated"].as_bool(), Some(true));
    // A small seed is delivered inline in the migrated result.
    assert!(
        out.content["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("seed-line"),
        "the seed is delivered inline: {:?}",
        out.content,
    );
    assert!(
        out.content.get("output_redirected").is_none(),
        "a small seed is inline, not redirected",
    );
    let process_id = out.content["process_id"].as_str().expect("process_id");
    let handle = manager.get(process_id).expect("registered");

    // Post-migration output arrives ~2s later.
    wait_spool_contains(&handle, "after-line").await;
    let (bytes, _) = manager
        .model_output(process_id)
        .await
        .expect("known id")
        .expect("read ok");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert!(
        text.contains("after-line"),
        "the first output returns the new post-migration output: {text}",
    );
    assert!(
        !text.contains("seed-line"),
        "the inline seed is NOT re-delivered — the model already saw it inline: {text}",
    );
    handle.kill().await;
}

/// R4 / F5: when the pre-migration seed is a disk REDIRECT (large output) the
/// model saw only a spool path, never the bytes — so the model cursor stays at
/// 0 and the first op=output returns the full seed from the start.
#[tokio::test]
#[serial_test::serial]
async fn redirected_migration_seed_is_delivered_by_first_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _home = HomeGuard::set(dir.path());
    let (ctx, manager) = ctx_with_manager("bash-migrate-redirect-seed");
    let tool = BashTool::new();
    // >22000 chars before the 1s timeout forces a redirect snapshot.
    let env = envelope(json!({
        "command": "yes REDIRECTSEED | head -c 23000; printf '\\n'; sleep 5",
        "timeout": 1_u64,
    }));
    let out = tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(out.content["migrated"].as_bool(), Some(true));
    assert_eq!(
        out.content["output_redirected"].as_bool(),
        Some(true),
        "a large seed is a redirect, not inline: {:?}",
        out.content,
    );
    let process_id = out.content["process_id"].as_str().expect("process_id");
    let handle = manager.get(process_id).expect("registered");

    // The model cursor is at 0: the first op=output returns the seed content the
    // model never saw inline.
    let (bytes, _) = manager
        .model_output(process_id)
        .await
        .expect("known id")
        .expect("read ok");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert!(
        text.contains("REDIRECTSEED"),
        "the redirected seed IS delivered by the first output (cursor started at 0)",
    );
    handle.kill().await;
}
