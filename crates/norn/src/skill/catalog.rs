//! In-memory skill catalog populated by scanning the configured search
//! directories.
//!
//! The catalog wraps [`crate::skill::loader::discover_skills`] with
//! shadow-aware merging across the seven-tier directory list. It holds
//! each [`LoadedSkill`] in directory-priority order (first match wins),
//! exposes fast lookup by name, and produces source-partitioned prompt
//! sections so the model can see which skills are available without
//! promoting repository-controlled metadata.
//!
//! Diagnostics are accumulated from three sources:
//!
//! 1. Skip-level diagnostics emitted by the loader (missing description,
//!    unparseable YAML, IO failures).
//! 2. Warning-level diagnostics carried on each [`LoadedSkill`] (name
//!    mismatch, name length).
//! 3. Shadow diagnostics produced here when a skill name collides with
//!    one already discovered in a higher-priority directory.
//!
//! The catalog stays passive on construction: it never installs tools,
//! mutates a [`SlashCommandRegistry`], or attaches `ToolContext`
//! extensions during [`SkillCatalog::scan`]. The runtime builder
//! (NS-006) drives those side effects, and it does so by calling
//! [`SkillCatalog::register_slash_commands`] with a registry it owns.
//! The catalog never reaches into a registry on its own.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::integration::diagnostics::{DiagnosticSeverity, NornDiagnostic};
use crate::r#loop::commands::{SlashCommand, SlashCommandHandler, SlashCommandRegistry};
use crate::skill::loader::{LoadedSkill, discover_skills, discover_skills_with_workspace};
use crate::skill::origin::SkillOrigin;
use crate::skill::types::SkillMetadata;

const SOURCE_TOOL: &str = "skill";

const SYSTEM_PROMPT_HEADER: &str = "# Available Skills\n\n\
The following skills provide specialized instructions for specific tasks.\n\
When a task matches a skill's description, call the skill tool with that\n\
skill's name to load its full instructions.";

/// Model-facing skill catalog split by trust provenance.
///
/// `policy` is compiled Norn guidance. Operator and workspace entries stay
/// separate so prompt assembly can assign Developer and User authority
/// without promoting repository-controlled metadata.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SkillPromptSections {
    policy: String,
    operator_entries: String,
    workspace_entries: String,
}

impl SkillPromptSections {
    /// Compiled guidance explaining how to use listed skills.
    #[must_use]
    pub fn policy(&self) -> &str {
        &self.policy
    }

    /// Entries discovered from caller-trusted paths.
    #[must_use]
    pub fn operator_entries(&self) -> &str {
        &self.operator_entries
    }

    /// Entries proven to come from inside the immutable workspace root.
    #[must_use]
    pub fn workspace_entries(&self) -> &str {
        &self.workspace_entries
    }

    /// Whether there are no model-invocable skills in either origin class.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.operator_entries.is_empty() && self.workspace_entries.is_empty()
    }

    /// Flattened compatibility view in the same order as typed prompt sources.
    #[must_use]
    pub(crate) fn flattened_content(&self) -> String {
        [
            self.policy.as_str(),
            self.operator_entries.as_str(),
            self.workspace_entries.as_str(),
        ]
        .into_iter()
        .filter(|section| !section.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
    }
}

/// Scanned skills together with the diagnostics produced while loading.
///
/// `SkillCatalog` is `Send + Sync` and intended to be wrapped in an
/// [`std::sync::Arc`] so the eventual `ToolContext` extension (NS-007)
/// can share a single immutable view across the runtime.
#[derive(Debug, Default, Clone)]
pub struct SkillCatalog {
    skills: Vec<LoadedSkill>,
    by_name: HashMap<String, usize>,
    diagnostics: Vec<NornDiagnostic>,
}

