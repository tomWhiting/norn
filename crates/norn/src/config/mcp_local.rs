//! User-owned project-local MCP settings.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::NornSettings;
use crate::error::ConfigError;

const PROJECTS_DIR: &str = "projects";
const SETTINGS_FILE: &str = "settings.json";

/// Resolve the private per-project MCP settings path under `$NORN_HOME/mcp`.
///
/// The canonical project path is used only to derive a stable directory key;
/// the path itself is not copied into the file name.
pub fn project_local_mcp_settings_path(project_root: &Path) -> Result<PathBuf, ConfigError> {
    let canonical = project_root
        .canonicalize()
        .map_err(|error| ConfigError::InvalidConfig {
            reason: format!("failed to canonicalize the MCP project root: {error}"),
        })?;
    let project = canonical
        .to_str()
        .ok_or_else(|| ConfigError::InvalidConfig {
            reason: "MCP project roots must be valid UTF-8".to_owned(),
        })?;
    let root = super::paths::norn_dir().ok_or_else(|| ConfigError::InvalidConfig {
        reason: "cannot resolve the user-level Norn directory for project-local MCP settings"
            .to_owned(),
    })?;
    let digest = Sha256::digest(project.as_bytes());
    Ok(root
        .join("mcp")
        .join(PROJECTS_DIR)
        .join(format!("{digest:x}"))
        .join(SETTINGS_FILE))
}

pub(crate) fn load_project_local_mcp_settings(
    project_root: &Path,
) -> Result<NornSettings, ConfigError> {
    let _descriptor_permit =
        crate::resource::acquire_private_fs().map_err(|error| ConfigError::InvalidConfig {
            reason: error.to_string(),
        })?;
    let path = project_local_mcp_settings_path(project_root)?;
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NornSettings::default());
        }
        Err(error) => {
            return Err(ConfigError::InvalidConfig {
                reason: format!(
                    "failed to read private project-local MCP settings {}: {error}",
                    path.display(),
                ),
            });
        }
    };
    let (settings, unknown) =
        super::loader::parse_settings_with_unknown_paths(&contents).map_err(|error| {
            ConfigError::InvalidConfig {
                reason: format!(
                    "failed to parse private project-local MCP settings {}: {error}",
                    path.display(),
                ),
            }
        })?;
    for key in unknown {
        tracing::warn!(
            path = %path.display(),
            key,
            "unknown key in private project-local MCP settings; ignoring",
        );
    }
    Ok(NornSettings {
        mcp_servers: settings.mcp_servers,
        ..NornSettings::default()
    })
}
