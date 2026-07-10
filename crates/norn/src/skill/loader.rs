//! Skill file discovery and frontmatter parsing.
//!
//! Reads SKILL.md (or flat `<name>.md`) files from a list of search
//! directories and parses each into a [`LoadedSkill`].
//!
//! Validation is lenient by design (per norn-skills DESIGN.md §D2):
//! a name that mismatches the directory or exceeds 64 characters
//! produces a [`NornDiagnostic`] warning but the skill still loads; a
//! skill with a missing or empty description is *skipped* with a
//! diagnostic. Unknown frontmatter fields are silently ignored.
//!
//! The loader stays pure — it never reaches for a
//! [`crate::integration::diagnostics::DiagnosticCollector`] directly.
//! Diagnostics are carried on each [`LoadedSkill`] (warnings) or
//! returned alongside dropped files in [`DiscoveryResult::diagnostics`]
//! (skips/parse failures). NS-002's catalog drains those into the
//! shared collector.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::error::SkillError;
use crate::integration::diagnostics::{DiagnosticSeverity, NornDiagnostic};
use crate::skill::types::SkillMetadata;
use crate::util::frontmatter::{FrontmatterError, split_frontmatter};
use crate::util::{
    WorkspaceEntryKind, read_workspace_directory, read_workspace_text_file,
    validate_workspace_regular_file, workspace_relative_path,
};

const SOURCE_TOOL: &str = "skill";
const MAX_NAME_LEN: usize = 64;

/// A skill file successfully loaded from disk.
///
/// `diagnostics` carries any non-fatal warnings (name mismatch, name
/// length); skip-level diagnostics never reach a `LoadedSkill` because
/// the skill is omitted from discovery instead.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    /// Canonical name (frontmatter `name`, falling back to the directory
    /// basename or file stem).
    pub name: String,
    /// Path to the SKILL.md or `<name>.md` file on disk.
    pub path: PathBuf,
    /// Parsed frontmatter.
    pub metadata: SkillMetadata,
    /// Markdown body following the frontmatter, ownership-cloned from
    /// the file content.
    pub body: String,
    /// Warning-level diagnostics produced while loading this skill.
    pub diagnostics: Vec<NornDiagnostic>,
}

/// Aggregate result of scanning one or more directories.
///
/// `skills` are the loaded entries in directory-list / dir-form-first
/// order. `diagnostics` covers files that were *not* loaded
/// (missing description, unparseable YAML, IO failure) — warning
/// diagnostics for loaded skills live on [`LoadedSkill::diagnostics`].
#[derive(Debug, Default, Clone)]
pub struct DiscoveryResult {
    /// Loaded skills, de-duplicated by name (first-seen wins).
    pub skills: Vec<LoadedSkill>,
    /// Diagnostics for files that were skipped or failed to parse.
    pub diagnostics: Vec<NornDiagnostic>,
}

/// Discover all skills across `dirs` in priority order.
///
/// # Shadowing precedence
///
/// Precedence is fully deterministic — `read_dir`'s platform-dependent
/// ordering never decides a winner:
///
/// 1. Across directories, the earlier entry in `dirs` wins on name
///    collision (first-match-wins).
/// 2. Within a single directory, every `<name>/SKILL.md` (directory
///    form) beats every flat `<name>.md`.
/// 3. Within each of those two groups, candidates are visited in
///    lexicographic path order, so when two files declare the same
///    frontmatter `name`, the lexicographically smaller path wins.
///
/// The same ordering governs [`DiscoveryResult::skills`], which flows
/// into the system-prompt skill listing — a stable listing is required
/// for prompt caching.
///
/// Files that fail to load are dropped with a skip diagnostic attached
/// to [`DiscoveryResult::diagnostics`]. Missing or unreadable
/// directories are silently skipped (logged at `tracing::debug!` level)
/// so an absent `~/.norn/skills/` does not produce noise; unreadable
/// individual entries are logged at `tracing::warn!` — never silently
/// dropped.
///
/// # Trust boundary
///
/// This low-level compatibility API uses ordinary filesystem traversal and is
/// only for caller-trusted directories. Runtime workspace discovery must go
/// through `AgentBuilder` / `load_runtime_base`, which uses provenance-aware,
/// descriptor-relative no-follow reads.
#[must_use]
pub fn discover_skills(dirs: &[PathBuf]) -> DiscoveryResult {
    discover_skills_impl(dirs, None)
}

