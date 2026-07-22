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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines
)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn write_skill(dir: &Path, name: &str, description: &str) {
        write_file(
            &dir.join(name).join("SKILL.md"),
            &format!("---\ndescription: {description}\n---\nbody\n"),
        );
    }

    #[test]
    fn assert_catalog_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SkillCatalog>();
    }

    #[test]
    fn empty_catalog_reports_is_empty_and_blank_listing() {
        let catalog = SkillCatalog::scan(&[]);
        assert!(catalog.is_empty());
        assert_eq!(catalog.len(), 0);
        assert!(catalog.list().is_empty());
        assert!(catalog.get("anything").is_none());
        assert!(catalog.system_prompt_listing().is_empty());
        assert!(catalog.diagnostics().is_empty());
    }

    #[test]
    fn missing_directory_is_silent_and_yields_empty_catalog() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let catalog = SkillCatalog::scan(&[missing]);
        assert!(catalog.is_empty());
        assert!(catalog.diagnostics().is_empty());
    }

    #[test]
    fn scan_lists_loaded_skills_in_discovery_order() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "alpha", "first");
        write_skill(dir.path(), "beta", "second");

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        assert_eq!(catalog.len(), 2);
        let listed = catalog.list();
        let names: Vec<&str> = listed.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
    }

    #[test]
    fn get_returns_metadata_for_known_skill_and_none_for_unknown() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "lookup", "find me");

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        let meta = catalog.get("lookup").expect("metadata present");
        assert_eq!(meta.description.as_deref(), Some("find me"));
        assert!(catalog.get("not-here").is_none());
    }

    #[test]
    fn first_directory_wins_on_name_collision() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        write_skill(dir_a.path(), "shared", "from A");
        write_skill(dir_b.path(), "shared", "from B");

        let catalog = SkillCatalog::scan(&[dir_a.path().to_path_buf(), dir_b.path().to_path_buf()]);

        assert_eq!(catalog.len(), 1);
        let list = catalog.list();
        assert_eq!(list.len(), 1);
        let meta = catalog.get("shared").expect("metadata present");
        assert_eq!(meta.description.as_deref(), Some("from A"));
    }

    #[test]
    fn shadow_diagnostic_records_the_shadowed_path() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        write_skill(dir_a.path(), "deploy", "from A");
        write_skill(dir_b.path(), "deploy", "from B");

        let catalog = SkillCatalog::scan(&[dir_a.path().to_path_buf(), dir_b.path().to_path_buf()]);

        let shadow_path = dir_b.path().join("deploy").join("SKILL.md");
        let winning_path = dir_a.path().join("deploy").join("SKILL.md");
        let shadow_str = shadow_path.display().to_string();
        let winning_str = winning_path.display().to_string();

        let shadow_diag = catalog
            .diagnostics()
            .iter()
            .find(|d| d.code == "skill-shadowed")
            .expect("shadow diagnostic emitted");

        assert_eq!(shadow_diag.severity, DiagnosticSeverity::Warning);
        assert_eq!(shadow_diag.file_path.as_deref(), Some(shadow_str.as_str()));
        assert!(
            shadow_diag
                .suggestion
                .as_deref()
                .is_some_and(|s| s.contains(&winning_str)),
            "suggestion should reference winning path, got {:?}",
            shadow_diag.suggestion
        );
        assert_eq!(shadow_diag.source_tool.as_deref(), Some(SOURCE_TOOL));
    }

    #[test]
    fn loader_skip_diagnostics_propagate_into_catalog() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("broken").join("SKILL.md"),
            "---\nname: broken\n---\nbody\n",
        );

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        assert!(catalog.is_empty());
        assert!(
            catalog
                .diagnostics()
                .iter()
                .any(|d| d.code == "skill-missing-description"),
            "missing-description diagnostic should reach catalog, got {:?}",
            catalog.diagnostics()
        );
    }

    #[test]
    fn per_skill_warning_diagnostics_propagate_into_catalog() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("real-dir").join("SKILL.md"),
            "---\nname: different-name\ndescription: hi\n---\n",
        );

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        assert_eq!(catalog.len(), 1);
        assert!(
            catalog
                .diagnostics()
                .iter()
                .any(|d| d.code == "skill-name-mismatch"),
            "name-mismatch diagnostic should reach catalog, got {:?}",
            catalog.diagnostics()
        );
    }

    #[test]
    fn system_prompt_listing_starts_with_header_and_instruction() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "deploy", "Deploy the service.");

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        let listing = catalog.system_prompt_listing();
        assert!(
            listing.starts_with("# Available Skills\n\n"),
            "listing should start with the heading, got: {listing}"
        );
        assert!(
            listing.contains(
                "The following skills provide specialized instructions for specific tasks."
            ),
            "listing should include the behavioral instruction line, got: {listing}"
        );
        assert!(
            listing.contains("call the skill tool with that"),
            "listing should include call-the-tool instruction, got: {listing}"
        );
    }

    #[test]
    fn prompt_sections_partition_operator_and_workspace_metadata() {
        let root = tempdir().unwrap();
        let operator_dir = root.path().join("operator");
        let workspace = root.path().join("workspace");
        let workspace_dir = workspace.join(".norn/skills");
        write_skill(&operator_dir, "trusted", "OPERATOR_DESCRIPTION_SENTINEL");
        write_file(
            &workspace_dir.join("repository/SKILL.md"),
            "---\ndescription: WORKSPACE_DESCRIPTION_SENTINEL\n\
             when-to-use: WORKSPACE_WHEN_SENTINEL\n---\nbody\n",
        );
        let workspace_root = workspace.canonicalize().expect("canonical workspace root");

        let catalog =
            SkillCatalog::scan_with_workspace(&[workspace_dir, operator_dir], &workspace_root);
        let sections = catalog.prompt_sections();

        assert_eq!(catalog.origin("trusted"), Some(SkillOrigin::Operator));
        assert_eq!(catalog.origin("repository"), Some(SkillOrigin::Workspace));
        assert!(sections.policy().contains("# Available Skills"));
        assert!(
            sections
                .operator_entries()
                .contains("OPERATOR_DESCRIPTION_SENTINEL")
        );
        assert!(
            !sections
                .operator_entries()
                .contains("WORKSPACE_DESCRIPTION_SENTINEL")
        );
        assert!(
            sections
                .workspace_entries()
                .contains("WORKSPACE_DESCRIPTION_SENTINEL")
        );
        assert!(
            sections
                .workspace_entries()
                .contains("WORKSPACE_WHEN_SENTINEL")
        );
        assert!(
            !sections
                .workspace_entries()
                .contains("OPERATOR_DESCRIPTION_SENTINEL")
        );
        assert!(!sections.policy().contains("OPERATOR_DESCRIPTION_SENTINEL"));
        assert!(!sections.policy().contains("WORKSPACE_DESCRIPTION_SENTINEL"));
    }

    #[test]
    fn system_prompt_listing_concatenates_when_to_use() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("fix-issue").join("SKILL.md"),
            "---\ndescription: Fix the issue.\nwhen-to-use: Use when a bug is reported.\n---\n",
        );

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        let listing = catalog.system_prompt_listing();
        assert!(
            listing.contains("- fix-issue: Fix the issue. Use when a bug is reported."),
            "expected description + when_to_use concatenation, got: {listing}"
        );
    }

    #[test]
    fn system_prompt_listing_omits_when_to_use_when_absent() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "simple", "Just a description");

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        let listing = catalog.system_prompt_listing();
        assert!(
            listing.contains("- simple: Just a description"),
            "expected bullet for simple skill, got: {listing}"
        );
        assert!(
            !listing.contains("- simple: Just a description "),
            "no trailing space when when_to_use is absent, got: {listing:?}"
        );
    }

    #[test]
    fn system_prompt_listing_excludes_disable_model_invocation_skills() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("hidden").join("SKILL.md"),
            "---\ndescription: hidden one\ndisable-model-invocation: true\n---\n",
        );
        write_skill(dir.path(), "visible", "visible one");

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        // list() still surfaces both for workflow authors.
        assert_eq!(catalog.list().len(), 2);

        let listing = catalog.system_prompt_listing();
        assert!(
            !listing.contains("hidden"),
            "hidden skill must not appear in listing, got: {listing}"
        );
        assert!(
            listing.contains("- visible: visible one"),
            "visible skill must appear in listing, got: {listing}"
        );
    }

    #[test]
    fn system_prompt_listing_is_empty_when_all_skills_are_hidden() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("hidden-a").join("SKILL.md"),
            "---\ndescription: a\ndisable-model-invocation: true\n---\n",
        );
        write_file(
            &dir.path().join("hidden-b").join("SKILL.md"),
            "---\ndescription: b\ndisable-model-invocation: true\n---\n",
        );

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        assert_eq!(catalog.len(), 2);
        assert!(
            catalog.system_prompt_listing().is_empty(),
            "listing should be empty when no skills are model-invocable"
        );
    }

    #[test]
    fn allowed_tools_emits_info_diagnostic() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("greet").join("SKILL.md"),
            "---\ndescription: hi\nallowed-tools: Read Write\n---\nbody\n",
        );

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        let diag = catalog
            .diagnostics()
            .iter()
            .find(|d| d.code == "skill-allowed-tools-not-enforced")
            .expect("allowed-tools-not-enforced diagnostic emitted");
        assert_eq!(diag.severity, DiagnosticSeverity::Info);
        assert_eq!(diag.source_tool.as_deref(), Some(SOURCE_TOOL));
        assert!(diag.message.contains("greet"));
        assert!(diag.suggestion.is_none());
    }

    #[test]
    fn no_allowed_tools_diagnostic_when_field_absent() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "plain", "no allowed tools");

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        assert!(
            !catalog
                .diagnostics()
                .iter()
                .any(|d| d.code == "skill-allowed-tools-not-enforced"),
            "should not emit when allowed-tools is absent",
        );
    }

    #[test]
    fn no_allowed_tools_diagnostic_when_field_empty() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("empty-tools").join("SKILL.md"),
            "---\ndescription: hi\nallowed-tools: \"\"\n---\nbody\n",
        );

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        assert!(
            !catalog
                .diagnostics()
                .iter()
                .any(|d| d.code == "skill-allowed-tools-not-enforced"),
            "should not emit for empty allowed-tools",
        );
    }

    #[test]
    fn shadowed_skill_does_not_emit_allowed_tools_diagnostic() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        write_skill(dir_a.path(), "deploy", "from A");
        write_file(
            &dir_b.path().join("deploy").join("SKILL.md"),
            "---\ndescription: from B\nallowed-tools: Read\n---\nbody\n",
        );

        let catalog = SkillCatalog::scan(&[dir_a.path().to_path_buf(), dir_b.path().to_path_buf()]);
        // The B copy is shadowed; only A wins and A has no allowed-tools.
        assert!(
            !catalog
                .diagnostics()
                .iter()
                .any(|d| d.code == "skill-allowed-tools-not-enforced"),
            "shadowed skill should not produce allowed-tools diagnostic",
        );
    }

    #[test]
    fn register_slash_commands_registers_user_invocable_skill() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "deploy", "deploy the service");

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        let mut registry = SlashCommandRegistry::new();
        catalog.register_slash_commands(&mut registry);

        let command = registry.get("deploy").expect("command registered");
        assert_eq!(command.name, "deploy");
        match &command.handler {
            SlashCommandHandler::Skill { skill_name } => {
                assert_eq!(skill_name, "deploy");
            }
            _ => panic!("expected Skill handler variant"),
        }
    }

    #[test]
    fn register_slash_commands_skips_non_user_invocable() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("background-knowledge").join("SKILL.md"),
            "---\ndescription: background\nuser-invocable: false\n---\n",
        );

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        let mut registry = SlashCommandRegistry::new();
        catalog.register_slash_commands(&mut registry);

        assert!(registry.get("background-knowledge").is_none());
        assert!(registry.is_empty());
    }

    #[test]
    fn register_slash_commands_preserves_existing_unrelated_entries() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "deploy", "deploy the service");

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        let mut registry = SlashCommandRegistry::new();
        registry.register(SlashCommand {
            name: "help".to_owned(),
            handler: SlashCommandHandler::Tool {
                tool_name: "noop".to_owned(),
                args: serde_json::json!({}),
            },
        });
        catalog.register_slash_commands(&mut registry);

        assert!(registry.get("help").is_some());
        assert!(registry.get("deploy").is_some());
    }

    #[test]
    fn preprocess_input_expands_registered_slash_skill_with_args() {
        use crate::r#loop::commands::{PreprocessResult, preprocess_input};

        let dir = tempdir().unwrap();
        write_skill(dir.path(), "deploy", "deploy the service");

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        let mut registry = SlashCommandRegistry::new();
        catalog.register_slash_commands(&mut registry);

        let out = preprocess_input("/deploy prod", &registry).unwrap();
        match out {
            PreprocessResult::Expanded { messages } => {
                assert_eq!(messages.len(), 1);
                let body = messages[0]
                    .content
                    .as_ref()
                    .expect("skill message has content");
                assert!(
                    body.contains("deploy"),
                    "body must mention skill name: {body}"
                );
                assert!(body.contains("prod"), "body must mention argument: {body}");
            }
            PreprocessResult::Passthrough(_) => panic!("expected expansion"),
        }
    }

    #[test]
    fn preprocess_input_expands_registered_slash_skill_without_args() {
        use crate::r#loop::commands::{PreprocessResult, preprocess_input};

        let dir = tempdir().unwrap();
        write_skill(dir.path(), "deploy", "deploy the service");

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        let mut registry = SlashCommandRegistry::new();
        catalog.register_slash_commands(&mut registry);

        let out = preprocess_input("/deploy", &registry).unwrap();
        match out {
            PreprocessResult::Expanded { messages } => {
                let body = messages[0]
                    .content
                    .as_ref()
                    .expect("skill message has content");
                assert!(body.contains("deploy"));
                assert!(
                    !body.contains("Argument:"),
                    "no Argument clause when slash command has no trailing text: {body}",
                );
            }
            PreprocessResult::Passthrough(_) => panic!("expected expansion"),
        }
    }

    #[test]
    fn invocation_matrix_default_is_in_listing_and_registry() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "default-skill", "default behaviour");
        write_file(
            &dir.path().join("model-hidden").join("SKILL.md"),
            "---\ndescription: hidden from model\ndisable-model-invocation: true\n---\n",
        );
        write_file(
            &dir.path().join("background").join("SKILL.md"),
            "---\ndescription: background only\nuser-invocable: false\n---\n",
        );

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        let listing = catalog.system_prompt_listing();
        let mut registry = SlashCommandRegistry::new();
        catalog.register_slash_commands(&mut registry);

        // Default: in listing + registered.
        assert!(
            listing.contains("- default-skill: default behaviour"),
            "default skill must appear in listing, got: {listing}"
        );
        assert!(registry.get("default-skill").is_some());

        // disable-model-invocation: registered but not in listing.
        assert!(
            !listing.contains("model-hidden"),
            "model-hidden must not appear in listing, got: {listing}"
        );
        assert!(registry.get("model-hidden").is_some());

        // user-invocable: false — in listing but not registered.
        assert!(
            listing.contains("- background: background only"),
            "background skill must appear in listing, got: {listing}"
        );
        assert!(registry.get("background").is_none());
    }

    #[test]
    fn system_prompt_listing_has_no_trailing_newline() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "alpha", "first");
        write_skill(dir.path(), "beta", "second");

        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
        let listing = catalog.system_prompt_listing();
        assert!(!listing.is_empty());
        assert!(
            !listing.ends_with('\n'),
            "listing should not end with newline, ends with: {:?}",
            listing.chars().last()
        );
    }
}
