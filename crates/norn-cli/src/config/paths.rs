//! Thin wrappers over [`norn::config::paths`] for CLI-only call sites.
//!
//! Path resolution lives in libnorn so consumers inside the runtime crate
//! (profile loader, session manager, task store) can resolve `~/.norn/`
//! paths without depending on the CLI. This module keeps a small wrapper
//! API for CLI ergonomics — chiefly to preserve the `PathBuf` return
//! shape of [`session_data_dir`], which several CLI consumers expect.
//!
//! It also exposes the seven-tier skill discovery roots from
//! `norn-skills` DESIGN.md §D1. The project-tier helpers
//! ([`project_skills_dir`], [`project_agents_skills_dir`],
//! [`project_claude_skills_dir`]) take
//! a working directory so the resolution matches the user's effective
//! project root after `--working-dir` has been applied. The user-tier
//! helpers ([`user_skills_dir`], [`user_agents_skills_dir`],
//! [`user_claude_skills_dir`]) probe directories under the *real* home
//! directory — `$NORN_HOME` is honoured only by the `~/.norn/skills/`
//! tier, never by `~/.agents/skills/` or `~/.claude/skills/`, because
//! those paths exist independently of Norn's environment override.

use std::path::{Path, PathBuf};

/// Resolve the norn root directory.
///
/// Delegates to [`norn::config::paths::norn_dir`]. Returns [`None`] when
/// the environment override is unset/empty and the home directory cannot
/// be resolved.
#[must_use]
pub fn norn_dir() -> Option<PathBuf> {
    norn::config::paths::norn_dir()
}

/// Resolve the directory containing named profile files: `~/.norn/profiles/`.
///
/// Delegates to [`norn::config::paths::profiles_dir`].
#[must_use]
pub fn profiles_dir() -> Option<PathBuf> {
    norn::config::paths::profiles_dir()
}

/// Resolve `{cwd}/.norn/rules/` — the project-level rules directory.
///
/// Project rules win on ID collision with user-level rules from
/// `~/.norn/rules/` (DESIGN.md §D5).
#[must_use]
pub fn project_rules_dir(cwd: &Path) -> PathBuf {
    cwd.join(".norn").join("rules")
}

/// Resolve the session-data directory: `~/.norn/sessions/`.
///
/// Delegates to [`norn::config::paths::session_data_dir`], falling back
/// to a relative `./.norn/sessions/` path when neither the environment
/// override nor the home directory resolves. The fallback mirrors the
/// pattern used by the runtime builder at `runtime/builder.rs:153` and
/// `:326`, where `norn_dir()` falls back to `PathBuf::from(".norn")` so
/// the CLI keeps working in unusual environments (chroots, CI runners
/// without a HOME).
#[must_use]
pub fn session_data_dir() -> PathBuf {
    norn::config::paths::session_data_dir()
        .unwrap_or_else(|| PathBuf::from(".norn").join("sessions"))
}

// ---------------------------------------------------------------------------
// Skill search-path tiers (norn-skills DESIGN.md §D1)
// ---------------------------------------------------------------------------

/// Resolve `{cwd}/.norn/skills/` — the highest-priority project tier.
#[must_use]
pub fn project_skills_dir(cwd: &Path) -> PathBuf {
    cwd.join(".norn").join("skills")
}

/// Resolve `{cwd}/.agents/skills/` — the cross-client project tier
/// recommended by the Agent Skills standard's "adding support" guide.
#[must_use]
pub fn project_agents_skills_dir(cwd: &Path) -> PathBuf {
    cwd.join(".agents").join("skills")
}

/// Resolve `{cwd}/.claude/skills/` — Claude Code's project-level skill
/// directory, scanned so SKILL.md files authored for Claude Code work
/// in Norn without modification. This is the lowest-priority project
/// tier (the legacy `.meridian/skills/` tier was removed — DECISIONS
/// §0.6(a)).
#[must_use]
pub fn project_claude_skills_dir(cwd: &Path) -> PathBuf {
    cwd.join(".claude").join("skills")
}

/// Resolve `~/.norn/skills/` — the user-level Norn skill directory.
///
/// Delegates to [`norn::config::paths::skills_dir`], which honours
/// `$NORN_HOME` when set. Returns [`None`] when neither the env override
/// nor [`dirs::home_dir`] resolves.
#[must_use]
pub fn user_skills_dir() -> Option<PathBuf> {
    norn::config::paths::skills_dir()
}

/// Resolve `~/.agents/skills/` — the cross-client user tier.
///
/// Reads the *real* home directory via [`dirs::home_dir`]; `$NORN_HOME`
/// is intentionally not consulted because `.agents/skills/` lives
/// alongside other client tools and is not under Norn's tree.
#[must_use]
pub fn user_agents_skills_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".agents").join("skills"))
}

/// Resolve `~/.claude/skills/` — Claude Code's user-level skill
/// directory.
///
/// Reads the *real* home directory via [`dirs::home_dir`]; `$NORN_HOME`
/// is intentionally not consulted because `.claude/skills/` is Claude
/// Code's tree, not Norn's.
#[must_use]
pub fn user_claude_skills_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("skills"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn norn_dir_returns_some_path() {
        // libnorn's `norn::config::paths::tests` exercises the fallback
        // shape and the environment-override branch directly. Here we
        // only confirm the wrapper produces a directory path when one is
        // resolvable in the current environment.
        if let Some(dir) = norn_dir() {
            assert!(dir.ends_with(".norn") || dir.is_absolute());
        }
    }

    #[test]
    fn profiles_dir_under_norn() {
        let Some(dir) = profiles_dir() else {
            return;
        };
        assert!(dir.ends_with("profiles"));
    }

    #[test]
    fn session_data_dir_always_returns_path() {
        let path = session_data_dir();
        assert!(path.ends_with("sessions"));
    }

    #[test]
    fn project_tier_helpers_join_under_cwd() {
        let cwd = PathBuf::from("/tmp/project");
        assert_eq!(
            project_skills_dir(&cwd),
            PathBuf::from("/tmp/project/.norn/skills"),
        );
        assert_eq!(
            project_agents_skills_dir(&cwd),
            PathBuf::from("/tmp/project/.agents/skills"),
        );
        assert_eq!(
            project_claude_skills_dir(&cwd),
            PathBuf::from("/tmp/project/.claude/skills"),
        );
    }

    #[test]
    fn user_skill_tier_helpers_resolve_under_real_home_or_norn_home() {
        // Each helper either resolves (CI environments with a HOME) or
        // returns None — both shapes are acceptable here. We only assert
        // that the trailing segments are correct when Some.
        if let Some(dir) = user_skills_dir() {
            assert!(dir.ends_with(PathBuf::from("skills")));
        }
        if let Some(dir) = user_agents_skills_dir() {
            assert!(dir.ends_with(PathBuf::from(".agents").join("skills")));
        }
        if let Some(dir) = user_claude_skills_dir() {
            assert!(dir.ends_with(PathBuf::from(".claude").join("skills")));
        }
    }

    #[test]
    fn project_rules_dir_joins_dot_norn_rules() {
        let cwd = Path::new("/some/workspace");
        let dir = project_rules_dir(cwd);
        assert_eq!(dir, Path::new("/some/workspace/.norn/rules"));
    }

    #[test]
    fn project_rules_dir_is_relative_when_cwd_is_relative() {
        let cwd = Path::new("rel-cwd");
        let dir = project_rules_dir(cwd);
        assert_eq!(dir, Path::new("rel-cwd/.norn/rules"));
    }
}
