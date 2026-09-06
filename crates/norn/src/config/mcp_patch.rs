//! MCP-only persistent settings document mutation.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::McpServerSettings;
use super::mcp::{fingerprint, validate_one};
use super::mcp_state_types::{McpPersistentChange, McpPersistentMutation, McpPersistentScope};
use super::settings_document::{SettingsDocument, workspace_path};
use super::settings_write::SettingsDocumentError;
use super::workspace_settings_document::WorkspaceSettingsFile;
use crate::error::{ConfigError, NornError};

pub(super) fn persist_mcp_mutation(
    project_root: &Path,
    scope: McpPersistentScope,
    mutation: &McpPersistentMutation,
) -> Result<McpPersistentChange, NornError> {
    validate_mutation(mutation)?;
    let descriptor_permit =
        crate::resource::acquire_private_fs().map_err(|error| ConfigError::InvalidConfig {
            reason: error.to_string(),
        })?;
    let (path, changed) = match scope {
        McpPersistentScope::User => {
            let path = super::paths::settings_file().ok_or_else(|| ConfigError::InvalidConfig {
                reason: "cannot resolve user settings path for MCP mutation".to_owned(),
            })?;
            let changed = mutate_private_document(&path, mutation)?;
            (path, changed)
        }
        McpPersistentScope::PrivateLocal => {
            let path = super::mcp_local::project_local_mcp_settings_path(project_root)?;
            let changed = mutate_private_document(&path, mutation)?;
            (path, changed)
        }
        McpPersistentScope::SharedProject => {
            mutate_workspace_document(project_root, WorkspaceSettingsFile::Shared, mutation)?
        }
        McpPersistentScope::WorkspaceLocal => {
            mutate_workspace_document(project_root, WorkspaceSettingsFile::Local, mutation)?
        }
    };
    let change = McpPersistentChange::new(scope, path, changed);
    drop(descriptor_permit);
    Ok(change)
}

fn mutate_workspace_document(
    project_root: &Path,
    kind: WorkspaceSettingsFile,
    mutation: &McpPersistentMutation,
) -> Result<(PathBuf, bool), ConfigError> {
    let document =
        SettingsDocument::workspace(project_root, kind).map_err(|error| document_config(&error))?;
    let path = workspace_path(project_root, kind);
    let original = document.read().map_err(|error| document_config(&error))?;
    let (bytes, changed) = patch_document(original.as_deref(), mutation, &path)?;
    if changed {
        document
            .replace(&bytes)
            .and_then(super::settings_write::SettingsPublication::require_durable)
            .map_err(|error| document_config(&error))?;
    }
    Ok((path, changed))
}

fn mutate_private_document(
    path: &Path,
    mutation: &McpPersistentMutation,
) -> Result<bool, ConfigError> {
    let document = SettingsDocument::private(path).map_err(|error| document_config(&error))?;
    let original = document.read().map_err(|error| document_config(&error))?;
    let (bytes, changed) = patch_document(original.as_deref(), mutation, path)?;
    if changed {
        document
            .replace(&bytes)
            .and_then(super::settings_write::SettingsPublication::require_durable)
            .map_err(|error| document_config(&error))?;
    }
    Ok(changed)
}

fn patch_document(
    original: Option<&str>,
    mutation: &McpPersistentMutation,
    path: &Path,
) -> Result<(Vec<u8>, bool), ConfigError> {
    let mut document = match original {
        Some(content) => {
            serde_json::from_str::<Value>(content).map_err(|error| ConfigError::InvalidConfig {
                reason: format!(
                    "failed to parse {} for MCP mutation: {error}",
                    path.display()
                ),
            })?
        }
        None => Value::Object(Map::new()),
    };
    let object = document
        .as_object_mut()
        .ok_or_else(|| ConfigError::InvalidConfig {
            reason: format!("settings document {} must be a JSON object", path.display()),
        })?;
    let changed = apply_mutation(object, mutation, path)?;
    if !changed {
        return Ok((Vec::new(), false));
    }
    let mut bytes =
        serde_json::to_vec_pretty(&document).map_err(|error| ConfigError::InvalidConfig {
            reason: format!(
                "failed to serialize {} after MCP mutation: {error}",
                path.display()
            ),
        })?;
    bytes.push(b'\n');
    Ok((bytes, true))
}

fn apply_mutation(
    document: &mut Map<String, Value>,
    mutation: &McpPersistentMutation,
    path: &Path,
) -> Result<bool, ConfigError> {
    match mutation {
        McpPersistentMutation::Upsert { name, definition } => {
            let encoded =
                serde_json::to_value(definition).map_err(|error| ConfigError::InvalidConfig {
                    reason: format!("failed to encode mcp server '{name}': {error}"),
                })?;
            let Some(servers) = servers_mut(document, path, true)? else {
                return Err(ConfigError::InvalidConfig {
                    reason: format!("failed to create mcp_servers object in {}", path.display()),
                });
            };
            let previous = servers.insert(name.clone(), encoded.clone());
            Ok(previous.as_ref() != Some(&encoded))
        }
        McpPersistentMutation::Remove { name } => {
            let Some(servers) = servers_mut(document, path, false)? else {
                return Ok(false);
            };
            Ok(servers.remove(name).is_some())
        }
        McpPersistentMutation::SetEnabled { name, enabled } => {
            let Some(servers) = servers_mut(document, path, false)? else {
                return Err(missing_persistent(name, path));
            };
            let definition = servers
                .get_mut(name)
                .ok_or_else(|| missing_persistent(name, path))?
                .as_object_mut()
                .ok_or_else(|| ConfigError::InvalidConfig {
                    reason: format!(
                        "mcp server '{name}' in {} must be a JSON object",
                        path.display(),
                    ),
                })?;
            let value = Value::Bool(*enabled);
            if definition.get("enabled") == Some(&value) {
                return Ok(false);
            }
            definition.insert("enabled".to_owned(), value);
            Ok(true)
        }
    }
}

fn servers_mut<'a>(
    document: &'a mut Map<String, Value>,
    path: &Path,
    create: bool,
) -> Result<Option<&'a mut Map<String, Value>>, ConfigError> {
    if !document.contains_key("mcp_servers") {
        if !create {
            return Ok(None);
        }
        document.insert("mcp_servers".to_owned(), Value::Object(Map::new()));
    }
    document
        .get_mut("mcp_servers")
        .and_then(Value::as_object_mut)
        .map(Some)
        .ok_or_else(|| ConfigError::InvalidConfig {
            reason: format!("mcp_servers in {} must be a JSON object", path.display()),
        })
}

fn validate_mutation(mutation: &McpPersistentMutation) -> Result<(), ConfigError> {
    match mutation {
        McpPersistentMutation::Upsert { name, definition } => {
            validate_one(name, definition)?;
            fingerprint(name, definition)?;
        }
        McpPersistentMutation::Remove { name } | McpPersistentMutation::SetEnabled { name, .. } => {
            validate_one(
                name,
                &McpServerSettings {
                    enabled: Some(false),
                    ..McpServerSettings::default()
                },
            )?;
        }
    }
    Ok(())
}

fn missing_persistent(name: &str, path: &Path) -> ConfigError {
    ConfigError::InvalidConfig {
        reason: format!(
            "cannot change enabled state for missing mcp server '{name}' in {}",
            path.display(),
        ),
    }
}

fn document_config(error: &SettingsDocumentError) -> ConfigError {
    ConfigError::InvalidConfig {
        reason: error.to_string(),
    }
}

#[cfg(test)]
#[path = "mcp_patch_tests.rs"]
mod tests;
