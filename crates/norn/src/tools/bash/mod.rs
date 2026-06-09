//! Bash tool — streaming subprocess execution with risk classification.
//!
//! Executes shell commands via `sh -c` using [`tokio::process::Command`].
//! Stdout and stderr are drained concurrently and captured into the result,
//! with each line emitted as a `tracing::debug!` event for progress
//! observability. The compile-time pre-validate phase classifies the
//! command's risk tier using [`classify_risk`] and embeds it in the
//! output metadata; blocking on risk is left to orchestrator policy.

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::error::ToolError;
use crate::tool::ToolArgs;
use crate::tool::context::{ProcessEnv, ToolContext};
use crate::tool::envelope::ToolEnvelope;
use crate::tool::follow_up::{Confidence, ExpiryCondition, FollowUpAction};
use crate::tool::lifecycle::PreValidateOutcome;
use crate::tool::risk::{BashRiskTier, classify_risk};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};

mod output;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_follow_up;

use output::{CapturedOutput, OutputCapture, drain_stderr, drain_stdout};
/// Default command timeout (seconds) when none is supplied by the caller.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Sentinel exit code reported when a process was killed by signal or timeout.
const SIGNAL_KILLED_EXIT_CODE: i32 = -1;

/// Combined stdout + stderr character budget for inline bash output.
pub(super) const INLINE_OUTPUT_THRESHOLD_CHARS: usize = 22_000;

const REDIRECT_HINT: &str = "Output exceeded the 22000-character inline threshold and was written to disk. Use the read tool with offset/limit to inspect specific sections, or grep the file for error patterns.";

/// Model-supplied arguments for [`BashTool`].
#[derive(Debug, Default, Deserialize, Serialize, ToolArgs)]
struct BashArgs {
    /// Shell command line. Executed via `sh -c`.
    command: String,
    /// Timeout in seconds. 0 means wait forever. Defaults to 120.
    #[serde(default)]
    timeout: Option<u64>,
    /// Working directory for the subprocess.
    #[serde(default)]
    working_dir: Option<String>,
}

/// Bash tool: executes shell commands with streaming output and risk tagging.
#[derive(Debug, Default)]
pub struct BashTool;

