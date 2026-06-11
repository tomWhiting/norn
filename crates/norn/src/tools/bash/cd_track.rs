//! Working-directory tracking for the bash tool.
//!
//! The model's command string is inspected for `cd` directives after
//! every execution and the agent's shared working directory is updated
//! accordingly, so subsequent tool calls resolve relative paths the way
//! the model expects.

use crate::tool::context::ToolContext;

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
pub(super) fn apply_cd_from_command(ctx: &ToolContext, command: &str) {
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

/// Lazily-compiled regex matching `cd <target>` directives.
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

/// Strips a single layer of matching surrounding `"` or `'` quotes.
pub(super) fn strip_surrounding_quotes(s: &str) -> &str {
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
