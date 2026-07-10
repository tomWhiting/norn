//! Dynamic environment section injected per-iteration.
//!
//! Gathers system and session information via Rust APIs (no shell
//! commands) and formats it as a `# Environment` section for the
//! system prompt. Designed to be called from the runner's iteration
//! top and appended as a dynamic system section.

use std::fmt::Write;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Git control files are tiny. A larger value is not valid branch metadata
/// and must never be read wholesale from a repository-selected path.
const MAX_GIT_METADATA_BYTES: u64 = 1024;

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
    let content = read_small_regular_file(&head_path)?;
    let trimmed = content.strip_suffix('\n').unwrap_or(&content);
    let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
    if trimmed.contains(['\n', '\r']) {
        return None;
    }

    if let Some(refname) = trimmed.strip_prefix("ref: refs/heads/")
        && valid_branch_ref(refname)
    {
        Some(refname.to_owned())
    } else if matches!(trimmed.len(), 40 | 64)
        && trimmed.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        Some(format!("detached at {}", &trimmed[..8]))
    } else {
        None
    }
}

fn valid_branch_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.starts_with('/')
        && !value.ends_with(['/', '.'])
        && !value.contains("..")
        && !value.contains("@{")
        && !value.contains("//")
        && value.split('/').all(|component| {
            !component.is_empty()
                && !component.starts_with('.')
                && !component.as_bytes().ends_with(b".lock")
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

/// Walk up from `start_dir` looking for a `.git` directory or file
/// (worktree gitlink).
fn find_git_dir(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join(".git");
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_dir() => return Some(candidate),
            Ok(metadata) if metadata.file_type().is_file() => {
                return resolve_gitlink(&candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) | Err(_) => return None,
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Resolve a `.git` file (worktree gitlink) to the actual git directory.
/// The file contains `gitdir: <path>`.
fn resolve_gitlink(gitlink: &Path) -> Option<PathBuf> {
    let content = read_small_regular_file(gitlink)?;
    let target = content.trim().strip_prefix("gitdir: ")?;
    if target.contains(['\n', '\r']) {
        return None;
    }
    let path = PathBuf::from(target);
    let resolved = if path.is_absolute() {
        path
    } else {
        gitlink.parent()?.join(path)
    };
    let canonical = resolved.canonicalize().ok()?;
    if canonical.is_dir() {
        Some(canonical)
    } else {
        None
    }
}

fn read_small_regular_file(path: &Path) -> Option<String> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path)
        .ok()?
        .file_type()
        .is_symlink()
    {
        return None;
    }
    let file = options.open(path).ok()?;
    if !file.metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut content = String::new();
    file.take(MAX_GIT_METADATA_BYTES + 1)
        .read_to_string(&mut content)
        .ok()?;
    if u64::try_from(content.len()).ok()? > MAX_GIT_METADATA_BYTES {
        None
    } else {
        Some(content)
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
