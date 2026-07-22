//! `norn auth …` subcommand dispatchers (NC-008 R8–R10).
//!
//! Wraps the synchronous `auth` flow around the async functions exposed
//! by [`norn::provider::auth`]. A short-lived multi-threaded tokio
//! runtime is created per invocation — `auth login` blocks for as long
//! as the user takes to complete the browser PKCE flow, so a runtime
//! that supports `block_on` for the full duration is required.
//!
//! Token-handling rule (CO5 + DESIGN.md NC13): raw access tokens,
//! refresh tokens, JWT bodies, and account identities MUST NEVER appear in
//! stdout or stderr output. `auth status` reports only side-effect-free local
//! credential classification; it never claims remote validity.

use std::sync::Arc;

use chrono::Utc;
use norn::provider::auth::{
    LoginConfig, LoginPrompt, LoginPromptError, LoginPromptPresenter, command_account_root,
    list_auth_accounts, login, login_named, logout, logout_all_auth_accounts, logout_named,
    use_auth_account,
};
use norn::provider::openai_oauth::{
    AuthCredentialsStoreMode, CredentialInspectionError, LocalCredentialState, LogoutReport,
    MalformedCredentialReason, NornAuthRoot, RefreshCandidateReason, RemoteRevokeOutcome,
    UnknownExpiryReason, inspect_file_credential,
};

use crate::cli::AuthCmd;
use crate::cli::ExitCode;

/// Top-level dispatcher for `norn auth`.
pub fn run_auth(cmd: &AuthCmd) -> ExitCode {
    match cmd {
        AuthCmd::Login { name, device_auth } => run_login(name.as_deref(), *device_auth),
        AuthCmd::Logout { name, all } => run_logout(name.as_deref(), *all),
        AuthCmd::Status { name } => run_status(name.as_deref()),
        AuthCmd::List => run_list(),
        AuthCmd::Use { name } => run_use(name),
    }
}

// ---------------------------------------------------------------------------
// R8: login
// ---------------------------------------------------------------------------

fn run_login(name: Option<&str>, device_auth: bool) -> ExitCode {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("norn: failed to create tokio runtime: {err}");
            return ExitCode::AuthError;
        }
    };
    let mut config = LoginConfig::default();
    config.device_code = device_auth;
    let config = config.with_prompt_presenter(Arc::new(TerminalLoginPromptPresenter));
    let result = match name {
        Some(name) => rt.block_on(login_named(config, name)),
        None => rt.block_on(login(config)),
    };
    match result {
        Ok(()) => {
            eprintln!("Login successful.");
            ExitCode::Success
        }
        Err(err) => {
            eprintln!("norn: login failed: {err}");
            ExitCode::AuthError
        }
    }
}

struct TerminalLoginPromptPresenter;

impl LoginPromptPresenter for TerminalLoginPromptPresenter {
    fn present(&self, prompt: LoginPrompt<'_>) -> Result<(), LoginPromptError> {
        let stderr = std::io::stderr();
        write_login_prompt(&mut stderr.lock(), &prompt)
            .map_err(|_error| LoginPromptError::terminal_output_unavailable())
    }
}

fn write_login_prompt(
    writer: &mut dyn std::io::Write,
    prompt: &LoginPrompt<'_>,
) -> std::io::Result<()> {
    match prompt {
        LoginPrompt::Browser { authorization_url } => writeln!(
            writer,
            "If your browser did not open, visit this URL on this machine:\n\n{authorization_url}\n\nOn a remote or headless machine, cancel and run `norn auth login --device-auth` instead."
        ),
        LoginPrompt::DeviceCode {
            verification_url,
            user_code,
            expires_after,
        } => {
            writeln!(
                writer,
                "Open this URL in any browser:\n\n{verification_url}\n\nEnter this one-time code:\n\n{user_code}\n\nNorn will wait up to {} for authorization. Never share a device code. Press Ctrl-C to cancel.",
                format_duration(*expires_after)
            )
        }
    }
}

fn format_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    if seconds > 0 && seconds.is_multiple_of(60) {
        let minutes = seconds / 60;
        let unit = if minutes == 1 { "minute" } else { "minutes" };
        format!("{minutes} {unit}")
    } else {
        let unit = if seconds == 1 { "second" } else { "seconds" };
        format!("{seconds} {unit}")
    }
}

