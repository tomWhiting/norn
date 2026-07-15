use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use base64::Engine as _;
use tokio::process::Command;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::super::auth_root::NornAuthRoot;
use super::super::storage::{
    AUTH_JSON_FILE, AuthCredentialsStoreMode, load_auth_dot_json, save_auth_dot_json,
};
use super::super::types::{AuthDotJson, CodexAuth};
use super::*;

type TestResult = Result<(), Box<dyn Error>>;

const CHILD_MODE: &str = "NORN_OAUTH_MANAGER_PROCESS_CHILD";
const CHILD_ROOT: &str = "NORN_OAUTH_MANAGER_PROCESS_ROOT";
const CHILD_TOKEN_URL: &str = "NORN_OAUTH_MANAGER_PROCESS_TOKEN_URL";
const CHILD_START: &str = "NORN_OAUTH_MANAGER_PROCESS_START";
const CHILD_READY: &str = "NORN_OAUTH_MANAGER_PROCESS_READY";
const CHILD_RESULT: &str = "NORN_OAUTH_MANAGER_PROCESS_RESULT";
const TEST_NAME: &str = "provider::openai_oauth::manager::process_tests::two_process_refresh_converges_with_one_authority_exchange";
const ACCOUNT_ID: &str = "process-fixture-account";
const ROTATED_ACCESS: &str = "process-fixture-rotated-access";
const ROTATED_REFRESH: &str = "process-fixture-rotated-refresh";

#[tokio::test]
async fn invalid_lock_timing_fails_before_manager_credential_access() -> TestResult {
    let directory = tempfile::tempdir()?;
    let cases = [
        OAuthHttpOptions {
            credential_lock_timeout: Duration::ZERO,
            ..OAuthHttpOptions::default()
        },
        OAuthHttpOptions {
            credential_lock_poll_interval: Duration::ZERO,
            ..OAuthHttpOptions::default()
        },
    ];

    for (index, http) in cases.into_iter().enumerate() {
        let root_path = directory.path().join(format!("invalid-{index}"));
        let root = NornAuthRoot::try_from(root_path.as_path())?;
        let result = AuthManager::shared(root, AuthCredentialsStoreMode::File, http).await;

        assert!(matches!(
            result,
            Err(AuthManagerBuildError::CredentialCoordination { .. })
        ));
        assert!(!root_path.exists());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_process_refresh_converges_with_one_authority_exchange() -> TestResult {
    if std::env::var_os(CHILD_MODE).is_some() {
        return run_child().await;
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(250))
                .set_body_json(refresh_response()),
        )
        .mount(&server)
        .await;

    let directory = tempfile::tempdir()?;
    let auth_root_path = directory.path().join("auth");
    let auth_root = NornAuthRoot::try_from(auth_root_path.clone())?;
    save_auth_dot_json(&auth_root_path, &expired_auth()?)?;
    let start = directory.path().join("start");
    let ready = [
        directory.path().join("ready-a"),
        directory.path().join("ready-b"),
    ];
    let results = [
        directory.path().join("result-a"),
        directory.path().join("result-b"),
    ];

    let first = spawn_child(
        &auth_root_path,
        &server.uri(),
        &start,
        &ready[0],
        &results[0],
    )?;
    let second = spawn_child(
        &auth_root_path,
        &server.uri(),
        &start,
        &ready[1],
        &results[1],
    )?;
    let readiness = wait_for_paths(&ready).await;
    std::fs::write(&start, b"start")?;

    let (first_output, second_output) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(first.wait_with_output(), second.wait_with_output())
    })
    .await
    .map_err(|elapsed| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("process fixture children did not finish: {elapsed}"),
        )
    })?;
    let first_output = first_output?;
    let second_output = second_output?;

    if let Err(error) = readiness {
        return Err(std::io::Error::other(format!(
            "children did not reach the refresh barrier: {error}\n{}\n{}",
            render_child("first", &first_output),
            render_child("second", &second_output),
        ))
        .into());
    }
    require_child_success("first", &first_output)?;
    require_child_success("second", &second_output)?;
    for result in &results {
        if std::fs::read(result)? != b"converged" {
            return Err(std::io::Error::other("child did not confirm durable convergence").into());
        }
    }

    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| std::io::Error::other("authority request recording is unavailable"))?;
    assert_eq!(
        requests.len(),
        1,
        "two processes must perform one authority exchange"
    );
    require_rotated(
        &load_auth_dot_json(&auth_root, AuthCredentialsStoreMode::File)?
            .ok_or_else(|| std::io::Error::other("durable credential is missing"))?,
    )?;
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[tokio::test]
async fn symlink_auth_root_fails_before_authority_request_without_mutating_target() -> TestResult {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let server = MockServer::start().await;
    let directory = tempfile::tempdir()?;
    let real_root = directory.path().join("real-auth");
    let linked_root = directory.path().join("linked-auth");
    save_auth_dot_json(&real_root, &expired_auth()?)?;
    symlink(&real_root, &linked_root)?;

    let auth_path = real_root.join(AUTH_JSON_FILE);
    let original_auth = std::fs::read(&auth_path)?;
    let original_root_mode = std::fs::metadata(&real_root)?.permissions().mode();
    let original_auth_mode = std::fs::metadata(&auth_path)?.permissions().mode();
    let root = NornAuthRoot::try_from(linked_root.as_path())?;

    let result =
        AuthManager::shared_for_tests(root, AuthCredentialsStoreMode::File, server.uri()).await;

    assert!(matches!(
        result,
        Err(AuthManagerBuildError::CredentialCoordination { .. })
    ));
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| std::io::Error::other("authority request recording is unavailable"))?;
    assert!(
        requests.is_empty(),
        "a rejected symlink root must not reach the token authority"
    );
    assert_eq!(std::fs::read(&auth_path)?, original_auth);
    assert_eq!(
        std::fs::metadata(&real_root)?.permissions().mode(),
        original_root_mode
    );
    assert_eq!(
        std::fs::metadata(&auth_path)?.permissions().mode(),
        original_auth_mode
    );
    assert!(!real_root.join(".norn-auth.lock").exists());
    assert!(
        std::fs::symlink_metadata(linked_root)?
            .file_type()
            .is_symlink()
    );
    Ok(())
}

