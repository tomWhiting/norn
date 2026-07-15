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
