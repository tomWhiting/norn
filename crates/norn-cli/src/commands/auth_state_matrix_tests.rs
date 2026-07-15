#![cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]

use std::fs::Permissions;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::Path;

use chrono::{DateTime, Utc};
use norn::provider::openai_oauth::{
    AUTH_JSON_FILE, LocalCredentialState, MalformedCredentialReason, NornAuthRoot,
    RefreshCandidateReason, UnknownExpiryReason,
};

use super::*;
use crate::commands::doctor::{auth_check_report_at, check_auth_at};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const ID_TOKEN: &str = concat!(
    "eyJhbGciOiJub25lIn0.",
    "eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lk",
    "IjoibWF0cml4LWFjY291bnQtc2VjcmV0In0sIm1hcmtlciI6ImlkLXRva2VuLXNlY3JldCJ9."
);
const ID_TOKEN_WITHOUT_ACCOUNT: &str = concat!(
    "eyJhbGciOiJub25lIn0.",
    "eyJtYXJrZXIiOiJtaXNzaW5nLWFjY291bnQtaWQtdG9rZW4tc2VjcmV0In0."
);
const EXPIRED_ACCESS_TOKEN: &str = concat!(
    "eyJhbGciOiJub25lIn0.",
    "eyJleHAiOjk0NjY4NDgwMCwibWFya2VyIjoiZXhwaXJlZC1hY2Nlc3Mtc2VjcmV0In0."
);
const FUTURE_ACCESS_TOKEN: &str = concat!(
    "eyJhbGciOiJub25lIn0.",
    "eyJleHAiOjQxMDI0NDQ4MDAsIm1hcmtlciI6ImZ1dHVyZS1hY2Nlc3Mtc2VjcmV0In0."
);
const MISSING_EXPIRY_ACCESS_TOKEN: &str = concat!(
    "eyJhbGciOiJub25lIn0.",
    "eyJzdWIiOiJtaXNzaW5nLWV4cGlyeS1hY2Nlc3Mtc2VjcmV0In0."
);
const MALFORMED_ACCESS_TOKEN: &str = "eyJhbGciOiJub25lIn0.%%%.";
const OPAQUE_ACCESS_TOKEN: &str = "opaque-access-token-secret";
const REFRESH_TOKEN: &str = "refresh-token-secret";
const ACCOUNT_ID: &str = "matrix-account-secret";
const INVALID_ACCESS_TOKEN: &str = " invalid-access-token-secret";
const INVALID_REFRESH_TOKEN: &str = "invalid-refresh-token-secret ";
const INVALID_ACCOUNT_ID: &str = "invalid-account-id-secret\n";
const CONFLICTING_ACCOUNT_ID: &str = "conflicting-account-id-secret";
const INVALID_JSON_SECRET: &str = "invalid-json-secret";
const MISSING_BUNDLE_SECRET: &str = "missing-token-bundle-secret";

// `MixedCredentialKinds` is intentionally absent: the file decoder accepts
// Codex dual-slot documents, and neither file decode nor evaluation emits it.
const FILE_BACKED_MALFORMED_REASONS: &[MalformedCredentialReason] = &[
    MalformedCredentialReason::InvalidJson,
    MalformedCredentialReason::UnsupportedAuthMode,
    MalformedCredentialReason::MissingTokenBundle,
    MalformedCredentialReason::MalformedIdTokenClaims,
    MalformedCredentialReason::InvalidAccessToken,
    MalformedCredentialReason::InvalidRefreshToken,
    MalformedCredentialReason::MissingAccountId,
    MalformedCredentialReason::InvalidAccountId,
    MalformedCredentialReason::ConflictingAccountIds,
    MalformedCredentialReason::MalformedAccessTokenClaims,
    MalformedCredentialReason::MissingUsableToken,
];
const FILE_BACKED_REFRESH_REASONS: &[RefreshCandidateReason] = &[
    RefreshCandidateReason::AccessExpired,
    RefreshCandidateReason::AccessMissing,
];
const FILE_BACKED_UNKNOWN_REASONS: &[UnknownExpiryReason] = &[
    UnknownExpiryReason::OpaqueAccessToken,
    UnknownExpiryReason::MissingExpiration,
];

