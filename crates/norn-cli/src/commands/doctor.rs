//! `norn doctor` subcommand (NC-008 R13).
//!
//! Three checks run unconditionally in order: OAuth status, provider
//! connectivity, and working-directory permissions. Each emits a
//! `[PASS]` or `[FAIL] … : <remediation>` line on stderr. The exit code
//! is `0` if every check passes, `1` if any check fails.
//!
//! Connectivity uses a plain reqwest HEAD against the public `OpenAI`
//! API base URL. Any HTTP status (2xx, 3xx, 4xx) is treated as a
//! successful network reach — only transport-level failures (DNS, TLS,
//! refused) count as a failed connectivity check. The endpoint is
//! mirrored as a local constant because the upstream value in
//! `norn::provider::openai` is private to that module.

use std::path::Path;
use std::time::Duration;

use crate::cli::ExitCode;
use crate::cli::ProviderKind;
use crate::commands::auth::{auth_health, resolve_codex_home};

/// `OpenAI` Responses API root used for the connectivity probe. Mirrors
/// `crate::provider::openai::DEFAULT_BASE_URL` which is module-private.
const DEFAULT_PROBE_URL: &str = "https://api.openai.com/v1";

/// Wall-clock budget for the connectivity HEAD request.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Top-level dispatcher for `norn doctor`.
pub fn run_doctor() -> ExitCode {
    let mut failed = false;

    failed |= !check_auth();
    failed |= !check_connectivity(ProviderKind::Openai);
    failed |= !check_working_dir();

    if failed {
        ExitCode::AgentError
    } else {
        ExitCode::Success
    }
}

// ---------------------------------------------------------------------------
// 1. Auth
// ---------------------------------------------------------------------------

fn check_auth() -> bool {
    let codex_home = resolve_codex_home();
    match auth_health(&codex_home) {
        Ok(true) => {
            eprintln!("[PASS] OAuth credentials present");
            true
        }
        Ok(false) => {
            eprintln!("[FAIL] OAuth credentials missing: Run `norn auth login` to authenticate.");
            false
        }
        Err(err) => {
            eprintln!(
                "[FAIL] OAuth credentials malformed ({err}): Run `norn auth login` to re-authenticate."
            );
            false
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Provider connectivity
// ---------------------------------------------------------------------------

fn check_connectivity(provider: ProviderKind) -> bool {
    // ClaudeRunner uses a local subprocess, not a network endpoint —
    // skip the HTTP probe and report PASS so the check is still
    // visible in the output.
    if matches!(provider, ProviderKind::ClaudeRunner) {
        eprintln!("[PASS] Provider transport is local (claude-runner)");
        return true;
    }

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!(
                "[FAIL] Cannot probe provider ({err}): Internal runtime construction failed."
            );
            return false;
        }
    };

    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(err) => {
            eprintln!(
                "[FAIL] Cannot probe provider ({err}): Internal HTTP client construction failed."
            );
            return false;
        }
    };

    match rt.block_on(client.head(DEFAULT_PROBE_URL).send()) {
        Ok(_resp) => {
            eprintln!("[PASS] Provider reachable at {DEFAULT_PROBE_URL}");
            true
        }
        Err(err) => {
            eprintln!(
                "[FAIL] Cannot reach provider ({err}): Check internet connection and proxy settings."
            );
            false
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Working directory permissions
// ---------------------------------------------------------------------------

fn check_working_dir() -> bool {
    let cwd = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("[FAIL] Cannot resolve working directory ({err}): Check shell environment.");
            return false;
        }
    };

    match check_dir_io(&cwd) {
        Ok(()) => {
            eprintln!("[PASS] Working directory is readable and writable");
            true
        }
        Err(err) => {
            eprintln!(
                "[FAIL] Working directory not writable ({err}): Check directory permissions."
            );
            false
        }
    }
}

fn check_dir_io(dir: &Path) -> std::io::Result<()> {
    // Reading metadata fails if the directory is unreadable or missing.
    let _ = std::fs::metadata(dir)?;
    let scratch = dir.join(format!(".norn-doctor-write-test-{}", std::process::id()));
    let result = std::fs::File::create(&scratch).map(|_| ());
    // Best-effort cleanup; if remove fails after a successful create
    // there is nothing actionable to report — the write test passed.
    let _ = std::fs::remove_file(&scratch);
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn auth_check_returns_false_when_unset() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: serial test prevents concurrent observers.
        unsafe { std::env::set_var("CODEX_HOME", tmp.path()) };
        let ok = check_auth();
        unsafe { std::env::remove_var("CODEX_HOME") };
        assert!(!ok);
    }

    #[test]
    fn check_dir_io_succeeds_on_temp_dir() {
        let tmp = tempfile::tempdir().unwrap();
        check_dir_io(tmp.path()).unwrap();
    }

    #[test]
    fn check_dir_io_fails_on_missing_path() {
        let result = check_dir_io(Path::new("/this/path/should/not/exist/norn-doctor"));
        assert!(result.is_err());
    }

    #[test]
    fn connectivity_for_local_provider_passes_without_network() {
        let ok = check_connectivity(ProviderKind::ClaudeRunner);
        assert!(ok);
    }
}
