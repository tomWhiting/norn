//! `norn doctor` subcommand (NC-008 R13).
//!
//! Five checks run unconditionally in order: OAuth status, provider
//! connectivity, working-directory permissions, PATH shims, and descriptor
//! capacity. Each emits a `[PASS]`, non-failing `[WARN]`, or
//! `[FAIL] … : <remediation>` line on stderr. The exit code is `0` if every
//! check passes or warns and `1` if any check fails.
//!
//! Connectivity uses a plain reqwest HEAD against the public `OpenAI`
//! API base URL. Any HTTP status (2xx, 3xx, 4xx) is treated as a
//! successful network reach — only transport-level failures (DNS, TLS,
//! refused) count as a failed connectivity check. The endpoint is
//! mirrored as a local constant because the upstream value in
//! `norn::provider::openai` is private to that module.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::cli::ExitCode;
use crate::cli::ProviderKind;
use crate::commands::auth::{
    inspect_auth_state, malformed_reason, refresh_candidate_detail, unknown_expiry_reason,
};
use norn::provider::openai_oauth::{LocalCredentialState, NornAuthRoot, resolve_norn_auth_root};

mod descriptors;

/// `OpenAI` Responses API root used for the connectivity probe. Mirrors
/// `crate::provider::openai::DEFAULT_BASE_URL` which is module-private.
const DEFAULT_PROBE_URL: &str = "https://api.openai.com/v1";

/// Wall-clock budget for the connectivity HEAD request.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Wall-clock budget for a single executable `--version` probe.
const EXECUTABLE_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Known tools worth probing for broken PATH shims. Missing tools are
/// ignored; the check flags only a command name that resolves to a path
/// but cannot be executed.
const PATH_PROBE_TOOLS: &[&str] = &["python3", "python", "node", "git", "rg", "cargo", "claude"];

/// Top-level dispatcher for `norn doctor`.
pub fn run_doctor() -> ExitCode {
    let mut failed = false;

    failed |= !check_auth();
    failed |= !check_connectivity(ProviderKind::Openai);
    failed |= !check_working_dir();
    failed |= !check_path_executable_shims();
    failed |= !descriptors::check_descriptors();

    if failed {
        ExitCode::AgentError
    } else {
        ExitCode::Success
    }
}

// ---------------------------------------------------------------------------
// 1. Auth
// ---------------------------------------------------------------------------

pub(crate) fn check_auth() -> bool {
    let auth_root = match resolve_norn_auth_root(None) {
        Ok(root) => root,
        Err(reason) => {
            eprintln!("[FAIL] OAuth credential path is unsafe: {reason}");
            return false;
        }
    };
    check_auth_at(&auth_root)
}

