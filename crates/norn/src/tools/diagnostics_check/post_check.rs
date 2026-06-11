//! Post-tool diagnostic check entry point for convention-driven validation.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use diagnostics::conventions::{CompiledRule, ConventionsConfig, LocCheck, TestTrigger, ToolRef};
use serde_json::json;

use crate::tool::context::ToolContext;
use crate::tool::lifecycle::{
    Advisory, PostCheckResult, PostValidateOutcome, RuntimePostValidateCheck,
};
use crate::tool::traits::ToolOutput;

use super::adapters::run_rule_diagnostic_tool;
use super::findings::Findings;
use super::infra::DiagnosticInfra;
use super::loc::run_loc_check;
use super::lsp_diagnostics::{LspDiagnosticsOutcome, try_lsp_diagnostics_for_rules};
use super::lsp_tests::run_lsp_tests_for_matched_rules;
use super::remediation::{run_remediation_tool, run_report_tool};
use super::trigger::{
    absolute_workspace_path, is_task_complete_output, matched_rules_for_trigger, run_pattern_tool,
};

/// Post-validate check that runs diagnostics on modified files.
pub struct DiagnosticsPostCheck;

#[async_trait]
impl RuntimePostValidateCheck for DiagnosticsPostCheck {
    async fn check(&self, output: &ToolOutput, ctx: &ToolContext) -> PostCheckResult {
        let tool_name = tool_name_from_output(output);
        tracing::debug!(tool = tool_name, "diagnostics post-check entry");

        let Some(infra) = ctx.get_extension::<DiagnosticInfra>() else {
            tracing::debug!(
                tool = tool_name,
                reason = "no DiagnosticInfra extension",
                "diagnostics post-check exit: pass"
            );
            return PostCheckResult::pass();
        };

        if is_task_complete_output(output) {
            let files: Vec<PathBuf> = infra.modified_files().into_iter().collect();
            let Some(conventions) = infra.conventions.as_ref() else {
                return PostCheckResult::pass();
            };
            return run_diagnostics_for_trigger(
                TestTrigger::TaskComplete,
                None,
                &files,
                conventions,
                infra.as_ref(),
            )
            .await;
        }

        let paths = extract_modified_paths(output);
        if paths.is_empty() {
            tracing::debug!(
                tool = tool_name,
                reason = "no modified paths in tool output",
                "diagnostics post-check exit: pass"
            );
            return PostCheckResult::pass();
        }

        for file_path in &paths {
            if let Ok(relative_path) = workspace_relative_path(file_path, &infra.workspace_root) {
                match infra.modified_files.lock() {
                    Ok(mut modified_files) => {
                        modified_files.insert(relative_path);
                    }
                    Err(poisoned) => {
                        tracing::warn!(
                            "modified-files accumulator mutex was poisoned; recording path"
                        );
                        poisoned.into_inner().insert(relative_path);
                    }
                }
            }
        }

        let Some(conventions) = infra.conventions.as_ref() else {
            tracing::debug!(
                tool = tool_name,
                reason = "no conventions configured",
                "diagnostics post-check exit: pass"
            );
            return PostCheckResult::pass();
        };

        run_diagnostics_for_trigger(
            TestTrigger::Tool,
            Some(tool_name),
            &paths,
            conventions,
            infra.as_ref(),
        )
        .await
    }
}

/// Run the full diagnostics pipeline for a lifecycle trigger and file set.
///
/// `tool_name` is the actual mutation tool (`write`/`edit`/`apply_patch`) for
/// `TestTrigger::Tool`, or `None` for non-tool triggers where the `tools`
/// list on rules is not checked.
pub async fn run_diagnostics_for_trigger(
    trigger: TestTrigger,
    tool_name: Option<&str>,
    files: &[PathBuf],
    conventions: &ConventionsConfig,
    infra: &DiagnosticInfra,
) -> PostCheckResult {
    if files.is_empty() {
        return PostCheckResult::pass();
    }

    let mut all_errors = Vec::new();
    let mut all_advisories = Vec::new();

    for file_path in files {
        let absolute_path = absolute_workspace_path(file_path, &infra.workspace_root);
        if let Err(error) = check_convention_file(
            &absolute_path,
            trigger,
            tool_name,
            conventions,
            infra,
            &mut all_errors,
            &mut all_advisories,
        )
        .await
        {
            tracing::warn!(
                path = %file_path.display(),
                error = %error,
                "diagnostics check failed for file"
            );
        }
    }

    PostCheckResult {
        outcome: if all_errors.is_empty() {
            PostValidateOutcome::Pass
        } else {
            PostValidateOutcome::Fail { errors: all_errors }
        },
        advisories: all_advisories,
    }
}

