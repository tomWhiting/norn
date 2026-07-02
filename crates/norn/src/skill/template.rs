//! Three-stage skill template expansion (NS-003).
//!
//! Skills authored against the `agentskills.io` standard contain three
//! kinds of placeholder. This module collapses them down to plain text
//! before the model sees the body.
//!
//! 1. Inline ``!`command` `` and fenced `` ```! `` blocks evaluate via
//!    a short-lived shell with strict safety controls (5-second
//!    timeout, 32 KB stdout cap, 1 KB stderr-in-failure-marker, global
//!    disable policy, explicit cwd).
//! 2. `$ARGUMENTS`, `$N`, `$ARGUMENTS[N]`, named `$name`,
//!    `${CLAUDE_SESSION_ID}`, `${CLAUDE_EFFORT}`, `${CLAUDE_SKILL_DIR}`,
//!    and `$$ -> $` substitute via a pure-string scanner. A `$N` whose
//!    index has no positional value passes through literally; a
//!    *recognised* token with no value (`$ARGUMENTS[N]` out of range, a
//!    declared named argument the user did not supply) resolves to the
//!    empty string.
//! 3. `{{name}}` resolves through the loop's [`VariableStore`],
//!    token-by-token: a variable that fails to resolve is replaced with
//!    an inline `[skill variable expansion failed: …]` marker (matching
//!    stage 1's failure policy) while the rest of the body still
//!    expands.
//!
//! # Code protection
//!
//! Standard fenced code blocks and inline backtick code spans are
//! protected from ALL three stages: nothing inside them is executed or
//! substituted, and they are emitted verbatim. Only `` ```! `` fences
//! and ``!`…` `` inline commands are active.
//!
//! Stages run in fixed order over the non-protected text. Replacement
//! text produced by stage N is visible to stage N+1. This is
//! intentional — shell output containing `$ARGUMENTS` will be
//! dollar-expanded; dollar output containing `{{var}}` will be
//! mustache-expanded. Skills can compose shell output with downstream
//! substitution; if literal `$` or `{{` are needed, the source must
//! escape (`$$` for stage 2) or place the text in protected code.
//!
//! Failures in stages 1 and 3 produce inline markers (never silent
//! drops). Stage 2 cannot fail — it is pure string manipulation.
//!
//! NS-003 boundary: this module receives already-parsed positional and
//! named arguments. Argument *parsing* from the `SkillTool` input string
//! and the `$ARGUMENTS` auto-append behaviour live in NS-004.

use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::integration::variables::VariableStore;
use crate::skill::types::SkillShell;

/// Wall-clock budget for any single shell invocation in stage 1. Mirrors
/// the variable / prompt-command timeouts elsewhere in the runtime so
/// the agent has a single predictable bound on synchronous shell work.
pub const SKILL_SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum number of bytes of decoded stdout retained per command.
const STDOUT_CAP_BYTES: usize = 32 * 1024;

/// Maximum number of bytes of decoded stderr embedded in a failure
/// marker for a non-zero-exit command.
const STDERR_CAP_BYTES: usize = 1024;

/// Marker appended to stdout once it exceeds [`STDOUT_CAP_BYTES`].
const TRUNCATED_MARKER: &str = "[truncated — output exceeded 32KB]";

/// Replacement injected when shell execution is disabled by policy.
const DISABLED_MARKER: &str = "[shell command execution disabled by policy]";

/// All inputs needed to expand a single skill body through the
/// three-stage pipeline.
///
/// Borrowed-everything by design: the caller owns the arguments and
/// paths, and the expander never needs to mutate them. Stage 3's
/// [`VariableStore`] is optional so the runtime can call `expand`
/// without a store wired (mirrors
/// [`crate::agent_loop::loop_context::LoopContext::variables`]'s
/// `Option<Arc<VariableStore>>` shape).
pub struct TemplateInputs<'a> {
    /// Markdown body after frontmatter has already been stripped.
    pub body: &'a str,
    /// Shell to use for stage 1 invocations.
    pub shell: SkillShell,
    /// Working directory for stage 1 commands. Per design §D5 this is
    /// the agent's cwd, *not* the skill directory.
    pub cwd: &'a Path,
    /// Skill directory (the directory containing `SKILL.md`). Resolves
    /// `${CLAUDE_SKILL_DIR}` in stage 2.
    pub skill_dir: &'a Path,
    /// When `true`, every stage 1 invocation is replaced with the
    /// policy-disabled marker without spawning a shell.
    pub disable_shell: bool,
    /// Full arguments string (the un-tokenised input passed to the
    /// `SkillTool`). Resolves `$ARGUMENTS`.
    pub arguments_raw: &'a str,
    /// Tokenised positional arguments. Resolves `$N` and
    /// `$ARGUMENTS[N]`. Out-of-range indices resolve to the empty
    /// string.
    pub arguments_positional: &'a [String],
    /// Names declared in the skill's `arguments` frontmatter. Position
    /// `i` maps the `$<argument_names[i]>` token to
    /// `arguments_positional[i]`. A name with no positional value
    /// resolves to the empty string (deliberate: an out-of-range
    /// recognised name is *not* the same as an unrecognised one — the
    /// frontmatter declared it, the user just did not supply a value).
    pub argument_names: &'a [String],
    /// Resolves `${CLAUDE_SESSION_ID}`.
    pub session_id: &'a str,
    /// Resolves `${CLAUDE_EFFORT}`. Empty string when no effort is set.
    pub effort: &'a str,
    /// Optional [`VariableStore`] driving stage 3. When `None`, stage 3
    /// is a no-op so callers without a store can still use the pipeline.
    pub variables: Option<&'a VariableStore>,
}