const DISCLOSURE_SENTINELS: &[&str] = &[
    ID_TOKEN,
    ID_TOKEN_WITHOUT_ACCOUNT,
    EXPIRED_ACCESS_TOKEN,
    FUTURE_ACCESS_TOKEN,
    MISSING_EXPIRY_ACCESS_TOKEN,
    MALFORMED_ACCESS_TOKEN,
    OPAQUE_ACCESS_TOKEN,
    REFRESH_TOKEN,
    ACCOUNT_ID,
    INVALID_ACCESS_TOKEN,
    INVALID_REFRESH_TOKEN,
    INVALID_ACCOUNT_ID,
    CONFLICTING_ACCOUNT_ID,
    "invalid-access-token-secret",
    "invalid-refresh-token-secret",
    "invalid-account-id-secret",
    INVALID_JSON_SECRET,
    MISSING_BUNDLE_SECRET,
    "id-token-secret",
    "missing-account-id-token-secret",
    "expired-access-secret",
    "future-access-secret",
    "missing-expiry-access-secret",
    "malformed-id-token-secret",
    "unsupported-mode-key-secret",
];

struct StateCase {
    name: &'static str,
    bytes: Option<Vec<u8>>,
    expected: LocalCredentialState,
    status_exit: ExitCode,
    doctor_passed: bool,
    doctor_prefix: &'static str,
}

fn oauth_document(access_token: &str, refresh_token: &str) -> Result<Vec<u8>, serde_json::Error> {
    oauth_document_with_identity(ID_TOKEN, access_token, refresh_token, Some(ACCOUNT_ID))
}

fn oauth_document_with_identity(
    id_token: &str,
    access_token: &str,
    refresh_token: &str,
    account_id: Option<&str>,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": account_id,
        }
    }))
}

fn malformed_case(
    name: &'static str,
    bytes: Vec<u8>,
    reason: MalformedCredentialReason,
) -> StateCase {
    StateCase {
        name,
        bytes: Some(bytes),
        expected: LocalCredentialState::Malformed { reason },
        status_exit: ExitCode::AuthError,
        doctor_passed: false,
        doctor_prefix: "[FAIL]",
    }
}

fn timestamp(seconds: i64) -> Result<DateTime<Utc>, std::io::Error> {
    DateTime::from_timestamp(seconds, 0)
        .ok_or_else(|| std::io::Error::other("fixture timestamp is outside chrono's range"))
}

