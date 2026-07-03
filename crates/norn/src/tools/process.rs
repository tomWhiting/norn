//! `ProcessTool` — model-facing control over manager-owned background
//! processes: `output`, `status`, `kill`, and `list`.
//!
//! Resolves the agent's [`ProcessManager`](crate::process::ProcessManager)
//! extension (installed at assembly alongside the manager that owns this
//! agent's processes) and drives it. The tool is registered **only** when that
//! extension is present — a registry assembled without a manager carries no
//! `process` tool, exactly as `cron` is gated on its schedule handle.
//!
//! - `output` returns the content appended since the model's last `output`
//!   call for that process (the cursor is owned by the manager, one per
//!   process), plus the current status. A large new region redirects to the
//!   spool path with a read/grep hint instead of flooding context — a display
//!   budget, never a spool cap.
//! - `status` reports status, pid, start time, exit information, spool path,
//!   and spool size.
//! - `kill` signals the process group (idempotent on an already-terminal
//!   process — it reports the terminal status rather than erroring).
//! - `list` returns every process this manager owns, with no pagination cap.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::process::{ProcessHandle, ProcessManager, ProcessStatus};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::output_budget::{READ_OUTPUT_CHAR_LIMIT, ToolOutputBudget};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

/// Stable tool name for background-process control.
pub const PROCESS_TOOL_NAME: &str = "process";

/// Model-facing control over manager-owned background processes.
pub struct ProcessTool;

impl ProcessTool {
    /// Construct the tool.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for ProcessTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct ProcessArgs {
    op: ProcessOp,
    #[serde(default)]
    id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProcessOp {
    Output,
    Status,
    Kill,
    List,
}

fn invalid_arguments(reason: String, detail: Value) -> ToolOutput {
    ToolOutput::failure(
        ToolErrorPayload::new(ToolErrorKind::InvalidArguments, reason).with_detail(detail),
    )
}

fn not_found(id: &str) -> ToolOutput {
    ToolOutput::failure(
        ToolErrorPayload::new(
            ToolErrorKind::NotFound,
            format!("no managed process with id {id}: it is unknown to this agent's manager"),
        )
        .with_detail(json!({ "id": id })),
    )
}

/// The required `id` argument for `output`/`status`/`kill`, or a structured
/// failure naming the missing argument. Boxed so the `Err` variant stays small.
fn require_id(args: &ProcessArgs) -> Result<&str, Box<ToolOutput>> {
    args.id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            Box::new(invalid_arguments(
                "this op requires \"id\" — the process id returned when it was backgrounded"
                    .to_string(),
                json!({ "argument": "id" }),
            ))
        })
}

/// Fold a process's status into JSON fields: the status label plus `exit_code`
/// when it exited on its own and `killed` when it was killed.
fn insert_status_fields(map: &mut serde_json::Map<String, Value>, status: ProcessStatus) {
    map.insert(
        "status".to_owned(),
        Value::String(status.label().to_owned()),
    );
    match status {
        ProcessStatus::Running => {}
        ProcessStatus::Exited { code } => {
            map.insert("exit_code".to_owned(), json!(code));
        }
        ProcessStatus::Killed => {
            map.insert("killed".to_owned(), Value::Bool(true));
        }
    }
}

fn status_object(id: &str, status: ProcessStatus) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("process_id".to_owned(), Value::String(id.to_owned()));
    insert_status_fields(&mut map, status);
    Value::Object(map)
}

