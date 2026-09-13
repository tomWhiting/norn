//! Restricted workspace checks retain their declared scope and handling without process authority.

use std::path::Path;

use diagnostics::conventions::{ConventionsConfig, Handling, TestTrigger, ToolRef};
use globset::GlobSet;

use crate::tool::lifecycle::{Advisory, AdvisorySeverity};

use super::findings::Findings;
use super::lsp_test_exec::handling_for_rule;

/// Check declarations that cannot execute from a workspace-owned configuration.
/// Contains matchers and reporting metadata only, never executable definitions.
#[derive(Default)]
pub struct UnavailableChecks {
    rules: Vec<UnavailableRule>,
}

struct UnavailableRule {
    name: String,
    matcher: GlobSet,
    tools: Vec<String>,
    checks: Vec<UnavailableCheck>,
}

struct UnavailableCheck {
    name: String,
    triggers: Vec<TestTrigger>,
    handling: Handling,
}

impl UnavailableChecks {
    pub(crate) fn from_declared(config: &ConventionsConfig) -> Self {
        let mut rules = Vec::new();
        for (name, compiled) in config.rules() {
            let mut checks = Vec::new();
            if let Some(language) = compiled.language.as_deref() {
                for (tool, activation) in &compiled.rule.activations {
                    if matches!(config.lookup_tool(language, tool), Some(def) if !matches!(def, ToolRef::Pattern(_)))
                    {
                        checks.push(UnavailableCheck {
                            name: format!("{language}.{tool}"),
                            triggers: activation.on.clone(),
                            handling: activation.handling,
                        });
                    }
                }
            }
            if let Some(lsp) = &compiled.rule.lsp {
                if let Some(diagnostics) = &lsp.diagnostics {
                    checks.push(UnavailableCheck {
                        name: "lsp.diagnostics".to_owned(),
                        triggers: vec![TestTrigger::Tool],
                        handling: diagnostics.handling,
                    });
                }
                if let Some(tests) = &lsp.tests {
                    checks.push(UnavailableCheck {
                        name: "lsp.tests".to_owned(),
                        triggers: tests.on.clone(),
                        handling: handling_for_rule(compiled),
                    });
                }
            }
            if !checks.is_empty() {
                rules.push(UnavailableRule {
                    name: name.clone(),
                    matcher: compiled.matcher.clone(),
                    tools: compiled.rule.tools.clone(),
                    checks,
                });
            }
        }
        Self { rules }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub(super) fn report(
        &self,
        relative_path: &Path,
        tool_name: Option<&str>,
        trigger: TestTrigger,
        findings: &mut Findings<'_>,
    ) {
        for rule in &self.rules {
            if !rule.matcher.is_match(relative_path)
                || tool_name.is_some_and(|tool| !rule.tools.iter().any(|name| name == tool))
            {
                continue;
            }
            for check in &rule.checks {
                if !check.triggers.contains(&trigger) {
                    continue;
                }
                let message = format!(
                    "{} [rule:{}] check `{}` was not run: workspace CONVENTIONS.toml cannot authorize subprocess or LSP execution; run this check through a trusted runtime or repository gate",
                    relative_path.display(),
                    rule.name,
                    check.name,
                );
                match check.handling {
                    Handling::Block => findings.errors.push(message),
                    Handling::Advise => findings.advisories.push(Advisory {
                        severity: AdvisorySeverity::Warning,
                        source: "conventions.unavailable".to_owned(),
                        message,
                    }),
                }
            }
        }
    }
}
