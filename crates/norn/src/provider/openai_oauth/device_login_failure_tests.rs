use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine as _;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::provider::openai_oauth::storage::save_auth_dot_json;
use crate::provider::openai_oauth::{ChatGptTokens, IdTokenInfo, load_auth_dot_json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[derive(Default)]
struct CountingPresenter {
    calls: AtomicUsize,
}

impl LoginPromptPresenter for CountingPresenter {
    fn present(
        &self,
        _prompt: LoginPrompt<'_>,
    ) -> Result<(), super::super::login_prompt::LoginPromptError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[derive(Clone)]
struct PollSequence {
    calls: Arc<AtomicUsize>,
}

impl Respond for PollSequence {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        match call {
            0 => ResponseTemplate::new(403),
            1 => ResponseTemplate::new(404),
            2 => poll_success_response(),
            _ => ResponseTemplate::new(500),
        }
    }
}

fn fixture_jwt(account_id: &str, user_id: &str) -> String {
    let header =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_user_id": user_id
            }
        })
        .to_string(),
    );
    format!("{header}.{claims}.")
}

fn test_options(
    root: NornAuthRoot,
    server: &MockServer,
    presenter: Arc<CountingPresenter>,
    request_timeout: Duration,
    device_code_timeout: Duration,
) -> DeviceLoginOptions {
    DeviceLoginOptions::new(
        root,
        "device-client-id".to_owned(),
        AuthCredentialsStoreMode::File,
        OAuthHttpOptions {
            request_timeout,
            device_code_timeout,
            ..OAuthHttpOptions::default()
        },
        presenter,
    )
    .with_test_authority(&server.uri())
}

fn poll_success_response() -> ResponseTemplate {
    let verifier = "device-code-verifier-secret";
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "authorization_code": "authorization-code-secret",
        "code_challenge": challenge_for(verifier),
        "code_verifier": verifier
    }))
}

async fn mount_user_code(server: &MockServer, interval: &str, response_delay: Option<Duration>) {
    let response = ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "device_auth_id": "device-auth-secret",
        "user_code": "CODE-1234",
        "interval": interval
    }));
    let response = response_delay.map_or(response.clone(), |delay| response.set_delay(delay));
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_poll_success(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(poll_success_response())
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_token_success(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": fixture_jwt("account-fixture", "user-fixture"),
            "access_token": "access-token-secret",
            "refresh_token": "refresh-token-secret"
        })))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn pending_403_then_404_then_success_polls_exactly_three_times() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = NornAuthRoot::try_from(directory.path())?;
    let server = MockServer::start().await;
    mount_user_code(&server, "1", None).await;
    let poll_calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(PollSequence {
            calls: Arc::clone(&poll_calls),
        })
        .expect(3)
        .mount(&server)
        .await;
    mount_token_success(&server).await;
    let presenter = Arc::new(CountingPresenter::default());

    run_device_login_with_hooks(
        test_options(
            root.clone(),
            &server,
            Arc::clone(&presenter),
            Duration::from_secs(2),
            Duration::from_secs(5),
        ),
        |_| Ok(()),
        || Ok(()),
    )
    .await?;

    assert_eq!(poll_calls.load(Ordering::Relaxed), 3);
    assert_eq!(presenter.calls.load(Ordering::Relaxed), 1);
    assert!(load_auth_dot_json(&root, AuthCredentialsStoreMode::File)?.is_some());
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| std::io::Error::other("wire requests were not recorded"))?;
    let polls = requests
        .iter()
        .filter(|request| request.url.path() == "/api/accounts/deviceauth/token")
        .collect::<Vec<_>>();
    assert_eq!(polls.len(), 3);
    for poll in polls {
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&poll.body)?,
            serde_json::json!({
                "device_auth_id": "device-auth-secret",
                "user_code": "CODE-1234"
            })
        );
    }
    Ok(())
}

#[tokio::test]
async fn zero_and_malformed_intervals_fail_before_presentation() -> TestResult {
    for interval in ["0", "not-a-duration"] {
        let directory = tempfile::tempdir()?;
        let root = NornAuthRoot::try_from(directory.path())?;
        let server = MockServer::start().await;
        mount_user_code(&server, interval, None).await;
        let presenter = Arc::new(CountingPresenter::default());

        let result = run_device_login_with_hooks(
            test_options(
                root.clone(),
                &server,
                Arc::clone(&presenter),
                Duration::from_secs(2),
                Duration::from_secs(5),
            ),
            |_| Ok(()),
            || Ok(()),
        )
        .await;

        assert!(matches!(
            result,
            Err(LoginError::DeviceCodeMalformed { stage: "user-code" })
        ));
        assert_eq!(presenter.calls.load(Ordering::Relaxed), 0);
        assert!(load_auth_dot_json(&root, AuthCredentialsStoreMode::File)?.is_none());
    }
    Ok(())
}

#[tokio::test]
async fn authority_failures_are_terminal_and_disclose_no_bodies() -> TestResult {
    for (status, stage, body_secret) in [
        (503, "requesting a user code", "initial-body-secret"),
        (429, "polling for authorization", "poll-body-secret"),
    ] {
        let directory = tempfile::tempdir()?;
        let root = NornAuthRoot::try_from(directory.path())?;
        let server = MockServer::start().await;
        if status == 429 {
            mount_user_code(&server, "1", None).await;
        }
        let endpoint = if status == 429 {
            "/api/accounts/deviceauth/token"
        } else {
            "/api/accounts/deviceauth/usercode"
        };
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(status).set_body_string(body_secret))
            .expect(1)
            .mount(&server)
            .await;
        let presenter = Arc::new(CountingPresenter::default());

        let result = run_device_login_with_hooks(
            test_options(
                root.clone(),
                &server,
                presenter,
                Duration::from_secs(2),
                Duration::from_secs(5),
            ),
            |_| Ok(()),
            || Ok(()),
        )
        .await;
        let Err(error) = result else {
            return Err(std::io::Error::other("authority rejection succeeded").into());
        };
        let rendered = format!("{error} {error:?}");

        assert!(matches!(
            error,
            LoginError::DeviceCodeAuthority {
                stage: actual_stage,
                status: actual_status
            } if actual_stage == stage && actual_status == status
        ));
        assert!(!rendered.contains(body_secret));
        assert!(load_auth_dot_json(&root, AuthCredentialsStoreMode::File)?.is_none());
    }
    Ok(())
}

