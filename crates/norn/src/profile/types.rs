//! Profile, capability, and prompt-command type definitions plus the
//! extension-dispatched [`Profile::from_file`] loader.
//!
//! A [`Profile`] describes everything an agent needs to run: model, optional
//! reasoning effort, the tool allow-list, system instructions, composable
//! [`Capability`] bundles, prompt commands whose stdout populates dynamic
//! system sections, and a free-form settings map for caller-specific knobs.
//!
//! [`Profile::from_file`] dispatches on the file extension: `.toml` is parsed
//! with the `toml` crate, `.json` with `serde_json`, and `.md` is parsed by
//! [`super::loader::parse_profile`] as YAML frontmatter plus a markdown body.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::provider::request::{ReasoningEffort, ReasoningSummary};

/// A shell command whose stdout is appended as a dynamic system section at
/// each iteration of the agent loop.
///
/// `cache_ttl` controls how long a successful run is reused. `None` means
/// re-run every iteration; `Some(_)` re-uses the cached stdout until the TTL
/// elapses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptCommand {
    /// Human-readable name used as the section heading.
    pub name: String,
    /// Shell command executed via `sh -c`.
    pub command: String,
    /// Optional time-to-live for the cached stdout. When `None` the command
    /// runs every iteration.
    #[serde(default, with = "duration_secs")]
    pub cache_ttl: Option<Duration>,
}

/// A composable bundle of tools, system instructions, and disallowed
/// patterns. Multiple capabilities merge into a profile via the
/// `resolved_*` helpers on [`Profile`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Capability {
    /// Capability name (for diagnostics and audit).
    pub name: String,
    /// Tool names contributed by this capability.
    #[serde(default)]
    pub tools: Vec<String>,
    /// System-instruction snippets contributed by this capability.
    #[serde(default)]
    pub system_instructions: Vec<String>,
    /// Disallowed patterns contributed by this capability (e.g. bash command
    /// substrings the runtime should refuse to execute).
    #[serde(default)]
    pub disallowed_patterns: Vec<String>,
}

/// Top-level agent configuration object.
///
/// Profiles are constructed by the orchestrator (or loaded from disk) and
/// fed to [`super::resolve::from_profile`] to produce a configured loop
/// context and a gated tool registry. Profiles intentionally describe
/// behaviour declaratively — they do not own the tool registry or rules
/// engine.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Profile {
    /// Profile name (for diagnostics and audit).
    pub name: String,
    /// Model identifier passed through to the provider.
    pub model: String,
    /// Optional reasoning-effort hint threaded through to the provider
    /// request.
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Optional reasoning-summary verbosity threaded through to the
    /// provider request.
    #[serde(default)]
    pub reasoning_summary: Option<ReasoningSummary>,
    /// Optional explicit tool allow-list. When `Some`, only the named tools
    /// (plus any contributed by capabilities) are available; when `None` and
    /// no capabilities contribute tools, [`Self::resolved_tools`] returns
    /// the empty vec — gating every registered tool.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// System-instruction snippets contributed at the profile level. Joined
    /// with [`Self::resolved_instructions`] and installed as the loop
    /// context's base instruction.
    #[serde(default)]
    pub system_instructions: Vec<String>,
    /// Composable capability bundles merged into the profile.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Free-form settings map for caller-specific configuration.
    #[serde(default)]
    pub settings: HashMap<String, serde_json::Value>,
    /// Shell commands evaluated at the start of every loop iteration.
    #[serde(default)]
    pub prompt_commands: Vec<PromptCommand>,
}

