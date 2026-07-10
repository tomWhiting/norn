//! Markdown + YAML frontmatter parser for Norn profiles and capabilities.
//!
//! The format mirrors the existing Meridian profiles at `.meridian/profiles/`:
//! a leading `---` line, a YAML block, a closing `---` line, and a markdown
//! body that becomes the agent's base system instruction.
//!
//! Self-contained: this module does NOT import any parsing helpers from
//! `claude-runner` (per brief Boundary). The shape of
//! [`crate::util::frontmatter::split_frontmatter`], `ToolsValue`, and
//! `split_comma_paren_aware` is modelled after the reference
//! implementation in `claude-runner/src/capabilities/parser.rs` but
//! written fresh against Norn's [`Profile`] / [`Capability`] types and
//! the project-wide [`ConfigError`] error type.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::types::{Capability, Profile};
use crate::error::ConfigError;
use crate::provider::request::{ReasoningEffort, ReasoningSummary, ServiceTier};
use crate::util::read_workspace_text_file;

/// File extensions checked by [`Scanner::resolve`], in priority order.
///
/// Markdown (the canonical NA-001 format) wins; `.toml` and `.json` remain
/// for backwards compatibility with profiles written under the previous
/// `~/.norn/config/profiles/` regime.
const PROFILE_EXTENSIONS: [&str; 3] = ["md", "toml", "json"];

/// Returns `true` when `name` is safe to use as a profile filename stem.
///
/// Bare names use a conservative visible ASCII alphabet: letters, digits,
/// hyphens, and underscores. Used by [`Scanner::resolve`] and
/// [`Scanner::list_profiles`] to reject path traversal and diagnostic control
/// characters before they touch the filesystem or an error surface.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Two-tier profile discovery: walks an ordered list of directories and
/// returns the first match for a requested profile name.
///
/// Within each directory the extension search order is `.md` → `.toml` →
/// `.json`. Across directories, the first match wins — so a workspace
/// profile shadows a user-level profile of the same name. The higher-level
/// [`resolve_workspace_profile`] API retains that origin and rejects automatic
/// commands from workspace profiles.
///
/// Construct via [`Scanner::new`]; the typical scan-dir list comes from
/// [`default_scan_dirs`]. Non-existent directories are silently skipped
/// (debug-logged, not errored) to match the claude-runner reference
/// implementation.
///
/// # Trust boundary
///
/// `Scanner` is a low-level compatibility API for caller-trusted directories.
/// It uses ordinary filesystem metadata and does not retain workspace origin.
/// Use [`resolve_workspace_profile`] or `AgentBuilder` for bare names that can
/// resolve beneath a repository.
pub struct Scanner {
    /// Ordered list of directories to search. First match wins across this
    /// list; `.md` beats `.toml` beats `.json` within each directory.
    scan_dirs: Vec<PathBuf>,
}

impl Scanner {
    /// Constructs a scanner that walks `scan_dirs` in the given order.
    ///
    /// The caller owns ordering: workspace-level directories should appear
    /// before user-level ones so workspace profiles win on name collision.
    #[must_use]
    pub fn new(scan_dirs: Vec<PathBuf>) -> Self {
        Self { scan_dirs }
    }

    /// Locates the first profile file named `name` across the scan dirs.
    ///
    /// Returns `Some(path)` for the first `{dir}/{name}.{md|toml|json}`
    /// that exists, checking `.md` before `.toml` before `.json` within
    /// each directory and walking directories in their configured order.
    /// Returns `None` when `name` is unsafe (path-traversal check) or no
    /// match is found anywhere.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<PathBuf> {
        self.resolve_with_directory_index(name)
            .map(|resolved| resolved.0)
    }

    fn resolve_with_directory_index(&self, name: &str) -> Option<(PathBuf, usize)> {
        if !is_safe_name(name) {
            return None;
        }
        for (index, dir) in self.scan_dirs.iter().enumerate() {
            for ext in PROFILE_EXTENSIONS {
                let candidate = dir.join(format!("{name}.{ext}"));
                match std::fs::symlink_metadata(&candidate) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Ok(_) | Err(_) => return Some((candidate, index)),
                }
            }
        }
        None
    }

    /// Returns the deduplicated, sorted list of profile names discovered
    /// across the scan dirs.
    ///
    /// Walks directories in their configured order; within each directory,
    /// `.md`/`.toml`/`.json` stems are gathered. First occurrence wins on
    /// name collision (workspace profiles shadow user-level profiles).
    /// Unsafe names are skipped. Non-existent directories are skipped
    /// silently. The returned vector is sorted alphabetically.
    #[must_use]
    pub fn list_profiles(&self) -> Vec<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut names: Vec<String> = Vec::new();
        for dir in &self.scan_dirs {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::debug!("Skipping profile dir {} during list: {e}", dir.display());
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!("Error reading directory entry in {}: {e}", dir.display());
                        continue;
                    }
                };
                let path = entry.path();
                let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                    continue;
                };
                if !PROFILE_EXTENSIONS.contains(&ext) {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if !is_safe_name(stem) {
                    tracing::warn!("Skipping unsafe profile filename: {}", path.display());
                    continue;
                }
                if seen.insert(stem.to_owned()) {
                    names.push(stem.to_owned());
                }
            }
        }
        names.sort();
        names
    }
}

/// Trust origin of a profile resolved through [`default_scan_dirs`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileOrigin {
    /// Profile file came from `.norn/profiles` or `.meridian/profiles` under
    /// the active working directory.
    WorkingDirectory,
    /// Profile file came from the user profile directory.
    User,
}

/// A workspace-aware profile resolution that retains its trust origin.
pub struct ResolvedWorkspaceProfile {
    /// Parsed profile with referenced capabilities resolved.
    pub profile: Profile,
    /// Trust origin of the profile file.
    pub origin: ProfileOrigin,
}

