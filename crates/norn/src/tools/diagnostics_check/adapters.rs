//! Rule diagnostic tool dispatch — routes activated language diagnostic tools
//! through the diagnostic server or inline adapter registry.

use std::path::Path;

use diagnostics::adapter::invoke::invoke_tool_with_env;
use diagnostics::conventions::{DiagnosticToolDef, Handling};
use diagnostics::event::DiagnosticEvent;
use diagnostics::languages::rust::crate_for_file;
use diagnostics::policy::PolicyVerdict;
use diagnostics::registry::PolicyRegistry;

use crate::tool::lifecycle::{Advisory, AdvisorySeverity};

use super::findings::Findings;
use super::infra::DiagnosticInfra;
use super::server_query::{ServerQueryOutcome, try_server_query_for_tool};

pub(super) async fn run_rule_diagnostic_tool(
    file_path: &Path,
    relative_path: &Path,
    tool_name: &str,
    def: &DiagnosticToolDef,
    handling: Handling,
    infra: &DiagnosticInfra,
    findings: &mut Findings<'_>,
) {
    let server_outcome = try_server_query_for_tool(
        file_path,
        relative_path,
        tool_name,
        handling,
        infra,
        findings,
    )
    .await;
    if matches!(server_outcome, ServerQueryOutcome::Used) {
        return;
    }

    let Some(adapter) = infra.adapters.adapter_by_name(tool_name) else {
        push_diagnostic_dispatch_failure(
            tool_name,
            relative_path,
            handling,
            "adapter name did not resolve in the inline adapter registry",
            findings,
        );
        return;
    };

    if adapter.file_patterns().is_empty() {
        push_diagnostic_dispatch_failure(
            tool_name,
            relative_path,
            handling,
            "adapter declares no file patterns",
            findings,
        );
        return;
    }

    let Some(command) = adapter.command() else {
        push_diagnostic_dispatch_failure(
            tool_name,
            relative_path,
            handling,
            "adapter has no subprocess command",
            findings,
        );
        return;
    };

    // Resolve the owning crate only when the invocation actually references the
    // `{crate}` template. Clippy needs it for `-p {crate}`, but a file-scoped
    // tool (e.g. biome) does not, and demanding a `Cargo.toml` for every `.rs`
    // file would wrongly skip diagnostics for files that have no owning crate.
    let needs_crate = command
        .args
        .iter()
        .chain(def.args.iter())
        .any(|arg| arg.contains("{crate}"));
    let crate_name = if needs_crate {
        match crate_for_file(file_path, &infra.workspace_root) {
            Ok(name) => Some(name),
            Err(e) => {
                findings.errors.push(format!(
                    "{} [scope] cannot resolve owning crate: {e}. Adapter diagnostics skipped for this file.",
                    file_path.display(),
                ));
                return;
            }
        }
    } else {
        None
    };

    // Build the invocation from the adapter's own command, expanding path and
    // crate templates. The adapter command carries the real tool invocation
    // (e.g. clippy's `-p {crate} --all-targets --message-format=json -D
    // warnings`); the convention declaration only contributes extra arguments
    // and environment on top. Earlier this used `def.args` alone, which is
    // empty for declarations like `clippy = { target = "package", handling =
    // "advise" }`, so the tool ran as a bare binary and produced no findings.
    let mut args: Vec<String> = command
        .args
        .iter()
        .map(|arg| {
            expand_template(
                arg,
                relative_path,
                &infra.workspace_root,
                crate_name.as_deref(),
            )
        })
        .collect();
    args.extend(def.args.iter().map(|arg| {
        expand_template(
            arg,
            relative_path,
            &infra.workspace_root,
            crate_name.as_deref(),
        )
    }));

    let mut env = command.env.clone();
    env.extend(parse_env(&def.env));
    // A diagnostic tool without a configured timeout runs to completion. This
    // is a deliberate, supported choice (e.g. clippy, which must not be cut
    // off mid-run), so `0` (no timeout) is used silently rather than warned.
    let timeout_ms = def.timeout.map_or(0, |secs| secs.saturating_mul(1_000));

    match invoke_tool_with_env(
        &command.binary,
        &args,
        &env,
        &infra.workspace_root,
        timeout_ms,
    )
    .await
    {
        Ok(invocation) => {
            let events =
                adapter.normalize(&invocation.stdout, &invocation.stderr, invocation.exit_code);
            collect_policy_findings(
                &events,
                tool_name,
                relative_path,
                handling,
                &infra.policies,
                findings,
            );
        }
        Err(error) => push_diagnostic_dispatch_failure(
            tool_name,
            relative_path,
            handling,
            &format!("subprocess invocation failed: {error}"),
            findings,
        ),
    }
}

