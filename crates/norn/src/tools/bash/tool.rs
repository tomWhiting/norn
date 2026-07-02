//! The [`BashTool`] implementation: argument schema, risk-classifying
//! pre-validation, execution via [`super::process::run_shell`], and
//! rerun/read-output follow-up registration.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::tool::ToolArgs;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::{ToolErrorKind, ToolErrorPayload};
use crate::tool::follow_up::{Confidence, ExpiryCondition, FollowUpAction};
use crate::tool::lifecycle::PreValidateOutcome;
use crate::tool::output_budget::READ_DEFAULT_LINE_LIMIT;
use crate::tool::risk::{BashRiskTier, classify_risk};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

use super::cd_track::apply_cd_from_command;
use super::output::{CapturedOutput, OutputCapture};
use super::process::{DEFAULT_DRAIN_GRACE, run_shell};
use crate::tools::confinement::check_confinement;

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

        let capture = OutputCapture::new(ctx, envelope);
        let execution = run_shell(
            &args.command,
            &child_cwd,
            timeout_secs,
            self.drain_grace,
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

        let mut content = match execution.captured {
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
            } => {
                let hint = if execution.timed_out {
                    format!(
                        "Command timed out after {timeout_secs}s. Partial output was captured to disk. Use the read tool to inspect what was produced before the timeout."
                    )
                } else {
                    REDIRECT_HINT.to_owned()
                };
                json!({
                    "exit_code": execution.exit_code,
                    "output_redirected": true,
                    "output_path": output_path,
                    "output_chars": output_chars,
                    "timed_out": execution.timed_out,
                    "streams_still_open": execution.streams_still_open,
                    "hint": hint,
                    "metadata": {
                        "risk_tier": tier,
                        "timeout_secs": timeout_secs,
                    }
                })
            }
        };

        if execution.streams_still_open
            && let Some(map) = content.as_object_mut()
        {
            map.insert(
                "streams_still_open_note".to_owned(),
                Value::String(STREAMS_OPEN_NOTE.to_owned()),
            );
        }

        if execution.timed_out {
            return Ok(ToolOutput::failure_with_content(
                content,
                ToolErrorPayload::new(
                    ToolErrorKind::Timeout,
                    format!("command timed out after {timeout_secs}s"),
                )
                .with_detail(serde_json::json!({
                    "timeout_secs": timeout_secs,
                    "exit_code": execution.exit_code,
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
                .with_detail(serde_json::json!({ "exit_code": execution.exit_code })),
            ));
        }
        Ok(ToolOutput::success(content))
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