impl fmt::Debug for TemplateInputs<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `VariableStore` does not impl `Debug`; show only its presence
        // so the rest of the inputs are still inspectable.
        f.debug_struct("TemplateInputs")
            .field("body_len", &self.body.len())
            .field("shell", &self.shell)
            .field("cwd", &self.cwd)
            .field("skill_dir", &self.skill_dir)
            .field("disable_shell", &self.disable_shell)
            .field("arguments_raw", &self.arguments_raw)
            .field("arguments_positional", &self.arguments_positional)
            .field("argument_names", &self.argument_names)
            .field("session_id", &self.session_id)
            .field("effort", &self.effort)
            .field("variables", &self.variables.map(|_| "<VariableStore>"))
            .finish()
    }
}

/// Run all three expansion stages in order and return the final string.
///
/// Stage 1 segments the body into protected code (standard fences,
/// inline code spans — emitted verbatim) and active text (with
/// backtick-bang commands executed inline). Stages 2 and 3 then run
/// over the active segments only, so protected code is never
/// substituted. Within active text, the output of each stage is fed
/// verbatim to the next — see the module doc comment.
///
/// Stage 1 failures (shell timeout, non-zero exit, spawn error) produce
/// inline `[skill shell command failed: …]` markers. Stage 3 failures
/// produce inline `[skill variable expansion failed: …]` markers per
/// unresolvable `{{name}}` token. Stage 2 cannot fail.
pub async fn expand(inputs: &TemplateInputs<'_>) -> String {
    let chunks = stage1_segment(inputs).await;
    let mut out = String::with_capacity(inputs.body.len());
    for chunk in chunks {
        match chunk {
            Chunk::Protected(text) => out.push_str(&text),
            Chunk::Active(text) => {
                let dollars = stage2_dollar(&text, inputs);
                out.push_str(&stage3_mustache(&dollars, inputs.variables).await);
            }
        }
    }
    out
}

/// One segment of the body after stage 1.
enum Chunk {
    /// Substitutable text (including stage 1 shell output and failure
    /// markers) — stages 2 and 3 run over it.
    Active(String),
    /// Verbatim code (a standard fenced block or an inline code span,
    /// delimiters included) — no stage touches it.
    Protected(String),
}

// ---------------------------------------------------------------------
// Stage 1: shell execution + code protection
// ---------------------------------------------------------------------

