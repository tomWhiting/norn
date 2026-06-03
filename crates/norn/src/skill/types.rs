//! Skill metadata types and supporting enums.
//!
//! The shape of [`SkillMetadata`] tracks the Agent Skills open standard
//! (agentskills.io) plus the Claude Code extensions enumerated in the
//! norn-skills DESIGN.md (D2). YAML field names use hyphenated case
//! (`disable-model-invocation`, `user-invocable`, `allowed-tools`,
//! `argument-hint`, `when-to-use`); the [`SkillMetadata`] type maps these
//! into `snake_case` Rust fields via `#[serde(rename_all = "kebab-case")]`.
//!
//! [`StringOrList`] is the in-tree solution to the standard's
//! string-or-list polymorphism (e.g. `arguments: issue branch` vs
//! `arguments: [issue, branch]`).
//!
//! No file IO lives in this module — see [`crate::skill::loader`] for
//! that.

use std::collections::HashMap;
use std::fmt;

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::provider::request::ReasoningEffort;

/// A YAML field that accepts either a single space-separated string or a
/// list of strings, normalised to an owned `Vec<String>`.
///
/// Used by [`SkillMetadata::arguments`], [`SkillMetadata::allowed_tools`],
/// and [`SkillMetadata::paths`] so existing Claude Code SKILL.md files
/// parse without modification.
///
/// Whitespace splitting trims and discards empty entries so multiple
/// spaces (or a leading/trailing space) do not produce empty tokens.
///
/// # Examples
///
/// ```yaml
/// arguments: issue branch        # -> ["issue", "branch"]
/// arguments:                     # -> ["issue", "branch"]
///   - issue
///   - branch
/// arguments: ""                  # -> []
/// arguments: []                  # -> []
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct StringOrList(pub Vec<String>);

impl StringOrList {
    /// Borrow the inner items as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    /// Consume and return the owned `Vec<String>`.
    #[must_use]
    pub fn into_vec(self) -> Vec<String> {
        self.0
    }

    /// True when no items are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl From<Vec<String>> for StringOrList {
    fn from(v: Vec<String>) -> Self {
        Self(v)
    }
}

struct StringOrListVisitor;

impl<'de> Visitor<'de> for StringOrListVisitor {
    type Value = StringOrList;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a whitespace-separated string or a sequence of strings")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        let items = value
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        Ok(StringOrList(items))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_str(&value)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(value) = seq.next_element::<String>()? {
            items.push(value);
        }
        Ok(StringOrList(items))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StringOrList(Vec::new()))
    }
}

impl<'de> Deserialize<'de> for StringOrList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StringOrListVisitor)
    }
}

/// Reasoning effort level a skill may request via its frontmatter.
///
/// Mirrors Claude Code's `effort` field. The skill module stores the
/// value verbatim; mapping to [`crate::provider::request::ReasoningEffort`]
/// happens at activation time in NS-005 (`max` ceilings at `XHigh` since
/// the provider has no `Max`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillEffort {
    /// Minimal reasoning effort.
    Low,
    /// Balanced reasoning effort.
    Medium,
    /// High reasoning effort.
    High,
    /// Extended reasoning effort.
    #[serde(rename = "xhigh")]
    XHigh,
    /// Maximum effort — ceilings at the provider's highest tier.
    Max,
}

/// Map a [`SkillEffort`] tier onto the provider's [`ReasoningEffort`].
///
/// `Max` ceilings at `XHigh` because the provider type has no `Max`
/// variant. `None` is never produced — a skill that opts into an effort
/// tier is always requesting a positive reasoning level, so callers that
/// want to leave the loop's effort untouched should simply skip the
/// override when the skill's `Option<SkillEffort>` is `None`.
impl From<SkillEffort> for ReasoningEffort {
    fn from(value: SkillEffort) -> Self {
        match value {
            SkillEffort::Low => Self::Low,
            SkillEffort::Medium => Self::Medium,
            SkillEffort::High => Self::High,
            SkillEffort::XHigh | SkillEffort::Max => Self::XHigh,
        }
    }
}

/// Execution context selector for a skill.
///
/// `Fork` requests that the skill run in a forked subagent's
/// [`crate::r#loop::loop_context::LoopContext`] rather than the parent's.
/// Wiring of the actual fork happens in NS-006 / norn-agents Group 3.
///
/// Fork-mode contract: when `context: fork` is paired with an
/// [`SkillMetadata::agent`] value, the agent name selects the subagent
/// configuration — that configuration supplies the subagent's system
/// prompt and tool set. The expanded skill body becomes the subagent's
/// **task input**, not its system prompt. This matches Claude Code's
/// behaviour where the agent type owns the system prompt and the skill
/// content describes the work to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillContext {
    /// Execute the skill in a forked subagent.
    Fork,
}

