//! Trusted Norn OAuth credential-root resolution.

use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use crate::config::paths::DEFAULT_NORN_DIRECTORY;

const NORN_HOME: &str = "NORN_HOME";
const AUTH_DIRECTORY: &str = "auth";

/// Validated absolute root containing Norn-owned OAuth credentials.
///
/// Constructing this type is an ownership declaration. It must not wrap a
/// foreign Codex credential directory; foreign-source support requires a
/// separate observational type and never uses this writable root.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct NornAuthRoot(PathBuf);

impl NornAuthRoot {
    /// Borrow the validated absolute path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Consume the root and return its validated absolute path.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for NornAuthRoot {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl TryFrom<PathBuf> for NornAuthRoot {
    type Error = NornAuthRootError;

    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        validate_absolute(&path, NornAuthRootSource::Explicit)
    }
}

impl TryFrom<&Path> for NornAuthRoot {
    type Error = NornAuthRootError;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        Self::try_from(path.to_path_buf())
    }
}

impl fmt::Debug for NornAuthRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NornAuthRoot")
            .finish_non_exhaustive()
    }
}

/// Authority source selected for a Norn credential root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NornAuthRootSource {
    /// Explicit library override.
    Explicit,
    /// Non-empty `NORN_HOME` process environment value.
    NornHome,
    /// Operating-system account home used for the `.norn` fallback.
    TrustedHome,
}

impl fmt::Display for NornAuthRootSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Explicit => "explicit Norn auth directory",
            Self::NornHome => "NORN_HOME",
            Self::TrustedHome => "trusted home directory",
        })
    }
}

/// Failure to establish an absolute Norn credential root.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum NornAuthRootError {
    /// A selected path would make credential authority depend on the working directory.
    #[error("{origin} must be absolute")]
    Relative {
        /// Source that supplied the relative path.
        origin: NornAuthRootSource,
    },
    /// Neither an explicit/`NORN_HOME` override nor an absolute account home exists.
    #[error("could not determine an absolute home directory for Norn credentials")]
    TrustedHomeUnavailable,
    /// The filesystem root itself is not a valid credential directory.
    #[error("{origin} must name a directory below the filesystem root")]
    FilesystemRoot {
        /// Source that supplied the root-only path.
        origin: NornAuthRootSource,
    },
}

/// Resolve the Norn OAuth root using explicit, `NORN_HOME`, then account-home precedence.
///
/// An explicit library path names an owned auth directory itself. Otherwise the resolver
/// appends `auth` to `$NORN_HOME` or to the default `~/.norn` root. An empty
/// `NORN_HOME` is treated as unset. `$CODEX_HOME` has no authority over Norn's
/// writable credentials.
///
/// # Errors
///
/// Returns a typed error when a selected override is relative or no absolute
/// trusted account home is available.
pub fn resolve_norn_auth_root(
    explicit: Option<PathBuf>,
) -> Result<NornAuthRoot, NornAuthRootError> {
    resolve_from_sources(
        explicit,
        std::env::var_os(NORN_HOME),
        crate::config::paths::trusted_home_dir(),
    )
}

fn resolve_from_sources(
    explicit: Option<PathBuf>,
    environment: Option<OsString>,
    trusted_home: Option<PathBuf>,
) -> Result<NornAuthRoot, NornAuthRootError> {
    if let Some(path) = explicit {
        return validate_absolute(&path, NornAuthRootSource::Explicit);
    }
    if let Some(value) = environment.filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        let root = validate_absolute(&path, NornAuthRootSource::NornHome)?;
        return validate_absolute(
            &root.into_path_buf().join(AUTH_DIRECTORY),
            NornAuthRootSource::NornHome,
        );
    }
    let home = trusted_home.ok_or(NornAuthRootError::TrustedHomeUnavailable)?;
    let home = validate_absolute(&home, NornAuthRootSource::TrustedHome)?;
    let default_root = home
        .into_path_buf()
        .join(DEFAULT_NORN_DIRECTORY)
        .join(AUTH_DIRECTORY);
    validate_absolute(&default_root, NornAuthRootSource::TrustedHome)
}

fn validate_absolute(
    path: &Path,
    source: NornAuthRootSource,
) -> Result<NornAuthRoot, NornAuthRootError> {
    if !path.is_absolute() {
        return Err(NornAuthRootError::Relative { origin: source });
    }
    normalize_absolute(path, source)
}

#[cfg(unix)]
fn normalize_absolute(
    path: &Path,
    source: NornAuthRootSource,
) -> Result<NornAuthRoot, NornAuthRootError> {
    let (_, mut components) = normalized_components(path, source)?;
    #[cfg(target_os = "macos")]
    if matches!(components.first(), Some(first) if first == "tmp" || first == "var") {
        components.insert(0, OsString::from("private"));
    }
    let mut normalized = PathBuf::from("/");
    normalized.extend(components);
    Ok(NornAuthRoot(normalized))
}

fn normalized_components(
    path: &Path,
    source: NornAuthRootSource,
) -> Result<(Option<OsString>, Vec<OsString>), NornAuthRootError> {
    let mut prefix = None::<OsString>;
    let mut components = Vec::<OsString>::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(value) if value == "." => {}
            Component::Normal(value) if value == ".." => {
                components.pop();
            }
            Component::Normal(value) => components.push(value.to_os_string()),
            Component::ParentDir => {
                components.pop();
            }
            Component::Prefix(value) => {
                prefix = Some(value.as_os_str().to_os_string());
            }
        }
    }
    if components.is_empty() {
        return Err(NornAuthRootError::FilesystemRoot { origin: source });
    }
    Ok((prefix, components))
}

