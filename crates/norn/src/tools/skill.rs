//! Skill activation tool.
//!
//! Loads a `SKILL.md` (or `<name>.md`) prompt template from a configurable
//! list of search directories, parses its frontmatter via
//! [`crate::skill::loader`], expands the body via
//! [`crate::skill::template`], and returns the expanded text together with
//! the skill directory path and a bundled-resource listing so the model
//! can load companion files on demand.
//!
//! The active list of directories is published on the [`ToolContext`] via
//! the [`SkillSearchPaths`] extension; tools that depend on lookup paths
//! must never read from a global.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::ToolError;
use crate::skill::loader::{load_skill_from_path, load_workspace_skill_from_path};
use crate::skill::{SkillShell, TemplateInputs, expand};
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::failure::ToolErrorKind;
use crate::tool::lifecycle::{BlockDecision, PreValidateOutcome};
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolCategory, ToolOutput};
use crate::util::{
    WorkspaceEntryKind, read_workspace_directory, validate_workspace_regular_file,
    workspace_relative_path,
};

/// Maximum number of bundled-resource file names included in the tool
/// activation result (per DESIGN.md §D13 — progressive disclosure tier 3).
const MAX_RESOURCE_ENTRIES: usize = 20;

/// Search paths inspected for skill files, in order.
///
/// The first directory containing `<name>/SKILL.md` or `<name>.md` wins.
pub struct SkillSearchPaths(pub Vec<PathBuf>);

/// Immutable launch root used to classify repository-controlled skills.
pub(crate) struct WorkspaceSkillRoot(pub(crate) PathBuf);

/// Behaviour policy for the skill tool, wired through tool construction
/// (the embedder's `[tool_config.skill]` settings surface).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SkillToolConfig {
    /// Whether fenced/inline shell commands in skill bodies execute
    /// during template expansion. When `false`, every shell placeholder
    /// is replaced with the policy-disabled marker without spawning a
    /// shell.
    pub shell_execution: bool,
}

impl Default for SkillToolConfig {
    /// Documented default for trusted user/programmatic skills: shell
    /// execution is enabled for `agentskills.io` compatibility. Repository
    /// skills are always activated with shell expansion disabled regardless
    /// of this value; project command execution requires a future consent
    /// design.
    fn default() -> Self {
        Self {
            shell_execution: true,
        }
    }
}

/// Loads a `SKILL.md` template by name.
pub struct SkillTool {
    config: SkillToolConfig,
}

impl SkillTool {
    /// Constructs the tool with the default [`SkillToolConfig`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: SkillToolConfig::default(),
        }
    }

    /// Constructs the tool with an explicit policy configuration.
    #[must_use]
    pub fn with_config(config: SkillToolConfig) -> Self {
        Self { config }
    }
}