/// Segment the body, executing `` ```! `` fences and ``!`…` `` inline
/// commands, and carving out standard fences / inline code spans as
/// protected chunks.
async fn stage1_segment(inputs: &TemplateInputs<'_>) -> Vec<Chunk> {
    let body = inputs.body;
    let bytes = body.as_bytes();
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut active = String::with_capacity(body.len());
    let mut i = 0;
    // Some(start) ⇒ we are inside an opened ```! fence; `start` is the
    // byte index in `body` where the fenced *body* begins (i.e. just
    // after the opening fence's newline).
    let mut bang_fence_start: Option<usize> = None;
    // Some(start) ⇒ inside a standard fence; `start` is the byte index
    // of the opening fence line, so the whole block (fences included)
    // can be emitted verbatim at close.
    let mut standard_fence_start: Option<usize> = None;

    while i < bytes.len() {
        let at_line_start = i == 0 || bytes[i - 1] == b'\n';

        if at_line_start {
            let line_end = next_newline(bytes, i);
            let line = &body[i..line_end];
            let trimmed = line.trim_start().trim_end();
            let after_line = if line_end < bytes.len() {
                line_end + 1
            } else {
                line_end
            };

            if let Some(start) = bang_fence_start {
                if trimmed == "```" {
                    let block = &body[start..i];
                    let replacement = run_or_marker(block, inputs).await;
                    active.push_str(&replacement);
                    bang_fence_start = None;
                }
                // Inside the bang fence body lines are consumed; the
                // slice is materialised at close.
                i = after_line;
                continue;
            }

            if let Some(start) = standard_fence_start {
                if trimmed == "```" {
                    flush_active(&mut chunks, &mut active);
                    chunks.push(Chunk::Protected(body[start..after_line].to_owned()));
                    standard_fence_start = None;
                }
                i = after_line;
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("```") {
                // Per scout note: any fence whose info-string-first
                // token starts with `!` is executed; the remainder of
                // the info string is currently ignored.
                let info = rest.split_whitespace().next().unwrap_or("");
                if info.starts_with('!') {
                    bang_fence_start = Some(after_line);
                } else {
                    standard_fence_start = Some(i);
                }
                i = after_line;
                continue;
            }
        }

        // Inline pattern: `!` immediately followed by `` ` ``.
        // Backtick escapes inside `!`...`` are not specified by
        // agentskills.io — treat the first un-paired `` ` `` after
        // ``!`` as the close.
        if bytes[i] == b'!' && i + 1 < bytes.len() && bytes[i + 1] == b'`' {
            let cmd_start = i + 2;
            if let Some(rel) = bytes[cmd_start..].iter().position(|&b| b == b'`') {
                let cmd_end = cmd_start + rel;
                let cmd = &body[cmd_start..cmd_end];
                let replacement = run_or_marker(cmd, inputs).await;
                active.push_str(&replacement);
                i = cmd_end + 1;
                continue;
            }
        }

        // Inline code span: a run of N backticks closed by the next run
        // of exactly N backticks (CommonMark pairing). The whole span —
        // delimiters included — is protected from every stage. An
        // unclosed opener is literal text.
        if bytes[i] == b'`' {
            let run_len = backtick_run_len(bytes, i);
            if let Some(close_start) = find_closing_run(bytes, i + run_len, run_len) {
                let end = close_start + run_len;
                flush_active(&mut chunks, &mut active);
                chunks.push(Chunk::Protected(body[i..end].to_owned()));
                i = end;
            } else {
                active.push_str(&body[i..i + run_len]);
                i += run_len;
            }
            continue;
        }

        // Ordinary character.
        let Some(c) = body[i..].chars().next() else {
            break;
        };
        active.push(c);
        i += c.len_utf8();
    }

    // Unterminated bang fence: execute the remainder as one block so
    // the model never sees a half-rendered fence.
    if let Some(start) = bang_fence_start {
        let block = &body[start..];
        let replacement = run_or_marker(block, inputs).await;
        active.push_str(&replacement);
    }
    // Unterminated standard fence: the remainder is code — protect it.
    if let Some(start) = standard_fence_start {
        flush_active(&mut chunks, &mut active);
        chunks.push(Chunk::Protected(body[start..].to_owned()));
    }
    flush_active(&mut chunks, &mut active);
    chunks
}

/// Move the accumulated active text into the chunk list.
fn flush_active(chunks: &mut Vec<Chunk>, active: &mut String) {
    if !active.is_empty() {
        chunks.push(Chunk::Active(std::mem::take(active)));
    }
}

/// Length of the backtick run starting at `from` (which must index a
/// backtick).
fn backtick_run_len(bytes: &[u8], from: usize) -> usize {
    bytes[from..].iter().take_while(|&&b| b == b'`').count()
}

/// Find the start of the next backtick run of *exactly* `n` backticks at
/// or after `from`.
fn find_closing_run(bytes: &[u8], from: usize, n: usize) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let start = i;
            let len = backtick_run_len(bytes, i);
            i += len;
            if len == n {
                return Some(start);
            }
        } else {
            i += 1;
        }
    }
    None
}

fn next_newline(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |p| from + p)
}

async fn run_or_marker(command: &str, inputs: &TemplateInputs<'_>) -> String {
    if inputs.disable_shell {
        return DISABLED_MARKER.to_owned();
    }
    match run_skill_shell(command, inputs.shell, inputs.cwd).await {
        Ok(stdout) => stdout,
        Err(marker) => marker,
    }
}

/// Spawn a single shell invocation under the safety envelope:
/// 5-second `tokio` timeout (the child is killed via `kill_on_drop`
/// when the future is cancelled), explicit cwd, stdout truncated at
/// 32 KB on a UTF-8 char boundary. On failure, returns the formatted
/// failure marker text (already wrapped in `[skill shell command
/// failed: …]`) ready to inline into the template output.
async fn run_skill_shell(command: &str, shell: SkillShell, cwd: &Path) -> Result<String, String> {
    let (prog, flag) = shell_argv(shell);
    tracing::info!(shell = ?shell, command = command, cwd = %cwd.display(), "skill shell exec");

    let mut cmd = Command::new(prog);
    cmd.arg(flag)
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Err(format!(
                "[skill shell command failed: failed to spawn shell: {e}]"
            ));
        }
    };

    let result = tokio::time::timeout(SKILL_SHELL_TIMEOUT, child.wait_with_output()).await;
    match result {
        Ok(Ok(output)) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout);
            let trimmed = raw.trim_end_matches('\n').trim_end_matches('\r');
            Ok(truncate_stdout(trimmed))
        }
        Ok(Ok(output)) => {
            let exit = output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |c| c.to_string());
            let stderr = String::from_utf8_lossy(&output.stderr);
            let excerpt = take_bytes(&stderr, STDERR_CAP_BYTES);
            Err(format!(
                "[skill shell command failed: exited {exit}: {excerpt}]"
            ))
        }
        Ok(Err(e)) => Err(format!(
            "[skill shell command failed: failed to spawn shell: {e}]"
        )),
        Err(_) => Err(format!(
            "[skill shell command failed: timed out after {}s]",
            SKILL_SHELL_TIMEOUT.as_secs()
        )),
    }
}