impl SkillCatalog {
    /// Scan `dirs` in priority order and populate the catalog.
    ///
    /// Directories are scanned independently so that name collisions
    /// across directories can be attributed to specific paths. Within a
    /// single directory the loader's own first-match-wins (dir form over
    /// flat `<name>.md`) still applies. The earliest directory in `dirs`
    /// has the highest priority; a later directory's same-name skill is
    /// skipped and a `skill-shadowed` warning is recorded with the
    /// shadowed path.
    ///
    /// Missing or unreadable directories are silently passed through —
    /// the loader logs them at `tracing::debug!` level so an absent
    /// `~/.norn/skills/` produces no diagnostic noise.
    ///
    /// # Trust boundary
    ///
    /// This compatibility constructor accepts caller-trusted directories only.
    /// It does not apply workspace no-follow traversal. Applications loading
    /// repository skills should use `AgentBuilder` / `load_runtime_base`, which
    /// constructs the catalog through the provenance-aware runtime path.
    #[must_use]
    pub fn scan(dirs: &[PathBuf]) -> Self {
        Self::scan_impl(dirs, None)
    }

    /// Scans runtime paths while securing entries beneath `workspace_root`.
    #[must_use]
    pub(crate) fn scan_with_workspace(dirs: &[PathBuf], workspace_root: &std::path::Path) -> Self {
        Self::scan_impl(dirs, Some(workspace_root))
    }

    fn scan_impl(dirs: &[PathBuf], workspace_root: Option<&std::path::Path>) -> Self {
        let mut catalog = Self::default();

        for dir in dirs {
            let single = std::slice::from_ref(dir);
            let result = workspace_root.map_or_else(
                || discover_skills(single),
                |root| discover_skills_with_workspace(single, root),
            );

            catalog.diagnostics.extend(result.diagnostics);

            for skill in result.skills {
                if let Some(existing_idx) = catalog.by_name.get(&skill.name).copied() {
                    let winning_path = catalog
                        .skills
                        .get(existing_idx)
                        .map(|s| s.path.display().to_string())
                        .unwrap_or_default();
                    catalog.diagnostics.push(NornDiagnostic {
                        severity: DiagnosticSeverity::Warning,
                        code: "skill-shadowed".to_owned(),
                        message: format!(
                            "skill '{}' at {} is shadowed by an earlier entry",
                            skill.name,
                            skill.path.display()
                        ),
                        source_tool: Some(SOURCE_TOOL.to_owned()),
                        file_path: Some(skill.path.display().to_string()),
                        suggestion: Some(format!("earlier match: {winning_path}")),
                    });
                    continue;
                }

                catalog
                    .diagnostics
                    .extend(skill.diagnostics.iter().cloned());

                if skill
                    .metadata
                    .allowed_tools
                    .as_ref()
                    .is_some_and(|t| !t.is_empty())
                {
                    catalog.diagnostics.push(NornDiagnostic {
                        severity: DiagnosticSeverity::Info,
                        code: "skill-allowed-tools-not-enforced".to_owned(),
                        message: format!(
                            "skill '{}' declares allowed-tools but Norn does not enforce them \
                             in this version",
                            skill.name
                        ),
                        source_tool: Some(SOURCE_TOOL.to_owned()),
                        file_path: Some(skill.path.display().to_string()),
                        suggestion: None,
                    });
                }

                let idx = catalog.skills.len();
                catalog.by_name.insert(skill.name.clone(), idx);
                catalog.skills.push(skill);
            }
        }

        catalog
    }

