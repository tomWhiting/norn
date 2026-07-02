//! Trigger evaluation: match rules against runtime events.
//!
//! Glob patterns are validated at parse time
//! ([`crate::rules::parser::parse_rule_file`]) and compiled exactly once
//! here — a process-wide cache keyed by the pattern string holds each
//! compiled [`Pattern`], so per-event evaluation never re-compiles. A
//! pattern that reaches evaluation without compiling (only possible for
//! programmatically constructed rules that bypassed the parser) is
//! logged at `error` level once and cached as a permanent non-match —
//! never silently indistinguishable from "no match".

use std::collections::HashMap;
use std::sync::OnceLock;

use glob::Pattern;
use parking_lot::Mutex;

use crate::rules::types::{Rule, RuntimeEvent, TriggerCondition, TriggerMatch};

/// Evaluate a rule's trigger conditions against a runtime event.
///
/// Returns a [`TriggerMatch`] if any of the rule's triggers match the event,
/// or `None` if no triggers match.
#[must_use]
pub fn evaluate_triggers(rule: &Rule, event: &RuntimeEvent) -> Option<TriggerMatch> {
    let matched = rule
        .triggers
        .iter()
        .any(|trigger| matches_event(trigger, event));

    if matched {
        Some(TriggerMatch {
            rule_id: rule.id.clone(),
            timing: rule.timing.clone(),
            delivery: rule.delivery.clone(),
        })
    } else {
        None
    }
}

fn matches_event(trigger: &TriggerCondition, event: &RuntimeEvent) -> bool {
    match (trigger, event) {
        (TriggerCondition::PathGlob { pattern }, RuntimeEvent::PathChanged { path, .. }) => {
            matches_glob(pattern, path)
        }

        (TriggerCondition::BashCommand { pattern }, RuntimeEvent::BashCommandRun { command }) => {
            command.contains(pattern.as_str())
        }

        (
            TriggerCondition::ToolInvocation { tool_name },
            RuntimeEvent::ToolInvoked {
                tool_name: invoked, ..
            },
        ) => tool_name == invoked,

        _ => false,
    }
}

/// Process-wide compiled-glob cache. `None` marks a pattern that failed
/// to compile (already reported), so the failure is logged exactly once
/// and never re-attempted per event. Bounded in practice by the number
/// of distinct rule patterns loaded into engines.
static COMPILED_GLOBS: OnceLock<Mutex<HashMap<String, Option<Pattern>>>> = OnceLock::new();