#[cfg(not(unix))]
fn normalize_absolute(
    path: &Path,
    source: NornAuthRootSource,
) -> Result<NornAuthRoot, NornAuthRootError> {
    let (prefix, components) = normalized_components(path, source)?;

    let mut normalized = PathBuf::new();
    if let Some(prefix) = prefix {
        normalized.push(prefix);
    }
    normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR));
    normalized.extend(components);
    Ok(NornAuthRoot(normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute_auth_path(suffix: &str) -> PathBuf {
        #[cfg(windows)]
        let base = PathBuf::from(r"C:\auth");
        #[cfg(not(windows))]
        let base = PathBuf::from("/auth");
        base.join(suffix)
    }

    fn filesystem_root_path() -> PathBuf {
        #[cfg(windows)]
        {
            PathBuf::from(r"C:\")
        }
        #[cfg(not(windows))]
        {
            PathBuf::from(std::path::MAIN_SEPARATOR_STR)
        }
    }

    #[test]
    fn explicit_root_has_precedence() -> Result<(), NornAuthRootError> {
        let explicit = absolute_auth_path("explicit");
        let root = resolve_from_sources(
            Some(explicit.clone()),
            Some(OsString::from("relative-environment")),
            None,
        )?;
        assert_eq!(root.as_path(), explicit.as_path());
        Ok(())
    }

    #[test]
    fn absolute_norn_home_selects_its_auth_directory() -> Result<(), NornAuthRootError> {
        let environment = absolute_auth_path("environment");
        let root = resolve_from_sources(None, Some(environment.clone().into_os_string()), None)?;
        assert_eq!(root.as_path(), environment.join("auth").as_path());
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn codex_home_cannot_redirect_norn_credentials() -> Result<(), NornAuthRootError> {
        let norn_home = absolute_auth_path("norn-home");
        let codex_home = absolute_auth_path("codex-home");
        let resolved = temp_env::with_vars(
            [
                ("NORN_HOME", Some(norn_home.as_os_str())),
                ("CODEX_HOME", Some(codex_home.as_os_str())),
            ],
            || resolve_norn_auth_root(None),
        )?;

        assert_eq!(resolved.as_path(), norn_home.join("auth").as_path());
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn codex_home_alone_falls_back_to_trusted_norn_home() -> Result<(), NornAuthRootError> {
        let trusted_home = crate::config::paths::trusted_home_dir()
            .ok_or(NornAuthRootError::TrustedHomeUnavailable)?;
        let expected = NornAuthRoot::try_from(
            trusted_home
                .join(DEFAULT_NORN_DIRECTORY)
                .join(AUTH_DIRECTORY),
        )?;
        let codex_home = absolute_auth_path("codex-home-only");
        let resolved = temp_env::with_vars(
            [
                ("NORN_HOME", None::<&std::ffi::OsStr>),
                ("CODEX_HOME", Some(codex_home.as_os_str())),
            ],
            || resolve_norn_auth_root(None),
        )?;

        assert_eq!(resolved, expected);
        Ok(())
    }

    #[test]
    fn explicit_path_conversion_validates_and_normalizes() -> Result<(), NornAuthRootError> {
        let root = NornAuthRoot::try_from(absolute_auth_path("one/../credentials"))?;
        assert_eq!(root.as_path(), absolute_auth_path("credentials").as_path());
        Ok(())
    }

    #[test]
    fn empty_environment_uses_trusted_home() -> Result<(), NornAuthRootError> {
        let trusted_home = absolute_auth_path("users/tester");
        let root = resolve_from_sources(None, Some(OsString::new()), Some(trusted_home.clone()))?;
        assert_eq!(
            root.as_path(),
            trusted_home.join(".norn").join("auth").as_path()
        );
        Ok(())
    }

    #[test]
    fn relative_explicit_root_is_rejected() {
        assert!(matches!(
            resolve_from_sources(Some(PathBuf::from("auth")), None, None),
            Err(NornAuthRootError::Relative {
                origin: NornAuthRootSource::Explicit,
            })
        ));
    }

    #[test]
    fn relative_norn_home_is_rejected() {
        assert!(matches!(
            resolve_from_sources(None, Some(OsString::from(".norn")), None),
            Err(NornAuthRootError::Relative {
                origin: NornAuthRootSource::NornHome,
            })
        ));
    }

    #[test]
    fn relative_trusted_home_is_rejected() {
        assert!(matches!(
            resolve_from_sources(None, None, Some(PathBuf::from("users/tester"))),
            Err(NornAuthRootError::Relative {
                origin: NornAuthRootSource::TrustedHome,
            })
        ));
    }

    #[test]
    fn unavailable_trusted_home_is_typed() {
        assert!(matches!(
            resolve_from_sources(None, None, None),
            Err(NornAuthRootError::TrustedHomeUnavailable)
        ));
    }

    #[test]
    fn lexical_aliases_have_one_storage_identity() -> Result<(), NornAuthRootError> {
        let root = resolve_from_sources(
            Some(absolute_auth_path("one/../shared/./credentials")),
            None,
            None,
        )?;
        assert_eq!(
            root.as_path(),
            absolute_auth_path("shared/credentials").as_path()
        );
        Ok(())
    }

    #[test]
    fn filesystem_root_is_rejected() {
        assert!(matches!(
            resolve_from_sources(Some(filesystem_root_path()), None, None),
            Err(NornAuthRootError::FilesystemRoot {
                origin: NornAuthRootSource::Explicit,
            })
        ));
    }
}
