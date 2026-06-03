//! Rule file parsing from YAML front matter.
//!
//! Delegates the frontmatter split to
//! [`crate::util::frontmatter::split_frontmatter`] — using the shared
//! helper means rules now gain CRLF and empty-frontmatter edge-case
//! handling for free (previously this parser only handled LF).
//!
//! Two frontmatter formats are recognised, dispatched by key presence:
//!
//! - **Norn native** — frontmatter contains a `triggers:` key. Parsed via
//!   [`parse_norn_format`]; supports all three trigger types and all three
//!   delivery modes.
//! - **Claude Code compatibility** — frontmatter contains `globs:` and/or
//!   `paths:` (and no `triggers:`). Parsed via [`parse_claude_code_format`];
//!   each glob maps to one [`TriggerCondition::PathGlob`], delivery
//!   defaults to [`DeliveryMode::SystemContextAppend`], timing defaults to
//!   [`TriggerTiming::After`].
//!
//! A file containing both `triggers:` and `globs:`/`paths:` is ambiguous and
//! rejected with [`RulesError::ParseFailed`].

use serde::Deserialize;

use crate::error::RulesError;
use crate::rules::types::{DeliveryMode, Rule, RuleId, TriggerCondition, TriggerTiming};

/// Parse a rule file consisting of YAML front matter delimited by `---`
/// followed by a plain-text body.
///
/// Detects the frontmatter format by examining keys: `triggers:` selects
/// the Norn native parser, `globs:`/`paths:` selects the Claude Code
/// compatibility parser. Presence of both is an ambiguous-format error.
///
/// # Errors
///
/// Returns [`RulesError::ParseFailed`] if the front matter is missing,
/// malformed, ambiguous, or lacks required fields.
pub fn parse_rule_file(id: RuleId, content: &str) -> Result<Rule, RulesError> {
    let (front_matter, body) =
        crate::util::frontmatter::split_frontmatter(content).map_err(|e| {
            RulesError::ParseFailed {
                reason: e.to_string(),
            }
        })?;

    let value: serde_yaml::Value =
        serde_yaml::from_str(front_matter).map_err(|e| RulesError::ParseFailed {
            reason: format!("invalid YAML front matter: {e}"),
        })?;

    let mapping = value.as_mapping().ok_or_else(|| RulesError::ParseFailed {
        reason: "rule frontmatter must be a YAML mapping".to_owned(),
    })?;

    let has_triggers = mapping.contains_key("triggers");
    let has_globs = mapping.contains_key("globs");
    let has_paths = mapping.contains_key("paths");

    if has_triggers && (has_globs || has_paths) {
        return Err(RulesError::ParseFailed {
            reason: "ambiguous rule format: both 'triggers:' and 'globs:'/'paths:' are present"
                .to_owned(),
        });
    }

    if has_triggers {
        parse_norn_format(id, value, body)
    } else if has_globs || has_paths {
        parse_claude_code_format(id, value, body)
    } else {
        Err(RulesError::ParseFailed {
            reason: "rule frontmatter must define either 'triggers:' (Norn) or 'globs:'/'paths:' \
                     (Claude Code)"
                .to_owned(),
        })
    }
}

/// Parse Norn native frontmatter — a `triggers:` list plus explicit
/// `delivery:` and (optional) `timing:` fields.
fn parse_norn_format(id: RuleId, value: serde_yaml::Value, body: &str) -> Result<Rule, RulesError> {
    let raw: RawFrontMatter =
        serde_yaml::from_value(value).map_err(|e| RulesError::ParseFailed {
            reason: format!("invalid YAML front matter: {e}"),
        })?;

    let triggers = raw
        .triggers
        .into_iter()
        .map(convert_trigger)
        .collect::<Result<Vec<_>, _>>()?;

    if triggers.is_empty() {
        return Err(RulesError::ParseFailed {
            reason: "at least one trigger is required".to_owned(),
        });
    }

    let delivery = convert_delivery(&raw.delivery)?;
    let timing = raw
        .timing
        .map(|t| convert_timing(&t))
        .transpose()?
        .unwrap_or(TriggerTiming::Before);

    Ok(Rule {
        id,
        name: raw.name.unwrap_or_default(),
        triggers,
        delivery,
        timing,
        body: body.to_owned(),
        shell_source: raw.shell_source,
    })
}