async fn check_convention_file(
    file_path: &Path,
    trigger: TestTrigger,
    tool_name: Option<&str>,
    conventions: &ConventionsConfig,
    infra: &DiagnosticInfra,
    errors: &mut Vec<String>,
    advisories: &mut Vec<Advisory>,
) -> Result<(), String> {
    let relative_path = workspace_relative_path(file_path, &infra.workspace_root)?;
    let loc_rules = matched_rules_for_loc(tool_name, &relative_path, conventions);
    let activation_rules =
        matched_rules_for_trigger(trigger, tool_name, &relative_path, conventions);

    if let Some(loc) = resolve_loc_winner(&loc_rules) {
        run_loc_check(file_path, loc, errors, advisories);
    }

    let mut findings = Findings { errors, advisories };

    // LSP is a special rule sub-struct and must run before the rule
    // activations: when it returns `Used`, the contract on
    // `LspDiagnosticsOutcome::Used` requires skipping the server-query
    // and inline-adapter paths, which are exactly the
    // `ToolRef::Diagnostic` dispatches inside `run_rule_activations`.
    let lsp_tool = tool_name.unwrap_or("write");
    let lsp_outcome = try_lsp_diagnostics_for_rules(
        file_path,
        &relative_path,
        lsp_tool,
        conventions,
        infra,
        &mut findings,
    )
    .await;

    run_rule_activations(
        &ActivationContext {
            file_path,
            relative_path: &relative_path,
            conventions,
            infra,
            trigger,
            lsp_outcome,
        },
        &activation_rules,
        &mut findings,
    )
    .await;

    run_lsp_tests_for_matched_rules(
        file_path,
        &relative_path,
        lsp_tool,
        trigger,
        conventions,
        infra,
        &mut findings,
    )
    .await;

    Ok(())
}

fn matched_rules_for_loc<'a>(
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
                && compiled.rule.loc.is_some()
        })
        .map(|(name, compiled)| (name.as_str(), compiled))
        .collect()
}

/// Shared inputs for dispatching one file's rule activations.
struct ActivationContext<'a> {
    file_path: &'a Path,
    relative_path: &'a Path,
    conventions: &'a ConventionsConfig,
    infra: &'a DiagnosticInfra,
    trigger: TestTrigger,
    lsp_outcome: LspDiagnosticsOutcome,
}