/// Kept tiny + unit-testable so we can verify the shell selector
/// without spawning a process.
///
/// [`SkillShell::Bash`] runs `bash` — what the frontmatter says — not
/// `sh`. When the binary is unavailable the spawn error surfaces as the
/// inline `[skill shell command failed: failed to spawn shell: …]`
/// marker, this pipeline's typed failure surface.
#[must_use]
pub(crate) fn shell_argv(shell: SkillShell) -> (&'static str, &'static str) {
    match shell {
        SkillShell::Bash => ("bash", "-c"),
        SkillShell::PowerShell => ("pwsh", "-c"),
    }
}

fn truncate_stdout(s: &str) -> String {
    if s.len() <= STDOUT_CAP_BYTES {
        return s.to_owned();
    }
    let mut out = String::with_capacity(STDOUT_CAP_BYTES + TRUNCATED_MARKER.len());
    let mut count = 0;
    for c in s.chars() {
        let next = count + c.len_utf8();
        if next > STDOUT_CAP_BYTES {
            break;
        }
        out.push(c);
        count = next;
    }
    out.push_str(TRUNCATED_MARKER);
    out
}

/// UTF-8-safe prefix-by-bytes truncation used for the stderr excerpt.
fn take_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut out = String::with_capacity(max);
    let mut count = 0;
    for c in s.chars() {
        let next = count + c.len_utf8();
        if next > max {
            break;
        }
        out.push(c);
        count = next;
    }
    out
}

// ---------------------------------------------------------------------
// Stage 2: dollar-sign substitution (pure string)
// ---------------------------------------------------------------------

fn stage2_dollar(input: &str, inputs: &TemplateInputs<'_>) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'$' {
            let Some(c) = input[i..].chars().next() else {
                break;
            };
            out.push(c);
            i += c.len_utf8();
            continue;
        }

        // bytes[i] == '$' — branch on next byte.
        if i + 1 >= bytes.len() {
            out.push('$');
            i += 1;
            continue;
        }

        match bytes[i + 1] {
            b'$' => {
                // Escape: $$ → $
                out.push('$');
                i += 2;
            }
            b'{' => {
                if let Some(close_rel) = input[i + 2..].find('}') {
                    let inner = &input[i + 2..i + 2 + close_rel];
                    if let Some(value) = resolve_brace(inner, inputs) {
                        out.push_str(&value);
                    } else {
                        // Unknown built-in: pass through verbatim for
                        // forward compatibility with future tokens.
                        out.push_str(&input[i..i + 3 + close_rel]);
                    }
                    i = i + 3 + close_rel;
                } else {
                    // No closing brace anywhere — emit literal `$` and
                    // let the next pass copy `{` etc.
                    out.push('$');
                    i += 1;
                }
            }
            b if b.is_ascii_digit() => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                match input[i + 1..j]
                    .parse::<usize>()
                    .ok()
                    .and_then(|idx| inputs.arguments_positional.get(idx))
                {
                    Some(v) => out.push_str(v),
                    // A `$N` with no positional value is treated like an
                    // unrecognised identifier: it passes through
                    // literally instead of being deleted. (Recognised
                    // tokens with a missing value — `$ARGUMENTS[N]`,
                    // declared named arguments — still resolve empty.)
                    None => out.push_str(&input[i..j]),
                }
                i = j;
            }
            b if is_ident_start(b) => {
                let mut j = i + 1;
                while j < bytes.len() && is_ident_cont(bytes[j]) {
                    j += 1;
                }
                let name = &input[i + 1..j];

                if name == "ARGUMENTS" {
                    // Optional [N] subscript.
                    if j < bytes.len() && bytes[j] == b'[' {
                        let bracket_start = j + 1;
                        let mut k = bracket_start;
                        while k < bytes.len() && bytes[k].is_ascii_digit() {
                            k += 1;
                        }
                        if k > bracket_start && k < bytes.len() && bytes[k] == b']' {
                            if let Ok(idx) = input[bracket_start..k].parse::<usize>()
                                && let Some(v) = inputs.arguments_positional.get(idx)
                            {
                                out.push_str(v);
                            }
                            i = k + 1;
                            continue;
                        }
                    }
                    out.push_str(inputs.arguments_raw);
                    i = j;
                    continue;
                }

                if let Some(idx) = inputs.argument_names.iter().position(|n| n == name) {
                    if let Some(v) = inputs.arguments_positional.get(idx) {
                        out.push_str(v);
                    } else {
                        tracing::debug!(
                            name = name,
                            "skill named argument unresolved; emitting empty string"
                        );
                    }
                    i = j;
                    continue;
                }

                // Unrecognised identifier — pass through verbatim
                // including the leading `$`.
                out.push_str(&input[i..j]);
                i = j;
            }
            _ => {
                // `$` followed by punctuation / space — emit literal `$`.
                out.push('$');
                i += 1;
            }
        }
    }

    out
}