/// Shell selection for backtick-bang shell expansion inside a skill body.
///
/// Defaults to `Bash` at the template expansion layer when the field is
/// not present on the [`SkillMetadata`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillShell {
    /// POSIX shell (`sh -c`).
    Bash,
    /// PowerShell.
    PowerShell,
}

/// Parsed YAML frontmatter for a single SKILL.md file.
///
/// Field names map to YAML via `#[serde(rename_all = "kebab-case")]` so
/// the YAML uses hyphens (`disable-model-invocation`, `user-invocable`,
/// `allowed-tools`, `argument-hint`, `when-to-use`) while the Rust
/// fields remain `snake_case`.
///
/// Unknown fields are silently ignored (no `deny_unknown_fields`) so
/// SKILL.md files authored for future Claude Code revisions still load.
///
/// `disable_model_invocation` defaults to `false`; `user_invocable`
/// defaults to `true`. All other fields default to the standard Rust
/// `None`/empty defaults.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct SkillMetadata {
    /// Skill name. Defaults to the directory or file stem at load time.
    pub name: Option<String>,
    /// Human-readable description.
    pub description: Option<String>,
    /// Optional guidance on when the model should invoke this skill.
    pub when_to_use: Option<String>,
    /// License identifier (Agent Skills standard field).
    pub license: Option<String>,
    /// Compatibility marker (Agent Skills standard field).
    pub compatibility: Option<String>,
    /// Free-form metadata map.
    pub metadata: Option<HashMap<String, String>>,
    /// Hint string describing the argument shape (e.g. `<issue>`).
    pub argument_hint: Option<String>,
    /// Named positional arguments expected by the skill body.
    pub arguments: StringOrList,
    /// When `true`, the model is not shown this skill in the catalog.
    pub disable_model_invocation: bool,
    /// When `true`, the skill is exposed as a `/<name>` slash command.
    pub user_invocable: bool,
    /// Pre-approved tools (experimental Agent Skills standard field).
    /// Stored only — enforcement happens once a permission system exists.
    pub allowed_tools: Option<StringOrList>,
    /// Preferred model identifier (parsed; provider switching deferred).
    pub model: Option<String>,
    /// Requested reasoning effort for this skill's activation turn.
    pub effort: Option<SkillEffort>,
    /// Execution context selector.
    pub context: Option<SkillContext>,
    /// Subagent configuration to fork into (paired with `context: fork`).
    ///
    /// When fork mode is active, this name selects the subagent
    /// configuration that supplies the child's system prompt and tool
    /// set. The expanded skill body is delivered to the subagent as its
    /// **task input**, not as part of its system prompt — see
    /// [`SkillContext::Fork`] for the full contract.
    pub agent: Option<String>,
    /// Path globs that scope automatic activation.
    /// Stored only — enforcement deferred.
    pub paths: Option<StringOrList>,
    /// Shell to use for backtick-bang expansion.
    pub shell: Option<SkillShell>,
    /// Per-skill hooks. Stored as raw JSON — execution deferred.
    pub hooks: Option<serde_json::Value>,
}

