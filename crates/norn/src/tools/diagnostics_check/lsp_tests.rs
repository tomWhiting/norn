//! Convention-driven LSP test execution.
//!
//! Walks the new-format `[rule]` sections in `CONVENTIONS.toml`, filters
//! by tool name + path, and — for rules whose `lsp.tests.on` includes the
//! current trigger — discovers and runs tests through the wired
//! [`crate::tools::lsp::LspBackend`] (LD-011). Failures are normalised
//! into [`DiagnosticEvent`]s and routed through the policy registry;
//! successes produce no events.
//!
//! The execution / parsing path lives in [`super::lsp_test_exec`] so
//! this file stays focused on orchestration: scope resolution, rule
//! iteration, backend selection, and policy routing.

use std::path::Path;

use diagnostics::conventions::{CompiledRule, ConventionsConfig, Handling, TestScope, TestTrigger};
use diagnostics::event::DiagnosticEvent;
use diagnostics::policy::PolicyVerdict;

use crate::tool::lifecycle::{Advisory, AdvisorySeverity};
use crate::tools::lsp::{LspBackend, LspBackendError, TestRunnable, TestRunnableKind};

use super::findings::Findings;
use super::infra::DiagnosticInfra;
use super::lsp_test_exec::{LSP_TEST_SOURCE, execute_runnable, handling_for_rule};

/// Resolve the test scope for `rule`, preferring a rule-level override
/// over the language-level default (R2 acceptance: "rule-level scope
/// overrides language-level scope").
///
/// Returns `None` when neither level configures a scope, in which case
/// the caller must NOT run tests for this rule (R2 acceptance: "no
/// scope at either level means no test execution").
#[must_use]
pub(super) fn resolve_test_scope(
    rule: &CompiledRule,
    conventions: &ConventionsConfig,
) -> Option<TestScope> {
    if let Some(scope) = rule
        .rule
        .lsp
        .as_ref()
        .and_then(|lsp| lsp.tests.as_ref())
        .map(|tests| tests.scope)
    {
        return Some(scope);
    }

    let language = rule.language.as_deref()?;
    conventions
        .lang_def(language)?
        .lsp
        .as_ref()?
        .tests
        .as_ref()
        .map(|tests| tests.scope)
}

/// Walk all new-format rules in `conventions` and, for each rule whose
/// trigger set includes `trigger` and whose matcher accepts
/// `relative_path` for `tool_name`, discover and execute LSP-driven
/// tests.
///
/// Silently no-ops when `infra.lsp_backend` is `None` (CO5 — graceful
/// degradation when LSP is not wired).
pub(super) async fn run_lsp_tests_for_matched_rules(
    file_path: &Path,
    relative_path: &Path,
    tool_name: &str,
    trigger: TestTrigger,
    conventions: &ConventionsConfig,
    infra: &DiagnosticInfra,
    findings: &mut Findings<'_>,
) {
    let Some(backend) = infra.lsp_backend.as_ref() else {
        return;
    };

    // Loop-invariant context for the per-rule runs, bundled so the
    // per-rule helper stays inside the `too_many_arguments` budget
    // without a lint bypass.
    let run_scope = LspTestRunScope {
        file_path,
        relative_path,
        backend: backend.as_ref(),
        infra,
    };

    for (rule_name, compiled) in conventions.rules() {
        if !compiled.triggers.contains(&trigger) {
            continue;
        }
        if !compiled.rule.tools.iter().any(|tool| tool == tool_name) {
            continue;
        }
        if !compiled.matcher.is_match(relative_path) {
            continue;
        }

        let Some(scope) = resolve_test_scope(compiled, conventions) else {
            continue;
        };

        run_lsp_tests_for_rule(&run_scope, rule_name, compiled, scope, findings).await;
    }
}

/// Loop-invariant context shared by every per-rule LSP test run within
/// one [`run_lsp_tests_for_matched_rules`] invocation: the file under
/// test, its workspace-relative path, the wired backend, and the
/// diagnostic infrastructure.
struct LspTestRunScope<'a> {
    file_path: &'a Path,
    relative_path: &'a Path,
    backend: &'a dyn LspBackend,
    infra: &'a DiagnosticInfra,
}

async fn run_lsp_tests_for_rule(
    run_scope: &LspTestRunScope<'_>,
    rule_name: &str,
    compiled: &CompiledRule,
    scope: TestScope,
    findings: &mut Findings<'_>,
) {
    let runnables = match discover_runnables(run_scope.backend, run_scope.file_path, scope).await {
        Ok(runnables) => runnables,
        Err(LspBackendError::NoServerForFile { .. }) => return,
        Err(error) => {
            tracing::warn!(
                rule = rule_name,
                path = %run_scope.file_path.display(),
                error = %error,
                "LSP test discovery failed; skipping"
            );
            return;
        }
    };

    if runnables.is_empty() {
        return;
    }

    let handling = handling_for_rule(compiled);
    for runnable in runnables {
        let events = match execute_runnable(
            &runnable,
            &run_scope.infra.workspace_root,
            run_scope.file_path,
        )
        .await
        {
            Ok(events) => events,
            Err(error) => {
                tracing::warn!(
                    rule = rule_name,
                    test = %runnable.label,
                    error = %error,
                    "LSP test execution failed; skipping runnable"
                );
                continue;
            }
        };

        route_events(
            &events,
            rule_name,
            run_scope.relative_path,
            handling,
            run_scope.infra,
            findings,
        );
    }
}

