//! The variant catalog: built-in definitions overlaid per-field by
//! configured `variants` settings, compiled once at agent assembly.
//!
//! Built at runtime init (and on the `AgentBuilder` path) from the MERGED
//! settings — layer precedence has already been applied wholesale-by-name
//! (D3). This module owns the one remaining resolution step: configured
//! fields overlay the built-in of the same name per-FIELD, so setting only
//! `variants.reviewer.model` keeps the built-in reviewer prompt and tool
//! subset (the reviewer-model ruling's required ergonomics, 2026-07-04).
//!
//! Prompt files are read EAGERLY here so a missing or unreadable
//! `prompt_file` fails loudly at startup, never at spawn time.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::types::VariantSettings;
use crate::provider::request::ReasoningEffort;
use crate::util::{read_workspace_text_file, workspace_relative_path};

use super::builtin::{BUILTIN_VARIANTS, BuiltinVariant};

/// A fully resolved variant: built-in data overlaid by configured fields,
/// prompt file loaded, reasoning effort parsed.
#[derive(Clone, Debug)]
pub struct ResolvedVariant {
    /// Variant name (spawn-time lookup key).
    pub name: String,
    /// One-line purpose description, if any layer supplied one.
    pub description: Option<String>,
    /// Fully loaded prompt text (inline or file contents). `None` only for
    /// configured variants that supplied neither `prompt` nor `prompt_file`
    /// and shadow no built-in.
    pub prompt: Option<String>,
    /// Tool-name allowlist; `None` = inherit the parent's registry surface.
    /// Always intersected with the child's granted delegation policy at
    /// assembly (policy WINS — brief R6).
    pub tools: Option<Vec<String>>,
    /// Model id; `None` = inherit the parent's model unless
    /// [`Self::model_required`].
    pub model: Option<String>,
    /// Spawning without a model anywhere is a typed error when set (the
    /// reviewer ruling).
    pub model_required: bool,
    /// Parsed reasoning effort for children of this variant.
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Failure building the catalog from merged settings.
#[derive(Debug, thiserror::Error)]
pub enum VariantCatalogError {
    /// A variant supplied both `prompt` and `prompt_file`.
    #[error("variant '{name}': prompt and prompt_file are mutually exclusive — set one")]
    PromptConflict {
        /// The offending variant name.
        name: String,
    },
    /// A variant's `prompt_file` could not be read.
    #[error("variant '{name}': failed to read prompt file '{path}': {source}")]
    PromptFileRead {
        /// The offending variant name.
        name: String,
        /// The path as resolved (absolute or relative to the build dir).
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// A variant's `reasoning_effort` is not a recognised effort name.
    #[error(
        "variant '{name}': unrecognised reasoning_effort '{value}' \
         (expected one of: none, low, medium, high, xhigh, max)"
    )]
    InvalidReasoningEffort {
        /// The offending variant name.
        name: String,
        /// The rejected value.
        value: String,
    },
}

/// The compiled variant catalog published on the tool context at assembly
/// and forwarded to children (grandchildren resolve variants too).
#[derive(Clone, Debug)]
pub struct VariantCatalog {
    variants: BTreeMap<String, ResolvedVariant>,
}

impl VariantCatalog {
    /// Build the catalog: built-ins first, then configured variants
    /// overlaid per-field (configured `Some` wins field-by-field; a
    /// configured name with no built-in counterpart stands alone).
    ///
    /// `base_dir` anchors relative `prompt_file` paths (the agent's
    /// working directory, threaded in by the assembling builder).
    ///
    /// # Errors
    ///
    /// [`VariantCatalogError`] on `prompt`/`prompt_file` conflict,
    /// unreadable prompt file, or unrecognised reasoning effort.
    pub fn build(
        configured: Option<&BTreeMap<String, VariantSettings>>,
        base_dir: &Path,
    ) -> Result<Self, VariantCatalogError> {
        let canonical_base = base_dir
            .canonicalize()
            .unwrap_or_else(|_| base_dir.to_path_buf());
        Self::build_at_launch_root(configured, &canonical_base)
    }