async fn op_output(
    manager: &ProcessManager,
    args: &ProcessArgs,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    let id = match require_id(args) {
        Ok(id) => id,
        Err(failure) => return Ok(*failure),
    };
    let Some(result) = manager.model_output(id).await else {
        return Ok(not_found(id));
    };
    let (bytes, status) = result.map_err(|e| ToolError::ExecutionFailed {
        reason: format!("failed to read output for process {id}: {e}"),
    })?;
    let spool_path = manager.get(id).map(|h| h.spool().display_path());

    let mut map = serde_json::Map::new();
    map.insert("process_id".to_owned(), Value::String(id.to_owned()));
    insert_status_fields(&mut map, status);
    if let Some(path) = &spool_path {
        map.insert("spool_path".to_owned(), Value::String(path.clone()));
    }

    if bytes.is_empty() {
        map.insert("new_output".to_owned(), Value::Bool(false));
        map.insert(
            "message".to_owned(),
            Value::String("No new output since your last check.".to_owned()),
        );
        return Ok(ToolOutput::success(Value::Object(map)));
    }

    let text = String::from_utf8_lossy(&bytes).into_owned();
    let budget = ctx
        .get_extension::<ToolOutputBudget>()
        .map_or(READ_OUTPUT_CHAR_LIMIT, |b| b.read_output_char_limit);
    map.insert("new_output".to_owned(), Value::Bool(true));
    if text.chars().count() > budget {
        // Display budget, not a spool cap: the whole region is on disk; point
        // the model at it instead of flooding context.
        map.insert("output_redirected".to_owned(), Value::Bool(true));
        map.insert("new_output_chars".to_owned(), json!(text.chars().count()));
        let hint = spool_path.as_deref().map_or_else(
            || "The new output is large; read it from the spool file instead of inlining it.".to_owned(),
            |path| format!(
                "The new output is large ({} chars) and was not inlined. Read it from the spool \
                 file with the read tool (path {path}) or grep it with the search tool.",
                text.chars().count(),
            ),
        );
        map.insert("hint".to_owned(), Value::String(hint));
    } else {
        map.insert("output".to_owned(), Value::String(text));
    }
    Ok(ToolOutput::success(Value::Object(map)))
}

fn op_status(manager: &ProcessManager, args: &ProcessArgs) -> ToolOutput {
    let id = match require_id(args) {
        Ok(id) => id,
        Err(failure) => return *failure,
    };
    let Some(handle) = manager.get(id) else {
        return not_found(id);
    };
    ToolOutput::success(describe(&handle))
}

async fn op_kill(manager: &ProcessManager, args: &ProcessArgs) -> ToolOutput {
    let id = match require_id(args) {
        Ok(id) => id,
        Err(failure) => return *failure,
    };
    let Some(handle) = manager.get(id) else {
        return not_found(id);
    };
    // Idempotent: an already-terminal process reports its terminal status
    // without erroring or re-killing.
    let status = handle.kill().await;
    let mut obj = status_object(id, status);
    if let Some(map) = obj.as_object_mut() {
        map.insert(
            "spool_path".to_owned(),
            Value::String(handle.spool().display_path()),
        );
    }
    ToolOutput::success(obj)
}

fn op_list(manager: &ProcessManager) -> ToolOutput {
    let processes: Vec<Value> = manager.list().iter().map(describe).collect();
    ToolOutput::success(json!({
        "count": processes.len(),
        "processes": processes,
    }))
}

/// The full status description of one process (shared by `status` and `list`).
fn describe(handle: &ProcessHandle) -> Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "process_id".to_owned(),
        Value::String(handle.label().to_owned()),
    );
    map.insert(
        "command".to_owned(),
        Value::String(handle.command().to_owned()),
    );
    insert_status_fields(&mut map, handle.status());
    map.insert("pid".to_owned(), json!(handle.pid()));
    map.insert("started_at".to_owned(), json!(handle.started_at()));
    if let Some(exited_at) = handle.exited_at() {
        map.insert("exited_at".to_owned(), json!(exited_at));
    }
    map.insert(
        "spool_path".to_owned(),
        Value::String(handle.spool().display_path()),
    );
    map.insert(
        "spool_size".to_owned(),
        json!(handle.spool().committed_len()),
    );
    Value::Object(map)
}