/// Branch on `scope` to choose the right `LspBackend` query.
///
/// `File` and `Module` query `test_runnables(path)` because
/// rust-analyzer reports module-level runnables on the same list as
/// file-level runnables. `Package` queries `test_runnables(path)` and
/// filters to [`TestRunnableKind::TestModule`] entries (package-level
/// runners). `Affected` queries `related_tests` at the file origin
/// (line 0, column 0). Backends without support degrade silently via
/// the default-impl empty `Vec` (CO5 / C77).
async fn discover_runnables(
    backend: &dyn LspBackend,
    file_path: &Path,
    scope: TestScope,
) -> Result<Vec<TestRunnable>, LspBackendError> {
    match scope {
        TestScope::File | TestScope::Module => backend.test_runnables(file_path).await,
        TestScope::Package => {
            let runnables = backend.test_runnables(file_path).await?;
            Ok(runnables
                .into_iter()
                .filter(|runnable| matches!(runnable.kind, TestRunnableKind::TestModule))
                .collect())
        }
        TestScope::Affected => backend.related_tests(file_path, 0, 0).await,
    }
}

/// Route a rule's test events through the policy registry and append
/// the formatted messages to `findings` according to the rule's
/// diagnostic handling.
fn route_events(
    events: &[DiagnosticEvent],
    rule_name: &str,
    relative_path: &Path,
    handling: Handling,
    infra: &DiagnosticInfra,
    findings: &mut Findings<'_>,
) {
    for event in events {
        let verdict = infra.policies.evaluate_all(event);
        let message = match verdict {
            PolicyVerdict::Report { guidance, tier } => {
                format_report(event, rule_name, relative_path, tier, &guidance)
            }
            PolicyVerdict::AutoFix { description, .. } => format!(
                "{}:{} [autofix] {}: {}",
                event.file.display(),
                event.line,
                event.code.as_deref().unwrap_or(LSP_TEST_SOURCE),
                description,
            ),
            PolicyVerdict::Pass => continue,
        };

        match handling {
            Handling::Advise => findings.advisories.push(Advisory {
                severity: AdvisorySeverity::Warning,
                message,
                source: LSP_TEST_SOURCE.to_owned(),
            }),
            Handling::Block => findings.errors.push(message),
        }
    }
}

fn format_report(
    event: &DiagnosticEvent,
    rule_name: &str,
    relative_path: &Path,
    tier: diagnostics::policy::Tier,
    guidance: &diagnostics::policy::Guidance,
) -> String {
    let do_not_text = if guidance.do_not.is_empty() {
        String::new()
    } else {
        format!("\n  DO NOT: {}", guidance.do_not.join("; "))
    };
    let display_file = if event.file.as_os_str().is_empty() {
        relative_path.display().to_string()
    } else {
        event.file.display().to_string()
    };
    format!(
        "{display_file}:{line} [{severity}] [{code}] {headline}\n  WHY: {why}\n  FIX: {fix}{do_not}",
        line = event.line,
        severity = format!("{tier:?}").to_lowercase(),
        code = event.code.as_deref().unwrap_or(rule_name),
        headline = guidance.headline,
        why = guidance.why,
        fix = guidance.fix,
        do_not = do_not_text,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::io::Write;

    fn load_config(toml_src: &str) -> ConventionsConfig {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("CONVENTIONS.toml");
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(toml_src.as_bytes()))
            .expect("write");
        ConventionsConfig::load(&path).expect("load")
    }

    #[test]
    fn resolve_scope_prefers_rule_override_over_language_default() {
        let config = load_config(
            r#"
[rust.lsp]
server = "rust-analyzer"
tests = { scope = "package" }

[rust-general]
tools = ["edit"]
paths = ["**/*.rs"]
lsp.tests = { on = "tool", scope = "affected" }
"#,
        );
        let rule = config.rule("rust-general").expect("rule");

        let scope = resolve_test_scope(rule, &config).expect("scope");
        assert_eq!(scope, TestScope::Affected);
    }

    #[test]
    fn resolve_scope_falls_back_to_language_default() {
        let config = load_config(
            r#"
[rust.lsp]
server = "rust-analyzer"
tests = { scope = "package" }

[rust-general]
tools = ["edit"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "block" }
"#,
        );
        let rule = config.rule("rust-general").expect("rule");

        let scope = resolve_test_scope(rule, &config).expect("scope");
        assert_eq!(scope, TestScope::Package);
    }

    #[test]
    fn resolve_scope_returns_none_when_neither_level_specifies() {
        let config = load_config(
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["edit"]
paths = ["**/*.rs"]
lsp.diagnostics = { handling = "block" }
"#,
        );
        let rule = config.rule("rust-general").expect("rule");

        assert!(resolve_test_scope(rule, &config).is_none());
    }

    #[test]
    fn compiled_triggers_carry_full_set_for_pipe_separated_string() {
        let config = load_config(
            r#"
[rust.lsp]
server = "rust-analyzer"

[rust-general]
tools = ["edit"]
paths = ["**/*.rs"]
lsp.tests = { on = "tool|task_complete", scope = "affected" }
"#,
        );
        let rule = config.rule("rust-general").expect("rule");

        let mut expected = HashSet::new();
        expected.insert(TestTrigger::Tool);
        expected.insert(TestTrigger::TaskComplete);
        assert_eq!(rule.triggers, expected);
    }
}