/// Parse Claude Code compatibility frontmatter — `globs:` (and/or `paths:`)
/// plus optional `description:`. Each glob becomes one
/// [`TriggerCondition::PathGlob`]; delivery and timing are fixed at
/// [`DeliveryMode::SystemContextAppend`] / [`TriggerTiming::After`] per
/// Claude Code's persistence semantics.
fn parse_claude_code_format(
    id: RuleId,
    value: serde_yaml::Value,
    body: &str,
) -> Result<Rule, RulesError> {
    let raw: RawClaudeFrontMatter =
        serde_yaml::from_value(value).map_err(|e| RulesError::ParseFailed {
            reason: format!("invalid YAML front matter: {e}"),
        })?;

    let mut triggers: Vec<TriggerCondition> = Vec::new();
    if let Some(globs) = raw.globs {
        for pattern in globs.into_vec() {
            triggers.push(TriggerCondition::PathGlob { pattern });
        }
    }
    if let Some(paths) = raw.paths {
        for pattern in paths.into_vec() {
            triggers.push(TriggerCondition::PathGlob { pattern });
        }
    }

    if triggers.is_empty() {
        return Err(RulesError::ParseFailed {
            reason: "Claude Code rule requires at least one entry in 'globs:' or 'paths:'"
                .to_owned(),
        });
    }

    Ok(Rule {
        id,
        name: raw.description.unwrap_or_default(),
        triggers,
        delivery: DeliveryMode::SystemContextAppend,
        timing: TriggerTiming::After,
        body: body.to_owned(),
        shell_source: None,
    })
}

// -- Raw deserialization types for YAML front matter ----------------------

#[derive(Deserialize)]
struct RawFrontMatter {
    name: Option<String>,
    triggers: Vec<RawTrigger>,
    delivery: String,
    timing: Option<String>,
    #[serde(default)]
    shell_source: Option<String>,
}

#[derive(Deserialize)]
struct RawTrigger {
    #[serde(rename = "type")]
    trigger_type: String,
    pattern: String,
}

/// Claude Code rule frontmatter — `globs:` and/or `paths:` (string or
/// sequence), optional `description:`. Unknown fields are ignored to stay
/// forward-compatible with future Claude Code metadata.
#[derive(Deserialize)]
struct RawClaudeFrontMatter {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    globs: Option<StringOrArray>,
    #[serde(default)]
    paths: Option<StringOrArray>,
}

/// A YAML scalar that may be either a single string or a sequence of
/// strings. Mirrors the pattern used by `profile/loader.rs::ToolsValue`.
#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrArray {
    Single(String),
    Multiple(Vec<String>),
}

impl StringOrArray {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::Single(s) => vec![s],
            Self::Multiple(items) => items,
        }
    }
}

fn convert_trigger(raw: RawTrigger) -> Result<TriggerCondition, RulesError> {
    match raw.trigger_type.as_str() {
        "path_glob" => Ok(TriggerCondition::PathGlob {
            pattern: raw.pattern,
        }),
        "bash_command" => Ok(TriggerCondition::BashCommand {
            pattern: raw.pattern,
        }),
        "tool" => Ok(TriggerCondition::ToolInvocation {
            tool_name: raw.pattern,
        }),
        other => Err(RulesError::ParseFailed {
            reason: format!("unknown trigger type: {other}"),
        }),
    }
}

fn convert_delivery(s: &str) -> Result<DeliveryMode, RulesError> {
    match s {
        "system_context" => Ok(DeliveryMode::SystemContextAppend),
        "context_injection" => Ok(DeliveryMode::ContextInjection),
        "message" => Ok(DeliveryMode::MessageDelivery),
        other => Err(RulesError::ParseFailed {
            reason: format!("unknown delivery mode: {other}"),
        }),
    }
}