/// The default profile scan order for a workspace rooted at `cwd`.
///
/// Returns three candidate directories in priority order:
///
/// 1. `{cwd}/.norn/profiles/` — workspace-level Norn profiles (highest).
/// 2. `{cwd}/.meridian/profiles/` — Meridian-integrated workspaces.
/// 3. `{NORN_HOME|~/.norn}/profiles/` — user-level fallback, resolved via
///    [`crate::config::paths::profiles_dir`] so the `NORN_HOME` override
///    is honoured exactly like every other `~/.norn/` path.
///
/// The user-level entry is omitted entirely when neither `NORN_HOME` nor
/// the home directory resolves. Existence-filtering is [`Scanner`]'s
/// responsibility — this function only produces the ordered candidate
/// list.
#[must_use]
pub fn default_scan_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        cwd.join(".norn").join("profiles"),
        cwd.join(".meridian").join("profiles"),
    ];
    if let Some(user) = crate::config::paths::profiles_dir() {
        dirs.push(user);
    }
    dirs
}

/// Derive the capability search directories from a profile scan-dir list.
///
/// Capabilities live in a `capabilities/` directory that is a sibling of
/// each `profiles/` directory (`{prefix}/profiles/` ↔
/// `{prefix}/capabilities/`), matching both the norn-agents `DESIGN.md`
/// layout (`~/.norn/capabilities/`) and the claude-runner reference
/// scanner. Scan dirs without a parent directory are skipped — they
/// cannot have a sibling.
#[must_use]
pub fn capability_scan_dirs(profile_scan_dirs: &[PathBuf]) -> Vec<PathBuf> {
    profile_scan_dirs
        .iter()
        .filter_map(|dir| dir.parent().map(|parent| parent.join("capabilities")))
        .collect()
}

/// Resolve a capability by bare name across the capability scan dirs.
///
/// Capabilities are markdown-only (`{dir}/{name}.md` — the NA-001 format);
/// the first directory with a match wins, mirroring profile shadowing.
///
/// # Errors
///
/// Returns [`ConfigError::InvalidConfig`] enumerating every searched
/// directory when no match is found (a capability reference is a
/// restriction surface — it must never be silently dropped), and
/// propagates parse errors from [`parse_capability`].
pub fn resolve_capability(name: &str, scan_dirs: &[PathBuf]) -> Result<Capability, ConfigError> {
    resolve_capability_in_dirs(name, scan_dirs, None)
}

fn resolve_capability_in_dirs(
    name: &str,
    scan_dirs: &[PathBuf],
    workspace_root: Option<&Path>,
) -> Result<Capability, ConfigError> {
    if !is_safe_name(name) {
        return Err(ConfigError::InvalidConfig {
            reason: "capability name is not a safe file stem".to_owned(),
        });
    }
    for dir in scan_dirs {
        let candidate = dir.join(format!("{name}.md"));
        if let Some(root) = workspace_root {
            let relative =
                candidate
                    .strip_prefix(root)
                    .map_err(|error| ConfigError::InvalidConfig {
                        reason: format!(
                            "working-directory capability escaped its workspace root: {error}"
                        ),
                    })?;
            let contents = match read_workspace_text_file(root, relative) {
                Ok(file) => file.content,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(ConfigError::InvalidConfig {
                        reason: format!(
                            "refused working-directory capability at {}: {error}",
                            candidate.display(),
                        ),
                    });
                }
            };
            return parse_capability(&contents, &candidate);
        }
        if candidate.is_file() {
            let contents = std::fs::read_to_string(&candidate).map_err(|error| {
                ConfigError::InvalidConfig {
                    reason: format!(
                        "failed to read capability at {}: {error}",
                        candidate.display(),
                    ),
                }
            })?;
            return parse_capability(&contents, &candidate);
        }
    }
    let searched = scan_dirs
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(ConfigError::InvalidConfig {
        reason: format!("capability '{name}' not found; searched: {searched}"),
    })
}

/// Loads a profile by bare name from the given scan directories,
/// resolving any `capabilities:` frontmatter references.
///
/// Walks `scan_dirs` via [`Scanner::resolve`] and dispatches the resolved
/// path through [`Profile::from_file`] (which itself dispatches on
/// extension: `.md` → frontmatter, `.toml` → toml, `.json` → JSON). Each
/// name in [`Profile::capability_names`] is then resolved against the
/// sibling `capabilities/` directories (see [`capability_scan_dirs`]) and
/// appended to [`Profile::capabilities`], so capability-declared tools,
/// instructions, and `disallowedTools` all flow through the standard
/// `resolved_*` merge (norn-agents C10).
///
/// # Errors
///
/// Returns [`ConfigError::InvalidConfig`] with a `reason` that enumerates
/// every searched directory when the profile — or any referenced
/// capability — is not found; propagates any parse error from
/// [`Profile::from_file`] or [`parse_capability`] otherwise. An
/// unresolvable capability is an error rather than a warning because
/// capabilities carry `disallowedTools` restrictions that must never be
/// silently dropped.
///
/// # Trust boundary
///
/// `scan_dirs` must be caller-trusted. This compatibility API uses the
/// unrestricted [`Scanner`]; repository-aware callers should use
/// [`resolve_workspace_profile`] or shared runtime assembly instead.
pub fn resolve_profile(name: &str, scan_dirs: &[PathBuf]) -> Result<Profile, ConfigError> {
    if !is_safe_name(name) {
        return Err(ConfigError::InvalidConfig {
            reason: "profile name is not a safe file stem".to_owned(),
        });
    }
    let scanner = Scanner::new(scan_dirs.to_vec());
    let Some(path) = scanner.resolve(name) else {
        let searched = scan_dirs
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ConfigError::InvalidConfig {
            reason: format!("profile '{name}' not found; searched: {searched}"),
        });
    };
    load_profile(path, scan_dirs)
}

/// Resolves a bare profile name using the standard workspace/user tiers while
/// retaining source trust and rejecting automatic commands from workspace
/// profiles.
///
/// # Errors
///
/// Returns [`ConfigError::InvalidConfig`] when the profile cannot be found,
/// parsed, or resolved, or when a working-directory profile declares a prompt
/// command. User-level profiles retain prompt-command support.
pub fn resolve_workspace_profile(
    name: &str,
    cwd: &Path,
) -> Result<ResolvedWorkspaceProfile, ConfigError> {
    let cwd = cwd
        .canonicalize()
        .map_err(|error| ConfigError::InvalidConfig {
            reason: format!("failed to resolve the profile workspace trust root: {error}"),
        })?;
    resolve_workspace_profile_at_launch_root(name, &cwd)
}