impl Default for SkillMetadata {
    fn default() -> Self {
        Self {
            name: None,
            description: None,
            when_to_use: None,
            license: None,
            compatibility: None,
            metadata: None,
            argument_hint: None,
            arguments: StringOrList::default(),
            disable_model_invocation: false,
            user_invocable: true,
            allowed_tools: None,
            model: None,
            effort: None,
            context: None,
            agent: None,
            paths: None,
            shell: None,
            hooks: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn string_or_list_from_space_separated_string() {
        let v: StringOrList = serde_yaml::from_str("\"issue branch\"").unwrap();
        assert_eq!(v.as_slice(), &["issue".to_owned(), "branch".to_owned()]);
    }

    #[test]
    fn string_or_list_from_yaml_list() {
        let v: StringOrList = serde_yaml::from_str("[\"issue\", \"branch\"]").unwrap();
        assert_eq!(v.as_slice(), &["issue".to_owned(), "branch".to_owned()]);
    }

    #[test]
    fn string_or_list_single_token_string() {
        let v: StringOrList = serde_yaml::from_str("\"single\"").unwrap();
        assert_eq!(v.as_slice(), &["single".to_owned()]);
    }

    #[test]
    fn string_or_list_empty_list_is_empty_vec() {
        let v: StringOrList = serde_yaml::from_str("[]").unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn string_or_list_collapses_multiple_spaces() {
        let v: StringOrList = serde_yaml::from_str("\"  issue   branch  \"").unwrap();
        assert_eq!(v.as_slice(), &["issue".to_owned(), "branch".to_owned()]);
    }

    #[test]
    fn string_or_list_round_trip_serialises_as_list() {
        let v = StringOrList(vec!["a".to_owned(), "b".to_owned()]);
        let yaml = serde_yaml::to_string(&v).unwrap();
        assert!(yaml.contains("- a"));
        assert!(yaml.contains("- b"));
    }

    #[test]
    fn metadata_kebab_case_round_trip() {
        let yaml = r#"
name: my-skill
description: do a thing
disable-model-invocation: true
user-invocable: false
argument-hint: "<issue>"
when-to-use: when stuck
allowed-tools: Read Write
"#;
        let m: SkillMetadata = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.name.as_deref(), Some("my-skill"));
        assert_eq!(m.description.as_deref(), Some("do a thing"));
        assert!(m.disable_model_invocation);
        assert!(!m.user_invocable);
        assert_eq!(m.argument_hint.as_deref(), Some("<issue>"));
        assert_eq!(m.when_to_use.as_deref(), Some("when stuck"));
        let allowed = m.allowed_tools.expect("allowed-tools parsed");
        assert_eq!(allowed.as_slice(), &["Read".to_owned(), "Write".to_owned()]);
    }

    #[test]
    fn metadata_defaults_match_design() {
        let m: SkillMetadata = serde_yaml::from_str("description: hi\n").unwrap();
        assert!(!m.disable_model_invocation);
        assert!(m.user_invocable);
        assert!(m.arguments.is_empty());
        assert!(m.allowed_tools.is_none());
        assert!(m.paths.is_none());
        assert!(m.hooks.is_none());
    }

    #[test]
    fn metadata_unknown_fields_are_ignored() {
        let yaml = "description: hi\nfoo: bar\nbaz:\n  - 1\n  - 2\n";
        let m: SkillMetadata = serde_yaml::from_str(yaml).expect("unknown fields ignored");
        assert_eq!(m.description.as_deref(), Some("hi"));
    }

    #[test]
    fn metadata_field_for_freeform_map_parses() {
        let yaml = "description: hi\nmetadata:\n  version: \"1.0\"\n  author: foo\n";
        let m: SkillMetadata = serde_yaml::from_str(yaml).unwrap();
        let map = m.metadata.expect("metadata parsed");
        assert_eq!(map.get("version").map(String::as_str), Some("1.0"));
        assert_eq!(map.get("author").map(String::as_str), Some("foo"));
    }

    #[test]
    fn skill_effort_lowercase_serde() {
        let low: SkillEffort = serde_yaml::from_str("low").unwrap();
        let med: SkillEffort = serde_yaml::from_str("medium").unwrap();
        let high: SkillEffort = serde_yaml::from_str("high").unwrap();
        let xhigh: SkillEffort = serde_yaml::from_str("xhigh").unwrap();
        let max: SkillEffort = serde_yaml::from_str("max").unwrap();
        assert_eq!(low, SkillEffort::Low);
        assert_eq!(med, SkillEffort::Medium);
        assert_eq!(high, SkillEffort::High);
        assert_eq!(xhigh, SkillEffort::XHigh);
        assert_eq!(max, SkillEffort::Max);
    }

    #[test]
    fn skill_context_fork_deserialises() {
        let c: SkillContext = serde_yaml::from_str("fork").unwrap();
        assert_eq!(c, SkillContext::Fork);
    }

    #[test]
    fn skill_shell_bash_and_powershell_deserialise() {
        let bash: SkillShell = serde_yaml::from_str("bash").unwrap();
        let ps: SkillShell = serde_yaml::from_str("powershell").unwrap();
        assert_eq!(bash, SkillShell::Bash);
        assert_eq!(ps, SkillShell::PowerShell);
    }

    #[test]
    fn skill_effort_maps_low_to_reasoning_low() {
        assert_eq!(
            ReasoningEffort::from(SkillEffort::Low),
            ReasoningEffort::Low
        );
    }

    #[test]
    fn skill_effort_maps_medium_to_reasoning_medium() {
        assert_eq!(
            ReasoningEffort::from(SkillEffort::Medium),
            ReasoningEffort::Medium
        );
    }

    #[test]
    fn skill_effort_maps_high_to_reasoning_high() {
        assert_eq!(
            ReasoningEffort::from(SkillEffort::High),
            ReasoningEffort::High
        );
    }

    #[test]
    fn skill_effort_maps_xhigh_to_reasoning_xhigh() {
        assert_eq!(
            ReasoningEffort::from(SkillEffort::XHigh),
            ReasoningEffort::XHigh
        );
    }

    #[test]
    fn skill_effort_max_ceilings_at_reasoning_xhigh() {
        assert_eq!(
            ReasoningEffort::from(SkillEffort::Max),
            ReasoningEffort::XHigh
        );
    }

    #[test]
    fn metadata_hooks_is_raw_json_value() {
        let yaml = "description: hi\nhooks:\n  pre: echo hi\n  post:\n    - one\n    - two\n";
        let m: SkillMetadata = serde_yaml::from_str(yaml).unwrap();
        let hooks = m.hooks.expect("hooks captured");
        assert!(hooks.is_object());
    }
}
