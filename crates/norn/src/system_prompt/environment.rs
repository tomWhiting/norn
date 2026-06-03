//! Dynamic environment section injected per-iteration.
//!
//! Gathers system and session information via Rust APIs (no shell
//! commands) and formats it as a `# Environment` section for the
//! system prompt. Designed to be called from the runner's iteration
//! top and appended as a dynamic system section.

use std::fmt::Write;
use std::path::{Path, PathBuf};

/// Stable session-level facts set once at build time. Combined with
/// per-iteration dynamic facts (current time, git state) by
/// [`format_environment_section`].
#[derive(Clone, Debug, Default)]
pub struct EnvironmentConfig {
    /// Session identifier, if known.
    pub session_id: Option<String>,
    /// Model name (e.g. "gpt-5.5").
    pub model: String,
}

/// Assemble the `# Environment` section from stable config and fresh
/// system state. Pure computation — no shell commands, no blocking I/O
/// beyond reading `.git/HEAD`.
///
/// `working_dir` is the agent's current directory, reported to the model
/// and used as the starting point for the git-branch walk. Callers pass
/// in `LoopContext::working_dir.get()` so the model's spatial awareness
/// matches the resolver used by tools.
#[must_use]
pub fn format_environment_section(config: &EnvironmentConfig, working_dir: &Path) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("# Environment\n\n");

    let _ = writeln!(out, "Working directory: {}", working_dir.display());

    let _ = writeln!(
        out,
        "Platform: {} {}",
        friendly_os(std::env::consts::OS),
        std::env::consts::ARCH,
    );

    if let Ok(shell) = std::env::var("SHELL")
        && let Some(name) = Path::new(&shell).file_name()
    {
        let _ = writeln!(out, "Shell: {}", name.to_string_lossy());
    }

    let _ = writeln!(
        out,
        "Time: {}",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
    );

    if let Some(branch) = read_git_branch(working_dir) {
        let _ = writeln!(out, "Git: {branch}");
    }

    if let Some(id) = &config.session_id {
        let _ = writeln!(out, "Session: {id}");
    }

    if !config.model.is_empty() {
        let _ = writeln!(out, "Model: {}", config.model);
    }

    out.truncate(out.trim_end().len());
    out
}

/// Map `std::env::consts::OS` to a friendlier display name.
fn friendly_os(os: &str) -> &str {
    match os {
        "macos" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        other => other,
    }
}

/// Read the current git branch from `.git/HEAD` without shelling out.
///
/// Walks upward from `start_dir` to find the `.git` directory, then reads
/// the `HEAD` file. Returns `None` when not inside a git repository or
/// when the HEAD file is unreadable.
fn read_git_branch(start_dir: &Path) -> Option<String> {
    let git_dir = find_git_dir(start_dir)?;
    let head_path = git_dir.join("HEAD");
    let content = std::fs::read_to_string(head_path).ok()?;
    let trimmed = content.trim();

    if let Some(refname) = trimmed.strip_prefix("ref: refs/heads/") {
        Some(refname.to_owned())
    } else if trimmed.len() >= 8 {
        let short: String = trimmed.chars().take(8).collect();
        Some(format!("detached at {short}"))
    } else {
        None
    }
}

/// Walk up from `start_dir` looking for a `.git` directory or file
/// (worktree gitlink).
fn find_git_dir(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if candidate.is_file() {
            return resolve_gitlink(&candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Resolve a `.git` file (worktree gitlink) to the actual git directory.
/// The file contains `gitdir: <path>`.
fn resolve_gitlink(gitlink: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(gitlink).ok()?;
    let target = content.trim().strip_prefix("gitdir: ")?;
    let path = PathBuf::from(target);
    if path.is_absolute() {
        Some(path)
    } else {
        gitlink.parent().map(|p| p.join(&path))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn format_includes_platform_and_time() {
        let config = EnvironmentConfig {
            session_id: Some("test-session-123".to_owned()),
            model: "gpt-5.5".to_owned(),
        };
        let section = format_environment_section(&config, Path::new("/tmp"));
        assert!(section.starts_with("# Environment"));
        assert!(section.contains("Platform:"));
        assert!(section.contains("Time:"));
        assert!(section.contains("Session: test-session-123"));
        assert!(section.contains("Model: gpt-5.5"));
    }

    #[test]
    fn format_omits_session_when_none() {
        let config = EnvironmentConfig {
            session_id: None,
            model: "gpt-5.5".to_owned(),
        };
        let section = format_environment_section(&config, Path::new("/tmp"));
        assert!(!section.contains("Session:"));
    }

    #[test]
    fn format_omits_model_when_empty() {
        let config = EnvironmentConfig {
            session_id: None,
            model: String::new(),
        };
        let section = format_environment_section(&config, Path::new("/tmp"));
        assert!(!section.contains("Model:"));
    }

    #[test]
    fn friendly_os_maps_known_values() {
        assert_eq!(friendly_os("macos"), "macOS");
        assert_eq!(friendly_os("linux"), "Linux");
        assert_eq!(friendly_os("windows"), "Windows");
        assert_eq!(friendly_os("freebsd"), "freebsd");
    }

    #[test]
    fn read_git_branch_finds_current_repo() {
        let cwd = std::env::current_dir().unwrap();
        let branch = read_git_branch(&cwd);
        assert!(
            branch.is_some(),
            "test is running inside a git repo, so branch should be Some",
        );
    }

    #[test]
    fn no_trailing_whitespace() {
        let config = EnvironmentConfig {
            session_id: None,
            model: String::new(),
        };
        let section = format_environment_section(&config, Path::new("/tmp"));
        assert_eq!(section, section.trim_end());
    }
}