#[async_trait]
impl Tool for ProcessTool {
    fn name(&self) -> &'static str {
        PROCESS_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        include_str!("guidance/process.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Shell
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("guidance/process.usage.md"))
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["op"],
            "additionalProperties": false,
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["output", "status", "kill", "list"],
                    "description": "The operation: fetch new output, inspect status, kill the process, or list all background processes."
                },
                "id": {
                    "type": "string",
                    "description": "The process id (e.g. \"p1\") for op output/status/kill. Not used by list."
                }
            }
        })
    }

    fn effect(&self) -> ToolEffect {
        // Whole-tool effect is the conservative union: `kill` mutates process
        // state, so the tool reports at least Process.
        ToolEffect::Process
    }

    fn effect_for_args(&self, args: &Value) -> ToolEffect {
        match args.get("op").and_then(Value::as_str) {
            Some("output" | "status" | "list") => ToolEffect::ReadOnly,
            _ => ToolEffect::Process,
        }
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ProcessArgs =
            serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
                ToolError::pre_validation(
                    ToolErrorKind::InvalidArguments,
                    format!("invalid process arguments: {e}"),
                )
            })?;
        let manager = ctx.require_extension::<ProcessManager>()?;
        match args.op {
            ProcessOp::Output => op_output(&manager, &args, ctx).await,
            ProcessOp::Status => Ok(op_status(&manager, &args)),
            ProcessOp::Kill => Ok(op_kill(&manager, &args).await),
            ProcessOp::List => Ok(op_list(&manager)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    struct HomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(path: &std::path::Path) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with `#[serial]`; no concurrent reader.
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

    fn envelope(args: Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: PROCESS_TOOL_NAME.to_string(),
            model_args: args,
            metadata: Value::Null,
        }
    }

    fn armed_ctx(manager: Arc<ProcessManager>) -> ToolContext {
        let ctx = ToolContext::empty();
        ctx.insert_extension(manager);
        ctx
    }

    async fn run(ctx: &ToolContext, args: Value) -> ToolOutput {
        ProcessTool::new()
            .execute(&envelope(args), ctx)
            .await
            .expect("process tool executes")
    }

    async fn wait_terminal(handle: &ProcessHandle) {
        for _ in 0..600 {
            if !handle.is_running() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("process did not terminate");
    }

    /// Wait until the spool's on-disk content contains `needle` (the drains
    /// flush asynchronously after the direct child exits).
    async fn wait_spool_contains(handle: &ProcessHandle, needle: &str) {
        for _ in 0..600 {
            let (bytes, _) = handle.spool().read_from(0).await.unwrap();
            if String::from_utf8_lossy(&bytes).contains(needle) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("spool never contained {needle:?}");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn output_returns_only_new_content_each_call() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let manager = Arc::new(ProcessManager::new(Some("sess".to_owned()), None));
        let cwd = std::env::current_dir().unwrap();
        let handle = manager
            .spawn("echo one; sleep 0.3; echo two", &cwd, None)
            .await
            .unwrap();
        let ctx = armed_ctx(Arc::clone(&manager));

        tokio::time::sleep(Duration::from_millis(120)).await;
        let first = run(&ctx, json!({ "op": "output", "id": "p1" })).await;
        assert_eq!(first.content["new_output"], true);
        assert!(first.content["output"].as_str().unwrap().contains("one"));
        assert!(!first.content["output"].as_str().unwrap().contains("two"));

        wait_terminal(&handle).await;
        wait_spool_contains(&handle, "two").await;
        let second = run(&ctx, json!({ "op": "output", "id": "p1" })).await;
        assert!(second.content["output"].as_str().unwrap().contains("two"));
        assert!(!second.content["output"].as_str().unwrap().contains("one"));
        assert_eq!(second.content["status"], "exited");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn output_with_no_new_content_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let manager = Arc::new(ProcessManager::new(Some("sess".to_owned()), None));
        let cwd = std::env::current_dir().unwrap();
        let handle = manager.spawn("echo hi", &cwd, None).await.unwrap();
        wait_terminal(&handle).await;
        let ctx = armed_ctx(Arc::clone(&manager));

        let _drain = run(&ctx, json!({ "op": "output", "id": "p1" })).await;
        let again = run(&ctx, json!({ "op": "output", "id": "p1" })).await;
        assert!(!again.is_error());
        assert_eq!(again.content["new_output"], false);
        assert!(again.content["message"].as_str().is_some());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn kill_running_then_idempotent_on_exited() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let manager = Arc::new(ProcessManager::new(Some("sess".to_owned()), None));
        let cwd = std::env::current_dir().unwrap();
        manager.spawn("sleep 30", &cwd, None).await.unwrap();
        let ctx = armed_ctx(Arc::clone(&manager));

        let killed = run(&ctx, json!({ "op": "kill", "id": "p1" })).await;
        assert!(!killed.is_error());
        assert_eq!(killed.content["status"], "killed");

        let quick = manager.spawn("true", &cwd, None).await.unwrap();
        wait_terminal(&quick).await;
        let again = run(&ctx, json!({ "op": "kill", "id": "p2" })).await;
        assert!(
            !again.is_error(),
            "killing an exited process is not an error"
        );
        assert_eq!(again.content["status"], "exited");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn list_reflects_all_states_with_no_cap() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let manager = Arc::new(ProcessManager::new(Some("sess".to_owned()), None));
        let cwd = std::env::current_dir().unwrap();
        let running = manager.spawn("sleep 30", &cwd, None).await.unwrap();
        let exited = manager.spawn("true", &cwd, None).await.unwrap();
        wait_terminal(&exited).await;
        let killed = manager.spawn("sleep 30", &cwd, None).await.unwrap();
        killed.kill().await;
        let ctx = armed_ctx(Arc::clone(&manager));

        let out = run(&ctx, json!({ "op": "list" })).await;
        assert_eq!(out.content["count"], 3);
        let statuses: Vec<&str> = out.content["processes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["status"].as_str().unwrap())
            .collect();
        assert!(statuses.contains(&"running"));
        assert!(statuses.contains(&"exited"));
        assert!(statuses.contains(&"killed"));
        let _ = running;
        manager.shutdown();
    }

    /// R6: a new-output region beyond the inline display budget is redirected
    /// to the spool path with a read/grep hint instead of being inlined.
    #[tokio::test]
    #[serial_test::serial]
    async fn large_output_redirects_to_the_spool_instead_of_inlining() {
        use crate::tool::output_budget::ToolOutputBudget;

        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let manager = Arc::new(ProcessManager::new(Some("sess".to_owned()), None));
        let cwd = std::env::current_dir().unwrap();
        // ~30000 chars of output, well over the smallest (8000-char) read
        // budget even before all drains flush.
        let handle = manager
            .spawn(
                "yes ................................................ | head -c 30000",
                &cwd,
                None,
            )
            .await
            .unwrap();
        wait_terminal(&handle).await;
        // The drains keep flushing after the direct child exits; wait until the
        // spool has committed enough to exceed the display budget deterministically.
        for _ in 0..600 {
            if handle.spool().committed_len() > 16_000 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let ctx = armed_ctx(Arc::clone(&manager));
        // Install a small display budget so the redirect branch engages.
        ctx.insert_extension(Arc::new(ToolOutputBudget::for_context_window(Some(64_000))));

        let out = run(&ctx, json!({ "op": "output", "id": "p1" })).await;
        assert!(!out.is_error());
        assert_eq!(out.content["new_output"], true);
        assert_eq!(
            out.content["output_redirected"], true,
            "a large region redirects instead of inlining",
        );
        assert!(
            out.content["output"].is_null(),
            "the raw output is not inlined"
        );
        assert!(out.content["spool_path"].as_str().is_some());
        assert!(out.content["hint"].as_str().unwrap().contains("read"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn status_and_output_unknown_id_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let manager = Arc::new(ProcessManager::new(Some("sess".to_owned()), None));
        let ctx = armed_ctx(manager);
        for op in ["status", "output"] {
            let out = run(&ctx, json!({ "op": op, "id": "p404" })).await;
            assert!(out.is_error());
            assert_eq!(out.error().unwrap().kind, ToolErrorKind::NotFound);
        }
    }

    #[tokio::test]
    async fn missing_manager_extension_is_a_typed_error() {
        let ctx = ToolContext::empty();
        let err = ProcessTool::new()
            .execute(&envelope(json!({ "op": "list" })), &ctx)
            .await
            .expect_err("no ProcessManager installed");
        assert!(matches!(err, ToolError::MissingExtension { .. }));
    }

    #[test]
    fn list_and_output_are_read_only_kill_is_process() {
        let tool = ProcessTool::new();
        assert_eq!(
            tool.effect_for_args(&json!({ "op": "list" })),
            ToolEffect::ReadOnly,
        );
        assert_eq!(
            tool.effect_for_args(&json!({ "op": "output" })),
            ToolEffect::ReadOnly,
        );
        assert_eq!(
            tool.effect_for_args(&json!({ "op": "kill" })),
            ToolEffect::Process,
        );
    }
}