async fn run_child() -> TestResult {
    let root = required_path(CHILD_ROOT)?;
    let token_url = required_string(CHILD_TOKEN_URL)?;
    let start = required_path(CHILD_START)?;
    let ready = required_path(CHILD_READY)?;
    let result = required_path(CHILD_RESULT)?;
    let auth_root = NornAuthRoot::try_from(root)?;
    let manager =
        AuthManager::shared_for_tests(auth_root.clone(), AuthCredentialsStoreMode::File, token_url)
            .await?;

    std::fs::write(&ready, b"ready")?;
    wait_for_paths(std::slice::from_ref(&start)).await?;
    let memory = manager
        .auth()
        .await?
        .ok_or_else(|| std::io::Error::other("manager returned no credential"))?;
    let durable = load_auth_dot_json(&auth_root, AuthCredentialsStoreMode::File)?
        .ok_or_else(|| std::io::Error::other("child observed no durable credential"))?;
    let CodexAuth::ChatGpt(memory) = memory else {
        return Err(std::io::Error::other("manager returned a non-OAuth credential").into());
    };
    if memory.as_ref() != &durable {
        return Err(std::io::Error::other("memory and durable credentials diverged").into());
    }
    require_rotated(&durable)?;
    std::fs::write(result, b"converged")?;
    Ok(())
}

fn spawn_child(
    root: &Path,
    token_url: &str,
    start: &Path,
    ready: &Path,
    result: &Path,
) -> Result<tokio::process::Child, std::io::Error> {
    Command::new(std::env::current_exe()?)
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_MODE, "1")
        .env(CHILD_ROOT, root)
        .env(CHILD_TOKEN_URL, token_url)
        .env(CHILD_START, start)
        .env(CHILD_READY, ready)
        .env(CHILD_RESULT, result)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

async fn wait_for_paths(paths: &[PathBuf]) -> Result<(), std::io::Error> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if paths.iter().all(|path| path.exists()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|elapsed| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("process fixture barrier timed out: {elapsed}"),
        )
    })
}

fn require_child_success(label: &str, output: &Output) -> Result<(), std::io::Error> {
    if output.status.success() {
        return Ok(());
    }
    Err(std::io::Error::other(render_child(label, output)))
}

fn render_child(label: &str, output: &Output) -> String {
    format!(
        "{label} child failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

fn required_path(name: &str) -> Result<PathBuf, std::io::Error> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::other(format!("missing child environment: {name}")))
}

fn required_string(name: &str) -> Result<String, std::io::Error> {
    std::env::var(name).map_err(|error| match error {
        std::env::VarError::NotPresent => {
            std::io::Error::other(format!("missing child environment: {name}"))
        }
        std::env::VarError::NotUnicode(_) => {
            std::io::Error::other(format!("child environment is not Unicode: {name}"))
        }
    })
}

fn expired_auth() -> Result<AuthDotJson, serde_json::Error> {
    serde_json::from_value(serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token(),
            "access_token": access_token(1),
            "refresh_token": "process-fixture-seed-refresh",
            "account_id": ACCOUNT_ID,
        }
    }))
}

fn refresh_response() -> serde_json::Value {
    serde_json::json!({
        "id_token": id_token(),
        "access_token": ROTATED_ACCESS,
        "refresh_token": ROTATED_REFRESH,
        "account_id": ACCOUNT_ID,
    })
}

fn id_token() -> String {
    jwt(&serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": ACCOUNT_ID,
        }
    }))
}

fn access_token(expiration: i64) -> String {
    jwt(&serde_json::json!({"exp": expiration}))
}

fn jwt(claims: &serde_json::Value) -> String {
    let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = encoder.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let claims = encoder.encode(claims.to_string());
    format!("{header}.{claims}.")
}

fn require_rotated(auth: &AuthDotJson) -> Result<(), std::io::Error> {
    let tokens = auth
        .tokens
        .as_ref()
        .ok_or_else(|| std::io::Error::other("rotated token bundle is missing"))?;
    if tokens.access_token != ROTATED_ACCESS || tokens.refresh_token != ROTATED_REFRESH {
        return Err(std::io::Error::other(
            "durable credential does not contain the rotated fixture values",
        ));
    }
    Ok(())
}
