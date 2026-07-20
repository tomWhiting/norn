//! Typed failures emitted by print-mode execution.

use norn::error::{ErrorClass, NornError};

use crate::cli::{BuildError, ExitCode};
use crate::session::SessionPersistError;

/// Errors that surface from the print orchestrator. Each variant maps
/// cleanly onto an [`ExitCode`] via [`PrintError::exit_code`].
#[derive(Debug, thiserror::Error)]
pub enum PrintError {
    /// Bad CLI argument — flag parsing or runtime assembly rejected the
    /// invocation (exit code 2).
    #[error("argument error: {0}")]
    Argument(String),
    /// Authentication failure (exit code 3).
    #[error("auth error: {0}")]
    Auth(String),
    /// Agent-runtime failure: provider call, tool error, schema budget
    /// exhausted, etc. (exit code 1).
    #[error("agent error: {0}")]
    Agent(String),
    /// Filesystem / I/O failure when reading stdin or writing output
    /// (exit code 1 — treated as an agent error per CO5).
    #[error("I/O error: {0}")]
    Io(String),
    /// Session persistence failed (exit code 1).
    #[error("session error: {0}")]
    Session(String),
    /// The stream renderer tore stdout mid-run — panic or cancellation —
    /// so the NDJSON already written is incomplete (exit code 1). Never
    /// followed by an error envelope: appending a well-formed terminal
    /// event to a torn stream would make the output look more
    /// trustworthy than it is (owner ruling R4, 2026-07-06). The Display
    /// prefix deliberately matches [`PrintError::Agent`] so the stderr
    /// line is unchanged from when this failure rode the `Agent` variant.
    #[error("agent error: {0}")]
    StreamTorn(String),
}

impl PrintError {
    /// Terminal exit code per CO5.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        match self {
            Self::Argument(_) => ExitCode::ArgumentError,
            Self::Auth(_) => ExitCode::AuthError,
            Self::Agent(_) | Self::Io(_) | Self::Session(_) | Self::StreamTorn(_) => {
                ExitCode::AgentError
            }
        }
    }

    /// The machine-stable `stop.class` this failure carries on the typed
    /// error envelope, or `None` when the failure must stay stderr-only:
    /// argument errors keep clap parity (exit 2 — owner ruling R2) and a
    /// torn stream gets no envelope at all (owner ruling R4).
    #[must_use]
    pub const fn envelope_class(&self) -> Option<&'static str> {
        match self {
            Self::Argument(_) | Self::StreamTorn(_) => None,
            Self::Auth(_) => Some("auth"),
            Self::Agent(_) => Some("agent"),
            Self::Io(_) => Some("io"),
            Self::Session(_) => Some("session"),
        }
    }

    /// Retain this failure's classification while appending a related failure.
    ///
    /// Compound driven-mode failures use the first causal run failure as the
    /// authority for exit classification. Transport teardown must remain
    /// visible, but must not turn an authentication failure into a generic
    /// agent error.
    pub(crate) fn with_related_failure(self, related: impl std::fmt::Display) -> Self {
        let related = related.to_string();
        let append = |message: String| format!("{message}; additionally, {related}");
        match self {
            Self::Argument(message) => Self::Argument(append(message)),
            Self::Auth(message) => Self::Auth(append(message)),
            Self::Agent(message) => Self::Agent(append(message)),
            Self::Io(message) => Self::Io(append(message)),
            Self::Session(message) => Self::Session(append(message)),
            Self::StreamTorn(message) => Self::StreamTorn(append(message)),
        }
    }
}

/// Preserve an existing failure and its exit class while retaining a later
/// transport failure. If the primary operation succeeded, the transport error
/// remains authoritative.
pub(crate) fn preserve_primary_failure<T>(
    primary: Result<T, PrintError>,
    related: Result<(), PrintError>,
) -> Result<T, PrintError> {
    match (primary, related) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) | (Err(error), Ok(())) => Err(error),
        (Err(primary), Err(related)) => Err(primary.with_related_failure(related)),
    }
}

/// Preserve an already-failed provider/run result as the exit-code authority
/// while retaining a background failure discovered during shutdown.
pub(crate) fn preserve_run_failure<T, E>(
    result: Result<T, E>,
    background: Option<PrintError>,
) -> Result<T, PrintError>
where
    E: Into<PrintError>,
{
    match (result, background) {
        (Ok(value), None) => Ok(value),
        (Ok(_), Some(error)) => Err(error),
        (Err(error), None) => Err(error.into()),
        (Err(error), Some(background)) => Err(error.into().with_related_failure(background)),
    }
}

impl From<BuildError> for PrintError {
    fn from(err: BuildError) -> Self {
        match err {
            BuildError::Argument(msg) => Self::Argument(msg),
            BuildError::Auth(msg) => Self::Auth(msg),
        }
    }
}

impl From<std::io::Error> for PrintError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<SessionPersistError> for PrintError {
    fn from(err: SessionPersistError) -> Self {
        Self::Session(err.to_string())
    }
}

impl From<NornError> for PrintError {
    fn from(err: NornError) -> Self {
        if let NornError::Provider(ref provider_err) = err
            && provider_err.class() == ErrorClass::Auth
        {
            return Self::Auth(err.to_string());
        }
        Self::Agent(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_failure_preserves_auth_class_and_background_diagnostics() {
        let background = PrintError::Io(
            "intervention channel torn; additionally, event emitter lost 7 events".to_owned(),
        );
        let outcome = preserve_run_failure::<(), _>(
            Err(PrintError::Auth("credential expired".to_owned())),
            Some(background),
        );
        let Err(error) = outcome else { return };
        assert!(matches!(&error, PrintError::Auth(_)), "error: {error:?}");
        let rendered = error.to_string();
        for expected in [
            "credential expired",
            "intervention channel torn",
            "event emitter lost 7 events",
        ] {
            assert!(rendered.contains(expected), "error: {rendered}");
        }
        assert_eq!(error.exit_code(), ExitCode::AuthError);
    }

    #[test]
    fn writer_failure_preserves_primary_class_and_all_diagnostics() {
        let primary = PrintError::Auth("credential expired".to_owned())
            .with_related_failure("event emitter lost 7 events");
        let outcome = preserve_primary_failure::<()>(
            Err(primary),
            Err(PrintError::Io(
                "stdout writer failed: broken pipe".to_owned(),
            )),
        );
        let Err(error) = outcome else { return };
        assert!(matches!(&error, PrintError::Auth(_)), "error: {error:?}");
        let rendered = error.to_string();
        for expected in [
            "credential expired",
            "event emitter lost 7 events",
            "broken pipe",
        ] {
            assert!(rendered.contains(expected), "error: {rendered}");
        }
        assert_eq!(error.exit_code(), ExitCode::AuthError);
    }
}
