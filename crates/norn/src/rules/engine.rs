//! Rule engine: evaluates rules against runtime events, produces injections.

use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;

use crate::integration::diagnostics::{DiagnosticCollector, DiagnosticSeverity, NornDiagnostic};
use crate::rules::lifecycle::RulePresenceSet;
use crate::rules::triggers::evaluate_triggers;
use crate::rules::types::{Rule, RuleInjection, RuntimeEvent};

/// Default wall-clock budget for `shell_source` rule commands.
const DEFAULT_SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// The rules engine: holds loaded rules and the presence set, evaluates
/// triggers against runtime events, and produces [`RuleInjection`] values
/// for rules that matched and are not already in context.
pub struct RuleEngine {
    rules: Vec<Rule>,
    presence: RulePresenceSet,
    shell_timeout: Duration,
    diagnostics: Option<Arc<DiagnosticCollector>>,
    working_dir: Option<crate::tool::context::SharedWorkingDir>,
}

impl RuleEngine {
    /// Create a new engine with the given rules.
    #[must_use]
    pub fn new(rules: Vec<Rule>) -> Self {
        Self {
            rules,
            presence: RulePresenceSet::new(),
            shell_timeout: DEFAULT_SHELL_TIMEOUT,
            diagnostics: None,
            working_dir: None,
        }
    }

    /// Override the wall-clock budget for `shell_source` rule commands.
    #[must_use]
    pub fn with_shell_timeout(mut self, timeout: Duration) -> Self {
        self.shell_timeout = timeout;
        self
    }

    /// Attach a shared diagnostic collector. Engines without a collector
    /// fall back silently on shell errors; engines with one record a
    /// `rule-shell-failure` diagnostic before falling back.
    #[must_use]
    pub fn with_diagnostics(mut self, collector: Arc<DiagnosticCollector>) -> Self {
        self.diagnostics = Some(collector);
        self
    }

    /// Install the agent's shared working directory. When set, each
    /// `shell_source` command spawned by `Self::resolve_shell_content`
    /// runs with the agent's CWD as `.current_dir`. When unset, the child
    /// inherits the process CWD (legacy behaviour for engines constructed
    /// outside an orchestrator).
    #[must_use]
    pub fn with_working_dir(mut self, working_dir: crate::tool::context::SharedWorkingDir) -> Self {
        self.working_dir = Some(working_dir);
        self
    }

    /// Add a rule to the engine.
    pub fn add_rule(&mut self, rule: Rule) {
        self.rules.push(rule);
    }

    /// Return a mutable reference to the presence set for rebuilding.
    pub fn presence_mut(&mut self) -> &mut RulePresenceSet {
        &mut self.presence
    }

    /// Evaluate all rules against a runtime event.
    ///
    /// Returns injections only for rules that matched AND are not already
    /// present in the active context (checked via the presence set). For
    /// rules with `shell_source` set, executes the command (subject to
    /// `Self::shell_timeout`) and substitutes its trimmed stdout for the
    /// rule body. Timeouts and non-zero exits fall back to `rule.body` and
    /// record a `rule-shell-failure` diagnostic (when a collector is
    /// attached).
    pub async fn process_event(&self, event: &RuntimeEvent) -> Vec<RuleInjection> {
        let mut injections = Vec::new();

        for rule in &self.rules {
            if self.presence.is_present(&rule.id) {
                continue;
            }

            let Some(trigger_match) = evaluate_triggers(rule, event) else {
                continue;
            };

            let content = match rule.shell_source.as_deref() {
                None => rule.body.clone(),
                Some(cmd) => self.resolve_shell_content(rule, cmd).await,
            };

            injections.push(RuleInjection {
                rule_id: trigger_match.rule_id,
                delivery: trigger_match.delivery,
                timing: trigger_match.timing,
                content,
            });
        }

        injections
    }

