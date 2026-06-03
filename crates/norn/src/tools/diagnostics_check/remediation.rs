//! Rule-based remediation and report subprocess runners.

use std::path::{Path, PathBuf};

use diagnostics::adapter::invoke::invoke_tool;
use diagnostics::conventions::{RemediationDef, ReportDef, ToolTarget};
use diagnostics::languages::rust::crate_root_for_file;

use crate::tool::lifecycle::{Advisory, AdvisorySeverity};

use super::findings::Findings;
use super::infra::DiagnosticInfra;

pub(super) async fn run_remediation_tool(
    file_path: &Path,
    tool_name: &str,
    def: &RemediationDef,
    infra: &DiagnosticInfra,
    findings: &mut Findings<'_>,
) {
    let target_path = match resolve_target_path(file_path, def.target, infra) {
        Ok(target_path) => target_path,
        Err(error) => {
            findings.errors.push(format!(
                "{} [scope] cannot resolve remediation target: {error}",
                file_path.display()
            ));
            return;
        }
    };
    let args = args_with_target(&def.args, &target_path);

    match invoke_tool(tool_name, &args, &infra.workspace_root, 0).await {
        Ok(result) if result.exit_code == 0 => {
            tracing::info!(
                tool = tool_name,
                file = %file_path.display(),
                target = %target_path.display(),
                "remediation tool completed"
            );
        }
        Ok(result) => findings.errors.push(format!(
            "{} [remediation:{tool_name}] subprocess failed with exit code {}. stderr: {}",
            file_path.display(),
            result.exit_code,
            result.stderr.trim()
        )),
        Err(error) => findings.errors.push(format!(
            "{} [remediation:{tool_name}] subprocess invocation failed: {error}",
            file_path.display()
        )),
    }
}

pub(super) async fn run_report_tool(
    file_path: &Path,
    tool_name: &str,
    def: &ReportDef,
    infra: &DiagnosticInfra,
    findings: &mut Findings<'_>,
) {
    let target_path = match resolve_target_path(file_path, def.target, infra) {
        Ok(target_path) => target_path,
        Err(error) => {
            findings.advisories.push(Advisory {
                severity: AdvisorySeverity::Warning,
                source: tool_name.to_owned(),
                message: format!(
                    "{} [report:{tool_name}] could not resolve invocation target: {error}",
                    file_path.display()
                ),
            });
            return;
        }
    };
    let args = args_with_target(&def.args, &target_path);

    match invoke_tool(tool_name, &args, &infra.workspace_root, 0).await {
        Ok(result) if result.exit_code == 0 => {
            tracing::info!(
                tool = tool_name,
                file = %file_path.display(),
                target = %target_path.display(),
                stdout = %result.stdout.trim(),
                "report tool completed"
            );
        }
        Ok(result) => findings.advisories.push(Advisory {
            severity: AdvisorySeverity::Warning,
            source: tool_name.to_owned(),
            message: format!(
                "{} [report:{tool_name}] subprocess failed with exit code {}. stderr: {}",
                file_path.display(),
                result.exit_code,
                result.stderr.trim()
            ),
        }),
        Err(error) => findings.advisories.push(Advisory {
            severity: AdvisorySeverity::Warning,
            source: tool_name.to_owned(),
            message: format!(
                "{} [report:{tool_name}] subprocess invocation failed: {error}",
                file_path.display()
            ),
        }),
    }
}

fn resolve_target_path(
    file_path: &Path,
    target: ToolTarget,
    infra: &DiagnosticInfra,
) -> Result<PathBuf, String> {
    match target {
        ToolTarget::File => Ok(file_path.to_path_buf()),
        ToolTarget::Package => match crate_root_for_file(file_path, &infra.workspace_root) {
            Ok(path) => Ok(path),
            Err(error) => Err(error.to_string()),
        },
        ToolTarget::Workspace => Ok(infra.workspace_root.clone()),
    }
}

fn args_with_target(args: &[String], target_path: &Path) -> Vec<String> {
    let mut resolved = args.to_vec();
    resolved.push(target_path.display().to_string());
    resolved
}