fn resolve_brace(inner: &str, inputs: &TemplateInputs<'_>) -> Option<String> {
    match inner {
        "CLAUDE_SESSION_ID" => Some(inputs.session_id.to_owned()),
        "CLAUDE_EFFORT" => Some(inputs.effort.to_owned()),
        "CLAUDE_SKILL_DIR" => Some(inputs.skill_dir.display().to_string()),
        _ => None,
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

// ---------------------------------------------------------------------
// Stage 3: mustache via VariableStore
// ---------------------------------------------------------------------

/// Substitute `{{name}}` tokens through `store`, token by token.
///
/// A token that fails to resolve is replaced with an inline
/// `[skill variable expansion failed: …]` marker — matching stage 1's
/// failure policy — while every other token still expands. When no
/// store is wired, the input passes through unchanged.
async fn stage3_mustache(input: &str, store: Option<&VariableStore>) -> String {
    let Some(store) = store else {
        return input.to_owned();
    };
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len()
            && bytes[i] == b'{'
            && bytes[i + 1] == b'{'
            && let Some(close_rel) = input[i + 2..].find("}}")
        {
            let end = i + 2 + close_rel;
            let name = input[i + 2..end].trim();
            match store.resolve(name).await {
                Ok(value) => out.push_str(&value),
                Err(err) => {
                    tracing::warn!(
                        variable = name,
                        error = %err,
                        "skill stage 3 variable failed to resolve; emitting inline marker",
                    );
                    out.push_str("[skill variable expansion failed: ");
                    out.push_str(name);
                    out.push_str(": ");
                    out.push_str(&err.to_string());
                    out.push(']');
                }
            }
            i = end + 2;
            continue;
        }
        let Some(c) = input[i..].chars().next() else {
            break;
        };
        out.push(c);
        i += c.len_utf8();
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::uninlined_format_args,
    clippy::similar_names,
    clippy::too_many_lines
)]
mod tests {
    use super::*;
    use crate::integration::variables::{SessionVariable, VariableSource, VariableStore};
    use std::path::PathBuf;

    fn cwd() -> PathBuf {
        // Stable test cwd that doesn't depend on the process's actual CWD
        // (which has agent-relative semantics now). The tests only use this
        // as a path to join onto; they don't inspect its contents.
        std::env::temp_dir()
    }

    fn skill_dir() -> PathBuf {
        cwd().join("skill-dir")
    }

    fn inputs(params: InputsParams<'_>) -> TemplateInputs<'_> {
        TemplateInputs {
            body: params.body,
            shell: SkillShell::Bash,
            cwd: params.cwd,
            skill_dir: params.skill_dir,
            disable_shell: false,
            arguments_raw: params.args_raw,
            arguments_positional: params.positional,
            argument_names: params.names,
            session_id: params.session_id,
            effort: params.effort,
            variables: params.variables,
        }
    }

    struct InputsParams<'a> {
        body: &'a str,
        cwd: &'a Path,
        skill_dir: &'a Path,
        positional: &'a [String],
        names: &'a [String],
        args_raw: &'a str,
        session_id: &'a str,
        effort: &'a str,
        variables: Option<&'a VariableStore>,
    }

    // ---------------- R1 ----------------

    #[tokio::test]
    async fn r1_inline_backtick_bang_executes() {
        let body = "out: !`echo hello` end";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "out: hello end");
    }

    #[tokio::test]
    async fn r1_multiple_inline_patterns_independent() {
        let body = "[!`echo a`][!`echo b`]";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "[a][b]");
    }

    #[tokio::test]
    async fn r1_text_between_inline_preserved() {
        let body = "before !`echo middle` after";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "before middle after");
    }

    // ---------------- R2 ----------------

    #[tokio::test]
    async fn r2_fenced_bang_block_executes_as_one_invocation() {
        // The replacement is the trimmed stdout. Surrounding newlines
        // come from the document — the opening + closing fence lines
        // (and the closing fence's trailing newline) are consumed.
        let body = "```!\necho one\necho two\n```";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "one\ntwo");
    }

    #[tokio::test]
    async fn r2_fenced_bang_block_consumes_opening_and_closing_fence_lines() {
        // Surrounding text plus a fenced-bang block. The open + close
        // fence lines (and the close's trailing \n) are removed; only
        // the trimmed stdout remains.
        let body = "before\n```!\necho mid\n```\nafter\n";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        // "before\n" + "mid" + "after\n" — surrounding markdown supplies
        // the spacing.
        assert_eq!(got, "before\nmidafter\n");
    }

    #[tokio::test]
    async fn r2_standard_fence_passes_through_verbatim() {
        let body = "doc\n```rust\nfn main() { println!(\"hi\"); }\n```\nend\n";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, body);
    }

    #[tokio::test]
    async fn r2_inline_inside_standard_fence_is_not_executed() {
        // !`echo nope` lives inside a standard fenced block so it must
        // stay verbatim.
        let body = "```\nthis !`echo nope` stays literal\n```\n";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, body);
    }

    // ---------------- R3 ----------------

    #[tokio::test]
    async fn r3_failed_command_produces_failure_marker_with_exit_code() {
        let body = "x !`exit 1` y";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert!(got.contains("[skill shell command failed:"), "got: {got}");
        assert!(got.contains("exited 1"), "got: {got}");
    }

    #[tokio::test]
    async fn r3_failed_command_includes_stderr_excerpt() {
        let body = "!`echo boom-error >&2; exit 2`";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert!(got.contains("boom-error"), "got: {got}");
        assert!(got.contains("exited 2"), "got: {got}");
    }

    #[tokio::test]
    async fn r3_timeout_produces_timeout_marker() {
        let body = "!`sleep 6`";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert!(
            got.contains("timed out after 5s"),
            "expected timeout marker, got: {got}"
        );
    }

    // ---------------- R4 ----------------

    #[test]
    fn r4_shell_argv_runs_what_it_says() {
        assert_eq!(
            shell_argv(SkillShell::Bash),
            ("bash", "-c"),
            "SkillShell::Bash must run bash, not sh",
        );
        assert_eq!(shell_argv(SkillShell::PowerShell), ("pwsh", "-c"));
    }

    #[tokio::test]
    async fn r4_command_runs_in_agent_cwd_not_skill_dir() {
        // Write a sentinel file in the cwd and a different one in the
        // skill dir. The body reads the sentinel — if cwd was respected,
        // we see the cwd sentinel content; if the skill dir was used by
        // mistake, the read fails.
        let tmp = tempfile::tempdir().unwrap();
        let cwd_p = tmp.path().to_path_buf();
        let sd = cwd_p.join("nested-skill-dir");
        std::fs::create_dir_all(&sd).unwrap();
        std::fs::write(cwd_p.join("from-cwd.txt"), "from-cwd").unwrap();
        std::fs::write(sd.join("from-skill.txt"), "from-skill").unwrap();
        let body = "src=!`cat from-cwd.txt`";
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "src=from-cwd", "shell cwd should be the agent cwd");
    }

    #[tokio::test]
    async fn r4_disable_policy_replaces_inline_and_fenced() {
        let body = "in: !`echo no` out\n```!\necho also no\n```\n";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let mut i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        i.disable_shell = true;
        let got = expand(&i).await;
        assert_eq!(
            got,
            "in: [shell command execution disabled by policy] out\n[shell command execution disabled by policy]"
        );
    }

    #[tokio::test]
    async fn r4_stdout_truncated_with_marker_when_over_cap() {
        // Produce > 32 KB of output via printf's repeat.
        let body = "!`yes hello | head -c 40000`";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert!(
            got.ends_with("[truncated — output exceeded 32KB]"),
            "expected truncation marker, got tail: {}",
            &got[got.len().saturating_sub(80)..]
        );
        // The kept body + marker should be no larger than cap + marker.
        assert!(got.len() <= 32 * 1024 + "[truncated — output exceeded 32KB]".len());
    }

    // ---------------- R5 ----------------

    #[tokio::test]
    async fn r5_arguments_resolves_full_string() {
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["foo".into(), "bar".into()];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "args: $ARGUMENTS",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "foo bar",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "args: foo bar");
    }

    #[tokio::test]
    async fn r5_positional_zero_resolves() {
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["first".into(), "second".into()];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "0: $0 1: $1 oob: $9",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "first second",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(
            got, "0: first 1: second oob: $9",
            "an undefined positional passes through literally, never deleted",
        );
    }

    /// `$<digits>` with no positional value must survive verbatim — it
    /// was previously deleted outright, silently corrupting bodies that
    /// mention dollar-number text (prices, regex backreferences).
    #[tokio::test]
    async fn r5_undefined_positional_passes_through_literally() {
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "the fee is $5 total",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "the fee is $5 total");
    }

    /// `$ARGUMENTS[N]` out of range still resolves to the empty string —
    /// the token is recognised (the skill asked for an argument), unlike
    /// a bare `$N` which may just be dollar-number text.
    #[tokio::test]
    async fn r5_arguments_subscript_out_of_range_resolves_empty() {
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["only".into()];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "[$ARGUMENTS[7]]",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "only",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "[]");
    }

    #[tokio::test]
    async fn r5_arguments_subscript_resolves() {
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["a".into(), "b".into()];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "$ARGUMENTS[1]",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "a b",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "b");
    }

    #[tokio::test]
    async fn r5_named_argument_resolves_via_argument_names() {
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["123".into()];
        let names: Vec<String> = vec!["issue".into()];
        let i = inputs(InputsParams {
            body: "fix issue $issue",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "123",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "fix issue 123");
    }

    #[tokio::test]
    async fn r5_session_id_resolves() {
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "sid=${CLAUDE_SESSION_ID}",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "abc-123",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "sid=abc-123");
    }

    #[tokio::test]
    async fn r5_effort_and_skill_dir_resolve() {
        let cwd_p = cwd();
        let sd = cwd_p.join("my-skill");
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "e=${CLAUDE_EFFORT} d=${CLAUDE_SKILL_DIR}",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "high",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, format!("e=high d={}", sd.display()));
    }

    #[tokio::test]
    async fn r5_double_dollar_escapes_to_literal() {
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["foo".into()];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "echo $$ARGUMENTS = $0",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "foo",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "echo $ARGUMENTS = foo");
    }

    #[tokio::test]
    async fn r5_unrecognised_name_passes_through_as_is() {
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "$unknown_ref ok",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "$unknown_ref ok");
    }

    #[tokio::test]
    async fn r5_unknown_brace_passes_through_verbatim() {
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "${SOME_OTHER_FUTURE_VAR}",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "${SOME_OTHER_FUTURE_VAR}");
    }

    // ---------------- R6 ----------------

    #[tokio::test]
    async fn r6_mustache_session_id_resolves_via_store() {
        let store = VariableStore::with_builtins();
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "sid={{session_id}}",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: Some(&store),
        });
        let got = expand(&i).await;
        let expected_sid = store.resolve("session_id").await.unwrap();
        assert_eq!(got, format!("sid={expected_sid}"));
    }

    #[tokio::test]
    async fn r6_mustache_working_dir_resolves() {
        let store = VariableStore::with_builtins();
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "wd={{working_dir}}",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: Some(&store),
        });
        let got = expand(&i).await;
        let expected = std::env::current_dir().unwrap().display().to_string();
        assert_eq!(got, format!("wd={expected}"));
    }

    #[tokio::test]
    async fn r6_text_without_mustache_passes_through() {
        let store = VariableStore::with_builtins();
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "no placeholders here",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: Some(&store),
        });
        let got = expand(&i).await;
        assert_eq!(got, "no placeholders here");
    }

    #[tokio::test]
    async fn r6_no_store_skips_stage3_cleanly() {
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "plain {{name}} stays",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "plain {{name}} stays");
    }

    // ---------------- stage 3 failure markers ----------------

    /// An unresolvable `{{name}}` must produce an inline failure marker
    /// (stage 1's policy) — not a silent whole-body passthrough that
    /// leaves every other variable unexpanded too.
    #[tokio::test]
    async fn stage3_unresolvable_variable_emits_inline_marker() {
        let store = VariableStore::with_builtins();
        store.set(SessionVariable {
            name: "team".to_owned(),
            source: VariableSource::Static {
                value: "norn".to_owned(),
            },
        });
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "a={{team}} b={{no_such_var}} c={{team}}",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: Some(&store),
        });
        let got = expand(&i).await;
        assert!(got.starts_with("a=norn b=["), "got: {got}");
        assert!(
            got.contains("[skill variable expansion failed: no_such_var:"),
            "inline marker must name the variable: {got}"
        );
        assert!(
            got.ends_with("c=norn"),
            "later variables must still expand: {got}"
        );
    }

    // ---------------- code protection (all stages) ----------------

    /// Fenced code blocks are protected from stage 2/3 substitution, not
    /// just stage 1 execution: `$0` and `{{var}}` inside a fence stay
    /// literal.
    #[tokio::test]
    async fn fenced_code_is_protected_from_dollar_and_mustache() {
        let store = VariableStore::with_builtins();
        store.set(SessionVariable {
            name: "team".to_owned(),
            source: VariableSource::Static {
                value: "norn".to_owned(),
            },
        });
        let body = "before $0\n```sh\necho $0 {{team}} $ARGUMENTS\n```\nafter {{team}}\n";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["VAL".into()];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "VAL",
            session_id: "sid",
            effort: "",
            variables: Some(&store),
        });
        let got = expand(&i).await;
        assert_eq!(
            got,
            "before VAL\n```sh\necho $0 {{team}} $ARGUMENTS\n```\nafter norn\n",
        );
    }

    /// Inline code spans are protected from every stage: no execution,
    /// no dollar substitution, no mustache substitution.
    #[tokio::test]
    async fn inline_code_span_is_protected_from_all_stages() {
        let store = VariableStore::with_builtins();
        store.set(SessionVariable {
            name: "team".to_owned(),
            source: VariableSource::Static {
                value: "norn".to_owned(),
            },
        });
        let body = "run `$0 {{team}}` then $0 {{team}}";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["VAL".into()];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "VAL",
            session_id: "sid",
            effort: "",
            variables: Some(&store),
        });
        let got = expand(&i).await;
        assert_eq!(got, "run `$0 {{team}}` then VAL norn");
    }

    /// Stage-1 execution protection for inline code spans: a
    /// backtick-bang sequence that sits inside an inline code span must
    /// stay literal.
    #[tokio::test]
    async fn inline_code_span_protects_backtick_bang_from_execution() {
        let body = "docs: `use !`echo nope` here` end";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec![];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(
            got, body,
            "a backtick-bang inside a code span must stay literal"
        );
    }

    /// Double-backtick spans pair per `CommonMark`: the span closes at the
    /// next run of exactly the same length.
    #[tokio::test]
    async fn double_backtick_span_protects_contents() {
        let body = "a ``$0`` b $0";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["VAL".into()];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "VAL",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "a ``$0`` b VAL");
    }

    /// An unterminated standard fence protects the remainder of the body.
    #[tokio::test]
    async fn unterminated_standard_fence_stays_protected() {
        let body = "text $0\n```rust\nlet x = $0; // {{team}}\n";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["VAL".into()];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "VAL",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "text VAL\n```rust\nlet x = $0; // {{team}}\n");
    }

    // ---------------- R7 ----------------

    #[tokio::test]
    async fn r7_shell_output_containing_dollar_arguments_is_dollar_expanded() {
        // Stage 1 prints `$ARGUMENTS`, which stage 2 then resolves.
        let body = "!`printf '%s' '$ARGUMENTS'`";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["X".into()];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "X",
            session_id: "sid",
            effort: "",
            variables: None,
        });
        let got = expand(&i).await;
        assert_eq!(got, "X");
    }

    #[tokio::test]
    async fn r7_dollar_output_containing_mustache_is_mustache_expanded() {
        let store = VariableStore::with_builtins();
        store.set(SessionVariable {
            name: "team".to_owned(),
            source: VariableSource::Static {
                value: "norn".to_owned(),
            },
        });
        // $0 expands to literal `{{team}}`, which stage 3 resolves.
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["{{team}}".into()];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body: "team is $0",
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "{{team}}",
            session_id: "sid",
            effort: "",
            variables: Some(&store),
        });
        let got = expand(&i).await;
        assert_eq!(got, "team is norn");
    }

    #[tokio::test]
    async fn r7_pipeline_threads_all_three_stages_in_order() {
        // Stage 1 emits `$ARGUMENTS` literally; stage 2 resolves it;
        // its result also contains `{{team}}` for stage 3 to resolve.
        let store = VariableStore::with_builtins();
        store.set(SessionVariable {
            name: "team".to_owned(),
            source: VariableSource::Static {
                value: "norn".to_owned(),
            },
        });
        let body = "!`printf '%s' '$ARGUMENTS'`";
        let cwd_p = cwd();
        let sd = skill_dir();
        let pos: Vec<String> = vec!["{{team}}".into()];
        let names: Vec<String> = vec![];
        let i = inputs(InputsParams {
            body,
            cwd: &cwd_p,
            skill_dir: &sd,
            positional: &pos,
            names: &names,
            args_raw: "{{team}}",
            session_id: "sid",
            effort: "",
            variables: Some(&store),
        });
        let got = expand(&i).await;
        assert_eq!(got, "norn");
    }

    // ---------------- truncation helpers ----------------

    #[test]
    fn truncate_stdout_passes_short_strings_through() {
        let out = truncate_stdout("hello");
        assert_eq!(out, "hello");
    }

    #[test]
    fn truncate_stdout_appends_marker_and_respects_char_boundary() {
        let s: String = "あ".repeat(20_000); // 3 bytes per char → 60_000 bytes
        let out = truncate_stdout(&s);
        assert!(out.ends_with(TRUNCATED_MARKER));
        // Body bytes (without marker) ≤ 32 KB.
        let body = &out[..out.len() - TRUNCATED_MARKER.len()];
        assert!(body.len() <= STDOUT_CAP_BYTES);
        // The body must still be valid UTF-8 — assertion is implicit
        // since we sliced on a byte boundary equal to char boundaries.
        assert!(body.chars().all(|c| c == 'あ'));
    }

    #[test]
    fn take_bytes_respects_char_boundary() {
        let s: String = "あ".repeat(10);
        let out = take_bytes(&s, 8); // 8 bytes ≈ 2 full chars (6 bytes)
        assert_eq!(out, "ああ");
    }
}
