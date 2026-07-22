//! Launch-path and profile resolution for agent assembly.

use std::path::{Path, PathBuf};

use crate::error::{ConfigError, NornError};
use crate::profile::Profile;
use crate::profile::loader::{ProfileOrigin, resolve_workspace_profile_at_launch_root};
use crate::system_prompt::PromptSource;

/// Resolve and canonicalize the agent's immutable launch working directory.
///
/// Relative segments and launch-time symlinks are resolved once. Security
/// consumers retain this absolute root rather than re-canonicalizing a mutable
/// pathname on each read.
pub(crate) fn resolve_working_dir(explicit: Option<PathBuf>) -> Result<PathBuf, NornError> {
    let requested = match explicit {
        Some(dir) => dir,
        None => std::env::current_dir().map_err(|error| {
            NornError::Config(ConfigError::InvalidConfig {
                reason: format!("cannot determine working directory: {error}"),
            })
        })?,
    };
    let canonical = requested.canonicalize().map_err(|error| {
        NornError::Config(ConfigError::InvalidConfig {
            reason: format!(
                "cannot canonicalize working directory {}: {error}",
                requested.display(),
            ),
        })
    })?;
    if !canonical.is_dir() {
        return Err(NornError::Config(ConfigError::InvalidConfig {
            reason: format!(
                "working directory {} is not a directory",
                requested.display()
            ),
        }));
    }
    Ok(canonical)
}

/// Validate and canonicalize a workspace-confinement root.
///
/// The root must exist and be a directory; canonicalizing it (resolving
/// symlinks and relative segments against the process working directory)
/// means the confinement checks enforced by
/// [`ToolContext::confine_to_workspace`](crate::tool::context::ToolContext::confine_to_workspace)
/// always compare against fully resolved real paths, and a misconfigured
/// root fails assembly loudly instead of silently confining nothing.
/// `None` passes through unchanged — no confinement was requested.
///
/// This is the single workspace-root validation shared by every launch
/// path: [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build)
/// applies it to the builder's `workspace_root`, and `norn-cli`'s
/// `build_runtime` applies it to the `--workspace-root` flag value.
///
/// # Errors
///
/// Returns [`NornError::Config`] when the root cannot be canonicalized
/// (does not exist / unresolvable) or resolves to something that is not
/// a directory.
pub fn validate_workspace_root(root: Option<PathBuf>) -> Result<Option<PathBuf>, NornError> {
    let Some(root) = root else {
        return Ok(None);
    };
    let canonical = root.canonicalize().map_err(|error| {
        NornError::Config(ConfigError::InvalidConfig {
            reason: format!(
                "workspace_root {} cannot be resolved: {error}; it must be an existing directory",
                root.display()
            ),
        })
    })?;
    if !canonical.is_dir() {
        return Err(NornError::Config(ConfigError::InvalidConfig {
            reason: format!(
                "workspace_root {} is not a directory (resolved to {})",
                root.display(),
                canonical.display()
            ),
        }));
    }
    Ok(Some(canonical))
}

/// Resolved profile plus the provenance of its instruction text.
pub(crate) struct ResolvedBaseProfile {
    /// Parsed profile used by runtime assembly.
    pub(crate) profile: Profile,
    /// Source from which prompt authority is derived.
    pub(crate) prompt_source: PromptSource,
}

/// Resolve the base profile: the explicit profile wins, then a named profile
/// resolved through the standard scan dirs, then the default profile.
pub(crate) fn resolve_base_profile(
    profile: Option<Profile>,
    explicit_origin: Option<ProfileOrigin>,
    profile_name: Option<&str>,
    working_dir: &Path,
) -> Result<ResolvedBaseProfile, NornError> {
    match profile {
        Some(profile) => Ok(ResolvedBaseProfile {
            profile,
            prompt_source: profile_prompt_source(explicit_origin),
        }),
        None => match profile_name {
            Some(name) => {
                let resolved = resolve_workspace_profile_at_launch_root(name, working_dir)?;
                Ok(ResolvedBaseProfile {
                    profile: resolved.profile,
                    prompt_source: profile_prompt_source(Some(resolved.origin)),
                })
            }
            None => Ok(ResolvedBaseProfile {
                profile: Profile::default(),
                prompt_source: PromptSource::OperatorProfile,
            }),
        },
    }
}

pub(super) const fn profile_prompt_source(origin: Option<ProfileOrigin>) -> PromptSource {
    match origin {
        Some(ProfileOrigin::WorkingDirectory) => PromptSource::WorkspaceProfile,
        Some(ProfileOrigin::User) | None => PromptSource::OperatorProfile,
    }
}