pub(super) fn check_auth_at(auth_root: &NornAuthRoot) -> bool {
    match auth_check_report_at(auth_root) {
        Ok(report) => {
            eprintln!("{}", report.message);
            report.passed
        }
        Err(error) => {
            eprintln!(
                "[FAIL] OAuth credential storage could not be inspected ({error}); remote validity is unverified: Check credential permissions and storage integrity."
            );
            false
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct AuthCheckReport {
    pub(super) passed: bool,
    pub(super) message: String,
}

pub(super) fn auth_check_report_at(
    auth_root: &NornAuthRoot,
) -> Result<AuthCheckReport, norn::provider::openai_oauth::CredentialInspectionError> {
    inspect_auth_state(auth_root).map(|state| auth_check_report(&state))
}

fn auth_check_report(state: &LocalCredentialState) -> AuthCheckReport {
    match state {
        LocalCredentialState::Missing => AuthCheckReport {
            passed: false,
            message: "[FAIL] OAuth credentials missing; remote validity is unverified: Run `norn auth login` to authenticate."
                .to_owned(),
        },
        LocalCredentialState::Malformed { reason } => AuthCheckReport {
            passed: false,
            message: format!(
                "[FAIL] OAuth credentials malformed ({}); remote validity is unverified: Run `norn auth login` to authenticate again.",
                malformed_reason(*reason)
            ),
        },
        LocalCredentialState::AccessExpired { expired_at } => AuthCheckReport {
            passed: false,
            message: format!(
                "[FAIL] OAuth access token expired at {} and cannot be refreshed locally; remote validity is unverified: Run `norn auth login` to authenticate again.",
                expired_at.format("%Y-%m-%dT%H:%M:%SZ")
            ),
        },
        LocalCredentialState::RefreshCandidate { reason, expired_at } => {
            let detail = refresh_candidate_detail(*reason, expired_at.as_ref());
            AuthCheckReport {
                passed: true,
                message: format!(
                    "[WARN] OAuth credentials require refresh before the next provider request because {detail}; remote validity is unverified."
                ),
            }
        }
        LocalCredentialState::LocallyValid { expires_at } => AuthCheckReport {
            passed: true,
            message: format!(
                "[PASS] OAuth access token is locally valid until {}; remote validity is unverified.",
                expires_at.format("%Y-%m-%dT%H:%M:%SZ")
            ),
        },
        LocalCredentialState::Unknown { reason } => AuthCheckReport {
            passed: true,
            message: format!(
                "[WARN] OAuth credentials are present, but access-token expiry is unknown ({}); remote validity is unverified.",
                unknown_expiry_reason(*reason)
            ),
        },
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

    let client = match reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .pool_max_idle_per_host(0)
        .build()
    {
        Ok(c) => c,
        Err(err) => {
            eprintln!(
                "[FAIL] Cannot probe provider ({err}): Internal HTTP client construction failed."
            );
            return false;
        }
    };

    let _descriptor_permit = match norn::resource::acquire_http_request() {
        Ok(permit) => permit,
        Err(err) => {
            eprintln!("[FAIL] Cannot probe provider ({err}): Descriptor admission failed.");
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
    let _descriptor_permit =
        norn::resource::acquire_filesystem_operation().map_err(std::io::Error::other)?;
    // Reading metadata fails if the directory is unreadable or missing.
    std::fs::metadata(dir)?;
    let scratch = dir.join(format!(".norn-doctor-write-test-{}", std::process::id()));
    let result = std::fs::File::create(&scratch).map(|_| ());
    // Best-effort cleanup; if remove fails after a successful create
    // there is nothing actionable to report — the write test passed.
    if let Err(error) = std::fs::remove_file(&scratch) {
        tracing::debug!(error = %error, "doctor scratch-file cleanup failed");
    }
    result
}

// ---------------------------------------------------------------------------
// 4. PATH executable shims
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
struct PathExecutableIssue {
    tool: &'static str,
    path: PathBuf,
    reason: String,
}

fn check_path_executable_shims() -> bool {
    let issues = path_executable_issues(PATH_PROBE_TOOLS);
    if issues.is_empty() {
        eprintln!("[PASS] PATH executable probes runnable");
        return true;
    }

    for issue in issues {
        eprintln!(
            "[FAIL] PATH executable probe failed for `{}` at {} ({}): Fix or remove the \
             broken shim, or move a working binary earlier in PATH.",
            issue.tool,
            issue.path.display(),
            issue.reason,
        );
    }
    false
}

fn path_executable_issues(tools: &[&'static str]) -> Vec<PathExecutableIssue> {
    tools
        .iter()
        .filter_map(|&tool| match resolve_on_path(tool) {
            Ok(Some(path)) => probe_executable(tool, path),
            Ok(None) => None,
            Err(error) => Some(PathExecutableIssue {
                tool,
                path: PathBuf::from(tool),
                reason: format!("descriptor admission failed during PATH resolution: {error}"),
            }),
        })
        .collect()
}

fn probe_executable(tool: &'static str, path: PathBuf) -> Option<PathExecutableIssue> {
    match is_executable_file(&path) {
        Ok(true) => {}
        Ok(false) => {
            return Some(PathExecutableIssue {
                tool,
                path,
                reason: "file exists but is not executable".to_owned(),
            });
        }
        Err(error) => {
            return Some(PathExecutableIssue {
                tool,
                path,
                reason: format!("cannot inspect executable metadata: {error}"),
            });
        }
    }

    match run_version_probe(&path) {
        Ok(ProbeStatus::Ok) => None,
        Ok(ProbeStatus::BadExit(code)) => Some(PathExecutableIssue {
            tool,
            path,
            reason: match code {
                Some(code) => format!("`--version` exited with status {code}"),
                None => "`--version` terminated without an exit status".to_owned(),
            },
        }),
        Ok(ProbeStatus::TimedOut) => Some(PathExecutableIssue {
            tool,
            path,
            reason: format!(
                "`--version` did not exit within {}s",
                EXECUTABLE_PROBE_TIMEOUT.as_secs()
            ),
        }),
        Err(err) => Some(PathExecutableIssue {
            tool,
            path,
            reason: format!("cannot execute: {err}"),
        }),
    }
}

enum ProbeStatus {
    Ok,
    BadExit(Option<i32>),
    TimedOut,
}

fn run_version_probe(path: &Path) -> std::io::Result<ProbeStatus> {
    let _descriptor_permit =
        norn::resource::acquire_null_stdio_subprocess().map_err(std::io::Error::other)?;
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let deadline = std::time::Instant::now() + EXECUTABLE_PROBE_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            // Shells report "cannot execute" as 126. Direct spawn usually
            // returns an io::Error first, but keeping 126 explicit makes
            // shim failures obvious even when a wrapper shell is involved.
            if status.success() {
                return Ok(ProbeStatus::Ok);
            }
            return Ok(ProbeStatus::BadExit(status.code()));
        }
        if std::time::Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Ok(ProbeStatus::TimedOut);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn resolve_on_path(
    tool: &str,
) -> Result<Option<PathBuf>, norn::resource::DescriptorAdmissionError> {
    let _descriptor_permit = norn::resource::acquire_filesystem_operation()?;
    let Some(paths) = std::env::var_os("PATH") else {
        return Ok(None);
    };
    Ok(std::env::split_paths(&paths)
        .map(|dir| dir.join(tool))
        .find(|candidate| candidate.is_file()))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let _descriptor_permit =
        norn::resource::acquire_filesystem_operation().map_err(std::io::Error::other)?;
    let metadata = std::fs::metadata(path)?;
    Ok(metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> std::io::Result<bool> {
    let _descriptor_permit =
        norn::resource::acquire_filesystem_operation().map_err(std::io::Error::other)?;
    Ok(path.is_file())
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;