/// Discovers skills while treating paths beneath `workspace_root` as
/// repository-controlled inputs that cannot follow symlinks.
pub(crate) fn discover_skills_with_workspace(
    dirs: &[PathBuf],
    workspace_root: &Path,
) -> DiscoveryResult {
    discover_skills_impl(dirs, Some(workspace_root))
}

fn discover_skills_impl(dirs: &[PathBuf], workspace_root: Option<&Path>) -> DiscoveryResult {
    let mut skills: Vec<LoadedSkill> = Vec::new();
    let mut diagnostics: Vec<NornDiagnostic> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for dir in dirs {
        let workspace_relative = workspace_root.and_then(|root| workspace_relative_path(root, dir));
        let entries: Vec<(PathBuf, WorkspaceEntryKind)> = match (workspace_root, workspace_relative)
        {
            (Some(root), Some(relative)) => match read_workspace_directory(root, &relative) {
                Ok(entries) => entries
                    .into_iter()
                    .map(|entry| (dir.join(entry.name), entry.kind))
                    .collect(),
                Err(error) => {
                    diagnostics.push(skip_diagnostic(
                        dir,
                        "skill-workspace-root-refused",
                        format!("refused workspace skill directory: {error}"),
                    ));
                    continue;
                }
            },
            _ => match std::fs::read_dir(dir) {
                Ok(entries) => entries
                    .filter_map(|entry| {
                        let entry = entry.ok()?;
                        let kind = match entry.file_type().ok()? {
                            file_type if file_type.is_dir() => WorkspaceEntryKind::Directory,
                            file_type if file_type.is_file() => WorkspaceEntryKind::File,
                            _ => WorkspaceEntryKind::Other,
                        };
                        Some((entry.path(), kind))
                    })
                    .collect(),
                Err(error) => {
                    tracing::debug!("Skipping skill dir {} during scan: {error}", dir.display());
                    continue;
                }
            },
        };

        let mut dir_skill_paths: Vec<PathBuf> = Vec::new();
        let mut flat_skill_paths: Vec<PathBuf> = Vec::new();

        for (path, kind) in entries {
            if kind == WorkspaceEntryKind::Directory {
                let candidate = path.join("SKILL.md");
                let exists = workspace_root
                    .and_then(|root| {
                        workspace_relative_path(root, &candidate).map(|relative| {
                            validate_workspace_regular_file(root, &relative).is_ok()
                        })
                    })
                    .unwrap_or_else(|| candidate.is_file());
                if exists {
                    dir_skill_paths.push(candidate);
                }
            } else if kind == WorkspaceEntryKind::File {
                let is_md = path.extension().and_then(OsStr::to_str) == Some("md");
                let is_skill_md = path.file_name().and_then(OsStr::to_str) == Some("SKILL.md");
                if is_md && !is_skill_md {
                    flat_skill_paths.push(path);
                }
            }
        }

        // Deterministic within-directory order: `read_dir` order is
        // platform-dependent, which would make both the prompt listing
        // and same-name shadowing nondeterministic.
        dir_skill_paths.sort();
        flat_skill_paths.sort();

        // Directory-form first so that within a single directory the
        // dir form wins over a same-name flat .md file.
        for path in dir_skill_paths.into_iter().chain(flat_skill_paths) {
            let loaded = match workspace_root {
                Some(root) if workspace_relative_path(root, &path).is_some() => {
                    load_workspace_skill_from_path(root, &path)
                }
                Some(_) | None => load_skill_from_path(&path),
            };
            match loaded {
                Ok(skill) => {
                    if seen.insert(skill.name.clone()) {
                        skills.push(skill);
                    } else {
                        tracing::debug!(
                            "Skipping duplicate-name skill at {}: name '{}' already seen",
                            path.display(),
                            skill.name
                        );
                    }
                }
                Err(diag) => diagnostics.extend(diag),
            }
        }
    }

    DiscoveryResult {
        skills,
        diagnostics,
    }
}

