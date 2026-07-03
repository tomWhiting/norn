//! Trigger-aware matching and helper functions for the post-check pipeline.

use std::path::{Path, PathBuf};

use diagnostics::conventions::{
    CompiledPatternDef, CompiledRule, ConventionsConfig, Handling, PatternDef, PatternMatch,
    TestTrigger, execute_patterns,
};

use crate::tool::lifecycle::{Advisory, AdvisorySeverity};
use crate::tool::traits::ToolOutput;

use super::findings::Findings;

/// Return rules whose activated tools include the given trigger.
///
/// When `tool_name` is provided (for `TestTrigger::Tool`), rules are
/// additionally filtered by their `tools` list. For non-tool triggers
/// (`TaskComplete`, `Stop`), `tool_name` is `None` and the `tools`
/// list is not checked — the trigger match alone determines activation.
pub(super) fn matched_rules_for_trigger<'a>(
    trigger: TestTrigger,
    tool_name: Option<&str>,
    relative_path: &Path,
    conventions: &'a ConventionsConfig,
) -> Vec<(&'a str, &'a CompiledRule)> {
    conventions
        .rules()
        .iter()
        .filter(|(_, compiled)| {
            compiled.matcher.is_match(relative_path)
                && tool_name.is_none_or(|name| compiled.rule.tools.iter().any(|tool| tool == name))
                && compiled
                    .rule
                    .activations
                    .values()
                    .any(|activation| activation.on.contains(&trigger))
        })
        .map(|(name, compiled)| (name.as_str(), compiled))
        .collect()
}

/// Detect whether a tool output represents a `TaskTool` `action=complete`.
pub(super) fn is_task_complete_output(output: &ToolOutput) -> bool {
    output
        .content
        .get("action")
        .and_then(serde_json::Value::as_str)
        == Some("complete")
        && output.content.get("task").is_some()
}

/// Resolve a potentially-relative file path to absolute using the
/// workspace root.
pub(super) fn absolute_workspace_path(file_path: &Path, workspace_root: &Path) -> PathBuf {
    if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        workspace_root.join(file_path)
    }
}

/// Run a single pattern tool and route findings by handling.
pub(super) fn run_pattern_tool(
    file_path: &Path,
    tool_name: &str,
    def: &PatternDef,
    handling: Handling,
    file_content: &str,
    language: Option<&str>,
    findings: &mut Findings<'_>,
) {
    let mut pattern_def = def.clone();
    pattern_def.handling = handling;
    // Compile here (the new conventions API precompiles patterns at config
    // load; this path receives a bare PatternDef, so it compiles one-off).
    // A pattern that fails to compile surfaces as a finding error.
    let compiled =
        match CompiledPatternDef::compile(tool_name, &pattern_def, language.unwrap_or_default()) {
            Ok(compiled) => vec![compiled],
            Err(error) => {
                findings.errors.push(format!(
                    "{} [pattern:{tool_name}] pattern failed to compile: {error}",
                    file_path.display()
                ));
                return;
            }
        };

    let result = execute_patterns(&compiled, file_content);
    for finding in result.errors {
        findings
            .errors
            .push(format_pattern_message(file_path, tool_name, &finding));
    }
    for finding in result.advisories {
        findings.advisories.push(Advisory {
            severity: AdvisorySeverity::Warning,
            source: tool_name.to_owned(),
            message: format_pattern_message(file_path, tool_name, &finding),
        });
    }
    for failure in result.failures {
        findings.errors.push(format!(
            "{} [pattern:{}] pattern execution failed: {}",
            file_path.display(),
            failure.pattern_name,
            failure.error
        ));
    }
}

/// Format a pattern match into a diagnostic message.
fn format_pattern_message(file_path: &Path, tool_name: &str, finding: &PatternMatch) -> String {
    format!(
        "{}:{}:{} [pattern:{tool_name}] {}",
        file_path.display(),
        finding.line,
        finding.column,
        finding.feedback
    )
}
