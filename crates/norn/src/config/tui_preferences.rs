//! Compare-and-set mutations of opaque frontend keys through the shared document writer.

use super::settings_document::{SettingsDocument, workspace_path};
use super::settings_write::SettingsPublication;
use super::tui_preferences_types::{
    TuiPreferenceScope, TuiPreferencesChange, TuiPreferencesError, TuiPreferencesSnapshot,
};
use super::workspace_settings_document::WorkspaceSettingsFile;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::{Component, Path};

impl TuiPreferencesSnapshot {
    /// Capture one already-loaded layer without reading, locking, creating or writing files.
    pub fn from_layer(
        scope: TuiPreferenceScope,
        project_root: &Path,
        original: Option<Value>,
    ) -> Result<Self, TuiPreferencesError> {
        if !project_root.is_absolute()
            || project_root
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
        {
            return Err(TuiPreferencesError::Target {
                reason: "launch root must be normalized and absolute",
            });
        }
        let path = match scope {
            TuiPreferenceScope::User => {
                super::paths::settings_file().ok_or(TuiPreferencesError::Target {
                    reason: "personal settings path is unavailable",
                })?
            }
            TuiPreferenceScope::WorkspaceLocal => {
                workspace_path(project_root, WorkspaceSettingsFile::Local)
            }
        };
        validate_tui(original.as_ref(), &path)?;
        Ok(Self {
            scope,
            project_root: project_root.to_path_buf(),
            path,
            original,
        })
    }

    /// Save only the declared owned keys, comparing their original values under the shared lock.
    /// Missing replacement keys remove their corresponding owned keys. Unowned values survive.
    pub fn patch(
        &self,
        owned_keys: &[&str],
        replacement: &Map<String, Value>,
    ) -> Result<TuiPreferencesChange, TuiPreferencesError> {
        validate_patch(self, owned_keys, replacement)?;
        let permit = crate::resource::acquire_private_fs()?;
        let result = self.patch_locked(owned_keys, replacement);
        drop(permit);
        result
    }

    fn patch_locked(
        &self,
        owned_keys: &[&str],
        replacement: &Map<String, Value>,
    ) -> Result<TuiPreferencesChange, TuiPreferencesError> {
        let document = match self.scope {
            TuiPreferenceScope::User => SettingsDocument::private(&self.path)?,
            TuiPreferenceScope::WorkspaceLocal => {
                SettingsDocument::workspace(&self.project_root, WorkspaceSettingsFile::Local)?
            }
        };
        let original = document.read()?;
        let (bytes, changed, tui) =
            patch_document(self, original.as_deref(), owned_keys, replacement)?;
        let publication = if changed {
            document.replace(&bytes)?
        } else {
            SettingsPublication::Unchanged
        };
        let mut snapshot = self.clone();
        snapshot.original = tui;
        Ok(TuiPreferencesChange {
            snapshot,
            publication,
        })
    }
}

fn validate_patch(
    snapshot: &TuiPreferencesSnapshot,
    owned_keys: &[&str],
    replacement: &Map<String, Value>,
) -> Result<(), TuiPreferencesError> {
    let owned: BTreeSet<_> = owned_keys.iter().copied().collect();
    let reason = if owned.is_empty() || owned.contains("") {
        Some("owned keys must be nonempty")
    } else if owned.len() != owned_keys.len() {
        Some("owned keys must be distinct")
    } else if replacement.keys().any(|key| !owned.contains(key.as_str())) {
        Some("replacement contains an unowned key")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(TuiPreferencesError::InvalidPatch {
            path: snapshot.path.clone(),
            reason,
        });
    }
    Ok(())
}

fn validate_tui(value: Option<&Value>, path: &Path) -> Result<(), TuiPreferencesError> {
    if value.is_some_and(|value| !value.is_object()) {
        return Err(TuiPreferencesError::InvalidTui {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn patch_document(
    snapshot: &TuiPreferencesSnapshot,
    original: Option<&str>,
    owned_keys: &[&str],
    replacement: &Map<String, Value>,
) -> Result<(Vec<u8>, bool, Option<Value>), TuiPreferencesError> {
    let mut document: Value = match original {
        Some(content) => {
            serde_json::from_str(content).map_err(|source| TuiPreferencesError::Json {
                path: snapshot.path.clone(),
                source,
            })?
        }
        None => Value::Object(Map::new()),
    };
    let object = document
        .as_object_mut()
        .ok_or_else(|| TuiPreferencesError::InvalidDocument {
            path: snapshot.path.clone(),
        })?;
    let before = object.get("tui");
    validate_tui(before, &snapshot.path)?;
    for key in owned_keys {
        if before.and_then(|value| value.get(key))
            != snapshot.original.as_ref().and_then(|value| value.get(key))
        {
            return Err(TuiPreferencesError::Conflict {
                path: snapshot.path.clone(),
                key: (*key).to_owned(),
            });
        }
    }
    let mut patched = before
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for key in owned_keys {
        if let Some(value) = replacement.get(*key) {
            patched.insert((*key).to_owned(), value.clone());
        } else {
            patched.remove(*key);
        }
    }
    let after = (!patched.is_empty()).then_some(Value::Object(patched));
    if before == after.as_ref() {
        return Ok((Vec::new(), false, after));
    }
    if let Some(value) = &after {
        object.insert("tui".to_owned(), value.clone());
    } else {
        object.remove("tui");
    }
    let mut bytes =
        serde_json::to_vec_pretty(&document).map_err(|source| TuiPreferencesError::Json {
            path: snapshot.path.clone(),
            source,
        })?;
    bytes.push(b'\n');
    Ok((bytes, true, after))
}

#[cfg(test)]
#[path = "tui_preferences_tests.rs"]
mod tests;
