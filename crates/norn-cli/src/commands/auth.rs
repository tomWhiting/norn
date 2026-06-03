//! `norn auth …` subcommand dispatchers (NC-008 R8–R10).
//!
//! Wraps the synchronous `auth` flow around the async functions exposed
//! by [`norn::provider::auth`]. A short-lived multi-threaded tokio
//! runtime is created per invocation — `auth login` blocks for as long
//! as the user takes to complete the browser PKCE flow, so a runtime
//! that supports `block_on` for the full duration is required.
//!
//! Token-handling rule (CO5 + DESIGN.md NC13): raw access tokens,
//! refresh tokens, and JWT bodies MUST NEVER appear in stdout or stderr
//! output. Only parsed metadata (expiry timestamp, account id) is
//! surfaced from `auth status`.

use std::path::{Path, PathBuf};

use norn::provider::auth::{LoginConfig, login, logout};
use norn::provider::openai_oauth::jwt::parse_jwt_expiration;
use norn::provider::openai_oauth::{AuthCredentialsStoreMode, AuthDotJson, load_auth_dot_json};

use crate::cli::AuthCmd;
use crate::cli::ExitCode;

/// Top-level dispatcher for `norn auth`.
pub fn run_auth(cmd: AuthCmd) -> ExitCode {
    match cmd {
        AuthCmd::Login { codex_home } => run_login(codex_home),
        AuthCmd::Logout => run_logout(),
        AuthCmd::Status => run_status(),
    }
}

// ---------------------------------------------------------------------------
// R8: login
// ---------------------------------------------------------------------------

fn run_login(codex_home: Option<PathBuf>) -> ExitCode {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("norn: failed to create tokio runtime: {err}");
            return ExitCode::AuthError;
        }
    };
    let config = LoginConfig {
        codex_home,
        device_code: false,
    };
    match rt.block_on(login(config)) {
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

// ---------------------------------------------------------------------------
// R9: logout
// ---------------------------------------------------------------------------

fn run_logout() -> ExitCode {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("norn: failed to create tokio runtime: {err}");
            return ExitCode::AuthError;
        }
    };
    match rt.block_on(logout(LoginConfig::default())) {
        Ok(()) => {
            eprintln!("Logged out.");
            ExitCode::Success
        }
        Err(err) => {
            eprintln!("norn: logout failed: {err}");
            ExitCode::AuthError
        }
    }
}

// ---------------------------------------------------------------------------
// R10: status
// ---------------------------------------------------------------------------

fn run_status() -> ExitCode {
    let codex_home = resolve_codex_home();
    if !codex_home.join("auth.json").exists() {
        println!("Not logged in.");
        return ExitCode::Success;
    }

    match load_auth_dot_json(&codex_home, AuthCredentialsStoreMode::File) {
        Ok(None) => {
            println!("Not logged in.");
        }
        Ok(Some(auth)) => {
            print_status(&auth);
        }
        Err(err) => {
            // Status is informational per DESIGN.md NC13: surface the
            // read failure on stderr but still exit 0 so consumers
            // (shell prompts, doctor) can treat it as 'not authenticated'.
            eprintln!("norn: failed to read auth state: {err}");
            println!("Not logged in.");
        }
    }
    ExitCode::Success
}

fn print_status(auth: &AuthDotJson) {
    let Some(tokens) = auth.tokens.as_ref() else {
        println!("Not logged in.");
        return;
    };

    let expiry = parse_jwt_expiration(&tokens.access_token)
        .ok()
        .flatten()
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string());

    let mut line = String::from("Logged in");
    if let Some(expiry_text) = expiry {
        use std::fmt::Write;
        let _ = write!(line, " (expires: {expiry_text})");
    }
    if let Some(account_id) = tokens.account_id.as_deref() {
        use std::fmt::Write;
        let _ = write!(line, " as account {account_id}");
    }
    println!("{line}");
}

/// Resolve the Codex home directory: `$CODEX_HOME` if set and non-empty,
/// otherwise `~/.codex`. Mirrors the private helper in
/// `norn::provider::auth` since that one is not exported.
pub(crate) fn resolve_codex_home() -> PathBuf {
    if let Ok(env_path) = std::env::var("CODEX_HOME")
        && !env_path.is_empty()
    {
        return PathBuf::from(env_path);
    }
    match dirs::home_dir() {
        Some(home) => home.join(".codex"),
        None => PathBuf::from(".codex"),
    }
}

/// Probe helper used by `norn doctor` — checks whether `auth.json`
/// exists at the resolved codex home and, if so, attempts to load it.
/// Returns `Ok(true)` for "logged in with tokens", `Ok(false)` for
/// "logged out / no credentials", and `Err(_)` for malformed storage.
pub(crate) fn auth_health(codex_home: &Path) -> Result<bool, std::io::Error> {
    if !codex_home.join("auth.json").exists() {
        return Ok(false);
    }
    match load_auth_dot_json(codex_home, AuthCredentialsStoreMode::File)? {
        Some(auth) => Ok(auth.tokens.is_some()),
        None => Ok(false),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn resolve_codex_home_honours_env() {
        // SAFETY: serial test; no concurrent reader observes the mutation.
        unsafe { std::env::set_var("CODEX_HOME", "/tmp/codex-test-home") };
        let home = resolve_codex_home();
        unsafe { std::env::remove_var("CODEX_HOME") };
        assert_eq!(home, PathBuf::from("/tmp/codex-test-home"));
    }

    #[test]
    #[serial]
    fn resolve_codex_home_empty_env_treated_as_unset() {
        // SAFETY: serial test prevents concurrent observers.
        unsafe { std::env::set_var("CODEX_HOME", "") };
        let home = resolve_codex_home();
        unsafe { std::env::remove_var("CODEX_HOME") };
        let expected =
            dirs::home_dir().map_or_else(|| PathBuf::from(".codex"), |h| h.join(".codex"));
        assert_eq!(home, expected);
    }

    #[test]
    #[serial]
    fn status_with_no_auth_file_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: serial-style isolation via the env var only being set
        // here; doctor tests are #[serial] in their own module.
        unsafe { std::env::set_var("CODEX_HOME", tmp.path()) };
        let code = run_status();
        unsafe { std::env::remove_var("CODEX_HOME") };
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn auth_health_returns_false_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let ok = auth_health(tmp.path()).unwrap();
        assert!(!ok);
    }
}