    /// Builds from an already-canonical immutable launch root.
    pub(crate) fn build_at_launch_root(
        configured: Option<&BTreeMap<String, VariantSettings>>,
        base_dir: &Path,
    ) -> Result<Self, VariantCatalogError> {
        let mut variants: BTreeMap<String, ResolvedVariant> = BUILTIN_VARIANTS
            .iter()
            .map(|builtin| (builtin.name.to_owned(), resolve_builtin(builtin)))
            .collect();

        if let Some(configured) = configured {
            for (name, settings) in configured {
                let overlaid = overlay(name, settings, variants.get(name.as_str()), base_dir)?;
                variants.insert(name.clone(), overlaid);
            }
        }

        Ok(Self { variants })
    }

    /// Look up a variant by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ResolvedVariant> {
        self.variants.get(name)
    }

    /// All variant names in stable (sorted) order — the listing surfaced by
    /// unknown-variant errors.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.variants.keys().map(String::as_str)
    }
}

/// Project a built-in definition into its resolved form.
fn resolve_builtin(builtin: &BuiltinVariant) -> ResolvedVariant {
    ResolvedVariant {
        name: builtin.name.to_owned(),
        description: Some(builtin.description.to_owned()),
        prompt: Some(builtin.prompt.to_owned()),
        tools: builtin
            .tools
            .map(|tools| tools.iter().map(|&tool| tool.to_owned()).collect()),
        model: None,
        model_required: builtin.model_required,
        reasoning_effort: None,
    }
}

/// Overlay configured settings onto the resolved base of the same name (a
/// built-in, if one exists): configured `Some` wins per field.
fn overlay(
    name: &str,
    settings: &VariantSettings,
    base: Option<&ResolvedVariant>,
    base_dir: &Path,
) -> Result<ResolvedVariant, VariantCatalogError> {
    let configured_prompt = load_prompt(name, settings, base_dir)?;
    Ok(ResolvedVariant {
        name: name.to_owned(),
        description: settings
            .description
            .clone()
            .or_else(|| base.and_then(|b| b.description.clone())),
        prompt: configured_prompt.or_else(|| base.and_then(|b| b.prompt.clone())),
        tools: settings
            .tools
            .clone()
            .or_else(|| base.and_then(|b| b.tools.clone())),
        model: settings
            .model
            .clone()
            .or_else(|| base.and_then(|b| b.model.clone())),
        model_required: settings
            .model_required
            .unwrap_or_else(|| base.is_some_and(|b| b.model_required)),
        reasoning_effort: match settings.reasoning_effort.as_deref() {
            Some(raw) => Some(parse_reasoning_effort(name, raw)?),
            None => base.and_then(|b| b.reasoning_effort),
        },
    })
}

/// Load the configured prompt: inline text, or the eagerly read
/// `prompt_file` contents. Both set is a typed conflict.
fn load_prompt(
    name: &str,
    settings: &VariantSettings,
    base_dir: &Path,
) -> Result<Option<String>, VariantCatalogError> {
    match (settings.prompt.as_ref(), settings.prompt_file.as_ref()) {
        (Some(_), Some(_)) => Err(VariantCatalogError::PromptConflict {
            name: name.to_owned(),
        }),
        (Some(inline), None) => Ok(Some(inline.clone())),
        (None, Some(file)) => {
            let path = base_dir.join(file);
            let content = match workspace_relative_path(base_dir, &path) {
                Some(relative) => {
                    read_workspace_text_file(base_dir, &relative).map(|loaded| loaded.content)
                }
                None => std::fs::read_to_string(&path),
            };
            content
                .map(Some)
                .map_err(|source| VariantCatalogError::PromptFileRead {
                    name: name.to_owned(),
                    path: path.display().to_string(),
                    source,
                })
        }
        (None, None) => Ok(None),
    }
}