// ---------------------------------------------------------------------------
// R9: logout
// ---------------------------------------------------------------------------

fn run_logout(name: Option<&str>, all: bool) -> ExitCode {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("norn: failed to create tokio runtime: {err}");
            return ExitCode::AuthError;
        }
    };
    if all {
        return run_logout_all(&rt);
    }
    let result = name.map_or_else(
        || rt.block_on(logout(LoginConfig::default())),
        |name| rt.block_on(logout_named(LoginConfig::default(), name)),
    );
    match result {
        Ok(report) => report_logout(report),
        Err(err) => {
            eprintln!("norn: logout failed: {err}");
            ExitCode::AuthError
        }
    }
}

fn run_logout_all(runtime: &tokio::runtime::Runtime) -> ExitCode {
    let reports = match runtime.block_on(logout_all_auth_accounts()) {
        Ok(reports) => reports,
        Err(error) => {
            eprintln!("norn: could not complete all-account logout: {error}");
            return ExitCode::AuthError;
        }
    };
    let mut exit_code = ExitCode::Success;
    for report in reports {
        if report_logout(report) != ExitCode::Success {
            exit_code = ExitCode::AuthError;
        }
    }
    exit_code
}

fn report_logout(report: LogoutReport) -> ExitCode {
    match (report.local, report.remote) {
        (Ok(_), RemoteRevokeOutcome::Revoked | RemoteRevokeOutcome::NotApplicable) => {
            eprintln!("Logged out.");
            ExitCode::Success
        }
        (Ok(_), RemoteRevokeOutcome::Failed(remote_error)) => {
            eprintln!(
                "Logged out locally; remote token revocation was not completed: {remote_error}"
            );
            ExitCode::AuthError
        }
        (Err(local_error), RemoteRevokeOutcome::Revoked) => {
            eprintln!(
                "norn: remote token revoked, but local credential removal failed: {local_error}"
            );
            ExitCode::AuthError
        }
        (Err(local_error), RemoteRevokeOutcome::NotApplicable) => {
            eprintln!("norn: local credential removal failed: {local_error}");
            ExitCode::AuthError
        }
        (Err(local_error), RemoteRevokeOutcome::Failed(remote_error)) => {
            eprintln!("norn: remote token revocation was not completed: {remote_error}");
            eprintln!("norn: local credential removal failed: {local_error}");
            ExitCode::AuthError
        }
    }
}

// ---------------------------------------------------------------------------
// R10: status
// ---------------------------------------------------------------------------

fn run_status(name: Option<&str>) -> ExitCode {
    let auth_root = match command_account_root(name) {
        Ok(root) => root,
        Err(reason) => {
            eprintln!("norn: cannot resolve auth state: {reason}");
            return ExitCode::AuthError;
        }
    };
    run_status_at(&auth_root)
}

fn run_list() -> ExitCode {
    match list_auth_accounts() {
        Ok(accounts) => {
            for account in accounts {
                let marker = if account.active { "*" } else { " " };
                println!("{marker} {}", account.alias);
            }
            ExitCode::Success
        }
        Err(error) => {
            eprintln!("norn: could not list auth accounts: {error}");
            ExitCode::AuthError
        }
    }
}

fn run_use(name: &str) -> ExitCode {
    match use_auth_account(name) {
        Ok(()) => {
            eprintln!("Selected OAuth account for new providers.");
            ExitCode::Success
        }
        Err(error) => {
            eprintln!("norn: could not select auth account: {error}");
            ExitCode::AuthError
        }
    }
}

fn run_status_at(auth_root: &NornAuthRoot) -> ExitCode {
    match status_report_at(auth_root) {
        Ok(report) => emit_status_report(&report),
        Err(error) => {
            eprintln!(
                "norn: OAuth credential storage could not be inspected ({error}); remote validity is unverified."
            );
            ExitCode::AuthError
        }
    }
}

pub(crate) fn inspect_auth_state(
    auth_root: &NornAuthRoot,
) -> Result<LocalCredentialState, CredentialInspectionError> {
    inspect_file_credential(auth_root, AuthCredentialsStoreMode::File, Utc::now())
}