async fn run_rule_activations(
    ctx: &ActivationContext<'_>,
    matched_rules: &[(&str, &CompiledRule)],
    findings: &mut Findings<'_>,
) {
    let mut file_content: Option<String> = None;

    for (rule_name, compiled) in matched_rules {
        for (tool_name, activation) in &compiled.rule.activations {
            if !activation.on.contains(&ctx.trigger) {
                continue;
            }
            let Some(language) = compiled.language.as_deref() else {
                tracing::warn!(
                    rule = rule_name,
                    tool = tool_name,
                    "matched rule has no resolved language; skipping tool activation"
                );
                continue;
            };
            let Some(tool) = ctx.conventions.lookup_tool(language, tool_name) else {
                tracing::warn!(
                    rule = rule_name,
                    language,
                    tool = tool_name,
                    "activated tool was not found in language definition; skipping"
                );
                continue;
            };

            match tool {
                ToolRef::Diagnostic(def) => {
                    // `LspDiagnosticsOutcome::Used` means the LSP fast
                    // path already produced findings for this file; its
                    // contract requires skipping the server-query and
                    // inline-adapter dispatch entirely.
                    if ctx.lsp_outcome == LspDiagnosticsOutcome::Used {
                        tracing::debug!(
                            rule = rule_name,
                            tool = tool_name,
                            "LSP diagnostics path satisfied this file; skipping diagnostic dispatch"
                        );
                        continue;
                    }
                    run_rule_diagnostic_tool(
                        ctx.file_path,
                        ctx.relative_path,
                        tool_name,
                        def,
                        activation.handling,
                        ctx.infra,
                        findings,
                    )
                    .await;
                }
                ToolRef::Pattern(def) => {
                    if file_content.is_none() {
                        match std::fs::read_to_string(ctx.file_path) {
                            Ok(content) => file_content = Some(content),
                            Err(error) => {
                                findings.errors.push(format!(
                                    "{} [pattern:{tool_name}] could not read file content: {error}",
                                    ctx.file_path.display()
                                ));
                                continue;
                            }
                        }
                    }
                    let Some(content) = file_content.as_deref() else {
                        continue;
                    };
                    run_pattern_tool(
                        ctx.file_path,
                        tool_name,
                        def,
                        activation.handling,
                        content,
                        Some(language),
                        findings,
                    );
                }
                ToolRef::Remediation(def) => {
                    run_remediation_tool(ctx.file_path, tool_name, def, ctx.infra, findings).await;
                }
                ToolRef::Report(def) => {
                    run_report_tool(ctx.file_path, tool_name, def, ctx.infra, findings).await;
                }
            }
        }
    }
}

fn resolve_loc_winner<'a>(matched_rules: &[(&'a str, &'a CompiledRule)]) -> Option<&'a LocCheck> {
    let mut winner = None;
    for (_, compiled) in matched_rules {
        let Some(loc) = compiled.rule.loc.as_ref() else {
            continue;
        };
        match winner {
            None => winner = Some((compiled.rule.priority, loc)),
            Some((priority, _)) if compiled.rule.priority > priority => {
                winner = Some((compiled.rule.priority, loc));
            }
            Some(_) => {}
        }
    }
    winner.map(|(_, loc)| loc)
}

fn workspace_relative_path(file_path: &Path, workspace_root: &Path) -> Result<PathBuf, String> {
    if file_path.is_relative() {
        return Ok(file_path.to_path_buf());
    }

    file_path
        .strip_prefix(workspace_root)
        .map(Path::to_path_buf)
        .map_err(|strip_error| {
            format!(
                "modified path `{}` is not under workspace root `{}`: {strip_error}",
                file_path.display(),
                workspace_root.display()
            )
        })
}

fn tool_name_from_output(output: &ToolOutput) -> &str {
    let obj = &output.content;
    if obj.get("files_modified").is_some() {
        "apply_patch"
    } else if obj
        .get("committed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        "edit"
    } else if obj.get("bytes_written").is_some() {
        "write"
    } else {
        "unknown"
    }
}

/// Extracts file paths from a tool's output content.
///
/// Only returns paths from tools that actually modified files:
/// - `Write`: has `"bytes_written"`
/// - `Edit`: has `"committed": true`
/// - `ApplyPatch`: has `"files_modified"`
///
/// Read tool output also carries a `"path"` field but is distinguished
/// by its `"kind"` being `"text"`, `"binary"`, or `"image"` and having
/// none of the modification markers above.
fn extract_modified_paths(output: &ToolOutput) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let obj = &output.content;

    let is_write = obj.get("bytes_written").is_some();
    let is_committed_edit = obj
        .get("committed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if (is_write || is_committed_edit)
        && let Some(path_str) = obj.get("path").and_then(|v| v.as_str())
    {
        paths.push(PathBuf::from(path_str));
    }

    if let Some(files) = obj.get("files_modified").and_then(|v| v.as_array()) {
        for f in files {
            if let Some(s) = f.as_str() {
                paths.push(PathBuf::from(s));
            }
        }
    }

    paths
}

/// Builds diagnostic JSON for inclusion in tool output.
pub fn errors_to_diagnostic_json(errors: &[String]) -> serde_json::Value {
    let diagnostics: Vec<serde_json::Value> = errors
        .iter()
        .map(|e| json!({ "message": e, "source": "norn-diagnostics" }))
        .collect();
    json!(diagnostics)
}