#[tokio::test]
async fn redirect_is_not_followed_or_disclosed() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = NornAuthRoot::try_from(directory.path())?;
    let source = MockServer::start().await;
    let target = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/redirect-target"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&target)
        .await;
    let location = format!(
        "{}/redirect-target?secret=redirect-target-secret",
        target.uri()
    );
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("Location", location.as_str())
                .set_body_string("redirect-body-secret"),
        )
        .expect(1)
        .mount(&source)
        .await;
    let presenter = Arc::new(CountingPresenter::default());

    let result = run_device_login_with_hooks(
        test_options(
            root,
            &source,
            presenter,
            Duration::from_secs(2),
            Duration::from_secs(5),
        ),
        |_| Ok(()),
        || Ok(()),
    )
    .await;
    let Err(error) = result else {
        return Err(std::io::Error::other("redirected device login succeeded").into());
    };
    let rendered = format!("{error} {error:?}");

    assert!(matches!(
        error,
        LoginError::DeviceCodeAuthority {
            stage: "requesting a user code",
            status: 307
        }
    ));
    assert!(!rendered.contains("redirect-body-secret"));
    assert!(!rendered.contains("redirect-target-secret"));
    let target_requests = target
        .received_requests()
        .await
        .ok_or_else(|| std::io::Error::other("redirect target requests were not recorded"))?;
    assert!(target_requests.is_empty());
    Ok(())
}

#[tokio::test]
async fn token_exchange_failure_preserves_existing_bytes_and_hides_body() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = NornAuthRoot::try_from(directory.path())?;
    let existing = AuthDotJson::from_tokens(ChatGptTokens {
        id_token: IdTokenInfo::from_raw_jwt(fixture_jwt("existing-account", "existing-user"))?,
        access_token: "existing-access-token".to_owned(),
        refresh_token: "existing-refresh-token".to_owned(),
        account_id: Some("existing-account".to_owned()),
        additional_fields: BTreeMap::new(),
    });
    save_auth_dot_json(root.as_path(), &existing)?;
    let before = std::fs::read(root.as_path().join(super::super::storage::AUTH_JSON_FILE))?;
    let server = MockServer::start().await;
    mount_user_code(&server, "1", None).await;
    mount_poll_success(&server).await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(500).set_body_string("exchange-body-secret"))
        .expect(1)
        .mount(&server)
        .await;
    let presenter = Arc::new(CountingPresenter::default());

    let result = run_device_login_with_hooks(
        test_options(
            root.clone(),
            &server,
            presenter,
            Duration::from_secs(2),
            Duration::from_secs(5),
        ),
        |_| Ok(()),
        || Ok(()),
    )
    .await;
    let Err(error) = result else {
        return Err(std::io::Error::other("failed token exchange succeeded").into());
    };
    let rendered = format!("{error} {error:?}");
    let after = std::fs::read(root.as_path().join(super::super::storage::AUTH_JSON_FILE))?;

    assert!(matches!(error, LoginError::TokenExchange(_)));
    assert!(!rendered.contains("exchange-body-secret"));
    assert_eq!(after, before);
    Ok(())
}

#[tokio::test]
async fn initial_request_consumes_whole_device_deadline() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = NornAuthRoot::try_from(directory.path())?;
    let server = MockServer::start().await;
    mount_user_code(&server, "1", Some(Duration::from_millis(400))).await;
    let presenter = Arc::new(CountingPresenter::default());
    let started = std::time::Instant::now();

    let result = run_device_login_with_hooks(
        test_options(
            root.clone(),
            &server,
            Arc::clone(&presenter),
            Duration::from_secs(2),
            Duration::from_millis(250),
        ),
        |_| Ok(()),
        || Ok(()),
    )
    .await;

    assert!(matches!(result, Err(LoginError::DeviceCodeExpired)));
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(presenter.calls.load(Ordering::Relaxed), 0);
    assert!(load_auth_dot_json(&root, AuthCredentialsStoreMode::File)?.is_none());
    Ok(())
}

#[tokio::test]
async fn exchange_receives_only_the_remaining_device_deadline() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = NornAuthRoot::try_from(directory.path())?;
    let server = MockServer::start().await;
    mount_user_code(&server, "1", Some(Duration::from_millis(150))).await;
    mount_poll_success(&server).await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(200))
                .set_body_json(serde_json::json!({
                    "id_token": fixture_jwt("account-fixture", "user-fixture"),
                    "access_token": "access-token-secret",
                    "refresh_token": "refresh-token-secret"
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let presenter = Arc::new(CountingPresenter::default());
    let started = std::time::Instant::now();

    let result = run_device_login_with_hooks(
        test_options(
            root.clone(),
            &server,
            presenter,
            Duration::from_secs(2),
            Duration::from_millis(250),
        ),
        |_| Ok(()),
        || Ok(()),
    )
    .await;

    assert!(matches!(result, Err(LoginError::DeviceCodeExpired)));
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(load_auth_dot_json(&root, AuthCredentialsStoreMode::File)?.is_none());
    Ok(())
}