fn status_report_at(auth_root: &NornAuthRoot) -> Result<StatusReport, CredentialInspectionError> {
    inspect_auth_state(auth_root).map(|state| status_report(&state))
}

fn emit_status_report(report: &StatusReport) -> ExitCode {
    if report.exit_code == ExitCode::Success {
        println!("{}", report.message);
    } else {
        eprintln!("norn: {}", report.message);
    }
    report.exit_code
}

#[derive(Debug, Eq, PartialEq)]
struct StatusReport {
    message: String,
    exit_code: ExitCode,
}

fn status_report(state: &LocalCredentialState) -> StatusReport {
    match state {
        LocalCredentialState::Missing => StatusReport {
            message: "No local OAuth credentials found; remote validity is unverified.".to_owned(),
            exit_code: ExitCode::Success,
        },
        LocalCredentialState::Malformed { reason } => StatusReport {
            message: format!(
                "local OAuth credentials are malformed ({}); remote validity is unverified. Run `norn auth login` to authenticate again.",
                malformed_reason(*reason)
            ),
            exit_code: ExitCode::AuthError,
        },
        LocalCredentialState::AccessExpired { expired_at } => StatusReport {
            message: format!(
                "OAuth access token expired at {} and cannot be refreshed locally; remote validity is unverified. Run `norn auth login` to authenticate again.",
                expired_at.format("%Y-%m-%dT%H:%M:%SZ")
            ),
            exit_code: ExitCode::AuthError,
        },
        LocalCredentialState::RefreshCandidate { reason, expired_at } => {
            let detail = refresh_candidate_detail(*reason, expired_at.as_ref());
            StatusReport {
                message: format!(
                    "OAuth credentials require refresh before the next provider request because {detail}; remote validity is unverified."
                ),
                exit_code: ExitCode::Success,
            }
        }
        LocalCredentialState::LocallyValid { expires_at } => StatusReport {
            message: format!(
                "OAuth access token is locally valid until {}; remote validity is unverified.",
                expires_at.format("%Y-%m-%dT%H:%M:%SZ")
            ),
            exit_code: ExitCode::Success,
        },
        LocalCredentialState::Unknown { reason } => StatusReport {
            message: format!(
                "OAuth credentials are present, but access-token expiry is unknown ({}); remote validity is unverified.",
                unknown_expiry_reason(*reason)
            ),
            exit_code: ExitCode::Success,
        },
    }
}

pub(crate) const fn malformed_reason(reason: MalformedCredentialReason) -> &'static str {
    match reason {
        MalformedCredentialReason::InvalidJson => "invalid JSON",
        MalformedCredentialReason::UnsupportedAuthMode => "unsupported authentication mode",
        MalformedCredentialReason::MixedCredentialKinds => "mixed credential kinds",
        MalformedCredentialReason::MissingTokenBundle => "missing token bundle",
        MalformedCredentialReason::MalformedIdTokenClaims => "malformed id-token claims",
        MalformedCredentialReason::InvalidAccessToken => "invalid access token shape",
        MalformedCredentialReason::InvalidRefreshToken => "invalid refresh token shape",
        MalformedCredentialReason::MissingAccountId => "missing account identity",
        MalformedCredentialReason::InvalidAccountId => "invalid account identity",
        MalformedCredentialReason::ConflictingAccountIds => "conflicting account identities",
        MalformedCredentialReason::MalformedAccessTokenClaims => "malformed access-token claims",
        MalformedCredentialReason::MissingUsableToken => "missing usable token",
    }
}

pub(crate) fn refresh_candidate_detail(
    reason: RefreshCandidateReason,
    expired_at: Option<&chrono::DateTime<Utc>>,
) -> String {
    match reason {
        RefreshCandidateReason::AccessExpired => expired_at.map_or_else(
            || "the access token is expired".to_owned(),
            |expired_at| {
                format!(
                    "the access token expired at {}",
                    expired_at.format("%Y-%m-%dT%H:%M:%SZ")
                )
            },
        ),
        RefreshCandidateReason::AccessMissing => "the access token is missing".to_owned(),
    }
}