impl Default for SkillTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Validates that a requested skill name is a single, plain path
/// component. Anything else — empty names, absolute paths, `.`/`..`,
/// separators, drive prefixes — could escape the configured skill roots
/// when joined, so it is rejected before any filesystem access.
fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("skill name must not be empty".to_owned());
    }
    // Both separator styles are rejected regardless of platform: on Unix a
    // backslash is a legal filename byte, but no skill is ever named with
    // one and accepting it would traverse on Windows checkouts.
    if name.contains('/') || name.contains('\\') {
        return Err(format!(
            "skill name {name:?} must not contain path separators"
        ));
    }
    let mut components = Path::new(name).components();
    let is_single_normal = matches!(
        components.next(),
        Some(Component::Normal(component)) if component == OsStr::new(name)
    ) && components.next().is_none();
    if !is_single_normal {
        return Err(format!(
            "skill name {name:?} is not a plain file name: path separators, \
             absolute paths, and `.`/`..` components are not permitted"
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct SkillArgs {
    name: String,
    #[serde(default)]
    arguments: Option<String>,
}

fn directory_entries(
    directory: &Path,
    workspace_root: Option<&Path>,
) -> Vec<(PathBuf, WorkspaceEntryKind)> {
    if let Some(root) = workspace_root
        && let Some(relative) = workspace_relative_path(root, directory)
    {
        return read_workspace_directory(root, &relative).map_or_else(
            |_| Vec::new(),
            |entries| {
                entries
                    .into_iter()
                    .map(|entry| (directory.join(entry.name), entry.kind))
                    .collect()
            },
        );
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let kind = match entry.file_type().ok()? {
                file_type if file_type.is_file() => WorkspaceEntryKind::File,
                file_type if file_type.is_dir() => WorkspaceEntryKind::Directory,
                _ => WorkspaceEntryKind::Other,
            };
            Some((entry.path(), kind))
        })
        .collect()
}

fn enumerate_available(dirs: &[PathBuf], workspace_root: Option<&Path>) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    for dir in dirs {
        for (path, kind) in directory_entries(dir, workspace_root) {
            if kind == WorkspaceEntryKind::Directory
                && workspace_root
                    .and_then(|root| {
                        workspace_relative_path(root, &path.join("SKILL.md")).map(|relative| {
                            validate_workspace_regular_file(root, &relative).is_ok()
                        })
                    })
                    .unwrap_or_else(|| path.join("SKILL.md").is_file())
                && let Some(name) = path.file_name().and_then(OsStr::to_str)
            {
                names.insert(name.to_string());
            } else if kind == WorkspaceEntryKind::File
                && path.extension().and_then(OsStr::to_str) == Some("md")
                && let Some(stem) = path.file_stem().and_then(OsStr::to_str)
            {
                names.insert(stem.to_string());
            }
        }
    }
    names.into_iter().collect()
}

/// Tokenise a free-form arguments string into positional arguments using
/// shell-style quoting.
///
/// - Whitespace separates tokens in unquoted regions.
/// - `"…"` and `'…'` group their contents into one token; the delimiters
///   are stripped.
/// - Inside a quoted region, `\"`, `\'`, and `\\` escape the matching
///   delimiter or a literal backslash. Other backslashes pass through
///   verbatim. Unquoted backslashes always pass through.
/// - An unterminated quote flushes its accumulator as the final token so
///   nothing is silently dropped.
fn parse_shell_args(input: &str) -> Vec<String> {
    enum State {
        Unquoted,
        DoubleQuoted,
        SingleQuoted,
    }

    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut state = State::Unquoted;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        match state {
            State::Unquoted => match c {
                ' ' | '\t' | '\n' | '\r' => {
                    if in_token {
                        tokens.push(std::mem::take(&mut current));
                        in_token = false;
                    }
                }
                '"' => {
                    state = State::DoubleQuoted;
                    in_token = true;
                }
                '\'' => {
                    state = State::SingleQuoted;
                    in_token = true;
                }
                _ => {
                    current.push(c);
                    in_token = true;
                }
            },
            State::DoubleQuoted => match c {
                '"' => state = State::Unquoted,
                '\\' => match chars.peek() {
                    Some(&next) if next == '"' || next == '\\' => {
                        current.push(next);
                        chars.next();
                    }
                    _ => current.push(c),
                },
                _ => current.push(c),
            },
            State::SingleQuoted => match c {
                '\'' => state = State::Unquoted,
                '\\' => match chars.peek() {
                    Some(&next) if next == '\'' || next == '\\' => {
                        current.push(next);
                        chars.next();
                    }
                    _ => current.push(c),
                },
                _ => current.push(c),
            },
        }
    }

    if in_token {
        tokens.push(current);
    }

    tokens
}