/// Parse a reasoning-effort serde name via the enum's own serde form — the
/// same authority `runtime_init` uses for `agent.reasoning_effort`.
fn parse_reasoning_effort(name: &str, value: &str) -> Result<ReasoningEffort, VariantCatalogError> {
    serde_json::from_value(serde_json::Value::String(value.to_lowercase()))
        .ok()
        .ok_or_else(|| VariantCatalogError::InvalidReasoningEffort {
            name: name.to_owned(),
            value: value.to_owned(),
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn empty_dir() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn builtins_present_with_no_configuration() {
        let catalog = VariantCatalog::build(None, &empty_dir()).expect("builtins alone build");
        for name in ["explorer", "reviewer", "implementer"] {
            assert!(catalog.get(name).is_some(), "built-in '{name}' must exist");
        }
    }

    #[test]
    fn reviewer_builtin_requires_model_and_pins_none() {
        let catalog = VariantCatalog::build(None, &empty_dir()).expect("build");
        let reviewer = catalog.get("reviewer").expect("reviewer exists");
        assert!(reviewer.model_required, "reviewer ships model_required");
        assert!(reviewer.model.is_none(), "reviewer pins NO model");
        let tools = reviewer.tools.as_ref().expect("reviewer has a tool subset");
        for write_tool in [
            "write",
            "edit",
            "bash",
            "apply_patch",
            "spawn_agent",
            "fork",
        ] {
            assert!(
                !tools.iter().any(|t| t == write_tool),
                "reviewer subset must not carry '{write_tool}'",
            );
        }
    }

    #[test]
    fn explorer_builtin_is_read_only() {
        let catalog = VariantCatalog::build(None, &empty_dir()).expect("build");
        let explorer = catalog.get("explorer").expect("explorer exists");
        let tools = explorer.tools.as_ref().expect("explorer has a tool subset");
        for write_tool in ["write", "edit", "bash", "apply_patch"] {
            assert!(
                !tools.iter().any(|t| t == write_tool),
                "explorer subset must not carry '{write_tool}'",
            );
        }
    }

    #[test]
    fn implementer_builtin_inherits_full_registry() {
        let catalog = VariantCatalog::build(None, &empty_dir()).expect("build");
        let implementer = catalog.get("implementer").expect("implementer exists");
        assert!(
            implementer.tools.is_none(),
            "implementer inherits the parent registry (tools: None)",
        );
    }

    #[test]
    fn model_only_override_keeps_builtin_prompt_and_tools() {
        let mut configured = BTreeMap::new();
        configured.insert(
            "reviewer".to_owned(),
            VariantSettings {
                model: Some("review-tier-model".to_owned()),
                ..VariantSettings::default()
            },
        );
        let catalog = VariantCatalog::build(Some(&configured), &empty_dir()).expect("build");
        let reviewer = catalog.get("reviewer").expect("reviewer exists");
        assert_eq!(reviewer.model.as_deref(), Some("review-tier-model"));
        assert!(
            reviewer
                .prompt
                .as_deref()
                .is_some_and(|p| p.contains("adversarial")),
            "built-in reviewer prompt survives a model-only override",
        );
        assert!(
            reviewer.tools.is_some(),
            "built-in reviewer tool subset survives a model-only override",
        );
        assert!(
            reviewer.model_required,
            "model_required flag survives (and is now satisfied by the model)",
        );
    }

    #[test]
    fn standalone_configured_variant_resolves() {
        let mut configured = BTreeMap::new();
        configured.insert(
            "scout".to_owned(),
            VariantSettings {
                description: Some("scouting".to_owned()),
                prompt: Some("Scout the area.".to_owned()),
                tools: Some(vec!["read".to_owned()]),
                reasoning_effort: Some("low".to_owned()),
                ..VariantSettings::default()
            },
        );
        let catalog = VariantCatalog::build(Some(&configured), &empty_dir()).expect("build");
        let scout = catalog.get("scout").expect("scout exists");
        assert_eq!(scout.prompt.as_deref(), Some("Scout the area."));
        assert_eq!(scout.reasoning_effort, Some(ReasoningEffort::Low));
        assert!(!scout.model_required, "standalone default is not required");
        assert!(scout.model.is_none(), "no model configured, none invented");
    }

    #[test]
    fn prompt_and_prompt_file_together_is_a_typed_conflict() {
        let mut configured = BTreeMap::new();
        configured.insert(
            "clash".to_owned(),
            VariantSettings {
                prompt: Some("inline".to_owned()),
                prompt_file: Some("also-a-file.md".to_owned()),
                ..VariantSettings::default()
            },
        );
        let err = VariantCatalog::build(Some(&configured), &empty_dir())
            .expect_err("conflict must be rejected");
        assert!(matches!(
            err,
            VariantCatalogError::PromptConflict { ref name } if name == "clash"
        ));
    }

    #[test]
    fn missing_prompt_file_fails_loudly_at_build() {
        let mut configured = BTreeMap::new();
        configured.insert(
            "ghost".to_owned(),
            VariantSettings {
                prompt_file: Some("does-not-exist-anywhere.md".to_owned()),
                ..VariantSettings::default()
            },
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let err = VariantCatalog::build(Some(&configured), dir.path())
            .expect_err("missing prompt file must fail the build");
        assert!(matches!(err, VariantCatalogError::PromptFileRead { .. }));
    }

    #[test]
    fn prompt_file_is_read_eagerly_relative_to_base_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("scout.md"), "From the file.").expect("write prompt");
        let mut configured = BTreeMap::new();
        configured.insert(
            "scout".to_owned(),
            VariantSettings {
                prompt_file: Some("scout.md".to_owned()),
                ..VariantSettings::default()
            },
        );
        let catalog = VariantCatalog::build(Some(&configured), dir.path()).expect("build");
        assert_eq!(
            catalog.get("scout").expect("scout").prompt.as_deref(),
            Some("From the file."),
        );
    }

    #[cfg(unix)]
    #[test]
    fn prompt_file_inside_workspace_refuses_symlink_even_when_path_is_absolute() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::NamedTempFile::new().expect("outside prompt");
        std::fs::write(outside.path(), "sentinel-private-variant").expect("write outside");
        let link = workspace.path().join("linked-prompt.md");
        symlink(outside.path(), &link).expect("symlink");
        let mut configured = BTreeMap::new();
        configured.insert(
            "linked".to_owned(),
            VariantSettings {
                prompt_file: Some(link.to_string_lossy().into_owned()),
                ..VariantSettings::default()
            },
        );

        let error = VariantCatalog::build(Some(&configured), workspace.path())
            .expect_err("workspace prompt symlink must be refused");

        assert!(!error.to_string().contains("sentinel-private-variant"));
    }

    #[test]
    fn absolute_prompt_file_outside_workspace_remains_a_trusted_source() {
        let workspace = tempfile::tempdir().expect("workspace");
        let trusted = tempfile::NamedTempFile::new().expect("trusted prompt");
        std::fs::write(trusted.path(), "trusted absolute prompt").expect("write prompt");
        let mut configured = BTreeMap::new();
        configured.insert(
            "trusted".to_owned(),
            VariantSettings {
                prompt_file: Some(trusted.path().to_string_lossy().into_owned()),
                ..VariantSettings::default()
            },
        );

        let catalog = VariantCatalog::build(Some(&configured), workspace.path()).expect("build");

        assert_eq!(
            catalog
                .get("trusted")
                .and_then(|variant| variant.prompt.as_deref()),
            Some("trusted absolute prompt"),
        );
    }

    #[test]
    fn invalid_reasoning_effort_is_a_typed_error_naming_the_variant() {
        let mut configured = BTreeMap::new();
        configured.insert(
            "hasty".to_owned(),
            VariantSettings {
                reasoning_effort: Some("turbo".to_owned()),
                ..VariantSettings::default()
            },
        );
        let err = VariantCatalog::build(Some(&configured), &empty_dir())
            .expect_err("unknown effort must be rejected");
        assert!(matches!(
            err,
            VariantCatalogError::InvalidReasoningEffort { ref name, ref value }
                if name == "hasty" && value == "turbo"
        ));
    }

    #[test]
    fn max_reasoning_effort_is_accepted() {
        let mut configured = BTreeMap::new();
        configured.insert(
            "deep".to_owned(),
            VariantSettings {
                reasoning_effort: Some("MAX".to_owned()),
                ..VariantSettings::default()
            },
        );
        let catalog = VariantCatalog::build(Some(&configured), &empty_dir())
            .expect("max effort must be accepted");
        assert_eq!(
            catalog
                .get("deep")
                .expect("configured variant")
                .reasoning_effort,
            Some(ReasoningEffort::Max),
        );
    }

    #[test]
    fn names_lists_builtins_and_configured_sorted() {
        let mut configured = BTreeMap::new();
        configured.insert("scout".to_owned(), VariantSettings::default());
        let catalog = VariantCatalog::build(Some(&configured), &empty_dir()).expect("build");
        let names: Vec<&str> = catalog.names().collect();
        assert_eq!(
            names,
            vec!["explorer", "implementer", "reviewer", "scout"],
            "sorted union of built-ins and configured",
        );
    }
}