impl Profile {
    /// Load a profile from disk, dispatching on file extension.
    ///
    /// `.toml` files are parsed with the `toml` crate; `.json` files with
    /// `serde_json`; `.md` files with [`super::loader::parse_profile`]
    /// (YAML frontmatter plus markdown body). Any other extension returns
    /// [`ConfigError::InvalidConfig`].
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidConfig`] when the file cannot be read,
    /// has an unsupported extension, or fails to deserialise.
    /// Returns [`ConfigError::MissingField`] when an `.md` profile's
    /// frontmatter omits `name`.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::InvalidConfig {
            reason: format!("failed to read profile at {}: {e}", path.display()),
        })?;
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("toml") => toml::from_str(&contents).map_err(|e| ConfigError::InvalidConfig {
                reason: format!("invalid TOML profile at {}: {e}", path.display()),
            }),
            Some("json") => {
                serde_json::from_str(&contents).map_err(|e| ConfigError::InvalidConfig {
                    reason: format!("invalid JSON profile at {}: {e}", path.display()),
                })
            }
            Some("md") => super::loader::parse_profile(&contents, path),
            other => Err(ConfigError::InvalidConfig {
                reason: format!(
                    "unsupported profile extension {:?} at {}; expected .toml, .json, or .md",
                    other,
                    path.display(),
                ),
            }),
        }
    }
}

mod duration_secs {
    //! Serde adapter that represents [`Duration`] as an integer number of
    //! seconds. Used by [`super::PromptCommand::cache_ttl`] so TOML/JSON
    //! config files can write `cache_ttl = 30` instead of struct-of-fields
    //! syntax.

    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    // Serde `with` modules require `&T` where T is the field type.
    // The field is `Option<Duration>`, so serde generates `&Option<Duration>`.
    #[allow(clippy::ref_option)]
    pub(super) fn serialize<S>(value: &Option<Duration>, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.map(|d| d.as_secs()).serialize(ser)
    }

    pub(super) fn deserialize<'de, D>(de: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<u64>::deserialize(de).map(|opt| opt.map(Duration::from_secs))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::uninlined_format_args,
    clippy::unnecessary_literal_bound
)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn sample_profile() -> Profile {
        Profile {
            name: "code-author".to_owned(),
            model: "gpt-5".to_owned(),
            reasoning_effort: Some(ReasoningEffort::Medium),
            reasoning_summary: None,
            tools: Some(vec!["read".to_owned(), "edit".to_owned()]),
            system_instructions: vec!["You are an expert author.".to_owned()],
            capabilities: vec![
                Capability {
                    name: "editing".to_owned(),
                    tools: vec!["edit".to_owned(), "write".to_owned()],
                    system_instructions: vec!["Prefer minimal diffs.".to_owned()],
                    disallowed_patterns: vec!["rm -rf".to_owned()],
                },
                Capability {
                    name: "shell".to_owned(),
                    tools: vec!["bash".to_owned()],
                    system_instructions: vec!["Use bash sparingly.".to_owned()],
                    disallowed_patterns: vec!["sudo".to_owned()],
                },
            ],
            settings: {
                let mut m = HashMap::new();
                m.insert("max_file_lines".to_owned(), serde_json::json!(500));
                m
            },
            prompt_commands: vec![PromptCommand {
                name: "cwd".to_owned(),
                command: "echo cwd".to_owned(),
                cache_ttl: Some(Duration::from_secs(30)),
            }],
        }
    }

    #[test]
    fn profile_roundtrip_serde() {
        let original = sample_profile();
        let json = serde_json::to_string(&original).unwrap();
        let from_json: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(from_json.name, original.name);
        assert_eq!(from_json.model, original.model);
        assert_eq!(from_json.reasoning_effort, original.reasoning_effort);
        assert_eq!(from_json.tools, original.tools);
        assert_eq!(from_json.system_instructions, original.system_instructions);
        assert_eq!(from_json.capabilities.len(), original.capabilities.len());
        assert_eq!(from_json.prompt_commands.len(), 1);
        assert_eq!(
            from_json.prompt_commands[0].cache_ttl,
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            from_json.settings.get("max_file_lines"),
            original.settings.get("max_file_lines"),
        );

        let toml_str = toml::to_string(&original).unwrap();
        let from_toml: Profile = toml::from_str(&toml_str).unwrap();
        assert_eq!(from_toml.name, original.name);
        assert_eq!(from_toml.model, original.model);
        assert_eq!(from_toml.reasoning_effort, original.reasoning_effort);
        assert_eq!(
            from_toml.capabilities[0].disallowed_patterns,
            original.capabilities[0].disallowed_patterns,
        );
        assert_eq!(from_toml.prompt_commands.len(), 1);
        assert_eq!(
            from_toml.prompt_commands[0].cache_ttl,
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn from_file_reads_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.toml");
        let toml_body = r#"
name = "fixture"
model = "gpt-5"
system_instructions = ["fixture instruction"]
"#;
        std::fs::write(&path, toml_body).unwrap();
        let profile = Profile::from_file(&path).unwrap();
        assert_eq!(profile.name, "fixture");
        assert_eq!(profile.model, "gpt-5");
        assert_eq!(profile.system_instructions, vec!["fixture instruction"]);
    }

    #[test]
    fn from_file_reads_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        let json_body = r#"{"name":"fx","model":"gpt-5","system_instructions":["one","two"]}"#;
        std::fs::write(&path, json_body).unwrap();
        let profile = Profile::from_file(&path).unwrap();
        assert_eq!(profile.name, "fx");
        assert_eq!(profile.system_instructions, vec!["one", "two"]);
    }

    #[test]
    fn from_file_rejects_unknown_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.ini");
        std::fs::write(&path, "ignored").unwrap();
        let err = Profile::from_file(&path).unwrap_err();
        match err {
            ConfigError::InvalidConfig { reason } => {
                assert!(
                    reason.contains("unsupported profile extension"),
                    "reason: {reason}",
                );
            }
            other @ ConfigError::MissingField { .. } => {
                panic!("expected InvalidConfig, got {other:?}")
            }
        }
    }

    /// `.md` dispatch end-to-end: a profile fixture matching the brief's
    /// verification scenario (`name: test`, `model: gpt-5`,
    /// `tools: Read, Bash(cargo build*)`, `disallowedTools: rm`, body
    /// `You are a test.`) round-trips through [`Profile::from_file`] with a
    /// synthetic `_profile_disallowed` capability holding the disallowed
    /// pattern.
    #[test]
    fn from_file_reads_md() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.md");
        let md_body = "---\nname: test\nmodel: gpt-5\ntools: Read, Bash(cargo build*)\ndisallowedTools: rm\n---\nYou are a test.\n";
        std::fs::write(&path, md_body).unwrap();
        let profile = Profile::from_file(&path).unwrap();
        assert_eq!(profile.name, "test");
        assert_eq!(profile.model, "gpt-5");
        let tools = profile.tools.as_deref().unwrap();
        assert!(
            tools.iter().any(|t| t == "Bash(cargo build*)"),
            "expected parenthesised tool preserved, got {tools:?}"
        );
        assert_eq!(
            profile.system_instructions,
            vec!["You are a test.".to_owned()]
        );
        let synthetic = profile
            .capabilities
            .iter()
            .find(|c| c.name == "_profile_disallowed")
            .expect("synthetic _profile_disallowed capability must exist");
        assert_eq!(synthetic.disallowed_patterns, vec!["rm".to_owned()]);
    }
}