    async fn resolve_shell_content(&self, rule: &Rule, cmd: &str) -> String {
        let mut command = Command::new("sh");
        command.arg("-c").arg(cmd);
        if let Some(ref wd) = self.working_dir {
            command.current_dir(wd.get());
        }
        let result = tokio::time::timeout(self.shell_timeout, command.output()).await;

        match result {
            Ok(Ok(output)) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                stdout
                    .trim_end_matches('\n')
                    .trim_end_matches('\r')
                    .to_owned()
            }
            Ok(Ok(output)) => {
                let exit = output
                    .status
                    .code()
                    .map_or("signal".to_owned(), |c| c.to_string());
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                self.record_failure(rule, &format!("non-zero exit ({exit}) — stderr: {stderr}"));
                rule.body.clone()
            }
            Ok(Err(io_err)) => {
                self.record_failure(rule, &format!("spawn failed: {io_err}"));
                rule.body.clone()
            }
            Err(_) => {
                self.record_failure(
                    rule,
                    &format!(
                        "timed out after {}ms",
                        u128::min(u128::from(u64::MAX), self.shell_timeout.as_millis())
                    ),
                );
                rule.body.clone()
            }
        }
    }

    fn record_failure(&self, rule: &Rule, detail: &str) {
        let message = format!("rule '{}' shell source failed: {detail}", rule.id);
        tracing::warn!(rule_id = %rule.id, "{message}");
        if let Some(collector) = self.diagnostics.as_ref() {
            collector.report(NornDiagnostic {
                severity: DiagnosticSeverity::Warning,
                code: "rule-shell-failure".to_owned(),
                message,
                source_tool: None,
                file_path: None,
                suggestion: Some(
                    "verify the rule's shell_source command and runtime environment".to_owned(),
                ),
            });
        }
    }
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
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use crate::r#loop::context::ContentTag;
    use crate::rules::types::{
        DeliveryMode, PathOperation, RuleId, TriggerCondition, TriggerTiming,
    };

    fn rust_conventions_rule() -> Rule {
        Rule {
            id: RuleId::from("rust-conventions"),
            name: "Rust Conventions".to_owned(),
            triggers: vec![TriggerCondition::PathGlob {
                pattern: "**/*.rs".to_owned(),
            }],
            delivery: DeliveryMode::SystemContextAppend,
            timing: TriggerTiming::Before,
            body: "Follow Rust conventions.".to_owned(),
            shell_source: None,
        }
    }

    fn cargo_test_rule() -> Rule {
        Rule {
            id: RuleId::from("cargo-test-hint"),
            name: "Cargo Test Hint".to_owned(),
            triggers: vec![TriggerCondition::BashCommand {
                pattern: "cargo test".to_owned(),
            }],
            delivery: DeliveryMode::ContextInjection,
            timing: TriggerTiming::Before,
            body: "Consider using yg diagnostics.".to_owned(),
            shell_source: None,
        }
    }

    #[tokio::test]
    async fn no_match_returns_empty() {
        let engine = RuleEngine::new(vec![rust_conventions_rule()]);
        let event = RuntimeEvent::BashCommandRun {
            command: "ls".to_owned(),
        };
        assert!(engine.process_event(&event).await.is_empty());
    }

    #[tokio::test]
    async fn matching_trigger_returns_injection() {
        let engine = RuleEngine::new(vec![rust_conventions_rule()]);
        let event = RuntimeEvent::PathChanged {
            path: "src/lib.rs".to_owned(),
            operation: PathOperation::Read,
        };
        let injections = engine.process_event(&event).await;
        assert_eq!(injections.len(), 1);
        assert_eq!(injections[0].rule_id.as_str(), "rust-conventions");
        assert_eq!(injections[0].delivery, DeliveryMode::SystemContextAppend);
        assert_eq!(injections[0].timing, TriggerTiming::Before);
        assert_eq!(injections[0].content, "Follow Rust conventions.");
    }

    #[tokio::test]
    async fn already_present_suppresses_injection() {
        let mut engine = RuleEngine::new(vec![rust_conventions_rule()]);

        let event = RuntimeEvent::PathChanged {
            path: "src/lib.rs".to_owned(),
            operation: PathOperation::Read,
        };

        let first = engine.process_event(&event).await;
        assert_eq!(first.len(), 1);

        engine
            .presence_mut()
            .rebuild(&[ContentTag::Rule("rust-conventions".to_owned())]);

        let second = engine.process_event(&event).await;
        assert!(second.is_empty(), "should suppress when rule is in context");
    }

    #[tokio::test]
    async fn re_inject_after_removal_from_context() {
        let mut engine = RuleEngine::new(vec![rust_conventions_rule()]);
        let event = RuntimeEvent::PathChanged {
            path: "src/lib.rs".to_owned(),
            operation: PathOperation::Read,
        };

        let first = engine.process_event(&event).await;
        assert_eq!(first.len(), 1);

        engine
            .presence_mut()
            .rebuild(&[ContentTag::Rule("rust-conventions".to_owned())]);
        assert!(engine.process_event(&event).await.is_empty());

        engine.presence_mut().rebuild(&[ContentTag::Message]);
        let third = engine.process_event(&event).await;
        assert_eq!(third.len(), 1, "should re-inject after rule leaves context");
    }

    #[tokio::test]
    async fn multiple_rules_independent() {
        let engine = RuleEngine::new(vec![rust_conventions_rule(), cargo_test_rule()]);

        let rs_event = RuntimeEvent::PathChanged {
            path: "src/main.rs".to_owned(),
            operation: PathOperation::Read,
        };
        let injections = engine.process_event(&rs_event).await;
        assert_eq!(injections.len(), 1);
        assert_eq!(injections[0].rule_id.as_str(), "rust-conventions");

        let bash_event = RuntimeEvent::BashCommandRun {
            command: "cargo test --workspace".to_owned(),
        };
        let injections = engine.process_event(&bash_event).await;
        assert_eq!(injections.len(), 1);
        assert_eq!(injections[0].rule_id.as_str(), "cargo-test-hint");
    }

    #[tokio::test]
    async fn add_rule_after_construction() {
        let mut engine = RuleEngine::new(vec![]);
        assert!(
            engine
                .process_event(&RuntimeEvent::PathChanged {
                    path: "a.rs".to_owned(),
                    operation: PathOperation::Read,
                })
                .await
                .is_empty()
        );

        engine.add_rule(rust_conventions_rule());
        let injections = engine
            .process_event(&RuntimeEvent::PathChanged {
                path: "a.rs".to_owned(),
                operation: PathOperation::Read,
            })
            .await;
        assert_eq!(injections.len(), 1);
    }

    // -- R10 acceptance: shell_source -----------------------------------------

    fn shell_rule(id: &str, cmd: &str, body: &str) -> Rule {
        Rule {
            id: RuleId::from(id),
            name: "Shell".to_owned(),
            triggers: vec![TriggerCondition::PathGlob {
                pattern: "**/*.rs".to_owned(),
            }],
            delivery: DeliveryMode::ContextInjection,
            timing: TriggerTiming::Before,
            body: body.to_owned(),
            shell_source: Some(cmd.to_owned()),
        }
    }

    fn rs_event() -> RuntimeEvent {
        RuntimeEvent::PathChanged {
            path: "src/lib.rs".to_owned(),
            operation: PathOperation::Read,
        }
    }

    #[tokio::test]
    async fn shell_source_replaces_body_on_success() {
        let engine = RuleEngine::new(vec![shell_rule("hello", "echo hello", "FALLBACK")]);
        let injections = engine.process_event(&rs_event()).await;
        assert_eq!(injections.len(), 1);
        assert_eq!(injections[0].content, "hello");
    }

    #[tokio::test]
    async fn shell_source_timeout_falls_back_and_emits_diagnostic() {
        let collector = DiagnosticCollector::shared();
        let engine = RuleEngine::new(vec![shell_rule("slow", "sleep 10", "FALLBACK")])
            .with_shell_timeout(Duration::from_millis(100))
            .with_diagnostics(Arc::clone(&collector));

        let injections = engine.process_event(&rs_event()).await;
        assert_eq!(injections.len(), 1);
        assert_eq!(injections[0].content, "FALLBACK");

        let diags = collector.drain();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "rule-shell-failure");
        assert_eq!(diags[0].severity, DiagnosticSeverity::Warning);
    }

    #[tokio::test]
    async fn shell_source_nonzero_exit_falls_back_and_emits_diagnostic() {
        let collector = DiagnosticCollector::shared();
        let engine = RuleEngine::new(vec![shell_rule("fail", "exit 1", "FALLBACK")])
            .with_diagnostics(Arc::clone(&collector));

        let injections = engine.process_event(&rs_event()).await;
        assert_eq!(injections.len(), 1);
        assert_eq!(injections[0].content, "FALLBACK");

        let diags = collector.drain();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "rule-shell-failure");
    }

    #[tokio::test]
    async fn shell_source_without_collector_still_falls_back() {
        let engine = RuleEngine::new(vec![shell_rule("fail", "exit 1", "FALLBACK")]);
        let injections = engine.process_event(&rs_event()).await;
        assert_eq!(injections.len(), 1);
        assert_eq!(injections[0].content, "FALLBACK");
    }
}