/// Load a single skill file by path.
///
/// Returns `Ok(LoadedSkill)` for files that parse and have a
/// non-empty description (possibly carrying warning diagnostics).
/// Returns `Err(Vec<NornDiagnostic>)` for files that should be dropped:
/// missing description, unparseable YAML, frontmatter delimiter
/// failures, or IO errors. The returned diagnostics are file-scoped so
/// the caller can fold them into a collector.
///
/// # Errors
///
/// Returns a non-empty vector of diagnostics describing why the file
/// was dropped.
pub fn load_skill_from_path(path: &Path) -> Result<LoadedSkill, Vec<NornDiagnostic>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return Err(vec![skip_diagnostic(
                path,
                "skill-io-error",
                format!("failed to read skill file: {e}"),
            )]);
        }
    };

    parse_loaded_skill(path, &content)
}

/// Loads a repository-controlled skill without following symlinks.
pub(crate) fn load_workspace_skill_from_path(
    workspace_root: &Path,
    path: &Path,
) -> Result<LoadedSkill, Vec<NornDiagnostic>> {
    let relative = workspace_relative_path(workspace_root, path).ok_or_else(|| {
        vec![skip_diagnostic(
            path,
            "skill-workspace-escape",
            "skill path escaped its workspace root".to_owned(),
        )]
    })?;
    let content = read_workspace_text_file(workspace_root, &relative)
        .map_err(|error| {
            vec![skip_diagnostic(
                path,
                "skill-io-error",
                format!("refused workspace skill file: {error}"),
            )]
        })?
        .content;
    parse_loaded_skill(path, &content)
}

fn parse_loaded_skill(path: &Path, content: &str) -> Result<LoadedSkill, Vec<NornDiagnostic>> {
    let default_name = match infer_default_name(path) {
        Ok(name) => name,
        Err(reason) => {
            return Err(vec![skip_diagnostic(path, "skill-invalid-path", reason)]);
        }
    };

    parse_skill_content(path, &default_name, content)
}

/// Parse skill content already read from disk.
///
/// Split out from [`load_skill_from_path`] so unit tests can exercise
/// parsing without touching the filesystem.
///
/// `default_name` is the fallback applied when the YAML omits `name`;
/// callers typically derive it from the file path via
/// [`infer_default_name`].
///
/// # Errors
///
/// Returns a non-empty vector of skip diagnostics if the file cannot
/// produce a usable skill (missing/empty description, unparseable
/// frontmatter, malformed delimiters).
pub fn parse_skill_content(
    path: &Path,
    default_name: &str,
    content: &str,
) -> Result<LoadedSkill, Vec<NornDiagnostic>> {
    let (yaml, body) = match split_frontmatter(content) {
        Ok(parts) => parts,
        Err(e) => {
            let code = match &e {
                FrontmatterError::MissingOpening { .. } => "skill-missing-frontmatter",
                FrontmatterError::MissingClosing { .. } => "skill-unterminated-frontmatter",
            };
            let err: SkillError = e.into();
            return Err(vec![skip_diagnostic(path, code, err.to_string())]);
        }
    };

    let mut metadata: SkillMetadata = if yaml.trim().is_empty() {
        SkillMetadata::default()
    } else {
        match serde_yaml::from_str(yaml) {
            Ok(m) => m,
            Err(e) => {
                return Err(vec![skip_diagnostic(
                    path,
                    "skill-yaml-parse-failed",
                    format!("invalid YAML frontmatter: {e}"),
                )]);
            }
        }
    };

    let description_present = metadata
        .description
        .as_ref()
        .is_some_and(|d| !d.trim().is_empty());
    if !description_present {
        return Err(vec![skip_diagnostic(
            path,
            "skill-missing-description",
            "skill is missing a description and was skipped".to_owned(),
        )]);
    }

    let explicit_name = metadata
        .name
        .as_ref()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());

    let resolved_name = explicit_name
        .clone()
        .unwrap_or_else(|| default_name.to_owned());

    let mut diagnostics: Vec<NornDiagnostic> = Vec::new();

    if let Some(explicit) = explicit_name.as_deref()
        && explicit != default_name
    {
        diagnostics.push(NornDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "skill-name-mismatch".to_owned(),
            message: format!(
                "skill name '{explicit}' does not match directory/file name '{default_name}'",
            ),
            source_tool: Some(SOURCE_TOOL.to_owned()),
            file_path: Some(path.display().to_string()),
            suggestion: Some(format!(
                "rename the skill or set name: {default_name} in frontmatter"
            )),
        });
    }

    if resolved_name.chars().count() > MAX_NAME_LEN {
        diagnostics.push(NornDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "skill-name-too-long".to_owned(),
            message: format!("skill name '{resolved_name}' exceeds {MAX_NAME_LEN} characters"),
            source_tool: Some(SOURCE_TOOL.to_owned()),
            file_path: Some(path.display().to_string()),
            suggestion: Some("shorten the skill name to 64 characters or fewer".to_owned()),
        });
    }

    metadata.name = Some(resolved_name.clone());

    Ok(LoadedSkill {
        name: resolved_name,
        path: path.to_path_buf(),
        metadata,
        body: body.to_owned(),
        diagnostics,
    })
}