/// Resolves a profile from an already-canonical immutable launch root.
pub(crate) fn resolve_workspace_profile_at_launch_root(
    name: &str,
    cwd: &Path,
) -> Result<ResolvedWorkspaceProfile, ConfigError> {
    let scan_dirs = default_scan_dirs(cwd);
    if !is_safe_name(name) {
        return Err(ConfigError::InvalidConfig {
            reason: "profile name is not a safe file stem".to_owned(),
        });
    }
    let mut resolved = None;
    for (directory_index, directory) in scan_dirs.iter().enumerate() {
        for extension in PROFILE_EXTENSIONS {
            let path = directory.join(format!("{name}.{extension}"));
            if directory_index < 2 {
                let relative =
                    path.strip_prefix(cwd)
                        .map_err(|error| ConfigError::InvalidConfig {
                            reason: format!(
                                "working-directory profile escaped its workspace root: {error}"
                            ),
                        })?;
                match read_workspace_text_file(cwd, relative) {
                    Ok(file) => {
                        resolved = Some((path, directory_index, Some(file.content)));
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(ConfigError::InvalidConfig {
                            reason: format!(
                                "refused working-directory profile at {}: {error}",
                                path.display(),
                            ),
                        });
                    }
                }
            } else if path.is_file() {
                resolved = Some((path, directory_index, None));
                break;
            }
        }
        if resolved.is_some() {
            break;
        }
    }
    let Some((path, directory_index, workspace_contents)) = resolved else {
        let searched = scan_dirs
            .iter()
            .map(|candidate| candidate.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ConfigError::InvalidConfig {
            reason: format!("profile '{name}' not found; searched: {searched}"),
        });
    };
    let origin = if directory_index < 2 {
        ProfileOrigin::WorkingDirectory
    } else {
        ProfileOrigin::User
    };
    let profile = if origin == ProfileOrigin::WorkingDirectory {
        let contents = workspace_contents.ok_or_else(|| ConfigError::InvalidConfig {
            reason: "working-directory profile lost its securely read contents".to_owned(),
        })?;
        Profile::from_contents(&path, &contents)?
    } else {
        Profile::from_file(&path)?
    };
    if origin == ProfileOrigin::WorkingDirectory && !profile.prompt_commands.is_empty() {
        return Err(ConfigError::InvalidConfig {
            reason: "working-directory profiles cannot declare prompt_commands because repository profiles cannot execute automatic commands; use a user profile or remove prompt_commands"
                .to_owned(),
        });
    }
    let mut profile = profile;
    let capability_dirs = capability_scan_dirs(&scan_dirs);
    match origin {
        ProfileOrigin::WorkingDirectory => {
            let workspace_dirs = capability_dirs.into_iter().take(2).collect::<Vec<_>>();
            resolve_profile_capabilities_in_dirs(&mut profile, &workspace_dirs, Some(cwd))?;
        }
        ProfileOrigin::User => {
            let user_dirs = capability_dirs.into_iter().skip(2).collect::<Vec<_>>();
            resolve_profile_capabilities_in_dirs(&mut profile, &user_dirs, None)?;
        }
    }
    Ok(ResolvedWorkspaceProfile { profile, origin })
}

fn load_profile(path: PathBuf, scan_dirs: &[PathBuf]) -> Result<Profile, ConfigError> {
    let mut profile = Profile::from_file(path)?;
    resolve_profile_capabilities(&mut profile, scan_dirs)?;
    Ok(profile)
}

/// Resolve every pending [`Profile::capability_names`] entry into a
/// [`Capability`] appended to [`Profile::capabilities`].
///
/// Names are searched in the sibling `capabilities/` directories of
/// `profile_scan_dirs`. Duplicate references to a capability already
/// resolved (by name) are skipped so a profile listing the same
/// capability twice does not double its contribution.
///
/// # Errors
///
/// Propagates [`resolve_capability`] errors — an unresolvable name is a
/// typed error, never a dropped restriction.
pub fn resolve_profile_capabilities(
    profile: &mut Profile,
    profile_scan_dirs: &[PathBuf],
) -> Result<(), ConfigError> {
    let capability_dirs = capability_scan_dirs(profile_scan_dirs);
    resolve_profile_capabilities_in_dirs(profile, &capability_dirs, None)
}

fn resolve_profile_capabilities_in_dirs(
    profile: &mut Profile,
    capability_dirs: &[PathBuf],
    workspace_root: Option<&Path>,
) -> Result<(), ConfigError> {
    if profile.capability_names.is_empty() {
        return Ok(());
    }
    let names = std::mem::take(&mut profile.capability_names);
    for name in names {
        if profile.capabilities.iter().any(|c| c.name == name) {
            continue;
        }
        let capability = resolve_capability_in_dirs(&name, capability_dirs, workspace_root)?;
        profile.capabilities.push(capability);
    }
    Ok(())
}

/// The `tools` and `disallowedTools` frontmatter fields can be either a
/// YAML sequence or a comma-separated string. Both forms normalise to
/// `Vec<String>` via [`Self::into_vec`].
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ToolsValue {
    /// YAML list form: `tools:\n  - Read\n  - Grep`.
    List(Vec<String>),
    /// Comma-separated form: `tools: "Read, Bash(cargo build*)"`.
    CommaSeparated(String),
}

impl ToolsValue {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::List(items) => items
                .into_iter()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect(),
            Self::CommaSeparated(s) => split_comma_paren_aware(&s),
        }
    }
}