pub(crate) const fn unknown_expiry_reason(reason: UnknownExpiryReason) -> &'static str {
    match reason {
        UnknownExpiryReason::OpaqueAccessToken => "opaque access token",
        UnknownExpiryReason::MissingExpiration => "missing expiration claim",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn terminal_prompts_show_only_intended_interactive_values() -> TestResult {
        let mut output = Vec::new();
        write_login_prompt(
            &mut output,
            &LoginPrompt::Browser {
                authorization_url: "https://auth.example/authorize?state=terminal-only",
            },
        )?;
        write_login_prompt(
            &mut output,
            &LoginPrompt::DeviceCode {
                verification_url: "https://auth.example/device",
                user_code: "CODE-1234",
                expires_after: std::time::Duration::from_secs(900),
            },
        )?;
        let output = String::from_utf8(output)?;

        assert!(output.contains("terminal-only"));
        assert!(output.contains("--device-auth"));
        assert!(output.contains("CODE-1234"));
        assert!(output.contains("15 minutes"));
        assert!(!output.contains("access_token"));
        assert!(!output.contains("refresh_token"));
        Ok(())
    }

    #[test]
    fn status_with_no_auth_file_succeeds() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let auth_root = NornAuthRoot::try_from(tmp.path())?;
        let code = run_status_at(&auth_root);
        assert_eq!(code, ExitCode::Success);
        Ok(())
    }

    #[test]
    fn status_with_malformed_auth_file_fails() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        std::fs::write(tmp.path().join("auth.json"), b"{malformed")?;
        let auth_root = NornAuthRoot::try_from(tmp.path())?;
        let code = run_status_at(&auth_root);
        assert_eq!(code, ExitCode::AuthError);
        Ok(())
    }

    #[test]
    fn inspection_classifies_missing_without_collapsing_to_a_boolean()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let auth_root = NornAuthRoot::try_from(tmp.path())?;
        let state = inspect_auth_state(&auth_root)?;
        assert_eq!(state, LocalCredentialState::Missing);
        Ok(())
    }

    #[test]
    fn every_present_status_disclaims_remote_validity_and_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let timestamp = chrono::DateTime::from_timestamp(1_700_000_000, 0)
            .ok_or_else(|| std::io::Error::other("test timestamp is invalid"))?;
        let states = [
            LocalCredentialState::Malformed {
                reason: MalformedCredentialReason::InvalidJson,
            },
            LocalCredentialState::AccessExpired {
                expired_at: timestamp,
            },
            LocalCredentialState::RefreshCandidate {
                reason: RefreshCandidateReason::AccessExpired,
                expired_at: Some(timestamp),
            },
            LocalCredentialState::LocallyValid {
                expires_at: timestamp,
            },
            LocalCredentialState::Unknown {
                reason: UnknownExpiryReason::OpaqueAccessToken,
            },
        ];

        for state in states {
            let report = status_report(&state);
            assert!(report.message.contains("remote validity is unverified"));
            assert!(!report.message.contains("account"));
        }
        Ok(())
    }

    #[test]
    fn status_exit_codes_distinguish_unusable_from_refreshable_states()
    -> Result<(), Box<dyn std::error::Error>> {
        let timestamp = chrono::DateTime::from_timestamp(1_700_000_000, 0)
            .ok_or_else(|| std::io::Error::other("test timestamp is invalid"))?;
        let malformed = status_report(&LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::InvalidJson,
        });
        let expired = status_report(&LocalCredentialState::AccessExpired {
            expired_at: timestamp,
        });
        let refreshable = status_report(&LocalCredentialState::RefreshCandidate {
            reason: RefreshCandidateReason::AccessExpired,
            expired_at: Some(timestamp),
        });

        assert_eq!(malformed.exit_code, ExitCode::AuthError);
        assert_eq!(expired.exit_code, ExitCode::AuthError);
        assert_eq!(refreshable.exit_code, ExitCode::Success);
        Ok(())
    }
}

#[cfg(test)]
#[path = "auth_state_matrix_tests.rs"]
mod state_matrix_tests;

#[cfg(all(test, unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[path = "auth_foreign_home_tests.rs"]
mod foreign_home_tests;
