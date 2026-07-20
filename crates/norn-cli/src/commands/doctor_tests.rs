use super::*;
use norn::provider::openai_oauth::{
    MalformedCredentialReason, RefreshCandidateReason, UnknownExpiryReason,
};

#[test]
fn auth_check_returns_false_when_unset() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let auth_root = NornAuthRoot::try_from(tmp.path())?;
    let ok = check_auth_at(&auth_root);
    assert!(!ok);
    Ok(())
}

#[test]
fn check_dir_io_succeeds_on_temp_dir() -> Result<(), std::io::Error> {
    let tmp = tempfile::tempdir()?;
    check_dir_io(tmp.path())?;
    Ok(())
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

#[test]
fn connectivity_probe_constructs_timed_request_inside_runtime()
-> Result<(), Box<dyn std::error::Error>> {
    assert!(tokio::runtime::Handle::try_current().is_err());
    let runtime = tokio::runtime::Runtime::new()?;
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .pool_max_idle_per_host(0)
        .build()?;

    let result = send_connectivity_probe(&runtime, &client, "http://127.0.0.1:0");

    assert!(
        result.is_err(),
        "reserved destination port must fail without panicking"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn path_probe_flags_non_executable_file() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir()?;
    let shim = tmp.path().join("python3");
    std::fs::write(&shim, "#!/bin/sh\nexit 0\n")?;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o644))?;
    let Some(issue) = probe_executable("python3", shim) else {
        return Err(std::io::Error::other("non-executable file passed its probe").into());
    };

    assert_eq!(issue.tool, "python3");
    assert!(issue.reason.contains("not executable"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn path_probe_flags_exec_format_error() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir()?;
    let shim = tmp.path().join("python3");
    std::fs::write(&shim, "not a native executable\n")?;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))?;
    let Some(issue) = probe_executable("python3", shim) else {
        return Err(std::io::Error::other("invalid executable passed its probe").into());
    };

    assert_eq!(issue.tool, "python3");
    assert!(
        issue.reason.contains("cannot execute") || issue.reason.contains("status"),
        "bad executable must be reported as an issue: {issue:?}",
    );
    Ok(())
}

#[test]
fn auth_reports_match_state_readiness_without_identity_disclosure()
-> Result<(), Box<dyn std::error::Error>> {
    let timestamp = chrono::DateTime::from_timestamp(1_700_000_000, 0)
        .ok_or_else(|| std::io::Error::other("test timestamp is invalid"))?;
    let cases = [
        (
            LocalCredentialState::Malformed {
                reason: MalformedCredentialReason::InvalidJson,
            },
            false,
            "[FAIL]",
        ),
        (
            LocalCredentialState::AccessExpired {
                expired_at: timestamp,
            },
            false,
            "[FAIL]",
        ),
        (
            LocalCredentialState::RefreshCandidate {
                reason: RefreshCandidateReason::AccessExpired,
                expired_at: Some(timestamp),
            },
            true,
            "[WARN]",
        ),
        (
            LocalCredentialState::LocallyValid {
                expires_at: timestamp,
            },
            true,
            "[PASS]",
        ),
        (
            LocalCredentialState::Unknown {
                reason: UnknownExpiryReason::OpaqueAccessToken,
            },
            true,
            "[WARN]",
        ),
    ];

    for (state, passed, prefix) in cases {
        let report = auth_check_report(&state);
        assert_eq!(report.passed, passed);
        assert!(report.message.starts_with(prefix));
        assert!(report.message.contains("remote validity is unverified"));
        assert!(!report.message.contains("account"));
    }
    Ok(())
}