fn convert_timing(s: &str) -> Result<TriggerTiming, RulesError> {
    match s {
        "before" => Ok(TriggerTiming::Before),
        "after" => Ok(TriggerTiming::After),
        other => Err(RulesError::ParseFailed {
            reason: format!("unknown timing: {other}"),
        }),
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

    #[test]
    fn parse_rule_with_two_triggers() {
        let content = r#"---
name: Rust conventions
triggers:
  - type: path_glob
    pattern: "**/*.rs"
  - type: tool
    pattern: Write
delivery: context_injection
timing: after
---
Follow Rust coding conventions.
No unwrap() in library code."#;

        let rule =
            parse_rule_file(RuleId::from("rust-conventions"), content).expect("should parse");

        assert_eq!(rule.id.as_str(), "rust-conventions");
        assert_eq!(rule.name, "Rust conventions");
        assert_eq!(rule.triggers.len(), 2);
        assert_eq!(rule.delivery, DeliveryMode::ContextInjection);
        assert_eq!(rule.timing, TriggerTiming::After);
        assert!(rule.body.contains("Follow Rust coding conventions."));
        assert!(rule.body.contains("No unwrap() in library code."));

        match &rule.triggers[0] {
            TriggerCondition::PathGlob { pattern } => assert_eq!(pattern, "**/*.rs"),
            other => panic!("expected PathGlob, got {other:?}"),
        }
        match &rule.triggers[1] {
            TriggerCondition::ToolInvocation { tool_name } => assert_eq!(tool_name, "Write"),
            other => panic!("expected ToolInvocation, got {other:?}"),
        }
    }

    #[test]
    fn parse_rule_timing_defaults_to_before() {
        let content = r"---
triggers:
  - type: bash_command
    pattern: cargo test
delivery: system_context
---
Use yg diagnostics instead.";

        let rule = parse_rule_file(RuleId::from("test-rule"), content).expect("should parse");
        assert_eq!(rule.timing, TriggerTiming::Before);
        assert_eq!(rule.delivery, DeliveryMode::SystemContextAppend);
    }

    #[test]
    fn parse_rule_message_delivery() {
        let content = r"---
triggers:
  - type: tool
    pattern: Edit
delivery: message
---
Remember to update docs.";

        let rule = parse_rule_file(RuleId::from("edit-reminder"), content).expect("should parse");
        assert_eq!(rule.delivery, DeliveryMode::MessageDelivery);
    }

    #[test]
    fn parse_missing_front_matter_delimiter() {
        let content = "no front matter here";
        let result = parse_rule_file(RuleId::from("bad"), content);
        assert!(result.is_err());
    }

    #[test]
    fn parse_missing_closing_delimiter() {
        let content = "---\ntriggers: []\ndelivery: message\n";
        let result = parse_rule_file(RuleId::from("bad"), content);
        assert!(result.is_err());
    }

    #[test]
    fn parse_empty_triggers_rejected() {
        let content = r"---
triggers: []
delivery: message
---
Body.";

        let result = parse_rule_file(RuleId::from("bad"), content);
        assert!(result.is_err());
    }

    #[test]
    fn parse_unknown_trigger_type_rejected() {
        let content = r"---
triggers:
  - type: unknown_thing
    pattern: foo
delivery: message
---
Body.";

        let result = parse_rule_file(RuleId::from("bad"), content);
        assert!(result.is_err());
    }

    #[test]
    fn parse_unknown_delivery_rejected() {
        let content = r"---
triggers:
  - type: tool
    pattern: Read
delivery: carrier_pigeon
---
Body.";

        let result = parse_rule_file(RuleId::from("bad"), content);
        assert!(result.is_err());
    }

    // -- Claude Code compatibility (R2..R7) -------------------------------

    #[test]
    fn parse_claude_code_single_glob_string() {
        let content = r#"---
description: Rust coding standards
globs: "**/*.rs"
---
Follow Rust coding conventions."#;

        let rule = parse_rule_file(RuleId::from("rust"), content).expect("should parse");
        assert_eq!(rule.name, "Rust coding standards");
        assert_eq!(rule.triggers.len(), 1);
        match &rule.triggers[0] {
            TriggerCondition::PathGlob { pattern } => assert_eq!(pattern, "**/*.rs"),
            other => panic!("expected PathGlob, got {other:?}"),
        }
        assert_eq!(rule.delivery, DeliveryMode::SystemContextAppend);
        assert_eq!(rule.timing, TriggerTiming::After);
        assert!(rule.shell_source.is_none());
        assert!(rule.body.contains("Follow Rust coding conventions."));
    }

    #[test]
    fn parse_claude_code_glob_array_preserves_order() {
        let content = r#"---
description: Rust + manifests
globs:
  - "**/*.rs"
  - "**/*.toml"
---
Body."#;

        let rule = parse_rule_file(RuleId::from("rust-toml"), content).expect("should parse");
        assert_eq!(rule.triggers.len(), 2);
        match &rule.triggers[0] {
            TriggerCondition::PathGlob { pattern } => assert_eq!(pattern, "**/*.rs"),
            other => panic!("expected PathGlob, got {other:?}"),
        }
        match &rule.triggers[1] {
            TriggerCondition::PathGlob { pattern } => assert_eq!(pattern, "**/*.toml"),
            other => panic!("expected PathGlob, got {other:?}"),
        }
    }

    #[test]
    fn parse_claude_code_paths_alias_single_string() {
        let content = r#"---
paths: "**/*.md"
---
Markdown guidance."#;

        let rule = parse_rule_file(RuleId::from("md"), content).expect("should parse");
        assert_eq!(rule.triggers.len(), 1);
        match &rule.triggers[0] {
            TriggerCondition::PathGlob { pattern } => assert_eq!(pattern, "**/*.md"),
            other => panic!("expected PathGlob, got {other:?}"),
        }
        assert_eq!(rule.delivery, DeliveryMode::SystemContextAppend);
        assert_eq!(rule.timing, TriggerTiming::After);
    }

    #[test]
    fn parse_claude_code_description_absent_leaves_name_empty() {
        let content = r#"---
globs: "**/*.rs"
---
Body."#;

        let rule = parse_rule_file(RuleId::from("no-desc"), content).expect("should parse");
        assert!(rule.name.is_empty());
    }

    #[test]
    fn parse_ambiguous_triggers_and_globs_rejected() {
        let content = r#"---
triggers:
  - type: tool
    pattern: Read
delivery: message
globs: "**/*.rs"
---
Body."#;

        let err = parse_rule_file(RuleId::from("amb"), content).expect_err("should error");
        match err {
            RulesError::ParseFailed { reason } => {
                assert!(reason.contains("ambiguous"), "got: {reason}");
            }
            other => panic!("expected ParseFailed, got {other:?}"),
        }
    }

    #[test]
    fn parse_ambiguous_triggers_and_paths_rejected() {
        let content = r#"---
triggers:
  - type: tool
    pattern: Read
delivery: message
paths: "**/*.md"
---
Body."#;

        let err = parse_rule_file(RuleId::from("amb"), content).expect_err("should error");
        match err {
            RulesError::ParseFailed { reason } => {
                assert!(reason.contains("ambiguous"), "got: {reason}");
            }
            other => panic!("expected ParseFailed, got {other:?}"),
        }
    }

    #[test]
    fn parse_neither_triggers_nor_globs_rejected() {
        let content = r"---
name: stray
---
Body.";

        let result = parse_rule_file(RuleId::from("stray"), content);
        assert!(result.is_err());
    }

    #[test]
    fn parse_claude_code_globs_and_paths_concat_in_order() {
        let content = r#"---
description: Combined
globs:
  - "**/*.rs"
paths:
  - "**/*.md"
---
Body."#;

        let rule = parse_rule_file(RuleId::from("combo"), content).expect("should parse");
        assert_eq!(rule.triggers.len(), 2);
        match &rule.triggers[0] {
            TriggerCondition::PathGlob { pattern } => assert_eq!(pattern, "**/*.rs"),
            other => panic!("expected PathGlob, got {other:?}"),
        }
        match &rule.triggers[1] {
            TriggerCondition::PathGlob { pattern } => assert_eq!(pattern, "**/*.md"),
            other => panic!("expected PathGlob, got {other:?}"),
        }
    }

    #[test]
    fn parse_claude_code_empty_globs_array_rejected() {
        let content = r"---
globs: []
---
Body.";

        let result = parse_rule_file(RuleId::from("empty"), content);
        assert!(result.is_err());
    }
}
