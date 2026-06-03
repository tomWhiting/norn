//! CLI-level error type for runtime assembly (NC-004 R1–R8).
//!
//! [`BuildError`] is the single error returned by every NC-004 helper that
//! transforms CLI flags or profile files into runtime objects. The variant
//! drives the exit-code mapping in [`crate::exit::ExitCode`] per
//! `DESIGN.md` CO5:
//!
//! - [`BuildError::Argument`] → [`crate::exit::ExitCode::ArgumentError`] (2)
//! - [`BuildError::Auth`] → [`crate::exit::ExitCode::AuthError`] (3)
//!
//! Conversions exist for the underlying library errors so the helpers can
//! use `?` without writing custom mapping at every call site.

use super::exit::ExitCode;

/// Top-level error returned by the NC-004 runtime-assembly helpers.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// Bad CLI argument or unreadable user-supplied file.
    ///
    /// Maps to [`ExitCode::ArgumentError`] so shell pipelines can detect
    /// invocation problems separately from agent failures.
    #[error("argument error: {0}")]
    Argument(String),

    /// Authentication failure (login expired, credentials missing). NC-004
    /// itself never returns this — it is reserved for the provider-
    /// construction helpers in NC-003 — but the variant lives here so the
    /// exit-code mapping is centralised.
    #[error("auth error: {0}")]
    Auth(String),
}

impl BuildError {
    /// Map a [`BuildError`] onto its terminal [`ExitCode`].
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        match self {
            Self::Argument(_) => ExitCode::ArgumentError,
            Self::Auth(_) => ExitCode::AuthError,
        }
    }
}

impl From<norn::error::ConfigError> for BuildError {
    fn from(err: norn::error::ConfigError) -> Self {
        Self::Argument(err.to_string())
    }
}

impl From<norn::error::RulesError> for BuildError {
    fn from(err: norn::error::RulesError) -> Self {
        Self::Argument(err.to_string())
    }
}

impl From<std::io::Error> for BuildError {
    fn from(err: std::io::Error) -> Self {
        Self::Argument(err.to_string())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn argument_maps_to_exit_code_two() {
        let err = BuildError::Argument("bad flag".to_owned());
        assert_eq!(err.exit_code(), ExitCode::ArgumentError);
    }

    #[test]
    fn auth_maps_to_exit_code_three() {
        let err = BuildError::Auth("login expired".to_owned());
        assert_eq!(err.exit_code(), ExitCode::AuthError);
    }

    #[test]
    fn config_error_converts_to_argument() {
        let err: BuildError = norn::error::ConfigError::InvalidConfig {
            reason: "bad".to_owned(),
        }
        .into();
        assert!(matches!(err, BuildError::Argument(_)));
        assert_eq!(err.exit_code(), ExitCode::ArgumentError);
    }

    #[test]
    fn io_error_converts_to_argument() {
        let err: BuildError = std::io::Error::other("missing").into();
        assert!(matches!(err, BuildError::Argument(_)));
    }
}