impl BashTool {
    /// Creates a new `BashTool`.
    #[must_use]
    pub fn new() -> Self {
        Self
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
        let started = Instant::now();
        let args: BashArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::PreValidationFailed {
                reason: format!("invalid bash arguments: {e}"),
            }
        })?;

        let tier = classify_risk(&args.command);
        let timeout_secs = args.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);

        // Child CWD precedence: model-supplied `working_dir` arg wins when
        // present (per-call override); otherwise default to the agent's
        // shared working directory.
        let child_cwd = match args.working_dir.as_deref() {
            Some(s) => std::path::PathBuf::from(s),
            None => ctx.working_dir(),
        };

        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&args.command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .current_dir(&child_cwd);
        if let Some(process_env) = ctx.get_extension::<ProcessEnv>() {
            for (key, value) in &process_env.0 {
                cmd.env(key, value);
            }
        }

        let mut child = cmd.spawn().map_err(|e| ToolError::ExecutionFailed {
            reason: format!("failed to spawn `sh`: {e}"),
        })?;

        let stdout_handle = child.stdout.take().ok_or(ToolError::ExecutionFailed {
            reason: "child stdout pipe was not captured".to_owned(),
        })?;
        let stderr_handle = child.stderr.take().ok_or(ToolError::ExecutionFailed {
            reason: "child stderr pipe was not captured".to_owned(),
        })?;

        let capture = OutputCapture::new(ctx, envelope);
        let stdout_task = tokio::spawn(drain_stdout(stdout_handle, Arc::clone(&capture)));
        let stderr_task = tokio::spawn(drain_stderr(stderr_handle, Arc::clone(&capture)));

        let (status, timed_out) = if timeout_secs == 0 {
            let status = child.wait().await.map_err(|e| ToolError::ExecutionFailed {
                reason: format!("failed to wait on child: {e}"),
            })?;
            (status, false)
        } else {
            let timeout = Duration::from_secs(timeout_secs);
            tokio::select! {
                wait_result = child.wait() => {
                    let status = wait_result.map_err(|e| ToolError::ExecutionFailed {
                        reason: format!("failed to wait on child: {e}"),
                    })?;
                    (status, false)
                }
                () = tokio::time::sleep(timeout) => {
                    if let Err(e) = child.start_kill() {
                        tracing::warn!(error = %e, "failed to send kill signal to timed-out bash child");
                    }
                    let status = child.wait().await.map_err(|e| ToolError::ExecutionFailed {
                        reason: format!("failed to wait on killed child: {e}"),
                    })?;
                    (status, true)
                }
            }
        };

        stdout_task
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!("stdout drain task failed: {e}"),
            })?
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!("stdout read failed: {e}"),
            })?;
        stderr_task
            .await
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!("stderr drain task failed: {e}"),
            })?
            .map_err(|e| ToolError::ExecutionFailed {
                reason: format!("stderr read failed: {e}"),
            })?;
        let captured = capture.finalize().await?;

        let exit_code = status.code().unwrap_or(SIGNAL_KILLED_EXIT_CODE);
        let is_error = timed_out || exit_code != 0;

        // Inspect the model's command string for `cd` directives and update
        // the agent's working directory accordingly. Done unconditionally
        // (regardless of overall exit status) because intermediate `cd`s in
        // a chain like `cd /foo && false` did happen even if a later step
        // failed. Targets that don't resolve to an existing directory are
        // skipped — the `is_dir` check is the safety net for typos and
        // failed substitutions like `cd $(cmd-that-failed)`.
        apply_cd_from_command(ctx, &args.command);

        let content = match captured {
            CapturedOutput::Inline { stdout, stderr } => json!({
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
                "timed_out": timed_out,
                "metadata": {
                    "risk_tier": tier,
                    "timeout_secs": timeout_secs,
                }
            }),
            CapturedOutput::Redirected {
                output_path,
                output_chars,
            } => {
                let hint = if timed_out {
                    format!(
                        "Command timed out after {timeout_secs}s. Partial output was captured to disk. Use the read tool to inspect what was produced before the timeout."
                    )
                } else {
                    REDIRECT_HINT.to_owned()
                };
                json!({
                    "exit_code": exit_code,
                    "output_redirected": true,
                    "output_path": output_path,
                    "output_chars": output_chars,
                    "timed_out": timed_out,
                    "hint": hint,
                    "metadata": {
                        "risk_tier": tier,
                        "timeout_secs": timeout_secs,
                    }
                })
            }
        };

        Ok(ToolOutput {
            content,
            is_error,
            duration: elapsed(started),
        })
    }

    /// Register rerun follow-ups available after any completed execution.
    ///
    /// `rerun` re-executes the identical command (no argument overrides);
    /// `rerun_with_timeout` re-executes with the timeout doubled. Both never
    /// expire — a command can always be run again.
    ///
    /// The override targets the live `timeout` field (seconds) used by
    /// [`BashArgs`] and the input schema; the brief's `timeout_ms` naming
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
                expires: ExpiryCondition::Never,
                confidence: Confidence::High,
                before_content: crate::tool::follow_up::BeforeContentSource::Unavailable,
            },
            FollowUpAction {
                action: "rerun_with_timeout".to_string(),
                description: format!("Re-run this command with a longer timeout ({doubled}s)"),
                tool: "bash".to_string(),
                args: json!({ "timeout": doubled }),
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
                description: "Read the first 200 lines of the redirected bash output".to_string(),
                tool: "read".to_string(),
                args: json!({ "path": output_path, "offset": 1, "limit": 200 }),
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
                expires: ExpiryCondition::Never,
                confidence: Confidence::Medium,
                before_content: crate::tool::follow_up::BeforeContentSource::Unavailable,
            });
        }

        follow_ups
    }
}

/// Apply each `cd` directive in `command` (in source order) to the agent's
/// working directory via [`ToolContext::set_working_dir`].
///
/// Recognises `cd <target>` separated by `;`, `&&`, `&`, `|`, `||`, `<`,
/// `>`, or end-of-line. Strips a single layer of surrounding `"` or `'`
/// from the target, then resolves it via [`ToolContext::resolve_path`]
/// (handles tilde, absolute, and relative). The target must resolve to an
/// existing directory; otherwise the update is skipped — this is the
/// safety net for typos, missing dirs, and failed shell substitutions
/// such as `cd $(cmd-that-failed)`.
///
/// Does not handle pushd/popd, cd inside `if`/`while` conditionals, or cd
/// inside shell functions — exotic constructs models rarely emit, per the
/// brief's scope guidance.
fn apply_cd_from_command(ctx: &ToolContext, command: &str) {
    let Some(re) = cd_regex() else { return };
    for cap in re.captures_iter(command) {
        let Some(arg) = cap.get(1) else { continue };
        let raw = arg.as_str().trim();
        if raw.is_empty() {
            continue;
        }
        let unquoted = strip_surrounding_quotes(raw);
        let resolved = ctx.resolve_path(unquoted);
        if resolved.is_dir() {
            let canonical = resolved.canonicalize().unwrap_or(resolved);
            ctx.set_working_dir(canonical);
        }
    }
}

fn cd_regex() -> Option<&'static regex::Regex> {
    use std::sync::OnceLock;
    static RE: OnceLock<Option<regex::Regex>> = OnceLock::new();
    RE.get_or_init(
        || match regex::Regex::new(r"\bcd\s+(.+?)(?:\s*[;&|<>]|$)") {
            Ok(re) => Some(re),
            Err(err) => {
                tracing::warn!(error = %err, "bash: cd regex compile failed; cd tracking disabled");
                None
            }
        },
    )
    .as_ref()
}

fn strip_surrounding_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn elapsed(started: Instant) -> Duration {
    Instant::now().saturating_duration_since(started)
}