pub(super) fn collect_policy_findings(
    events: &[DiagnosticEvent],
    adapter_name: &str,
    modified_file: &Path,
    handling: Handling,
    policies: &PolicyRegistry,
    findings: &mut Findings<'_>,
) {
    for event in events {
        if event.file != modified_file {
            continue;
        }
        let verdict = policies.evaluate_all(event);
        let Some(message) = format_verdict_message(event, adapter_name, &verdict) else {
            continue;
        };

        match handling {
            Handling::Advise => findings.advisories.push(Advisory {
                severity: AdvisorySeverity::Warning,
                message,
                source: adapter_name.to_owned(),
            }),
            Handling::Block => findings.errors.push(message),
        }
    }
}

fn push_diagnostic_dispatch_failure(
    tool_name: &str,
    relative_path: &Path,
    handling: Handling,
    reason: &str,
    findings: &mut Findings<'_>,
) {
    let message = format!(
        "{} [diagnostic:{tool_name}] cannot run activated diagnostic tool: {reason}",
        relative_path.display()
    );
    tracing::warn!(tool = tool_name, path = %relative_path.display(), reason, "activated diagnostic tool could not be dispatched");
    match handling {
        Handling::Advise => findings.advisories.push(Advisory {
            severity: AdvisorySeverity::Warning,
            source: tool_name.to_owned(),
            message,
        }),
        Handling::Block => findings.errors.push(message),
    }
}

fn parse_env(entries: &[String]) -> Vec<(String, String)> {
    entries
        .iter()
        .filter_map(|entry| {
            let Some((key, value)) = entry.split_once('=') else {
                tracing::warn!(
                    env = entry,
                    "diagnostic tool env entry is not KEY=VALUE; skipping"
                );
                return None;
            };
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn expand_template(
    template: &str,
    file: &Path,
    workspace_root: &Path,
    crate_name: Option<&str>,
) -> String {
    let mut result = template.replace("{file}", &file.display().to_string());
    result = result.replace("{project_root}", &workspace_root.display().to_string());
    if let Some(name) = crate_name {
        result = result.replace("{crate}", name);
    }
    result
}

/// Renders a policy verdict as the multi-line message presented to the model.
///
/// Returns `None` for [`PolicyVerdict::Pass`] so callers know to skip the
/// event entirely. Shared between the inline adapter dispatch path and the
/// LD-003 server-query fast path so both surface identical text.
pub(super) fn format_verdict_message(
    event: &DiagnosticEvent,
    adapter_name: &str,
    verdict: &PolicyVerdict,
) -> Option<String> {
    match verdict {
        PolicyVerdict::Report { guidance, tier } => {
            let do_not_text = if guidance.do_not.is_empty() {
                String::new()
            } else {
                format!("\n  DO NOT: {}", guidance.do_not.join("; "))
            };
            Some(format!(
                "{}:{} [{severity}] [{code}] {headline}\n  WHY: {why}\n  FIX: {fix}{do_not}",
                event.file.display(),
                event.line,
                severity = format!("{tier:?}").to_lowercase(),
                code = event.code.as_deref().unwrap_or(adapter_name),
                headline = guidance.headline,
                why = guidance.why,
                fix = guidance.fix,
                do_not = do_not_text,
            ))
        }
        PolicyVerdict::AutoFix { description, .. } => Some(format!(
            "{}:{} [autofix] {}: {}",
            event.file.display(),
            event.line,
            event.code.as_deref().unwrap_or(adapter_name),
            description,
        )),
        PolicyVerdict::Pass => None,
    }
}