/// List bundled resources in a skill directory.
///
/// Returns file names (not full paths), sorted for deterministic output,
/// truncated to [`MAX_RESOURCE_ENTRIES`]. Always excludes `SKILL.md` and
/// the loaded skill file itself (which matters for flat-form skills where
/// `skill_dir` is the search directory and the loaded file sits alongside
/// other skills).
fn list_resources(
    skill_dir: &Path,
    skill_path: &Path,
    workspace_root: Option<&Path>,
) -> Vec<String> {
    let loaded_file_name = skill_path.file_name();

    let mut names: Vec<String> = Vec::new();
    for (path, kind) in directory_entries(skill_dir, workspace_root) {
        if kind != WorkspaceEntryKind::File {
            continue;
        }
        let Some(entry_name) = path.file_name() else {
            continue;
        };
        if entry_name == OsStr::new("SKILL.md") {
            continue;
        }
        if Some(entry_name) == loaded_file_name {
            continue;
        }
        if let Some(s) = entry_name.to_str() {
            names.push(s.to_owned());
        }
    }

    names.sort();
    names.truncate(MAX_RESOURCE_ENTRIES);
    names
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "skill"
    }

    fn description(&self) -> &'static str {
        include_str!("guidance/skill.description.md")
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Skills
    }

    fn usage_guidance(&self) -> Option<&str> {
        Some(include_str!("guidance/skill.usage.md"))
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the skill to load. Matches a <name>/SKILL.md or <name>.md file in the configured skill directories."
                },
                "arguments": {
                    "type": "string",
                    "description": "Optional free-form argument string. Parsed with shell-style quoting (double-quoted and single-quoted strings group, backslash escapes inside quotes). Tokens are mapped positionally to named arguments declared in the skill's frontmatter."
                }
            },
            "additionalProperties": false
        })
    }

    /// Skill activation reads templates from disk, but when shell
    /// execution is enabled the expansion phase runs skill-authored
    /// shell commands — an external-process effect that must never be
    /// scheduled as concurrent read-only work.
    fn effect(&self) -> ToolEffect {
        if self.config.shell_execution {
            ToolEffect::Process
        } else {
            ToolEffect::ReadOnly
        }
    }

    async fn pre_validate(
        &self,
        envelope: &ToolEnvelope,
        _ctx: &ToolContext,
    ) -> PreValidateOutcome {
        let args: SkillArgs = match serde_json::from_value(envelope.model_args.clone()) {
            Ok(args) => args,
            Err(e) => {
                return PreValidateOutcome::Block(
                    BlockDecision::new(format!("invalid arguments: {e}"))
                        .with_kind(ToolErrorKind::InvalidArguments),
                );
            }
        };
        if let Err(reason) = validate_skill_name(&args.name) {
            return PreValidateOutcome::Block(
                BlockDecision::new(reason)
                    .with_kind(ToolErrorKind::InvalidArguments)
                    .with_guidance(
                        "Pass the bare skill name as listed by the skill directories, \
                         without any path syntax.",
                    )
                    .with_detail(serde_json::json!({ "name": args.name })),
            );
        }
        PreValidateOutcome::Proceed
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SkillArgs = serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("invalid arguments: {e}"),
            }
        })?;
        // Re-checked at execute time so a direct invocation cannot bypass
        // the pre_validate gate and traverse out of the skill roots.
        if let Err(reason) = validate_skill_name(&args.name) {
            return Err(ToolError::pre_validation(
                ToolErrorKind::InvalidArguments,
                reason,
            ));
        }
        let paths: Arc<SkillSearchPaths> = ctx.require_extension::<SkillSearchPaths>()?;
        let workspace_root = ctx.get_extension::<WorkspaceSkillRoot>();

        let arguments_raw = args.arguments.unwrap_or_default();
        let positional = parse_shell_args(&arguments_raw);

        let working_dir = ctx.working_dir();
        let activation = ActivationInputs {
            requested_name: &args.name,
            arguments_raw: &arguments_raw,
            positional: &positional,
            working_dir: &working_dir,
            shell_execution: self.config.shell_execution,
        };
        for dir in &paths.0 {
            for candidate in [
                dir.join(&args.name).join("SKILL.md"),
                dir.join(format!("{}.md", args.name)),
            ] {
                if let Some(root) = workspace_root.as_ref()
                    && let Some(relative) = workspace_relative_path(&root.0, &candidate)
                {
                    let absent = matches!(
                        validate_workspace_regular_file(&root.0, &relative),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound
                    );
                    if !absent {
                        return activate(&activation, &candidate, Some(&root.0)).await;
                    }
                } else if candidate.is_file() {
                    return activate(&activation, &candidate, None).await;
                }
            }
        }

        let available = enumerate_available(
            &paths.0,
            workspace_root.as_deref().map(|root| root.0.as_path()),
        );
        Err(ToolError::ExecutionFailed {
            reason: format!(
                "skill \"{}\" not found. Available: {:?}",
                args.name, available
            ),
        })
    }
}

/// Everything `activate` needs beyond the resolved skill path.
struct ActivationInputs<'a> {
    requested_name: &'a str,
    arguments_raw: &'a str,
    positional: &'a [String],
    working_dir: &'a Path,
    /// From [`SkillToolConfig::shell_execution`]; `false` disables stage 1
    /// of template expansion.
    shell_execution: bool,
}