fn classified_cases() -> Result<Vec<StateCase>, Box<dyn std::error::Error>> {
    Ok(vec![
        StateCase {
            name: "missing",
            bytes: None,
            expected: LocalCredentialState::Missing,
            status_exit: ExitCode::Success,
            doctor_passed: false,
            doctor_prefix: "[FAIL]",
        },
        malformed_case(
            "invalid JSON",
            format!(r#"{{"marker":"{INVALID_JSON_SECRET}",}}"#).into_bytes(),
            MalformedCredentialReason::InvalidJson,
        ),
        malformed_case(
            "unsupported mode",
            serde_json::to_vec(&serde_json::json!({
                "auth_mode": "api_key",
                "OPENAI_API_KEY": "unsupported-mode-key-secret",
            }))?,
            MalformedCredentialReason::UnsupportedAuthMode,
        ),
        malformed_case(
            "missing token bundle",
            serde_json::to_vec(&serde_json::json!({
                "auth_mode": "chatgpt",
                "marker": MISSING_BUNDLE_SECRET,
            }))?,
            MalformedCredentialReason::MissingTokenBundle,
        ),
        malformed_case(
            "malformed id token",
            serde_json::to_vec(&serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": {
                    "id_token": "malformed-id-token-secret",
                    "access_token": OPAQUE_ACCESS_TOKEN,
                    "refresh_token": REFRESH_TOKEN,
                    "account_id": ACCOUNT_ID,
                }
            }))?,
            MalformedCredentialReason::MalformedIdTokenClaims,
        ),
        malformed_case(
            "invalid access token",
            oauth_document(INVALID_ACCESS_TOKEN, REFRESH_TOKEN)?,
            MalformedCredentialReason::InvalidAccessToken,
        ),
        malformed_case(
            "invalid refresh token",
            oauth_document(OPAQUE_ACCESS_TOKEN, INVALID_REFRESH_TOKEN)?,
            MalformedCredentialReason::InvalidRefreshToken,
        ),
        malformed_case(
            "missing account identity",
            oauth_document_with_identity(
                ID_TOKEN_WITHOUT_ACCOUNT,
                OPAQUE_ACCESS_TOKEN,
                REFRESH_TOKEN,
                None,
            )?,
            MalformedCredentialReason::MissingAccountId,
        ),
        malformed_case(
            "invalid account identity",
            oauth_document_with_identity(
                ID_TOKEN_WITHOUT_ACCOUNT,
                OPAQUE_ACCESS_TOKEN,
                REFRESH_TOKEN,
                Some(INVALID_ACCOUNT_ID),
            )?,
            MalformedCredentialReason::InvalidAccountId,
        ),
        malformed_case(
            "conflicting account identities",
            oauth_document_with_identity(
                ID_TOKEN,
                OPAQUE_ACCESS_TOKEN,
                REFRESH_TOKEN,
                Some(CONFLICTING_ACCOUNT_ID),
            )?,
            MalformedCredentialReason::ConflictingAccountIds,
        ),
        malformed_case(
            "malformed access token",
            oauth_document(MALFORMED_ACCESS_TOKEN, REFRESH_TOKEN)?,
            MalformedCredentialReason::MalformedAccessTokenClaims,
        ),
        malformed_case(
            "missing usable token",
            oauth_document("", "")?,
            MalformedCredentialReason::MissingUsableToken,
        ),
        StateCase {
            name: "expired without refresh",
            bytes: Some(oauth_document(EXPIRED_ACCESS_TOKEN, "")?),
            expected: LocalCredentialState::AccessExpired {
                expired_at: timestamp(946_684_800)?,
            },
            status_exit: ExitCode::AuthError,
            doctor_passed: false,
            doctor_prefix: "[FAIL]",
        },
        StateCase {
            name: "expired with refresh",
            bytes: Some(oauth_document(EXPIRED_ACCESS_TOKEN, REFRESH_TOKEN)?),
            expected: LocalCredentialState::RefreshCandidate {
                reason: RefreshCandidateReason::AccessExpired,
                expired_at: Some(timestamp(946_684_800)?),
            },
            status_exit: ExitCode::Success,
            doctor_passed: true,
            doctor_prefix: "[WARN]",
        },
        StateCase {
            name: "missing access with refresh",
            bytes: Some(oauth_document("", REFRESH_TOKEN)?),
            expected: LocalCredentialState::RefreshCandidate {
                reason: RefreshCandidateReason::AccessMissing,
                expired_at: None,
            },
            status_exit: ExitCode::Success,
            doctor_passed: true,
            doctor_prefix: "[WARN]",
        },
        StateCase {
            name: "future expiry",
            bytes: Some(oauth_document(FUTURE_ACCESS_TOKEN, REFRESH_TOKEN)?),
            expected: LocalCredentialState::LocallyValid {
                expires_at: timestamp(4_102_444_800)?,
            },
            status_exit: ExitCode::Success,
            doctor_passed: true,
            doctor_prefix: "[PASS]",
        },
        StateCase {
            name: "opaque access token",
            bytes: Some(oauth_document(OPAQUE_ACCESS_TOKEN, REFRESH_TOKEN)?),
            expected: LocalCredentialState::Unknown {
                reason: UnknownExpiryReason::OpaqueAccessToken,
            },
            status_exit: ExitCode::Success,
            doctor_passed: true,
            doctor_prefix: "[WARN]",
        },
        StateCase {
            name: "missing expiry",
            bytes: Some(oauth_document(MISSING_EXPIRY_ACCESS_TOKEN, REFRESH_TOKEN)?),
            expected: LocalCredentialState::Unknown {
                reason: UnknownExpiryReason::MissingExpiration,
            },
            status_exit: ExitCode::Success,
            doctor_passed: true,
            doctor_prefix: "[WARN]",
        },
    ])
}

fn assert_no_disclosure(rendered: &str, case: &str) {
    for sentinel in DISCLOSURE_SENTINELS {
        assert!(
            !rendered.contains(sentinel),
            "{case} disclosed credential sentinel `{sentinel}`"
        );
    }
}