/// Derive the default skill name from a file path.
///
/// - `<dir>/SKILL.md` → name from the parent directory's basename.
/// - `<dir>/<stem>.md` → name from the file's stem.
///
/// # Errors
///
/// Returns a description of the failure when the path lacks a valid
/// UTF-8 file name or parent.
pub fn infer_default_name(path: &Path) -> Result<String, String> {
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| format!("path '{}' has no file name", path.display()))?;

    if file_name == "SKILL.md" {
        let parent = path
            .parent()
            .ok_or_else(|| format!("path '{}' has no parent directory", path.display()))?;
        let parent_name = parent
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| format!("parent directory of '{}' has no UTF-8 name", path.display()))?;
        if parent_name.is_empty() {
            return Err(format!(
                "parent directory name for '{}' is empty",
                path.display()
            ));
        }
        return Ok(parent_name.to_owned());
    }

    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| format!("path '{}' has no file stem", path.display()))?;
    if stem.is_empty() {
        return Err(format!("file stem for '{}' is empty", path.display()));
    }
    Ok(stem.to_owned())
}

fn skip_diagnostic(path: &Path, code: &str, message: String) -> NornDiagnostic {
    NornDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: code.to_owned(),
        message,
        source_tool: Some(SOURCE_TOOL.to_owned()),
        file_path: Some(path.display().to_string()),
        suggestion: None,
    }
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

    use tempfile::tempdir;

    use super::*;

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn infer_default_name_from_skill_md_uses_directory() {
        let p = Path::new("/tmp/my-skill/SKILL.md");
        assert_eq!(infer_default_name(p).unwrap(), "my-skill");
    }

    #[test]
    fn infer_default_name_from_flat_md_uses_stem() {
        let p = Path::new("/tmp/deploy.md");
        assert_eq!(infer_default_name(p).unwrap(), "deploy");
    }

    #[test]
    fn parse_skill_content_minimal() {
        let yaml = "---\ndescription: do thing\n---\nbody\n";
        let skill = parse_skill_content(Path::new("/tmp/my-skill/SKILL.md"), "my-skill", yaml)
            .expect("parses");
        assert_eq!(skill.name, "my-skill");
        // R6: metadata.name is filled with the defaulted name when absent.
        assert_eq!(skill.metadata.name.as_deref(), Some("my-skill"));
        assert_eq!(skill.metadata.description.as_deref(), Some("do thing"));
        assert_eq!(skill.body, "body\n");
        assert!(skill.diagnostics.is_empty());
    }

    #[test]
    fn parse_skill_content_flat_md_defaults_metadata_name_to_stem() {
        let yaml = "---\ndescription: deploy\n---\nbody\n";
        let skill =
            parse_skill_content(Path::new("/tmp/deploy.md"), "deploy", yaml).expect("parses");
        assert_eq!(skill.metadata.name.as_deref(), Some("deploy"));
    }

    #[test]
    fn parse_skill_content_explicit_name_appears_in_metadata() {
        let yaml = "---\nname: custom\ndescription: hi\n---\nbody\n";
        let skill =
            parse_skill_content(Path::new("/tmp/custom/SKILL.md"), "custom", yaml).expect("parses");
        assert_eq!(skill.metadata.name.as_deref(), Some("custom"));
        assert!(skill.diagnostics.is_empty());
    }

    #[test]
    fn parse_skill_content_with_explicit_name_overrides_default() {
        let yaml = "---\nname: custom\ndescription: hi\n---\nbody\n";
        let skill =
            parse_skill_content(Path::new("/tmp/foo/SKILL.md"), "foo", yaml).expect("parses");
        assert_eq!(skill.name, "custom");
        // Name does not match directory → warning diagnostic emitted.
        assert!(
            skill
                .diagnostics
                .iter()
                .any(|d| d.code == "skill-name-mismatch"),
            "expected skill-name-mismatch diagnostic, got {:?}",
            skill.diagnostics
        );
    }

    #[test]
    fn parse_skill_content_missing_description_is_skipped() {
        let yaml = "---\nname: foo\n---\nbody\n";
        let err =
            parse_skill_content(Path::new("/tmp/foo/SKILL.md"), "foo", yaml).expect_err("skipped");
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].code, "skill-missing-description");
    }

    #[test]
    fn parse_skill_content_empty_description_is_skipped() {
        let yaml = "---\nname: foo\ndescription: \"\"\n---\nbody\n";
        let err =
            parse_skill_content(Path::new("/tmp/foo/SKILL.md"), "foo", yaml).expect_err("skipped");
        assert_eq!(err[0].code, "skill-missing-description");
    }

    #[test]
    fn parse_skill_content_long_name_warns_but_loads() {
        let long = "a".repeat(80);
        let yaml = format!("---\nname: {long}\ndescription: hi\n---\n");
        let skill = parse_skill_content(Path::new("/tmp/short/SKILL.md"), "short", &yaml)
            .expect("loads despite long name");
        assert_eq!(skill.name, long);
        assert!(
            skill
                .diagnostics
                .iter()
                .any(|d| d.code == "skill-name-too-long"),
            "expected skill-name-too-long, got {:?}",
            skill.diagnostics
        );
    }

    #[test]
    fn parse_skill_content_unknown_fields_are_ignored() {
        let yaml = "---\ndescription: hi\nfoo: bar\nbaz: [1, 2]\n---\nbody\n";
        let skill = parse_skill_content(Path::new("/tmp/x/SKILL.md"), "x", yaml)
            .expect("parses with unknown fields");
        assert_eq!(skill.body, "body\n");
    }

    #[test]
    fn parse_skill_content_unparseable_yaml_is_skipped() {
        let yaml = "---\ndescription: : : invalid\nname: [unclosed\n---\n";
        let err =
            parse_skill_content(Path::new("/tmp/x/SKILL.md"), "x", yaml).expect_err("yaml fails");
        assert_eq!(err[0].code, "skill-yaml-parse-failed");
    }

    #[test]
    fn parse_skill_content_missing_frontmatter_is_skipped() {
        let yaml = "no frontmatter here\n";
        let err = parse_skill_content(Path::new("/tmp/x/SKILL.md"), "x", yaml)
            .expect_err("no frontmatter");
        assert_eq!(err[0].code, "skill-missing-frontmatter");
    }

    #[test]
    fn load_skill_from_path_directory_form() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("my-skill");
        let path = skill_dir.join("SKILL.md");
        write_file(&path, "---\ndescription: hi\n---\nbody\n");

        let skill = load_skill_from_path(&path).expect("loads");
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.body, "body\n");
    }

    #[test]
    fn load_skill_from_path_flat_md_defaults_name_to_stem() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("deploy.md");
        write_file(&path, "---\ndescription: deploy\n---\nbody\n");

        let skill = load_skill_from_path(&path).expect("loads");
        assert_eq!(skill.name, "deploy");
    }

    #[test]
    fn discover_skills_prefers_directory_form_over_flat() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("my-skill");
        write_file(
            &skill_dir.join("SKILL.md"),
            "---\ndescription: dir form\n---\n",
        );
        write_file(
            &dir.path().join("my-skill.md"),
            "---\ndescription: flat form\n---\n",
        );

        let result = discover_skills(&[dir.path().to_path_buf()]);
        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].name, "my-skill");
        assert_eq!(
            result.skills[0].metadata.description.as_deref(),
            Some("dir form")
        );
    }

    #[test]
    fn discover_skills_first_match_wins_across_dirs() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        write_file(
            &dir_a.path().join("shared.md"),
            "---\ndescription: from A\n---\n",
        );
        write_file(
            &dir_b.path().join("shared.md"),
            "---\ndescription: from B\n---\n",
        );

        let result = discover_skills(&[dir_a.path().to_path_buf(), dir_b.path().to_path_buf()]);
        assert_eq!(result.skills.len(), 1);
        assert_eq!(
            result.skills[0].metadata.description.as_deref(),
            Some("from A")
        );
    }

    /// Discovery order must be lexicographic within a directory (dir
    /// forms first, then flat files) regardless of `read_dir`'s
    /// platform-dependent order — the system-prompt listing is built
    /// from this order and must be stable for prompt caching.
    #[test]
    fn discover_skills_orders_deterministically_within_a_directory() {
        let dir = tempdir().unwrap();
        // Written in a scrambled order on purpose.
        write_file(
            &dir.path().join("zeta").join("SKILL.md"),
            "---\ndescription: z\n---\n",
        );
        write_file(&dir.path().join("beta.md"), "---\ndescription: fb\n---\n");
        write_file(
            &dir.path().join("alpha").join("SKILL.md"),
            "---\ndescription: a\n---\n",
        );
        write_file(&dir.path().join("acme.md"), "---\ndescription: fa\n---\n");

        let result = discover_skills(&[dir.path().to_path_buf()]);
        let names: Vec<&str> = result.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["alpha", "zeta", "acme", "beta"],
            "dir forms sorted first, then flat files sorted",
        );
    }

    /// Same-name shadowing within a directory has a deterministic
    /// winner: two flat files declaring the same frontmatter `name`
    /// resolve to the lexicographically smaller path.
    #[test]
    fn discover_skills_same_name_shadowing_is_deterministic() {
        let dir = tempdir().unwrap();
        write_file(
            &dir.path().join("bbb.md"),
            "---\nname: clash\ndescription: from bbb\n---\n",
        );
        write_file(
            &dir.path().join("aaa.md"),
            "---\nname: clash\ndescription: from aaa\n---\n",
        );

        let result = discover_skills(&[dir.path().to_path_buf()]);
        assert_eq!(result.skills.len(), 1);
        assert_eq!(
            result.skills[0].metadata.description.as_deref(),
            Some("from aaa"),
            "lexicographically smaller path must win the name clash",
        );
    }

    #[test]
    fn discover_skills_skips_missing_description() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("incomplete");
        write_file(
            &skill_dir.join("SKILL.md"),
            "---\nname: incomplete\n---\nbody\n",
        );

        let result = discover_skills(&[dir.path().to_path_buf()]);
        assert!(result.skills.is_empty());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "skill-missing-description"),
            "expected skill-missing-description, got {:?}",
            result.diagnostics
        );
    }

    #[test]
    fn discover_skills_skips_missing_directories_silently() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope");
        let result = discover_skills(&[missing]);
        assert!(result.skills.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn discover_skills_returns_metadata_path_body_tuples() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("good");
        write_file(
            &skill_dir.join("SKILL.md"),
            "---\ndescription: thing\n---\nbody one\n",
        );
        write_file(
            &dir.path().join("alias.md"),
            "---\ndescription: alias\n---\nbody two\n",
        );

        let result = discover_skills(&[dir.path().to_path_buf()]);
        let names: Vec<_> = result.skills.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"good".to_owned()));
        assert!(names.contains(&"alias".to_owned()));
        for s in &result.skills {
            assert!(s.path.is_file());
            assert!(s.metadata.description.is_some());
            assert!(!s.body.is_empty());
        }
    }
}
