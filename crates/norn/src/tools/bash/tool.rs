//! The [`BashTool`] implementation: argument schema, risk-classifying
//! pre-validation, execution via [`super::process::run_shell`], and
//! rerun/read-output follow-up registration.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::process::{ProcessHandle, ProcessManager};
use crate::tool::ToolArgs;
use crate::tool::context::{ProcessEnv, ToolContext};
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::follow_up::{Confidence, ExpiryCondition, FollowUpAction, FollowUpArgsMode};
use crate::tool::lifecycle::PreValidateOutcome;
use crate::tool::output_budget::READ_DEFAULT_LINE_LIMIT;
use crate::tool::risk::{BashRiskTier, classify_risk};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

use super::cd_track::apply_cd_from_command;
use super::output::{CapturedOutput, OutputCapture};
use super::process::{DEFAULT_DRAIN_GRACE, ShellOutcome, run_shell};
use crate::tools::confinement::check_confinement;

/// The [`ProcessManager`] extension type name, used in the typed
/// missing-extension failure when a background spawn or migration is requested
/// without a manager wired.
const PROCESS_MANAGER_EXTENSION: &str = "norn::process::ProcessManager";

/// Default command timeout (seconds) when none is supplied by the caller.
pub(super) const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Combined stdout + stderr character budget for inline bash output.
pub(super) const INLINE_OUTPUT_THRESHOLD_CHARS: usize = 22_000;

const REDIRECT_HINT: &str = "Output exceeded the 22000-character inline threshold and was written to disk. Use the read tool with offset/limit to inspect specific sections, or grep the file for error patterns.";

/// Annotation attached when stdout/stderr were still held open by a
/// background child at the end of the drain grace period.
const STREAMS_OPEN_NOTE: &str = "stdout/stderr were still open when the command finished (a background child is holding the pipe); buffered output was returned but later output from that child is not captured.";

/// Model-supplied arguments for [`BashTool`].
#[derive(Debug, Default, Deserialize, Serialize, ToolArgs)]
pub(super) struct BashArgs {
    /// Shell command line. Executed via `sh -c`.
    command: String,
    /// Timeout in seconds. 0 means wait forever. Defaults to 120.
    #[serde(default)]
    timeout: Option<u64>,
    /// Working directory for the subprocess. Resolved like file-tool paths: `~` expands to the home directory and relative paths resolve against the agent's working directory.
    #[serde(default)]
    working_dir: Option<String>,
    /// Run detached in the background instead of waiting. Returns immediately with a process id and spool path; the process runs with no timeout until it exits or you kill it. Cannot be combined with `timeout`. Check on it with the `process` tool.
    #[serde(default)]
    run_in_background: Option<bool>,
    /// Optional deterministic watch to attach at spawn (only with `run_in_background`). A `{brief, filter}` object: the filter runs via `sh -c` over each new spool region and a match (exit 0) wakes you with the matching excerpt. Attach more later with the `process` tool (op=watch).
    #[serde(default)]
    watch: Option<WatchSpec>,
}

/// A watch to attach at spawn time (NP-002 R1a): an agent-authored filter over
/// a background process's output.
#[derive(Debug, Default, Deserialize, Serialize, ToolArgs)]
pub(super) struct WatchSpec {
    /// A human-readable statement of what to watch for (e.g. "a compile error").
    brief: String,
    /// A shell filter script run via `sh -c` over each new spool region on stdin. Exit 0 means match (its stdout is the excerpt that wakes you); any other exit means no match, except exits 126 and 127, which the shell reserves (command not executable, and command not found) and which are reported as watch errors rather than no-matches.
    filter: String,
}

/// Bash tool: executes shell commands with streaming output and risk tagging.
///
/// # Workspace confinement
///
/// When the [`ToolContext`] carries a workspace-confinement root, the
/// model-supplied `working_dir` argument is refused if it resolves outside
/// that root. The executed command itself is **not** confined: it can still
/// `cd` elsewhere or touch absolute paths outside the root. Confining what
/// an arbitrary shell command does is out of scope for path-level checks —
/// a known, documented limitation of bash relative to the file tools.
#[derive(Debug)]
pub struct BashTool {
    /// Grace period granted to the stdout/stderr drains after the shell
    /// exits. See [`Self::with_drain_grace`].
    drain_grace: Duration,
}

impl BashTool {
    /// Creates a new `BashTool` with the default drain grace period
    /// (2 seconds — the owner-approved default).
    #[must_use]
    pub fn new() -> Self {
        Self {
            drain_grace: DEFAULT_DRAIN_GRACE,
        }
    }