/// Load, expand, and package a skill into a [`ToolOutput`].
///
/// Auto-appends `\nARGUMENTS: <raw>` when the source body did not mention
/// `$ARGUMENTS` and a non-empty argument string was supplied
/// (Claude Code parity, NS-004 R4).
async fn activate(
    inputs: &ActivationInputs<'_>,
    skill_path: &Path,
    workspace_root: Option<&Path>,
) -> Result<ToolOutput, ToolError> {
    let loaded = workspace_root.map_or_else(
        || load_skill_from_path(skill_path),
        |root| load_workspace_skill_from_path(root, skill_path),
    );
    let loaded = loaded.map_err(|diags| {
        let reason = diags.first().map_or_else(
            || format!("failed to load skill at {}", skill_path.display()),
            |d| d.message.clone(),
        );
        ToolError::ExecutionFailed { reason }
    })?;

    let body_uses_args = loaded.body.contains("$ARGUMENTS")
        || loaded
            .metadata
            .arguments
            .as_slice()
            .iter()
            .any(|name| loaded.body.contains(&format!("${name}")));
    let auto_append_arguments = !body_uses_args && !inputs.arguments_raw.is_empty();

    let skill_dir = skill_path.parent().unwrap_or(skill_path);

    let template_inputs = TemplateInputs {
        body: &loaded.body,
        shell: loaded.metadata.shell.unwrap_or(SkillShell::Bash),
        cwd: inputs.working_dir,
        skill_dir,
        disable_shell: !inputs.shell_execution || workspace_root.is_some(),
        arguments_raw: inputs.arguments_raw,
        arguments_positional: inputs.positional,
        argument_names: loaded.metadata.arguments.as_slice(),
        session_id: "",
        effort: "",
        variables: None,
    };
    let mut expanded = expand(&template_inputs).await;

    if auto_append_arguments {
        expanded.push('\n');
        expanded.push_str("ARGUMENTS: ");
        expanded.push_str(inputs.arguments_raw);
    }

    let resources = list_resources(skill_dir, skill_path, workspace_root);

    let payload = serde_json::json!({
        "name": inputs.requested_name,
        "path": skill_path.display().to_string(),
        "content": expanded,
        "skill_dir": skill_dir.display().to_string(),
        "resources": resources,
    });

    Ok(ToolOutput::success(payload))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants,
    clippy::too_many_lines
)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::tool::envelope::ToolEnvelope;

    fn envelope_for(args: serde_json::Value) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "call-1".to_string(),
            tool_name: "skill".to_string(),
            model_args: args,
            metadata: serde_json::Value::Null,
        }
    }

    // ---------------- parse_shell_args ----------------

    #[test]
    fn parse_shell_args_splits_on_whitespace() {
        assert_eq!(parse_shell_args("hello world"), vec!["hello", "world"]);
    }

    #[test]
    fn parse_shell_args_double_quoted_groups() {
        assert_eq!(
            parse_shell_args("\"hello world\" second"),
            vec!["hello world", "second"]
        );
    }

    #[test]
    fn parse_shell_args_single_quoted_groups() {
        assert_eq!(
            parse_shell_args("'hello world' second"),
            vec!["hello world", "second"]
        );
    }

    #[test]
    fn parse_shell_args_empty_input_is_empty_vec() {
        let parsed: Vec<String> = parse_shell_args("");
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_shell_args_collapses_repeated_whitespace() {
        assert_eq!(
            parse_shell_args("  a   b\tc\n d "),
            vec!["a", "b", "c", "d"]
        );
    }

    #[test]
    fn parse_shell_args_mixed_quoted_and_unquoted() {
        assert_eq!(parse_shell_args("\"a\" b 'c d'"), vec!["a", "b", "c d"]);
    }

    #[test]
    fn parse_shell_args_backslash_escapes_inside_double_quotes() {
        assert_eq!(
            parse_shell_args("\"he said \\\"hi\\\"\""),
            vec!["he said \"hi\""]
        );
    }

    #[test]
    fn parse_shell_args_backslash_escapes_inside_single_quotes() {
        assert_eq!(parse_shell_args("'it\\'s'"), vec!["it's"]);
    }

    #[test]
    fn parse_shell_args_escaped_backslash_in_double_quotes() {
        assert_eq!(parse_shell_args("\"a\\\\b\""), vec!["a\\b"]);
    }

    #[test]
    fn parse_shell_args_unterminated_quote_flushes_accumulator() {
        // Visible failure rather than silent token loss: the accumulator
        // is emitted as the final token.
        assert_eq!(parse_shell_args("\"unterminated"), vec!["unterminated"]);
    }

    // ---------------- list_resources ----------------

    #[test]
    fn list_resources_excludes_skill_md_and_caps_at_twenty() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("with-resources");
        std::fs::create_dir(&skill_dir).unwrap();
        let skill_path = skill_dir.join("SKILL.md");
        std::fs::write(&skill_path, "---\ndescription: r\n---\n").unwrap();
        for i in 0..25 {
            std::fs::write(skill_dir.join(format!("ref-{i:02}.md")), "ref").unwrap();
        }

        let names = list_resources(&skill_dir, &skill_path, None);
        assert_eq!(names.len(), 20);
        assert!(!names.iter().any(|n| n == "SKILL.md"));
        // Sorted: the first should be ref-00 with a leading zero.
        assert_eq!(names[0], "ref-00.md");
    }

    #[test]
    fn list_resources_excludes_loaded_flat_file() {
        let dir = tempdir().unwrap();
        let skill_path = dir.path().join("alias.md");
        std::fs::write(&skill_path, "---\ndescription: a\n---\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "readme").unwrap();
        std::fs::write(dir.path().join("data.json"), "{}").unwrap();

        let names = list_resources(dir.path(), &skill_path, None);
        assert!(!names.iter().any(|n| n == "alias.md"));
        assert!(names.iter().any(|n| n == "README.md"));
        assert!(names.iter().any(|n| n == "data.json"));
    }

    #[test]
    fn list_resources_missing_dir_returns_empty() {
        let names = list_resources(
            Path::new("/nonexistent/path"),
            Path::new("/nonexistent/SKILL.md"),
            None,
        );
        assert!(names.is_empty());
    }

    // ---------------- name validation / traversal ----------------

    #[test]
    fn validate_skill_name_rejects_escapes() {
        for bad in [
            "",
            ".",
            "..",
            "../etc",
            "../../secrets",
            "a/b",
            "a\\b",
            "/etc/passwd",
            "..\\windows",
            "nested/../../up",
        ] {
            assert!(
                validate_skill_name(bad).is_err(),
                "name {bad:?} must be rejected",
            );
        }
        for good in ["deploy", "my-skill", "skill_2", "review.notes"] {
            assert!(
                validate_skill_name(good).is_ok(),
                "name {good:?} must be accepted",
            );
        }
    }

    #[tokio::test]
    async fn pre_validate_blocks_traversal_names() {
        let tool = SkillTool::new();
        let ctx = ToolContext::empty();
        for bad in ["../../../etc/passwd", "..", "a/b", "/abs"] {
            match tool
                .pre_validate(&envelope_for(json!({"name": bad})), &ctx)
                .await
            {
                PreValidateOutcome::Block(decision) => {
                    assert_eq!(
                        decision.kind,
                        crate::tool::failure::ToolErrorKind::InvalidArguments,
                        "typed block for {bad:?}",
                    );
                }
                PreValidateOutcome::Proceed => panic!("name {bad:?} must be blocked"),
            }
        }
    }

    #[tokio::test]
    async fn pre_validate_proceeds_for_plain_names() {
        let tool = SkillTool::new();
        let ctx = ToolContext::empty();
        assert!(matches!(
            tool.pre_validate(&envelope_for(json!({"name": "deploy"})), &ctx)
                .await,
            PreValidateOutcome::Proceed
        ));
    }

    /// Direct `execute` (bypassing pre_validate) must independently refuse
    /// a traversal name before any filesystem access — even when a file
    /// actually exists at the escaped location.
    #[tokio::test]
    async fn execute_refuses_traversal_even_when_target_exists() {
        let outer = tempdir().unwrap();
        let skills = outer.path().join("skills");
        std::fs::create_dir(&skills).unwrap();
        // A skill-shaped file OUTSIDE the configured root.
        std::fs::write(
            outer.path().join("secret.md"),
            "---\ndescription: s\n---\nstolen",
        )
        .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![skills])));
        let tool = SkillTool::new();
        let err = tool
            .execute(&envelope_for(json!({"name": "../secret"})), &ctx)
            .await
            .expect_err("traversal must be refused");
        assert!(
            err.to_string().contains("separators"),
            "refusal must name the violation: {err}",
        );
    }

    // ---------------- shell execution policy ----------------

    #[tokio::test]
    async fn shell_disabled_config_suppresses_shell_expansion() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("sh-skill");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: s\n---\nvalue: !`echo leaked`",
        )
        .await
        .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::with_config(SkillToolConfig {
            shell_execution: false,
        });
        let out = tool
            .execute(&envelope_for(json!({"name": "sh-skill"})), &ctx)
            .await
            .unwrap();
        let content = out.content["content"].as_str().unwrap();
        assert!(
            !content.contains("leaked"),
            "no shell output may appear when execution is disabled: {content}",
        );
        assert!(
            content.contains("disabled by policy"),
            "the disabled marker must be visible: {content}",
        );
    }

    #[tokio::test]
    async fn shell_enabled_default_expands_shell_and_reports_process_effect() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("sh-skill");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: s\n---\nvalue: !`echo expanded`",
        )
        .await
        .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        assert_eq!(
            tool.effect(),
            ToolEffect::Process,
            "shell-enabled skill activation must not schedule as read-only",
        );
        let out = tool
            .execute(&envelope_for(json!({"name": "sh-skill"})), &ctx)
            .await
            .unwrap();
        let content = out.content["content"].as_str().unwrap();
        assert!(content.contains("expanded"), "{content}");
    }

    #[tokio::test]
    async fn workspace_skill_shell_is_disabled_even_when_tool_default_is_enabled() {
        let workspace = tempdir().unwrap();
        let skill_dir = workspace.path().join("skills/repository-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: repository skill\n---\n!`touch repository-command-ran`",
        )
        .await
        .unwrap();

        let launch_root = workspace.path().canonicalize().unwrap();
        let ctx = ToolContext::empty();
        ctx.set_working_dir(launch_root.clone());
        ctx.insert_extension(Arc::new(WorkspaceSkillRoot(launch_root.clone())));
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![launch_root.join("skills")])));
        std::fs::create_dir(launch_root.join("subdir")).unwrap();
        ctx.set_working_dir(launch_root.join("subdir"));
        let out = SkillTool::new()
            .execute(&envelope_for(json!({"name": "repository-skill"})), &ctx)
            .await
            .unwrap();
        let content = out.content["content"].as_str().unwrap();

        assert!(content.contains("disabled by policy"));
        assert!(!workspace.path().join("repository-command-ran").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_skill_symlink_is_refused_before_activation() {
        use std::os::unix::fs::symlink;

        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let skill_dir = workspace.path().join("skills/leak");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let target = outside.path().join("SKILL.md");
        std::fs::write(
            &target,
            "---\ndescription: leak\n---\nsentinel-private-skill",
        )
        .unwrap();
        symlink(&target, skill_dir.join("SKILL.md")).unwrap();
        let launch_root = workspace.path().canonicalize().unwrap();
        let ctx = ToolContext::empty();
        ctx.set_working_dir(launch_root.clone());
        ctx.insert_extension(Arc::new(WorkspaceSkillRoot(launch_root.clone())));
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![launch_root.join("skills")])));

        let error = SkillTool::new()
            .execute(&envelope_for(json!({"name": "leak"})), &ctx)
            .await
            .expect_err("workspace skill symlink must be refused");

        assert!(!error.to_string().contains("sentinel-private-skill"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_workspace_skill_does_not_enumerate_symlinked_external_root() {
        use std::os::unix::fs::symlink;

        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(
            outside.path().join("sentinel-external-skill-name.md"),
            "---\ndescription: external\n---\nexternal",
        )
        .unwrap();
        let norn_dir = workspace.path().join(".norn");
        std::fs::create_dir(&norn_dir).unwrap();
        symlink(outside.path(), norn_dir.join("skills")).unwrap();
        let launch_root = workspace.path().canonicalize().unwrap();
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(WorkspaceSkillRoot(launch_root.clone())));
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![
            launch_root.join(".norn/skills"),
        ])));

        let error = SkillTool::new()
            .execute(&envelope_for(json!({"name": "missing"})), &ctx)
            .await
            .expect_err("missing skill must remain missing");

        assert!(!error.to_string().contains("sentinel-external-skill-name"));
    }

    #[test]
    fn shell_disabled_config_reports_read_only_effect() {
        let tool = SkillTool::with_config(SkillToolConfig {
            shell_execution: false,
        });
        assert_eq!(tool.effect(), ToolEffect::ReadOnly);
    }

    // ---------------- existing behaviour, fixtures with frontmatter ----------------

    #[tokio::test]
    async fn loads_skill_md_from_directory() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("my-skill");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: do the thing\n---\nDo the thing.",
        )
        .await
        .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let out = tool
            .execute(&envelope_for(json!({"name": "my-skill"})), &ctx)
            .await
            .unwrap();
        assert!(!out.is_error());
        assert_eq!(out.content["content"], "Do the thing.");
        assert_eq!(out.content["name"], "my-skill");
        // R5: skill_dir + resources are present.
        assert!(out.content["skill_dir"].is_string());
        assert!(out.content["resources"].is_array());
    }

    #[tokio::test]
    async fn falls_back_to_flat_md_file() {
        let dir = tempdir().unwrap();
        tokio::fs::write(
            dir.path().join("alias.md"),
            "---\ndescription: alias\n---\nalias body",
        )
        .await
        .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let out = tool
            .execute(&envelope_for(json!({"name": "alias"})), &ctx)
            .await
            .unwrap();
        assert_eq!(out.content["content"], "alias body");
    }

    #[tokio::test]
    async fn missing_skill_lists_available_choices() {
        let dir = tempdir().unwrap();
        let s1 = dir.path().join("one");
        std::fs::create_dir(&s1).unwrap();
        tokio::fs::write(s1.join("SKILL.md"), "---\ndescription: one\n---\none body")
            .await
            .unwrap();
        tokio::fs::write(
            dir.path().join("two.md"),
            "---\ndescription: two\n---\ntwo body",
        )
        .await
        .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let err = tool
            .execute(&envelope_for(json!({"name": "missing"})), &ctx)
            .await
            .expect_err("missing");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(reason.contains("missing"));
                assert!(reason.contains("one"));
                assert!(reason.contains("two"));
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_search_paths_returns_missing_extension() {
        let tool = SkillTool::new();
        let ctx = ToolContext::empty();
        let err = tool
            .execute(&envelope_for(json!({"name": "anything"})), &ctx)
            .await
            .expect_err("no paths");
        match err {
            ToolError::MissingExtension { extension } => {
                assert!(extension.contains("SkillSearchPaths"), "{extension}");
            }
            other => panic!("expected MissingExtension, got {other:?}"),
        }
    }

    // ---------------- R1: arguments parameter ----------------

    #[tokio::test]
    async fn r1_input_schema_advertises_optional_arguments() {
        let tool = SkillTool::new();
        let schema = tool.input_schema();
        let props = schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("properties");
        assert!(props.contains_key("arguments"));
        assert_eq!(
            props["arguments"]["type"].as_str(),
            Some("string"),
            "arguments must be typed as string"
        );
        let required = schema
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        let required_names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(!required_names.contains(&"arguments"));
        assert!(required_names.contains(&"name"));
    }

    #[tokio::test]
    async fn r1_arguments_passes_through_to_template() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("deploy");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: deploy\n---\nDeploy to $ARGUMENTS now.",
        )
        .await
        .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"name": "deploy", "arguments": "prod"})),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["content"], "Deploy to prod now.");
    }

    // ---------------- R3: named argument mapping ----------------

    #[tokio::test]
    async fn r3_positional_args_map_to_named_arguments() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("fix");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: fix\narguments: [issue, branch]\n---\nFix $issue on $branch.",
        )
        .await
        .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"name": "fix", "arguments": "123 main"})),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["content"], "Fix 123 on main.");
    }

    #[tokio::test]
    async fn r3_excess_names_resolve_to_empty_string() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("fix");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: fix\narguments: [issue, branch]\n---\nFix [$issue][$branch].",
        )
        .await
        .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"name": "fix", "arguments": "123"})),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["content"], "Fix [123][].");
    }

    #[tokio::test]
    async fn r3_excess_args_only_reachable_via_positional() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("fix");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: fix\narguments: [issue]\n---\n$issue and $1.",
        )
        .await
        .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"name": "fix", "arguments": "123 main"})),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["content"], "123 and main.");
    }

    // ---------------- R4: $ARGUMENTS auto-append ----------------

    #[tokio::test]
    async fn r4_no_auto_append_when_body_mentions_arguments() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("mentions");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: m\n---\nuse $ARGUMENTS",
        )
        .await
        .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"name": "mentions", "arguments": "hello"})),
                &ctx,
            )
            .await
            .unwrap();
        // The substring was expanded inline; no trailing ARGUMENTS: line.
        let content = out.content["content"].as_str().unwrap();
        assert_eq!(content, "use hello");
        assert!(!content.contains("\nARGUMENTS:"));
    }

    #[tokio::test]
    async fn r4_auto_append_when_body_lacks_arguments_with_value() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("plain");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(skill_dir.join("SKILL.md"), "---\ndescription: p\n---\nbody")
            .await
            .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let out = tool
            .execute(
                &envelope_for(json!({"name": "plain", "arguments": "hello"})),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(out.content["content"], "body\nARGUMENTS: hello");
    }

    #[tokio::test]
    async fn r4_no_auto_append_when_no_arguments_supplied() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("plain");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(skill_dir.join("SKILL.md"), "---\ndescription: p\n---\nbody")
            .await
            .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let out = tool
            .execute(&envelope_for(json!({"name": "plain"})), &ctx)
            .await
            .unwrap();
        let content = out.content["content"].as_str().unwrap();
        assert_eq!(content, "body");
        assert!(!content.contains("ARGUMENTS:"));
    }

    // ---------------- R5: JSON payload contract ----------------

    #[tokio::test]
    async fn r5_returned_payload_has_five_documented_fields() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("with-refs");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: r\n---\nthe body",
        )
        .await
        .unwrap();
        std::fs::write(skill_dir.join("notes.md"), "notes").unwrap();
        std::fs::write(skill_dir.join("data.json"), "{}").unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let out = tool
            .execute(&envelope_for(json!({"name": "with-refs"})), &ctx)
            .await
            .unwrap();

        let obj = out.content.as_object().expect("object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> =
            ["name", "path", "content", "skill_dir", "resources"]
                .iter()
                .copied()
                .collect();
        assert_eq!(keys, expected);

        assert_eq!(out.content["content"], "the body");
        // Frontmatter must be stripped — content never contains the
        // delimiter or YAML field syntax.
        let content_str = out.content["content"].as_str().unwrap();
        assert!(!content_str.contains("---"));
        assert!(!content_str.contains("description:"));

        let resources = out.content["resources"].as_array().expect("array");
        let names: Vec<&str> = resources.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"notes.md"));
        assert!(names.contains(&"data.json"));
        assert!(!names.contains(&"SKILL.md"));
        assert!(names.len() <= 20);
    }

    #[tokio::test]
    async fn r5_skill_dir_matches_parent_for_dir_form() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("with-dir");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(skill_dir.join("SKILL.md"), "---\ndescription: d\n---\nbody")
            .await
            .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let out = tool
            .execute(&envelope_for(json!({"name": "with-dir"})), &ctx)
            .await
            .unwrap();
        assert_eq!(
            out.content["skill_dir"].as_str().unwrap(),
            skill_dir.display().to_string()
        );
    }

    #[tokio::test]
    async fn r5_skill_with_missing_description_surfaces_diagnostic() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("broken");
        std::fs::create_dir(&skill_dir).unwrap();
        tokio::fs::write(skill_dir.join("SKILL.md"), "---\nname: broken\n---\nbody")
            .await
            .unwrap();

        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SkillSearchPaths(vec![dir.path().to_path_buf()])));
        let tool = SkillTool::new();
        let err = tool
            .execute(&envelope_for(json!({"name": "broken"})), &ctx)
            .await
            .expect_err("missing description");
        match err {
            ToolError::ExecutionFailed { reason } => {
                assert!(
                    reason.contains("description"),
                    "expected diagnostic message, got: {reason}"
                );
            }
            other => panic!("expected ExecutionFailed, got {other:?}"),
        }
    }
}