/// Split a comma-separated string into items, treating commas inside
/// parenthesised groups as literal characters rather than delimiters.
///
/// This is essential for the Meridian-style tool patterns like
/// `Bash(cargo build*)` and `Task(general-purpose, explorer)` where the
/// parenthesised content is part of the tool spec.
pub(super) fn split_comma_paren_aware(input: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut depth: u32 = 0;

    for ch in input.chars() {
        match ch {
            '(' => {
                depth = depth.saturating_add(1);
                current.push(ch);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if depth == 0 => {
                let trimmed = current.trim().to_owned();
                if !trimmed.is_empty() {
                    result.push(trimmed);
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    let trimmed = current.trim().to_owned();
    if !trimmed.is_empty() {
        result.push(trimmed);
    }
    result
}

/// Intermediate deserialisation struct for a Norn profile's YAML
/// frontmatter. `description` is accepted for compatibility with the 31
/// existing Meridian profiles but is dropped during mapping — norn's
/// [`Profile`] has no description field.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawNornProfileFrontmatter {
    name: Option<String>,
    description: Option<String>,
    model: Option<String>,
    tools: Option<ToolsValue>,
    #[serde(alias = "disallowedTools")]
    disallowed_tools: Option<ToolsValue>,
    reasoning_effort: Option<ReasoningEffort>,
    reasoning_summary: Option<ReasoningSummary>,
    service_tier: Option<ServiceTier>,
    capabilities: Option<ToolsValue>,
}

/// Intermediate deserialisation struct for a capability's YAML
/// frontmatter. `description` is accepted but dropped during mapping —
/// norn's [`Capability`] has no description field.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawCapabilityFrontmatter {
    name: Option<String>,
    description: Option<String>,
    tools: Option<ToolsValue>,
    #[serde(alias = "disallowedTools")]
    disallowed_tools: Option<ToolsValue>,
}

/// Parse a markdown profile file (frontmatter + body) into a [`Profile`].
///
/// `path` is used only for error messages. The markdown body (trimmed)
/// becomes the first entry in [`Profile::system_instructions`]. The raw
/// `description` field is accepted but not projected onto [`Profile`] —
/// norn has no description field. `capabilities: [name1, name2]` is
/// preserved on [`Profile::capability_names`] for
/// [`resolve_profile_capabilities`] to resolve against the capability
/// scan dirs. The `disallowedTools` field is preserved by appending a
/// synthetic [`Capability`] named `_profile_disallowed` to
/// [`Profile::capabilities`], so capability resolution still applies the
/// patterns downstream.
///
/// # Errors
///
/// Returns [`ConfigError::InvalidConfig`] for malformed frontmatter or
/// invalid YAML; returns [`ConfigError::MissingField`] when `name` or
/// `model` is missing or empty — matching the TOML/JSON forms, where
/// serde rejects a missing `model` at deserialisation.
pub fn parse_profile(content: &str, path: &Path) -> Result<Profile, ConfigError> {
    let (yaml, body) = crate::util::frontmatter::split_frontmatter(content).map_err(|e| {
        ConfigError::InvalidConfig {
            reason: e.to_string(),
        }
    })?;
    let raw: RawNornProfileFrontmatter =
        serde_yaml::from_str(yaml).map_err(|e| ConfigError::InvalidConfig {
            reason: format!("invalid YAML frontmatter in {}: {e}", path.display()),
        })?;

    let name = raw
        .name
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ConfigError::MissingField {
            field: "name".to_owned(),
        })?;

    let model = raw
        .model
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ConfigError::MissingField {
            field: "model".to_owned(),
        })?;

    let tools = raw.tools.map(ToolsValue::into_vec);

    let body_trim = body.trim();
    let system_instructions = if body_trim.is_empty() {
        Vec::new()
    } else {
        vec![body_trim.to_owned()]
    };

    // Synthetic capability preserves `disallowedTools` so capability
    // resolution (NA-006 spawn-with-profile) can honour them downstream.
    let mut capabilities: Vec<Capability> = Vec::new();
    if let Some(d) = raw.disallowed_tools {
        let patterns = d.into_vec();
        if !patterns.is_empty() {
            capabilities.push(Capability {
                name: "_profile_disallowed".to_owned(),
                tools: Vec::new(),
                system_instructions: Vec::new(),
                disallowed_patterns: patterns,
            });
        }
    }

    let capability_names = raw
        .capabilities
        .map(ToolsValue::into_vec)
        .unwrap_or_default();

    let _ = raw.description;

    Ok(Profile {
        name,
        model,
        reasoning_effort: raw.reasoning_effort,
        reasoning_summary: raw.reasoning_summary,
        service_tier: raw.service_tier,
        tools,
        system_instructions,
        capabilities,
        capability_names,
        settings: std::collections::HashMap::new(),
        prompt_commands: Vec::new(),
    })
}

/// Parse a markdown capability file (frontmatter + body) into a
/// [`Capability`].
///
/// The markdown body (trimmed) becomes the first entry in
/// [`Capability::system_instructions`]. The raw `description` field is
/// accepted but dropped during mapping — norn's [`Capability`] has no
/// description field.
///
/// # Errors
///
/// Returns [`ConfigError::InvalidConfig`] for malformed frontmatter or
/// invalid YAML; returns [`ConfigError::MissingField`] when `name` is
/// missing or empty.
pub fn parse_capability(content: &str, path: &Path) -> Result<Capability, ConfigError> {
    let (yaml, body) = crate::util::frontmatter::split_frontmatter(content).map_err(|e| {
        ConfigError::InvalidConfig {
            reason: e.to_string(),
        }
    })?;
    let raw: RawCapabilityFrontmatter =
        serde_yaml::from_str(yaml).map_err(|e| ConfigError::InvalidConfig {
            reason: format!("invalid YAML frontmatter in {}: {e}", path.display()),
        })?;

    let name = raw
        .name
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ConfigError::MissingField {
            field: "name".to_owned(),
        })?;

    let tools = raw.tools.map(ToolsValue::into_vec).unwrap_or_default();
    let disallowed_patterns = raw
        .disallowed_tools
        .map(ToolsValue::into_vec)
        .unwrap_or_default();

    let body_trim = body.trim();
    let system_instructions = if body_trim.is_empty() {
        Vec::new()
    } else {
        vec![body_trim.to_owned()]
    };

    let _ = raw.description;

    Ok(Capability {
        name,
        tools,
        system_instructions,
        disallowed_patterns,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::uninlined_format_args,
    clippy::needless_raw_string_hashes,
    unsafe_code
)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn p(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    // ── split_comma_paren_aware ────────────────────────────────────────

    #[test]
    fn split_comma_paren_aware_handles_parenthesised_tools() {
        // Covers parenthesised tools (Bash(cargo build*) etc.), nested
        // parens, and empty-item skipping in one fixture.
        let items = split_comma_paren_aware(
            "Read, Write, Task(general-purpose, explorer), Bash(cargo build*),   , A(B(C, D), E)",
        );
        assert_eq!(
            items,
            vec![
                "Read".to_owned(),
                "Write".to_owned(),
                "Task(general-purpose, explorer)".to_owned(),
                "Bash(cargo build*)".to_owned(),
                "A(B(C, D), E)".to_owned(),
            ]
        );
    }

    // ── parse_profile ──────────────────────────────────────────────────

    #[test]
    fn parse_profile_all_fields() {
        let content = r#"---
name: developer
description: Full-stack developer profile
model: gpt-5
tools: Read, Write, Bash(cargo build*)
disallowedTools: rm, shutdown
reasoning_effort: medium
reasoning_summary: concise
capabilities:
  - research
  - code-intelligence
---

You are a full-stack developer.
Multiple lines of system prompt.
"#;
        let profile = parse_profile(content, &p("developer.md")).unwrap();
        assert_eq!(profile.name, "developer");
        assert_eq!(profile.model, "gpt-5");
        assert_eq!(
            profile.reasoning_effort,
            Some(ReasoningEffort::Medium),
            "reasoning_effort should deserialise from the lowercase string"
        );
        assert_eq!(profile.reasoning_summary, Some(ReasoningSummary::Concise));
        let tools = profile.tools.as_deref().unwrap();
        assert_eq!(
            tools,
            &[
                "Read".to_owned(),
                "Write".to_owned(),
                "Bash(cargo build*)".to_owned(),
            ]
        );
        assert_eq!(
            profile.system_instructions,
            vec!["You are a full-stack developer.\nMultiple lines of system prompt.".to_owned()]
        );
        let synthetic = profile
            .capabilities
            .iter()
            .find(|c| c.name == "_profile_disallowed")
            .expect("synthetic capability must be present");
        assert_eq!(
            synthetic.disallowed_patterns,
            vec!["rm".to_owned(), "shutdown".to_owned()]
        );
        assert_eq!(
            profile.capability_names,
            vec!["research".to_owned(), "code-intelligence".to_owned()],
            "capability references must be preserved for resolution, not dropped",
        );
    }

    #[test]
    fn parse_profile_minimal() {
        let content = "---\nname: minimal\nmodel: gpt-5\n---\n";
        let profile = parse_profile(content, &p("minimal.md")).unwrap();
        assert_eq!(profile.name, "minimal");
        assert_eq!(profile.model, "gpt-5");
        assert!(profile.reasoning_effort.is_none());
        assert!(profile.reasoning_summary.is_none());
        assert!(profile.tools.is_none());
        assert!(profile.system_instructions.is_empty());
        assert!(profile.capabilities.is_empty());
        assert!(profile.capability_names.is_empty());
        assert!(profile.prompt_commands.is_empty());
        assert!(profile.settings.is_empty());
    }

    /// `.md` profiles must reject a missing or blank `model` exactly like
    /// the TOML/JSON forms (where serde enforces the field) — an empty
    /// model previously slipped through as `""` and failed much later at
    /// provider binding.
    #[test]
    fn parse_profile_missing_model_errors() {
        for content in [
            "---\nname: nomodel\n---\nbody\n",
            "---\nname: nomodel\nmodel: \"   \"\n---\nbody\n",
        ] {
            let err = parse_profile(content, &p("nomodel.md")).unwrap_err();
            match err {
                ConfigError::MissingField { field } => assert_eq!(field, "model"),
                ConfigError::InvalidConfig { .. } => {
                    panic!("expected MissingField, got InvalidConfig")
                }
            }
        }
    }

    /// Covers both "no name field" and "blank name field" cases — both
    /// must surface as `MissingField` since the trimmed name is empty.
    #[test]
    fn parse_profile_missing_name_errors() {
        for content in [
            "---\nmodel: gpt-5\n---\nbody\n",
            "---\nname: \"   \"\nmodel: gpt-5\n---\nbody\n",
        ] {
            let err = parse_profile(content, &p("noname.md")).unwrap_err();
            match err {
                ConfigError::MissingField { field } => assert_eq!(field, "name"),
                ConfigError::InvalidConfig { .. } => {
                    panic!("expected MissingField, got InvalidConfig")
                }
            }
        }
    }

    #[test]
    fn parse_profile_tools_as_yaml_list() {
        let content = "---\nname: list-form\nmodel: gpt-5\ntools:\n  - Read\n  - Write\n  - Glob\n---\nbody\n";
        let profile = parse_profile(content, &p("list.md")).unwrap();
        let tools = profile.tools.as_deref().unwrap();
        assert_eq!(
            tools,
            &["Read".to_owned(), "Write".to_owned(), "Glob".to_owned()]
        );
    }

    /// `disallowed_tools` (`snake_case`) must be accepted via the serde
    /// alias on the raw struct — the 31 existing Meridian profiles use
    /// `disallowedTools` but the alias keeps both forms loading.
    #[test]
    fn parse_profile_disallowed_tools_alias_snake_case() {
        let content = "---\nname: snake\nmodel: gpt-5\ndisallowed_tools: rm, shutdown\n---\nbody\n";
        let profile = parse_profile(content, &p("snake.md")).unwrap();
        let synthetic = profile
            .capabilities
            .iter()
            .find(|c| c.name == "_profile_disallowed")
            .expect("synthetic capability must be present");
        assert_eq!(
            synthetic.disallowed_patterns,
            vec!["rm".to_owned(), "shutdown".to_owned()]
        );
    }

    // ── parse_capability ───────────────────────────────────────────────

    #[test]
    fn parse_capability_full() {
        let content = r#"---
name: code-reviewer
description: Adds code review tools
tools: Read, Grep, Glob
disallowedTools: Write, Edit
---

Review code for bugs and style issues.
Focus on correctness over aesthetics.
"#;
        let cap = parse_capability(content, &p("reviewer.md")).unwrap();
        assert_eq!(cap.name, "code-reviewer");
        assert_eq!(
            cap.tools,
            vec!["Read".to_owned(), "Grep".to_owned(), "Glob".to_owned()]
        );
        assert_eq!(
            cap.disallowed_patterns,
            vec!["Write".to_owned(), "Edit".to_owned()]
        );
        assert_eq!(
            cap.system_instructions,
            vec![
                "Review code for bugs and style issues.\nFocus on correctness over aesthetics."
                    .to_owned()
            ]
        );
    }

    #[test]
    fn parse_capability_body_only() {
        let content =
            "---\nname: prompt-only\ndescription: Just a prompt fragment\n---\n\nDo the thing.\n";
        let cap = parse_capability(content, &p("prompt.md")).unwrap();
        assert_eq!(cap.name, "prompt-only");
        assert!(cap.tools.is_empty());
        assert!(cap.disallowed_patterns.is_empty());
        assert_eq!(cap.system_instructions, vec!["Do the thing.".to_owned()]);
    }

    // ── is_safe_name ───────────────────────────────────────────────────

    #[test]
    fn is_safe_name_accepts_typical_stems() {
        assert!(is_safe_name("dev"));
        assert!(is_safe_name("code-reviewer"));
        assert!(is_safe_name("my_profile_v2"));
        assert!(is_safe_name("a"));
    }

    #[test]
    fn is_safe_name_rejects_unsafe_inputs() {
        assert!(!is_safe_name(""));
        assert!(!is_safe_name(".."));
        assert!(!is_safe_name("foo..bar"));
        assert!(!is_safe_name("../etc/passwd"));
        assert!(!is_safe_name("foo/bar"));
        assert!(!is_safe_name("foo\\bar"));
        assert!(!is_safe_name("foo\0bar"));
        assert!(!is_safe_name("foo.bar"));
        assert!(!is_safe_name("foo:bar"));
        assert!(!is_safe_name("foo\nbar"));
        assert!(!is_safe_name("foo\u{1b}[31m"));
        assert!(!is_safe_name("foo\u{7}bar"));
        assert!(!is_safe_name("föö"));
    }

    // ── Scanner::resolve ───────────────────────────────────────────────

    /// Writes a minimal valid markdown profile to `path`.
    fn write_md(path: &Path, name: &str) {
        let content = format!("---\nname: {name}\nmodel: gpt-5\n---\nbody\n");
        std::fs::write(path, content).unwrap();
    }

    /// Writes a minimal valid TOML profile to `path`.
    fn write_toml(path: &Path, name: &str) {
        let content = format!("name = \"{name}\"\nmodel = \"gpt-5\"\n");
        std::fs::write(path, content).unwrap();
    }

    /// Writes a minimal valid JSON profile to `path`.
    fn write_json(path: &Path, name: &str) {
        let content = format!("{{\"name\":\"{name}\",\"model\":\"gpt-5\"}}");
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn scanner_resolve_first_directory_wins() {
        let workspace = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        write_md(&workspace.path().join("shared.md"), "shared-workspace");
        write_md(&user.path().join("shared.md"), "shared-user");

        let scanner = Scanner::new(vec![
            workspace.path().to_path_buf(),
            user.path().to_path_buf(),
        ]);
        let resolved = scanner.resolve("shared").unwrap();
        assert_eq!(resolved, workspace.path().join("shared.md"));
    }

    #[test]
    fn scanner_resolve_prefers_md_over_toml() {
        let dir = tempfile::tempdir().unwrap();
        write_md(&dir.path().join("dual.md"), "dual");
        write_toml(&dir.path().join("dual.toml"), "dual");

        let scanner = Scanner::new(vec![dir.path().to_path_buf()]);
        let resolved = scanner.resolve("dual").unwrap();
        assert_eq!(resolved, dir.path().join("dual.md"));
    }

    #[test]
    fn scanner_resolve_falls_back_to_toml_then_json() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(&dir.path().join("a.toml"), "a");
        write_json(&dir.path().join("b.json"), "b");

        let scanner = Scanner::new(vec![dir.path().to_path_buf()]);
        assert_eq!(scanner.resolve("a").unwrap(), dir.path().join("a.toml"));
        assert_eq!(scanner.resolve("b").unwrap(), dir.path().join("b.json"));
    }

    #[test]
    fn scanner_resolve_returns_none_for_unsafe_name() {
        let dir = tempfile::tempdir().unwrap();
        let scanner = Scanner::new(vec![dir.path().to_path_buf()]);
        assert!(scanner.resolve("../etc/passwd").is_none());
        assert!(scanner.resolve("").is_none());
        assert!(scanner.resolve("foo/bar").is_none());
    }

    #[test]
    fn scanner_resolve_skips_missing_directories() {
        let real = tempfile::tempdir().unwrap();
        write_md(&real.path().join("only.md"), "only");

        let scanner = Scanner::new(vec![
            PathBuf::from("/this/path/does/not/exist/anywhere"),
            real.path().to_path_buf(),
        ]);
        let resolved = scanner.resolve("only").unwrap();
        assert_eq!(resolved, real.path().join("only.md"));
    }

    // ── Scanner::list_profiles ─────────────────────────────────────────

    #[test]
    fn scanner_list_profiles_dedupes_and_sorts() {
        let workspace = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        write_md(&workspace.path().join("zebra.md"), "zebra");
        write_md(&workspace.path().join("alpha.md"), "alpha");
        // Duplicate name in lower-priority dir — should be deduped.
        write_md(&user.path().join("alpha.md"), "alpha-user");
        write_md(&user.path().join("middle.md"), "middle");

        let scanner = Scanner::new(vec![
            workspace.path().to_path_buf(),
            user.path().to_path_buf(),
        ]);
        let names = scanner.list_profiles();
        assert_eq!(
            names,
            vec!["alpha".to_owned(), "middle".to_owned(), "zebra".to_owned(),]
        );
    }

    #[test]
    fn scanner_list_profiles_skips_unrelated_extensions() {
        let dir = tempfile::tempdir().unwrap();
        write_md(&dir.path().join("real.md"), "real");
        std::fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();
        std::fs::write(dir.path().join("README.markdown"), "ignore me").unwrap();

        let scanner = Scanner::new(vec![dir.path().to_path_buf()]);
        let names = scanner.list_profiles();
        assert_eq!(names, vec!["real".to_owned()]);
    }

    // ── default_scan_dirs ──────────────────────────────────────────────

    /// Guard that swaps `NORN_HOME` for the duration of a test and
    /// restores the prior value on drop. Paired with
    /// `#[serial_test::serial]` on every consumer.
    struct NornHomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl NornHomeGuard {
        fn set(path: &Path) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with `#[serial_test::serial]`; no concurrent
            // reader observes the mutated env.
            unsafe { std::env::set_var("NORN_HOME", path) }
            Self { prior }
        }
    }

    impl Drop for NornHomeGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(val) => unsafe { std::env::set_var("NORN_HOME", val) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    /// The user tier must honour `NORN_HOME` exactly like every other
    /// `~/.norn/` path — it previously hardcoded `dirs::home_dir()`.
    #[test]
    #[serial_test::serial]
    fn default_scan_dirs_orders_workspace_meridian_then_norn_home() {
        let norn_home = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(norn_home.path());
        let cwd = PathBuf::from("/tmp/some/project");
        let dirs = default_scan_dirs(&cwd);
        assert_eq!(dirs.len(), 3);
        assert_eq!(dirs[0], cwd.join(".norn").join("profiles"));
        assert_eq!(dirs[1], cwd.join(".meridian").join("profiles"));
        assert_eq!(dirs[2], norn_home.path().join("profiles"));
    }

    #[test]
    #[serial_test::serial]
    fn workspace_profile_prompt_commands_are_rejected_without_execution()
    -> Result<(), Box<dyn std::error::Error>> {
        let norn_home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let norn_home_guard = NornHomeGuard::set(norn_home.path());
        let profiles = cwd.path().join(".norn").join("profiles");
        std::fs::create_dir_all(&profiles)?;
        std::fs::write(
            profiles.join("hostile.json"),
            r#"{
                "name": "hostile",
                "model": "gpt-5.6-sol",
                "prompt_commands": [{
                    "name": "private",
                    "command": "touch profile-command-secret",
                    "cache_ttl": null
                }]
            }"#,
        )?;

        let Err(error) = resolve_workspace_profile("hostile", cwd.path()) else {
            return Err(std::io::Error::other("workspace prompt command was accepted").into());
        };
        let rendered = error.to_string();
        assert!(rendered.contains("prompt_commands"));
        assert!(!rendered.contains("profile-command-secret"));
        assert!(!cwd.path().join("profile-command-secret").exists());

        drop(norn_home_guard);
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn static_workspace_and_user_prompt_command_profiles_keep_their_trust_contract()
    -> Result<(), Box<dyn std::error::Error>> {
        let norn_home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let norn_home_guard = NornHomeGuard::set(norn_home.path());
        let workspace_profiles = cwd.path().join(".norn").join("profiles");
        let user_profiles = norn_home.path().join("profiles");
        std::fs::create_dir_all(&workspace_profiles)?;
        std::fs::create_dir_all(&user_profiles)?;
        write_md(&workspace_profiles.join("static.md"), "static");
        std::fs::write(
            user_profiles.join("trusted.json"),
            r#"{
                "name": "trusted",
                "model": "gpt-5.6-sol",
                "prompt_commands": [{
                    "name": "trusted",
                    "command": "touch sentinel-model-selected-user-command",
                    "cache_ttl": null
                }]
            }"#,
        )?;

        let workspace = resolve_workspace_profile("static", cwd.path())?;
        assert_eq!(workspace.origin, ProfileOrigin::WorkingDirectory);
        assert!(workspace.profile.prompt_commands.is_empty());

        let user = resolve_workspace_profile("trusted", cwd.path())?;
        assert_eq!(user.origin, ProfileOrigin::User);
        assert_eq!(user.profile.prompt_commands.len(), 1);
        let error =
            crate::tools::agent::spawn_context::validate_model_selected_profile(&user.profile)
                .expect_err("model-selected user profiles cannot run prompt commands");
        let rendered = error.to_string();
        assert!(rendered.contains("prompt_commands"));
        assert!(!rendered.contains("sentinel-model-selected-user-command"));
        assert!(
            !cwd.path()
                .join("sentinel-model-selected-user-command")
                .exists()
        );

        drop(norn_home_guard);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn workspace_profile_symlink_is_refused_without_reading_target()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let norn_home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let outside = tempfile::NamedTempFile::new()?;
        std::fs::write(
            outside.path(),
            r#"{"name":"leak","model":"gpt-5.6-sol","system_instructions":["sentinel-private-profile"]}"#,
        )?;
        let profiles = cwd.path().join(".norn/profiles");
        std::fs::create_dir_all(&profiles)?;
        symlink(outside.path(), profiles.join("leak.json"))?;
        let norn_home_guard = NornHomeGuard::set(norn_home.path());

        let Err(error) = resolve_workspace_profile("leak", cwd.path()) else {
            return Err(std::io::Error::other("workspace profile symlink was accepted").into());
        };

        assert!(!error.to_string().contains("sentinel-private-profile"));
        drop(norn_home_guard);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn workspace_capability_symlink_is_refused_without_reading_target()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let norn_home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let profiles = cwd.path().join(".norn/profiles");
        let capabilities = cwd.path().join(".norn/capabilities");
        std::fs::create_dir_all(&profiles)?;
        std::fs::create_dir_all(&capabilities)?;
        std::fs::write(
            profiles.join("dev.md"),
            "---\nname: dev\nmodel: gpt-5.6-sol\ncapabilities: [leak]\n---\nDeveloper.\n",
        )?;
        let outside = tempfile::NamedTempFile::new()?;
        std::fs::write(
            outside.path(),
            "---\nname: leak\n---\nsentinel-private-capability\n",
        )?;
        symlink(outside.path(), capabilities.join("leak.md"))?;
        let norn_home_guard = NornHomeGuard::set(norn_home.path());

        let Err(error) = resolve_workspace_profile("dev", cwd.path()) else {
            return Err(std::io::Error::other("workspace capability symlink was accepted").into());
        };

        assert!(!error.to_string().contains("sentinel-private-capability"));
        drop(norn_home_guard);
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn user_and_workspace_profiles_resolve_capabilities_within_their_own_trust_tier()
    -> Result<(), Box<dyn std::error::Error>> {
        let norn_home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let workspace_profiles = cwd.path().join(".norn/profiles");
        let workspace_capabilities = cwd.path().join(".norn/capabilities");
        let user_profiles = norn_home.path().join("profiles");
        let user_capabilities = norn_home.path().join("capabilities");
        for directory in [
            &workspace_profiles,
            &workspace_capabilities,
            &user_profiles,
            &user_capabilities,
        ] {
            std::fs::create_dir_all(directory)?;
        }
        std::fs::write(
            user_profiles.join("trusted.md"),
            "---\nname: trusted\nmodel: gpt-5.6-sol\ncapabilities: [shared]\n---\nTrusted.\n",
        )?;
        std::fs::write(
            workspace_capabilities.join("shared.md"),
            "---\nname: shared\n---\nWORKSPACE_CAPABILITY\n",
        )?;
        std::fs::write(
            user_capabilities.join("shared.md"),
            "---\nname: shared\n---\nUSER_CAPABILITY\n",
        )?;
        std::fs::write(
            workspace_profiles.join("repository.md"),
            "---\nname: repository\nmodel: gpt-5.6-sol\ncapabilities: [user-only]\n---\nRepository.\n",
        )?;
        std::fs::write(
            user_capabilities.join("user-only.md"),
            "---\nname: user-only\n---\nUSER_ONLY_CAPABILITY\n",
        )?;
        let norn_home_guard = NornHomeGuard::set(norn_home.path());

        let trusted = resolve_workspace_profile("trusted", cwd.path())?;
        assert_eq!(trusted.origin, ProfileOrigin::User);
        assert_eq!(
            trusted.profile.capabilities[0].system_instructions,
            vec!["USER_CAPABILITY".to_owned()],
        );

        let Err(error) = resolve_workspace_profile("repository", cwd.path()) else {
            return Err(std::io::Error::other("workspace profile borrowed user capability").into());
        };
        assert!(!error.to_string().contains("USER_ONLY_CAPABILITY"));

        drop(norn_home_guard);
        Ok(())
    }

    // ── capability resolution ──────────────────────────────────────────

    #[test]
    fn capability_scan_dirs_are_siblings_of_profile_dirs() {
        let dirs = capability_scan_dirs(&[
            PathBuf::from("/w/.norn/profiles"),
            PathBuf::from("/w/.meridian/profiles"),
        ]);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/w/.norn/capabilities"),
                PathBuf::from("/w/.meridian/capabilities"),
            ]
        );
    }

    /// A profile's `capabilities:` frontmatter references resolve from the
    /// sibling `capabilities/` directory — including `disallowedTools`,
    /// which previously vanished with the entire capability list.
    #[test]
    fn resolve_profile_resolves_capability_references() {
        let workspace = tempfile::tempdir().unwrap();
        let profiles = workspace.path().join(".norn").join("profiles");
        let capabilities = workspace.path().join(".norn").join("capabilities");
        std::fs::create_dir_all(&profiles).unwrap();
        std::fs::create_dir_all(&capabilities).unwrap();

        std::fs::write(
            profiles.join("dev.md"),
            "---\nname: dev\nmodel: gpt-5\ncapabilities:\n  - reviewer\n---\nBe a dev.\n",
        )
        .unwrap();
        std::fs::write(
            capabilities.join("reviewer.md"),
            "---\nname: reviewer\ntools: Read, Grep\ndisallowedTools: bash(rm *)\n---\nReview closely.\n",
        )
        .unwrap();

        let profile = resolve_profile("dev", &[profiles]).unwrap();
        assert!(profile.capability_names.is_empty(), "names consumed");
        let reviewer = profile
            .capabilities
            .iter()
            .find(|c| c.name == "reviewer")
            .expect("referenced capability resolved");
        assert_eq!(reviewer.tools, vec!["Read".to_owned(), "Grep".to_owned()]);
        assert_eq!(reviewer.disallowed_patterns, vec!["bash(rm *)".to_owned()]);
        assert_eq!(
            reviewer.system_instructions,
            vec!["Review closely.".to_owned()]
        );
        // The merged views observe the capability's contribution (C10).
        let disallowed = profile.resolved_disallowed();
        assert!(disallowed.contains(&"bash(rm *)".to_owned()));
    }

    /// An unresolvable capability reference is a typed error naming the
    /// searched directories — never a silently dropped restriction.
    #[test]
    fn resolve_profile_errors_on_missing_capability() {
        let workspace = tempfile::tempdir().unwrap();
        let profiles = workspace.path().join(".norn").join("profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        std::fs::write(
            profiles.join("dev.md"),
            "---\nname: dev\nmodel: gpt-5\ncapabilities:\n  - ghost\n---\nBody.\n",
        )
        .unwrap();

        let err = resolve_profile("dev", &[profiles]).unwrap_err();
        match err {
            ConfigError::InvalidConfig { reason } => {
                assert!(reason.contains("ghost"), "reason: {reason}");
                assert!(reason.contains("capabilities"), "reason: {reason}");
            }
            ConfigError::MissingField { .. } => panic!("expected InvalidConfig"),
        }
    }

    /// First capability dir wins on name collision, mirroring profile
    /// shadowing.
    #[test]
    fn resolve_capability_first_directory_wins() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(
            a.path().join("shared.md"),
            "---\nname: shared\n---\nFrom A.\n",
        )
        .unwrap();
        std::fs::write(
            b.path().join("shared.md"),
            "---\nname: shared\n---\nFrom B.\n",
        )
        .unwrap();
        let cap = resolve_capability("shared", &[a.path().to_path_buf(), b.path().to_path_buf()])
            .unwrap();
        assert_eq!(cap.system_instructions, vec!["From A.".to_owned()]);
    }

    #[test]
    fn resolve_capability_rejects_unsafe_names() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_capability("../etc/passwd", &[dir.path().to_path_buf()]).unwrap_err();
        match err {
            ConfigError::InvalidConfig { reason } => {
                assert!(reason.contains("safe"), "reason: {reason}");
            }
            ConfigError::MissingField { .. } => panic!("expected InvalidConfig"),
        }
    }

    // ── resolve_profile ────────────────────────────────────────────────

    #[test]
    fn resolve_profile_loads_markdown_by_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        write_md(&dir.path().join("coding.md"), "coding");

        let scan_dirs = vec![dir.path().to_path_buf()];
        let profile = resolve_profile("coding", &scan_dirs).unwrap();
        assert_eq!(profile.name, "coding");
        assert_eq!(profile.model, "gpt-5");
    }

    #[test]
    fn resolve_profile_error_lists_searched_paths() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let scan_dirs = vec![a.path().to_path_buf(), b.path().to_path_buf()];

        let err = resolve_profile("nope", &scan_dirs).unwrap_err();
        match err {
            ConfigError::InvalidConfig { reason } => {
                assert!(reason.contains("nope"), "reason: {reason}");
                assert!(
                    reason.contains(&a.path().display().to_string()),
                    "reason: {reason}"
                );
                assert!(
                    reason.contains(&b.path().display().to_string()),
                    "reason: {reason}"
                );
            }
            ConfigError::MissingField { .. } => panic!("expected InvalidConfig"),
        }
    }
}
