//! Validated repository-relative paths used by policy identities.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// A normalized, slash-separated path relative to the repository root.
///
/// Values never contain an absolute/platform prefix, an empty component,
/// traversal, a backslash, or a control character. This makes equality and
/// ordering independent of the host platform.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RepositoryPath(String);

impl RepositoryPath {
    /// Parse and validate one repository-relative path.
    ///
    /// # Errors
    ///
    /// Returns the precise structural reason the path is not normalized.
    pub fn parse(value: impl Into<String>) -> Result<Self, RepositoryPathError> {
        let value = value.into();
        validate(&value)?;
        Ok(Self(value))
    }

    /// Return the normalized slash-separated representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the final normalized component.
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.0
            .rsplit_once('/')
            .map_or(self.0.as_str(), |(_, file_name)| file_name)
    }

    /// Return the normalized parent path, or `None` for a root-level entry.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        self.0
            .rsplit_once('/')
            .map(|(parent, _)| Self(parent.to_owned()))
    }
}

impl AsRef<str> for RepositoryPath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for RepositoryPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Debug for RepositoryPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("RepositoryPath")
            .field(&self.0)
            .finish()
    }
}

impl FromStr for RepositoryPath {
    type Err = RepositoryPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for RepositoryPath {
    type Error = RepositoryPathError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for RepositoryPath {
    type Error = RepositoryPathError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl Serialize for RepositoryPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RepositoryPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// Structural reasons a repository path is not normalized.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RepositoryPathError {
    /// No path components were supplied.
    #[error("repository path is empty")]
    Empty,
    /// The path begins at a filesystem root.
    #[error("repository path is absolute")]
    Absolute,
    /// The path begins with a Windows drive prefix.
    #[error("repository path has a Windows drive prefix")]
    WindowsPrefix,
    /// The path contains a platform-dependent backslash separator.
    #[error("repository path contains a backslash")]
    Backslash,
    /// The path contains adjacent or trailing separators.
    #[error("repository path contains an empty component")]
    EmptyComponent,
    /// The path contains a current-directory component.
    #[error("repository path contains a dot component")]
    DotComponent,
    /// The path contains parent traversal.
    #[error("repository path contains a parent component")]
    ParentComponent,
    /// The path contains a control character.
    #[error("repository path contains a control character")]
    ControlCharacter,
}

fn validate(value: &str) -> Result<(), RepositoryPathError> {
    if value.is_empty() {
        return Err(RepositoryPathError::Empty);
    }
    if value.starts_with('/') {
        return Err(RepositoryPathError::Absolute);
    }
    if has_windows_drive_prefix(value) {
        return Err(RepositoryPathError::WindowsPrefix);
    }
    if value.contains('\\') {
        return Err(RepositoryPathError::Backslash);
    }
    if value.chars().any(char::is_control) {
        return Err(RepositoryPathError::ControlCharacter);
    }
    for component in value.split('/') {
        match component {
            "" => return Err(RepositoryPathError::EmptyComponent),
            "." => return Err(RepositoryPathError::DotComponent),
            ".." => return Err(RepositoryPathError::ParentComponent),
            _ => {}
        }
    }
    Ok(())
}

fn has_windows_drive_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}