fn matches_glob(pattern: &str, path: &str) -> bool {
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };
    let cache = COMPILED_GLOBS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock();
    let compiled = guard.entry(pattern.to_owned()).or_insert_with(|| {
        match Pattern::new(pattern) {
            Ok(p) => Some(p),
            Err(e) => {
                // Parse-time validation makes this unreachable for rules
                // loaded from disk; a programmatically constructed rule
                // that bypassed the parser is reported loudly instead of
                // masquerading as "no match".
                tracing::error!(
                    pattern = pattern,
                    error = %e,
                    "rule glob pattern failed to compile; the trigger can never match",
                );
                None
            }
        }
    });
    compiled
        .as_ref()
        .is_some_and(|p| p.matches_with(path, opts))
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
    use crate::rules::types::{DeliveryMode, PathOperation, RuleId, TriggerTiming};

    fn make_rule(triggers: Vec<TriggerCondition>) -> Rule {
        Rule {
            id: RuleId::from("test-rule"),
            name: "Test".to_owned(),
            triggers,
            delivery: DeliveryMode::ContextInjection,
            timing: TriggerTiming::Before,
            body: "test body".to_owned(),
            shell_source: None,
        }
    }

    // -- PathGlob tests ---------------------------------------------------

    #[test]
    fn path_glob_matches_rs_files() {
        let rule = make_rule(vec![TriggerCondition::PathGlob {
            pattern: "**/*.rs".to_owned(),
        }]);
        let event = RuntimeEvent::PathChanged {
            path: "src/main.rs".to_owned(),
            operation: PathOperation::Read,
        };
        assert!(evaluate_triggers(&rule, &event).is_some());
    }

    #[test]
    fn path_glob_mod_rs_does_not_match_main_rs() {
        let rule = make_rule(vec![TriggerCondition::PathGlob {
            pattern: "**/mod.rs".to_owned(),
        }]);
        let event = RuntimeEvent::PathChanged {
            path: "src/main.rs".to_owned(),
            operation: PathOperation::Read,
        };
        assert!(evaluate_triggers(&rule, &event).is_none());
    }

    #[test]
    fn path_glob_mod_rs_matches_mod_rs() {
        let rule = make_rule(vec![TriggerCondition::PathGlob {
            pattern: "**/mod.rs".to_owned(),
        }]);
        let event = RuntimeEvent::PathChanged {
            path: "src/rules/mod.rs".to_owned(),
            operation: PathOperation::Write,
        };
        assert!(evaluate_triggers(&rule, &event).is_some());
    }

    // -- BashCommand tests ------------------------------------------------

    #[test]
    fn bash_command_substring_match() {
        let rule = make_rule(vec![TriggerCondition::BashCommand {
            pattern: "cargo test".to_owned(),
        }]);
        let event = RuntimeEvent::BashCommandRun {
            command: "cargo test --workspace".to_owned(),
        };
        assert!(evaluate_triggers(&rule, &event).is_some());
    }

    #[test]
    fn bash_command_no_match() {
        let rule = make_rule(vec![TriggerCondition::BashCommand {
            pattern: "cargo test".to_owned(),
        }]);
        let event = RuntimeEvent::BashCommandRun {
            command: "cargo build".to_owned(),
        };
        assert!(evaluate_triggers(&rule, &event).is_none());
    }

    // -- ToolInvocation tests ---------------------------------------------

    #[test]
    fn tool_invocation_exact_match() {
        let rule = make_rule(vec![TriggerCondition::ToolInvocation {
            tool_name: "Write".to_owned(),
        }]);
        let event = RuntimeEvent::ToolInvoked {
            tool_name: "Write".to_owned(),
            arguments: None,
        };
        assert!(evaluate_triggers(&rule, &event).is_some());
    }

    #[test]
    fn tool_invocation_no_match() {
        let rule = make_rule(vec![TriggerCondition::ToolInvocation {
            tool_name: "Write".to_owned(),
        }]);
        let event = RuntimeEvent::ToolInvoked {
            tool_name: "Read".to_owned(),
            arguments: None,
        };
        assert!(evaluate_triggers(&rule, &event).is_none());
    }

    // -- Cross-type non-match tests ---------------------------------------

    #[test]
    fn path_trigger_ignores_bash_event() {
        let rule = make_rule(vec![TriggerCondition::PathGlob {
            pattern: "**/*.rs".to_owned(),
        }]);
        let event = RuntimeEvent::BashCommandRun {
            command: "echo hello".to_owned(),
        };
        assert!(evaluate_triggers(&rule, &event).is_none());
    }

    #[test]
    fn tool_trigger_ignores_path_event() {
        let rule = make_rule(vec![TriggerCondition::ToolInvocation {
            tool_name: "Write".to_owned(),
        }]);
        let event = RuntimeEvent::PathChanged {
            path: "foo.rs".to_owned(),
            operation: PathOperation::Write,
        };
        assert!(evaluate_triggers(&rule, &event).is_none());
    }

    // -- Multiple triggers (any-match) ------------------------------------

    #[test]
    fn multiple_triggers_any_match() {
        let rule = make_rule(vec![
            TriggerCondition::PathGlob {
                pattern: "**/*.ts".to_owned(),
            },
            TriggerCondition::ToolInvocation {
                tool_name: "Write".to_owned(),
            },
        ]);

        let path_event = RuntimeEvent::PathChanged {
            path: "src/app.ts".to_owned(),
            operation: PathOperation::Read,
        };
        assert!(evaluate_triggers(&rule, &path_event).is_some());

        let tool_event = RuntimeEvent::ToolInvoked {
            tool_name: "Write".to_owned(),
            arguments: None,
        };
        assert!(evaluate_triggers(&rule, &tool_event).is_some());

        let no_match = RuntimeEvent::BashCommandRun {
            command: "ls".to_owned(),
        };
        assert!(evaluate_triggers(&rule, &no_match).is_none());
    }

    // -- TriggerMatch carries correct metadata ----------------------------

    #[test]
    fn trigger_match_carries_rule_metadata() {
        let rule = Rule {
            id: RuleId::from("my-rule"),
            name: "My Rule".to_owned(),
            triggers: vec![TriggerCondition::PathGlob {
                pattern: "**/*.rs".to_owned(),
            }],
            delivery: DeliveryMode::SystemContextAppend,
            timing: TriggerTiming::After,
            body: "body".to_owned(),
            shell_source: None,
        };
        let event = RuntimeEvent::PathChanged {
            path: "lib.rs".to_owned(),
            operation: PathOperation::Read,
        };
        let m = evaluate_triggers(&rule, &event).expect("should match");
        assert_eq!(m.rule_id.as_str(), "my-rule");
        assert_eq!(m.timing, TriggerTiming::After);
        assert_eq!(m.delivery, DeliveryMode::SystemContextAppend);
    }
}