#[test]
fn status_and_doctor_share_real_file_classification_and_semantics() -> TestResult {
    let mut malformed_reasons = Vec::new();
    let mut refresh_reasons = Vec::new();
    let mut unknown_reasons = Vec::new();
    for case in classified_cases()? {
        let directory = tempfile::tempdir()?;
        if let Some(bytes) = &case.bytes {
            std::fs::write(directory.path().join(AUTH_JSON_FILE), bytes)?;
        }
        let auth_root = NornAuthRoot::try_from(directory.path())?;

        match &case.expected {
            LocalCredentialState::Malformed { reason } => malformed_reasons.push(*reason),
            LocalCredentialState::RefreshCandidate { reason, .. } => {
                refresh_reasons.push(*reason);
            }
            LocalCredentialState::Unknown { reason } => unknown_reasons.push(*reason),
            LocalCredentialState::Missing
            | LocalCredentialState::AccessExpired { .. }
            | LocalCredentialState::LocallyValid { .. } => {}
        }

        assert_eq!(
            inspect_auth_state(&auth_root)?,
            case.expected,
            "{} classification",
            case.name
        );
        let status = status_report_at(&auth_root)?;
        let doctor = auth_check_report_at(&auth_root)?;

        assert_eq!(status.exit_code, case.status_exit, "{} status", case.name);
        assert_eq!(doctor.passed, case.doctor_passed, "{} doctor", case.name);
        assert!(
            doctor.message.starts_with(case.doctor_prefix),
            "{} doctor severity: {}",
            case.name,
            doctor.message
        );
        assert!(status.message.contains("remote validity is unverified"));
        assert!(doctor.message.contains("remote validity is unverified"));
        assert_no_disclosure(&status.message, case.name);
        assert_no_disclosure(&doctor.message, case.name);
    }
    assert_eq!(malformed_reasons.as_slice(), FILE_BACKED_MALFORMED_REASONS);
    assert_eq!(refresh_reasons.as_slice(), FILE_BACKED_REFRESH_REASONS);
    assert_eq!(unknown_reasons.as_slice(), FILE_BACKED_UNKNOWN_REASONS);
    Ok(())
}

fn assert_storage_error_surfaces(auth_root: &NornAuthRoot, case: &str) -> TestResult {
    let status_error = status_report_at(auth_root).err().ok_or_else(|| {
        std::io::Error::other(format!(
            "{case} unexpectedly produced an auth status report"
        ))
    })?;
    let doctor_error = auth_check_report_at(auth_root).err().ok_or_else(|| {
        std::io::Error::other(format!("{case} unexpectedly produced a doctor auth report"))
    })?;

    assert_no_disclosure(&status_error.to_string(), case);
    assert_no_disclosure(&doctor_error.to_string(), case);
    assert_eq!(run_status_at(auth_root), ExitCode::AuthError);
    assert!(!check_auth_at(auth_root));
    Ok(())
}

#[test]
fn symlink_and_non_regular_credentials_fail_both_surfaces() -> TestResult {
    let symlink_container = tempfile::tempdir()?;
    let symlink_root = symlink_container.path().join("auth-root");
    std::fs::create_dir(&symlink_root)?;
    let target = symlink_container.path().join("target.json");
    std::fs::write(&target, oauth_document(OPAQUE_ACCESS_TOKEN, REFRESH_TOKEN)?)?;
    symlink(&target, symlink_root.join(AUTH_JSON_FILE))?;
    assert_storage_error_surfaces(&NornAuthRoot::try_from(symlink_root)?, "symlink")?;

    let non_regular_root = tempfile::tempdir()?;
    std::fs::create_dir(non_regular_root.path().join(AUTH_JSON_FILE))?;
    assert_storage_error_surfaces(
        &NornAuthRoot::try_from(non_regular_root.path())?,
        "non-regular entry",
    )?;
    Ok(())
}

#[test]
fn unreadable_credential_fails_both_surfaces_when_permissions_are_enforced() -> TestResult {
    let directory = tempfile::tempdir()?;
    let auth_path = directory.path().join(AUTH_JSON_FILE);
    std::fs::write(
        &auth_path,
        oauth_document(OPAQUE_ACCESS_TOKEN, REFRESH_TOKEN)?,
    )?;
    std::fs::set_permissions(&auth_path, Permissions::from_mode(0o000))?;

    let result = if std::fs::File::open(&auth_path).is_err() {
        assert_storage_error_surfaces(
            &NornAuthRoot::try_from(directory.path())?,
            "unreadable credential",
        )
    } else {
        Ok(())
    };
    std::fs::set_permissions(&auth_path, Permissions::from_mode(0o600))?;
    result
}

#[test]
fn report_matrix_never_mutates_observed_storage_modes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let auth_path = directory.path().join(AUTH_JSON_FILE);
    std::fs::write(
        &auth_path,
        oauth_document(OPAQUE_ACCESS_TOKEN, REFRESH_TOKEN)?,
    )?;
    std::fs::set_permissions(directory.path(), Permissions::from_mode(0o755))?;
    std::fs::set_permissions(&auth_path, Permissions::from_mode(0o644))?;
    let auth_root = NornAuthRoot::try_from(directory.path())?;

    let _status = status_report_at(&auth_root)?;
    let _doctor = auth_check_report_at(&auth_root)?;

    assert_eq!(mode(directory.path())?, 0o755);
    assert_eq!(mode(&auth_path)?, 0o644);
    Ok(())
}

fn mode(path: &Path) -> Result<u32, std::io::Error> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}
