//! Admission-aware profile path discovery.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::error::ConfigError;

pub(super) const PROFILE_EXTENSIONS: [&str; 3] = ["md", "toml", "json"];

pub(super) fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Two-tier profile discovery over an ordered directory list.
pub struct Scanner {
    scan_dirs: Vec<PathBuf>,
}

impl Scanner {
    /// Constructs a scanner that walks `scan_dirs` in the given order.
    #[must_use]
    pub fn new(scan_dirs: Vec<PathBuf>) -> Self {
        Self { scan_dirs }
    }

    /// Compatibility lookup that reports admission failures before returning
    /// no result. Fallible callers should prefer [`Self::try_resolve`].
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<PathBuf> {
        match self.try_resolve(name) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(%error, "profile lookup could not obtain descriptor admission");
                None
            }
        }
    }

    /// Locates the first profile with operation-scoped descriptor admission.
    ///
    /// # Errors
    ///
    /// Returns a self-diagnosing configuration error when descriptor
    /// admission fails.
    pub fn try_resolve(&self, name: &str) -> Result<Option<PathBuf>, ConfigError> {
        self.resolve_with_directory_index(name)
            .map(|resolved| resolved.map(|value| value.0))
    }

    pub(super) fn resolve_with_directory_index(
        &self,
        name: &str,
    ) -> Result<Option<(PathBuf, usize)>, ConfigError> {
        if !is_safe_name(name) {
            return Ok(None);
        }
        for (index, dir) in self.scan_dirs.iter().enumerate() {
            for ext in PROFILE_EXTENSIONS {
                let candidate = dir.join(format!("{name}.{ext}"));
                let _descriptor_permit = admit(&candidate, "stat")?;
                match std::fs::symlink_metadata(&candidate) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Ok(_) | Err(_) => return Ok(Some((candidate, index))),
                }
            }
        }
        Ok(None)
    }

    /// Compatibility listing that reports admission failures before
    /// returning the successfully enumerated prefix.
    #[must_use]
    pub fn list_profiles(&self) -> Vec<String> {
        match self.try_list_profiles() {
            Ok(names) => names,
            Err(error) => {
                tracing::warn!(%error, "profile listing stopped by descriptor admission");
                Vec::new()
            }
        }
    }

    /// Returns the deduplicated, sorted profile names.
    ///
    /// # Errors
    ///
    /// Returns a self-diagnosing configuration error instead of treating an
    /// admission failure as an absent directory.
    pub fn try_list_profiles(&self) -> Result<Vec<String>, ConfigError> {
        let mut seen = HashSet::new();
        let mut names = Vec::new();
        for dir in &self.scan_dirs {
            let _descriptor_permit = admit(dir, "enumerate")?;
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    tracing::debug!(
                        "Skipping profile dir {} during list: {error}",
                        dir.display()
                    );
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        tracing::warn!(
                            "Error reading directory entry in {}: {error}",
                            dir.display()
                        );
                        continue;
                    }
                };
                let path = entry.path();
                let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
                    continue;
                };
                if !PROFILE_EXTENSIONS.contains(&extension) {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
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
        Ok(names)
    }
}

fn admit(
    path: &std::path::Path,
    operation: &str,
) -> Result<crate::resource::FilesystemOperationPermit, ConfigError> {
    crate::resource::acquire_filesystem_operation().map_err(|error| ConfigError::InvalidConfig {
        reason: format!(
            "descriptor admission failed while attempting to {operation} profile path {}: {error}",
            path.display()
        ),
    })
}