    /// Pairs of `(name, description)` for every loaded skill in
    /// discovery order.
    ///
    /// Includes skills regardless of `disable_model_invocation`; the
    /// filtered, model-facing view is [`Self::system_prompt_listing`].
    /// Description is guaranteed non-empty because the loader skips
    /// skills missing a description, but a defensive `unwrap_or_default`
    /// is used to keep this method panic-free.
    #[must_use]
    pub fn list(&self) -> Vec<(String, String)> {
        self.skills
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    s.metadata.description.clone().unwrap_or_default(),
                )
            })
            .collect()
    }

    /// Look up a skill's metadata by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SkillMetadata> {
        self.by_name
            .get(name)
            .and_then(|idx| self.skills.get(*idx))
            .map(|s| &s.metadata)
    }

    /// Trust provenance of a loaded skill.
    #[must_use]
    pub fn origin(&self, name: &str) -> Option<SkillOrigin> {
        self.by_name
            .get(name)
            .and_then(|idx| self.skills.get(*idx))
            .map(|skill| skill.origin)
    }

    /// True when no skills were discovered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Number of loaded skills.
    #[must_use]
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Diagnostics accumulated during [`Self::scan`].
    ///
    /// Includes skip diagnostics from the loader, per-skill warnings
    /// (name mismatch, length), and `skill-shadowed` warnings produced
    /// when a name collides across directories.
    #[must_use]
    pub fn diagnostics(&self) -> &[NornDiagnostic] {
        &self.diagnostics
    }

    /// Flattened compatibility listing for authority-unaware display surfaces.
    ///
    /// Excludes skills where `disable_model_invocation` is `true`. For
    /// each included skill the description and (when present)
    /// `when_to_use` are concatenated, separated by a single space.
    /// Returns an empty string when no model-invocable skills exist. Runtime
    /// prompt assembly uses [`Self::prompt_sections`] instead; callers must not
    /// assign this mixed-origin rendering one model authority.
    ///
    /// The output has no trailing newline so it joins cleanly into a
    /// larger prompt assembly.
    #[must_use]
    pub fn system_prompt_listing(&self) -> String {
        let visible: Vec<&LoadedSkill> = self
            .skills
            .iter()
            .filter(|s| !s.metadata.disable_model_invocation)
            .collect();

        if visible.is_empty() {
            return String::new();
        }

        let entries = render_entries(visible.into_iter());
        let mut out = String::with_capacity(SYSTEM_PROMPT_HEADER.len() + 2 + entries.len());
        out.push_str(SYSTEM_PROMPT_HEADER);
        out.push_str("\n\n");
        out.push_str(&entries);
        out
    }

    /// Model-facing listing split into compiled policy, trusted entries, and
    /// repository-controlled entries.
    #[must_use]
    pub fn prompt_sections(&self) -> SkillPromptSections {
        let visible = |origin| {
            self.skills.iter().filter(move |skill| {
                !skill.metadata.disable_model_invocation && skill.origin == origin
            })
        };
        let operator_entries = render_entries(visible(SkillOrigin::Operator));
        let workspace_entries = render_entries(visible(SkillOrigin::Workspace));
        let policy = if operator_entries.is_empty() && workspace_entries.is_empty() {
            String::new()
        } else {
            SYSTEM_PROMPT_HEADER.to_owned()
        };
        SkillPromptSections {
            policy,
            operator_entries,
            workspace_entries,
        }
    }

    /// Register every `user_invocable` skill in `registry` as a
    /// `/<skill-name>` slash command backed by the
    /// [`SlashCommandHandler::Skill`] variant.
    ///
    /// Skills where `user_invocable` is `false` are not registered, even
    /// though they remain visible in the catalog and (when
    /// `disable_model_invocation` is `false`) in the system-prompt
    /// listing. Existing entries with a colliding name are replaced —
    /// matching the registry's own `register` semantics.
    ///
    /// The catalog itself stays inert: callers (the runtime builder, in
    /// practice) pass in the registry they own and observe the new
    /// entries after the call returns.
    pub fn register_slash_commands(&self, registry: &mut SlashCommandRegistry) {
        for skill in &self.skills {
            if !skill.metadata.user_invocable {
                continue;
            }
            registry.register(SlashCommand {
                name: skill.name.clone(),
                handler: SlashCommandHandler::Skill {
                    skill_name: skill.name.clone(),
                },
            });
        }
    }
}

fn render_entries<'a>(skills: impl Iterator<Item = &'a LoadedSkill>) -> String {
    let mut out = String::new();
    for skill in skills {
        if !out.is_empty() {
            out.push('\n');
        }
        let description = skill.metadata.description.as_deref().unwrap_or("").trim();
        out.push_str("- ");
        out.push_str(&skill.name);
        out.push_str(": ");
        out.push_str(description);
        if let Some(when) = skill
            .metadata
            .when_to_use
            .as_deref()
            .map(str::trim)
            .filter(|when| !when.is_empty())
        {
            out.push(' ');
            out.push_str(when);
        }
    }
    out
}

#[cfg(test)]
mod tests;
