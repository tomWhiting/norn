//! Exit code conventions for the Norn CLI (NC-001 R5).
//!
//! Maps logical CLI outcomes to numeric exit codes per `DESIGN.md` CO5:
//! `0` = success, `1` = agent error, `2` = CLI argument error (handled
//! automatically by clap on parse failure), `3` = authentication error.

/// Typed exit code surface for the `norn` binary.
///
/// Use [`ExitCode::Success`] for clean completion, [`ExitCode::AgentError`]
/// for any runtime failure inside the agent loop (provider failure, tool
/// error, schema unreachable), [`ExitCode::ArgumentError`] for invalid
/// invocation (clap emits this automatically on parse failure), and
/// [`ExitCode::AuthError`] for authentication failures (login expired,
/// OAuth flow rejected, credentials missing).
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitCode {
    /// Successful completion (exit code 0).
    Success = 0,
    /// Agent runtime error — provider failure, tool error, schema
    /// unreachable, etc. (exit code 1).
    AgentError = 1,
    /// CLI argument parsing error — clap returns this automatically
    /// on invalid flags or unknown subcommands (exit code 2).
    ArgumentError = 2,
    /// Authentication error — login expired, OAuth rejected, credentials
    /// missing (exit code 3).
    AuthError = 3,
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        // All variants are in 0..=3, so the `as u8` cast is lossless.
        Self::from(code as u8)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn success_is_zero() {
        assert_eq!(ExitCode::Success as i32, 0);
    }

    #[test]
    fn agent_error_is_one() {
        assert_eq!(ExitCode::AgentError as i32, 1);
    }

    #[test]
    fn argument_error_is_two() {
        assert_eq!(ExitCode::ArgumentError as i32, 2);
    }

    #[test]
    fn auth_error_is_three() {
        assert_eq!(ExitCode::AuthError as i32, 3);
    }

    #[test]
    fn variants_are_distinct() {
        assert_ne!(ExitCode::Success, ExitCode::AgentError);
        assert_ne!(ExitCode::Success, ExitCode::ArgumentError);
        assert_ne!(ExitCode::Success, ExitCode::AuthError);
        assert_ne!(ExitCode::AgentError, ExitCode::ArgumentError);
        assert_ne!(ExitCode::AgentError, ExitCode::AuthError);
        assert_ne!(ExitCode::ArgumentError, ExitCode::AuthError);
    }

    #[test]
    fn converts_to_process_exit_code() {
        // The conversion compiles and runs; `std::process::ExitCode` does
        // not expose its inner numeric value for direct comparison, so
        // observing successful construction is the meaningful check.
        let _: std::process::ExitCode = ExitCode::Success.into();
        let _: std::process::ExitCode = ExitCode::AgentError.into();
        let _: std::process::ExitCode = ExitCode::ArgumentError.into();
        let _: std::process::ExitCode = ExitCode::AuthError.into();
    }
}