    /// Override the grace period granted to the stdout/stderr drain tasks
    /// after the shell process has exited. A command that backgrounds a
    /// child (`server &`) leaves that child holding the output pipes; the
    /// grace period bounds how long the tool waits for them to close before
    /// returning the buffered output annotated with `streams_still_open`.
    ///
    /// Defaults to 2 seconds (owner-approved). Embedders can install a
    /// replacement bash tool with a different grace via
    /// [`AgentBuilder::bash_drain_grace`](crate::agent::AgentBuilder::bash_drain_grace)
    /// or by registering `BashTool::new().with_drain_grace(..)` directly.
    #[must_use]
    pub fn with_drain_grace(mut self, grace: Duration) -> Self {
        self.drain_grace = grace;
        self
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the model-facing content for a completed foreground run (the
/// historical inline / disk-redirected result shape).
fn completed_content(
    execution: &super::process::ShellExecution,
    timeout_secs: u64,
    tier: BashRiskTier,
) -> Value {
    match &execution.captured {
        CapturedOutput::Inline { stdout, stderr } => json!({
            "exit_code": execution.exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "timed_out": execution.timed_out,
            "streams_still_open": execution.streams_still_open,
            "metadata": {
                "risk_tier": tier,
                "timeout_secs": timeout_secs,
            }
        }),
        CapturedOutput::Redirected {
            output_path,
            output_chars,
        } => json!({
            "exit_code": execution.exit_code,
            "output_redirected": true,
            "output_path": output_path,
            "output_chars": output_chars,
            "timed_out": execution.timed_out,
            "streams_still_open": execution.streams_still_open,
            "hint": REDIRECT_HINT,
            "metadata": {
                "risk_tier": tier,
                "timeout_secs": timeout_secs,
            }
        }),
    }
}

/// Build the model-facing content for a `run_in_background` spawn (R3): the
/// process id, spool path, command, and check guidance.
fn background_content(handle: &ProcessHandle, command: &str, tier: BashRiskTier) -> Value {
    let id = handle.label();
    json!({
        "background": true,
        "process_id": id,
        "spool_path": handle.spool().display_path(),
        "command": command,
        "hint": format!(
            "Started in the background as process {id}. It runs with no timeout until it exits \
             or you kill it. Check its output with the process tool (op=output, id={id}), or \
             stop it (op=kill, id={id})."
        ),
        "metadata": { "risk_tier": tier }
    })
}

/// Attach a spawn-time watch (NP-002 R1a) to a just-backgrounded process and
/// fold the result into the tool's response `content`. The process is freshly
/// spawned and `Running`, so attach normally succeeds; a command that exits in
/// the microsecond before the watch attaches surfaces a `watch_error` note
/// rather than silently dropping the watch (its output remains readable).
fn attach_spawn_watch(
    manager: &ProcessManager,
    handle: &ProcessHandle,
    watch: &WatchSpec,
    cwd: &std::path::Path,
    process_env: Option<&ProcessEnv>,
    content: &mut Value,
) {
    let (key, value) = match manager.attach_watch(
        handle.label(),
        watch.brief.clone(),
        watch.filter.clone(),
        cwd.to_path_buf(),
        process_env.cloned(),
    ) {
        Ok(attached) => ("watch_id", attached.watch_id),
        Err(error) => (
            "watch_error",
            format!(
                "the watch could not be attached ({error:?}); the process is spooling and its \
                 output remains readable with the process tool (op=output)"
            ),
        ),
    };
    if let Some(map) = content.as_object_mut() {
        map.insert(key.to_owned(), Value::String(value));
    }
}

/// Build the model-facing content for a command migrated at its timeout
/// boundary (R4): `migrated: true`, the process id, spool path, the output
/// captured before migration, and explicit guidance on how to check on it.
fn migrated_content(
    handle: &ProcessHandle,
    command: &str,
    timeout_secs: u64,
    tier: BashRiskTier,
    snapshot: CapturedOutput,
) -> Value {
    let id = handle.label();
    let spool_path = handle.spool().display_path();
    let hint = format!(
        "This command exceeded its {timeout_secs}s timeout and was moved to the background as \
         process {id} instead of being killed — it is still running and nothing was lost. Check \
         its new output with the process tool (op=output, id={id}), or stop it (op=kill, \
         id={id}). Its full output is spooled at {spool_path}."
    );
    let mut content = match snapshot {
        CapturedOutput::Inline { stdout, stderr } => json!({
            "stdout": stdout,
            "stderr": stderr,
        }),
        CapturedOutput::Redirected {
            output_path,
            output_chars,
        } => json!({
            "output_redirected": true,
            "output_path": output_path,
            "output_chars": output_chars,
        }),
    };
    if let Some(map) = content.as_object_mut() {
        map.insert("migrated".to_owned(), Value::Bool(true));
        map.insert("process_id".to_owned(), Value::String(id.to_owned()));
        map.insert("spool_path".to_owned(), Value::String(spool_path));
        map.insert("command".to_owned(), Value::String(command.to_owned()));
        map.insert("hint".to_owned(), Value::String(hint));
        map.insert(
            "metadata".to_owned(),
            json!({ "risk_tier": tier, "timeout_secs": timeout_secs }),
        );
    }
    content
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        include_str!("../guidance/bash.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Shell
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("../guidance/bash.usage.md"))
    }

    fn input_schema(&self) -> Value {
        BashArgs::json_schema()
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Process
    }

    async fn pre_validate(&self, envelope: &ToolEnvelope, ctx: &ToolContext) -> PreValidateOutcome {
        let _ = ctx;
        let Some(cmd) = envelope.model_args.get("command").and_then(Value::as_str) else {
            // Missing command — surface as an execute-time pre-validation error
            // (more informative than blocking here). Proceed so execute() can
            // produce a structured PreValidationFailed.
            return PreValidateOutcome::Proceed;
        };
        let tier = classify_risk(cmd);
        if tier == BashRiskTier::Critical {
            tracing::warn!(
                command = %cmd,
                risk = ?tier,
                "critical-risk bash command — orchestrator policy decides whether to block",
            );
        }
        PreValidateOutcome::Proceed
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: BashArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                format!("invalid bash arguments: {e}"),
            )
        })?;

