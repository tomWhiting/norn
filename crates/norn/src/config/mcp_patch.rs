//! MCP-only persistent settings document mutation.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::McpServerSettings;
use super::mcp::{fingerprint, validate_one};
use super::mcp_state_types::{McpPersistentChange, McpPersistentMutation, McpPersistentScope};
use super::mcp_workspace_write::{WorkspaceSettingsDocument, WorkspaceSettingsFile};
use crate::error::{ConfigError, NornError};
use crate::util::PrivateRoot;

pub(super) fn persist_mcp_mutation(
    project_root: &Path,
    scope: McpPersistentScope,
    mutation: &McpPersistentMutation,
) -> Result<McpPersistentChange, NornError> {
    validate_mutation(mutation)?;
    let _descriptor_permit =
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
    Ok(McpPersistentChange::new(scope, path, changed))
}

fn mutate_workspace_document(
    project_root: &Path,
    kind: WorkspaceSettingsFile,
    mutation: &McpPersistentMutation,
) -> Result<(PathBuf, bool), ConfigError> {
    let document = WorkspaceSettingsDocument::open(project_root, kind)
        .map_err(|error| io_config("open workspace MCP settings", project_root, &error))?;
    let path = document.display_path();
    let original = document
        .read()
        .map_err(|error| io_config("read workspace MCP settings", &path, &error))?;
    let (bytes, changed) = patch_document(original.as_deref(), mutation, &path)?;
    if changed {
        document
            .replace(&bytes)
            .map_err(|error| io_config("replace workspace MCP settings", &path, &error))?;
    }
    Ok((path, changed))
}

struct PrivateSettingsDocument {
    root: PrivateRoot,
    file_name: PathBuf,
    display_path: PathBuf,
    lock: File,
}

impl PrivateSettingsDocument {
    fn open(path: &Path) -> Result<Self, ConfigError> {
        let parent = path.parent().ok_or_else(|| ConfigError::InvalidConfig {
            reason: format!(
                "private MCP settings path has no parent: {}",
                path.display()
            ),
        })?;
        let file_name =
            path.file_name()
                .map(PathBuf::from)
                .ok_or_else(|| ConfigError::InvalidConfig {
                    reason: format!(
                        "private MCP settings path has no file name: {}",
                        path.display()
                    ),
                })?;
        let root = PrivateRoot::create(parent)
            .map_err(|error| io_config("open private MCP settings root", path, &error))?;
        let lock = root
            .open_lock(Path::new(".mcp-settings.lock"))
            .map_err(|error| io_config("open private MCP settings lock", path, &error))?;
        lock.lock()
            .map_err(|error| io_config("lock private MCP settings", path, &error))?;
        Ok(Self {
            root,
            file_name,
            display_path: path.to_path_buf(),
            lock,
        })
    }

    fn read(&self) -> Result<Option<String>, ConfigError> {
        let mut file = match self.root.open_read(&self.file_name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(io_config(
                    "read private MCP settings",
                    &self.display_path,
                    &error,
                ));
            }
        };
        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|error| io_config("read private MCP settings", &self.display_path, &error))?;
        Ok(Some(content))
    }

    fn replace(&self, bytes: &[u8]) -> Result<(), ConfigError> {
        let temporary = PathBuf::from(format!(
            ".{}.mcp.tmp.{}",
            self.file_name.display(),
            uuid::Uuid::new_v4(),
        ));
        let result = self.write_and_publish(&temporary, bytes);
        if result.is_err()
            && let Err(error) = self.root.remove_file(&temporary)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.display_path.display(),
                temporary = %temporary.display(),
                %error,
                "failed to remove temporary private MCP settings file",
            );
        }
        result
    }

    fn write_and_publish(&self, temporary: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
        let mut file = self.root.create_new(temporary).map_err(|error| {
            io_config(
                "create private MCP settings temp",
                &self.display_path,
                &error,
            )
        })?;
        file.write_all(bytes)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
            .map_err(|error| io_config("write private MCP settings", &self.display_path, &error))?;
        drop(file);
        self.root
            .rename(temporary, &self.file_name)
            .map_err(|error| {
                io_config("replace private MCP settings", &self.display_path, &error)
            })?;
        self.root.sync_dir(Path::new("")).map_err(|error| {
            io_config(
                "sync private MCP settings directory",
                &self.display_path,
                &error,
            )
        })
    }
}

impl Drop for PrivateSettingsDocument {
    fn drop(&mut self) {
        if let Err(error) = self.lock.unlock() {
            tracing::warn!(
                path = %self.display_path.display(),
                %error,
                "failed to explicitly unlock private MCP settings",
            );
        }
    }
}

fn mutate_private_document(
    path: &Path,
    mutation: &McpPersistentMutation,
) -> Result<bool, ConfigError> {
    let document = PrivateSettingsDocument::open(path)?;
    let original = document.read()?;
    let (bytes, changed) = patch_document(original.as_deref(), mutation, path)?;
    if changed {
        document.replace(&bytes)?;
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

fn io_config(operation: &str, path: &Path, error: &io::Error) -> ConfigError {
    ConfigError::InvalidConfig {
        reason: format!("failed to {operation} at {}: {error}", path.display()),
    }
}

#[cfg(test)]
#[path = "mcp_patch_tests.rs"]
mod tests;