        let tier = classify_risk(&args.command);
        let timeout_secs = args.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);

        // Child CWD precedence: model-supplied `working_dir` arg wins when
        // present (per-call override); otherwise default to the agent's
        // shared working directory. The argument resolves through the tool
        // context (tilde expansion, relative-to-agent-working-dir) exactly
        // like file-tool paths, and a confined context refuses an
        // out-of-root target. The command itself is not confined — it can
        // still `cd` out — see the type-level docs for that limitation.
        let child_cwd = match args.working_dir.as_deref() {
            Some(raw) => {
                let resolved = ctx.resolve_path(raw);
                if let Err(reason) = check_confinement(ctx, &resolved) {
                    return Err(ToolError::PreValidationFailed {
                        payload: ToolErrorPayload::new(ToolErrorKind::PermissionDenied, reason)
                            .with_detail(json!({ "working_dir": raw })),
                    });
                }
                if !resolved.is_dir() {
                    return Err(ToolError::PreValidationFailed {
                        payload: ToolErrorPayload::new(
                            ToolErrorKind::NotFound,
                            format!(
                                "working_dir `{raw}` resolves to {} which is not an existing \
                                 directory",
                                resolved.display()
                            ),
                        )
                        .with_detail(json!({ "working_dir": raw })),
                    });
                }
                resolved
            }
            None => ctx.working_dir(),
        };

        let manager = ctx.get_extension::<ProcessManager>();

        // A spawn-time watch only makes sense for a manager-owned background
        // process — there is nothing to attach it to in the foreground path.
        if args.watch.is_some() && !args.run_in_background.unwrap_or(false) {
            return Err(ToolError::PreValidationFailed {
                payload: ToolErrorPayload::new(
                    ToolErrorKind::InvalidArguments,
                    "watch requires run_in_background: a watch attaches to a managed background \
                     process. Background the command, or attach a watch later with the process \
                     tool (op=watch).",
                )
                .with_detail(json!({ "run_in_background": args.run_in_background })),
            });
        }

        // Explicit backgrounding (R3): spawn through the manager and return at
        // once. A background process has no timeout (owner ruling), so
        // combining `run_in_background` with `timeout` is a structured error.
        if args.run_in_background.unwrap_or(false) {
            if args.timeout.is_some() {
                return Err(ToolError::PreValidationFailed {
                    payload: ToolErrorPayload::new(
                        ToolErrorKind::InvalidArguments,
                        "run_in_background cannot be combined with timeout: a background \
                         process has no timeout — it runs until it exits or is killed",
                    )
                    .with_detail(json!({
                        "run_in_background": true,
                        "timeout": args.timeout,
                    })),
                });
            }
            let Some(manager) = manager else {
                return Err(ToolError::MissingExtension {
                    extension: PROCESS_MANAGER_EXTENSION.to_owned(),
                });
            };
            let process_env = ctx.get_extension::<ProcessEnv>();
            let handle = manager
                .spawn(&args.command, &child_cwd, process_env.as_deref())
                .await
                .map_err(ToolError::from)?;
            let mut content = background_content(&handle, &args.command, tier);
            if let Some(watch) = &args.watch {
                attach_spawn_watch(
                    &manager,
                    &handle,
                    watch,
                    &child_cwd,
                    process_env.as_deref(),
                    &mut content,
                );
            }
            return Ok(ToolOutput::success(content));
        }

        // Foreground path. When a manager is wired and the timeout is bounded,
        // reaching it migrates the command to the background instead of killing
        // it (R4); timeout: 0 waits forever and never migrates.
        let migrate = manager.is_some() && timeout_secs != 0;
        let capture = OutputCapture::new(ctx, envelope);
        let outcome = run_shell(
            &args.command,
            &child_cwd,
            timeout_secs,
            self.drain_grace,
            migrate,
            ctx,
            Arc::clone(&capture),
        )
        .await?;

        // Inspect the model's command string for `cd` directives and update
        // the agent's working directory accordingly. Done unconditionally
        // (regardless of overall exit status) because intermediate `cd`s in
        // a chain like `cd /foo && false` did happen even if a later step
        // failed. Targets that don't resolve to an existing directory are
        // skipped — the `is_dir` check is the safety net for typos and
        // failed substitutions like `cd $(cmd-that-failed)`.
        apply_cd_from_command(ctx, &args.command);

        match outcome {
            ShellOutcome::Migrated(handoff) => {
                let manager = manager.ok_or_else(|| ToolError::ExecutionFailed {
                    reason: "internal error: a command migrated without a process manager"
                        .to_owned(),
                })?;
                let private_fs_permit = capture.take_auxiliary_permit().await?;
                let mut adoption = manager
                    .adopt(&args.command, handoff, private_fs_permit)
                    .await
                    .map_err(ToolError::from)?;
                // Attach the spool AFTER adopt (the spool is created inside
                // adopt, keyed to the assigned id, so it cannot be built
                // earlier). If teeing cannot be enabled, the child is already
                // registered and supervised but its post-migration output would
                // go nowhere — a half-migrated zombie. Rather than leave that,
                // kill the adoptee and return a named error (F6).
                let spool = Arc::clone(adoption.handle().spool());
                let private_fs_permit =
                    adoption.take_private_fs_permit().map_err(ToolError::from)?;
                let snapshot = match capture.attach_spool(spool, private_fs_permit).await {
                    Ok(snapshot) => snapshot,
                    Err(attach_error) => {
                        let label = adoption.handle().label().to_owned();
                        let final_status = adoption.abort();
                        return Err(ToolError::ExecutionFailed {
                            reason: format!(
                                "failed to attach the migrated command's output spool for \
                                 process {label}: {attach_error}; the adopted process was killed \
                                 to avoid a half-migrated state (final status {final_status:?})",
                            ),
                        });
                    }
                };
                // F5: when the seed was delivered inline the model has already
                // seen it, so its output cursor starts past the seed; a redirect
                // seed leaves the cursor at 0 (see `MigrationSnapshot`).
                manager.set_model_cursor(adoption.handle().label(), snapshot.model_cursor_seed);
                let content = migrated_content(
                    adoption.handle(),
                    &args.command,
                    timeout_secs,
                    tier,
                    snapshot.output,
                );
                let _handle = adoption.commit().map_err(ToolError::from)?;
                Ok(ToolOutput::success(content))
            }
            ShellOutcome::Completed(execution) => {
                let mut content = completed_content(&execution, timeout_secs, tier);
                if execution.streams_still_open
                    && let Some(map) = content.as_object_mut()
                {
                    map.insert(
                        "streams_still_open_note".to_owned(),
                        Value::String(STREAMS_OPEN_NOTE.to_owned()),
                    );
                }
                if execution.timed_out {
                    // Degenerate path: the command reached its timeout but no
                    // ProcessManager is wired to migrate it, so it was killed.
                    // The old `Timeout` failure was replaced by migration; the
                    // honest signal here is the missing infrastructure.
                    return Ok(ToolOutput::failure_with_content(
                        content,
                        ToolErrorPayload::new(
                            ToolErrorKind::MissingExtension,
                            format!(
                                "command exceeded its {timeout_secs}s timeout and was killed: no \
                                 ProcessManager is wired to migrate it to the background"
                            ),
                        )
                        .with_detail(json!({
                            "timeout_secs": timeout_secs,
                            "extension": PROCESS_MANAGER_EXTENSION,
                        })),
                    ));
                }
                if execution.exit_code != 0 {
                    return Ok(ToolOutput::failure_with_content(
                        content,
                        ToolErrorPayload::new(
                            ToolErrorKind::ExecutionFailed,
                            format!("command exited with code {}", execution.exit_code),
                        )
                        .with_detail(json!({ "exit_code": execution.exit_code })),
                    ));
                }
                Ok(ToolOutput::success(content))
            }
        }
    }

    /// Register rerun follow-ups available after any completed execution.
    ///
    /// `rerun` re-executes the identical command (no argument overrides);
    /// `rerun_with_timeout` re-executes with the timeout doubled. Both never
    /// expire — a command can always be run again.
    ///
    /// The override targets the live `timeout` field (seconds) used by
    /// `BashArgs` and the input schema; the brief's `timeout_ms` naming
    /// predates the seconds-based schema, and overriding a field that does not
    /// exist would be inert.
    async fn register_follow_ups(
        &self,
        output: &ToolOutput,
        ctx: &ToolContext,
    ) -> Vec<FollowUpAction> {
        let _ = ctx;
        // Backgrounded (R3) and migrated (R4) results target the process tool:
        // check the process's new output, or kill it. These replace the
        // foreground rerun set — a background spawn is not "rerun".
        if let Some(process_id) = output.content.get("process_id").and_then(Value::as_str) {
            return vec![
                FollowUpAction {
                    action: "check_output".to_string(),
                    description: format!("Fetch new output from background process {process_id}"),
                    tool: "process".to_string(),
                    args: json!({ "op": "output", "id": process_id }),
                    args_mode: FollowUpArgsMode::Replace,
                    expires: ExpiryCondition::Never,
                    confidence: Confidence::High,
                    before_content: crate::tool::follow_up::BeforeContentSource::Unavailable,
                },
                FollowUpAction {
                    action: "kill_process".to_string(),
                    description: format!("Kill background process {process_id}"),
                    tool: "process".to_string(),
                    args: json!({ "op": "kill", "id": process_id }),
                    args_mode: FollowUpArgsMode::Replace,
                    expires: ExpiryCondition::Never,
                    confidence: Confidence::Medium,
                    before_content: crate::tool::follow_up::BeforeContentSource::Unavailable,
                },
            ];
        }
        let timeout_secs = output
            .content
            .get("metadata")
            .and_then(|m| m.get("timeout_secs"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        let doubled = timeout_secs.saturating_mul(2);
        let mut follow_ups = vec![
            FollowUpAction {
                action: "rerun".to_string(),
                description: "Re-run this command with the same arguments".to_string(),
                tool: "bash".to_string(),
                args: json!({}),
                args_mode: crate::tool::follow_up::FollowUpArgsMode::MergeOriginal,
                expires: ExpiryCondition::Never,
                confidence: Confidence::High,
                before_content: crate::tool::follow_up::BeforeContentSource::Unavailable,
            },
            FollowUpAction {
                action: "rerun_with_timeout".to_string(),
                description: format!("Re-run this command with a longer timeout ({doubled}s)"),
                tool: "bash".to_string(),
                args: json!({ "timeout": doubled }),
                args_mode: crate::tool::follow_up::FollowUpArgsMode::MergeOriginal,
                expires: ExpiryCondition::Never,
                confidence: Confidence::Medium,
                before_content: crate::tool::follow_up::BeforeContentSource::Unavailable,
            },
        ];

        if output
            .content
            .get("output_redirected")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && let Some(output_path) = output.content.get("output_path").and_then(Value::as_str)
        {
            follow_ups.push(FollowUpAction {
                action: "read_output".to_string(),
                description: "Read a bounded first window of the redirected bash output"
                    .to_string(),
                tool: "read".to_string(),
                args: json!({ "path": output_path, "offset": 1, "limit": READ_DEFAULT_LINE_LIMIT }),
                args_mode: crate::tool::follow_up::FollowUpArgsMode::MergeOriginal,
                expires: ExpiryCondition::Never,
                confidence: Confidence::High,
                before_content: crate::tool::follow_up::BeforeContentSource::Unavailable,
            });
            follow_ups.push(FollowUpAction {
                action: "grep_output".to_string(),
                description: "Search the redirected bash output for a supplied regex pattern"
                    .to_string(),
                tool: "search".to_string(),
                args: json!({ "path": output_path, "mode": "content" }),
                args_mode: crate::tool::follow_up::FollowUpArgsMode::MergeOriginal,
                expires: ExpiryCondition::Never,
                confidence: Confidence::Medium,
                before_content: crate::tool::follow_up::BeforeContentSource::Unavailable,
            });
        }

        follow_ups
    }
}
