use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use parking_lot::Mutex;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::provider::openai_oauth::{
    NamedLoginPreparation, list_accounts, load_auth_dot_json, prepare_named_login,
};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[derive(Default)]
struct CapturingPresenter {
    prompts: Mutex<Vec<CapturedPrompt>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CapturedPrompt {
    verification_url: String,
    user_code: String,
    expires_after: Duration,
}

impl LoginPromptPresenter for CapturingPresenter {
    fn present(
        &self,
        prompt: LoginPrompt<'_>,
    ) -> Result<(), super::super::login_prompt::LoginPromptError> {
        let LoginPrompt::DeviceCode {
            verification_url,
            user_code,
            expires_after,
        } = prompt
        else {
            return Err(
                super::super::login_prompt::LoginPromptError::terminal_output_unavailable(),
            );
        };
        self.prompts.lock().push(CapturedPrompt {
            verification_url: verification_url.to_owned(),
            user_code: user_code.to_owned(),
            expires_after,
        });
        Ok(())
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

fn options(
    root: NornAuthRoot,
    server: &MockServer,
    presenter: Arc<CapturingPresenter>,
) -> DeviceLoginOptions {
    let http = OAuthHttpOptions {
        request_timeout: Duration::from_secs(2),
        device_code_timeout: Duration::from_secs(5),
        ..OAuthHttpOptions::default()
    };
    let mut options = DeviceLoginOptions::new(
        root,
        "device-client-id".to_owned(),
        AuthCredentialsStoreMode::File,
        http,
        presenter,
    );
    options.endpoints = DeviceEndpoints::test_authority(&server.uri());
    options
}

async fn mount_user_code(server: &MockServer, user_code: &str, interval: &str) {
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_auth_id": "device-auth-secret",
            "user_code": user_code,
            "interval": interval
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_poll_success(server: &MockServer) {
    let verifier = "device-code-verifier-secret";
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "authorization_code": "authorization-code-secret",
            "code_challenge": challenge_for(verifier),
            "code_verifier": verifier
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_token_success(server: &MockServer, account_id: &str, user_id: &str) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": fixture_jwt(account_id, user_id),
            "access_token": "access-token-secret",
            "refresh_token": "refresh-token-secret"
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_success(server: &MockServer) {
    mount_user_code(server, "CODE-1234", "1").await;
    mount_poll_success(server).await;
    mount_token_success(server, "account-fixture", "user-fixture").await;
}

#[tokio::test]
async fn device_login_presents_then_durably_saves_exact_wire_contract() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = NornAuthRoot::try_from(directory.path())?;
    let server = MockServer::start().await;
    mount_success(&server).await;
    let presenter = Arc::new(CapturingPresenter::default());

    run_device_login_with_hooks(
        options(root.clone(), &server, Arc::clone(&presenter)),
        |_| Ok(()),
        || Ok(()),
    )
    .await?;

    let auth = load_auth_dot_json(&root, AuthCredentialsStoreMode::File)?
        .ok_or_else(|| std::io::Error::other("device login did not save credentials"))?;
    let tokens = auth
        .tokens
        .ok_or_else(|| std::io::Error::other("saved credential omitted tokens"))?;
    assert_eq!(tokens.account_id.as_deref(), Some("account-fixture"));
    assert_eq!(
        tokens.id_token.chatgpt_user_id.as_deref(),
        Some("user-fixture")
    );

    {
        let prompts = presenter.prompts.lock();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].user_code, "CODE-1234");
        assert_eq!(prompts[0].expires_after, Duration::from_secs(5));
        assert_eq!(
            prompts[0].verification_url,
            format!("{}/codex/device", server.uri())
        );
    }

    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| std::io::Error::other("wire requests were not recorded"))?;
    assert_eq!(requests.len(), 3);
    assert_json_body(
        &requests[0],
        &serde_json::json!({"client_id": "device-client-id"}),
    )?;
    assert_json_body(
        &requests[1],
        &serde_json::json!({
            "device_auth_id": "device-auth-secret",
            "user_code": "CODE-1234"
        }),
    )?;
    let form = url::form_urlencoded::parse(&requests[2].body)
        .into_owned()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("authorization_code")
    );
    assert_eq!(
        form.get("code").map(String::as_str),
        Some("authorization-code-secret")
    );
    assert_eq!(
        form.get("client_id").map(String::as_str),
        Some("device-client-id")
    );
    let expected_redirect = format!("{}/deviceauth/callback", server.uri());
    assert_eq!(
        form.get("redirect_uri").map(String::as_str),
        Some(expected_redirect.as_str())
    );
    assert_eq!(
        form.get("code_verifier").map(String::as_str),
        Some("device-code-verifier-secret")
    );
    Ok(())
}

#[tokio::test]
async fn unsupported_device_login_is_typed_and_discloses_no_body() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = NornAuthRoot::try_from(directory.path())?;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(
            ResponseTemplate::new(404).set_body_string(
                "authority-body-secret access-token-secret https://private.invalid",
            ),
        )
        .mount(&server)
        .await;
    let presenter = Arc::new(CapturingPresenter::default());

    let result = run_device_login_with_hooks(
        options(root.clone(), &server, presenter),
        |_| Ok(()),
        || Ok(()),
    )
    .await;
    let Err(error) = result else {
        return Err(std::io::Error::other("unsupported device login succeeded").into());
    };
    let rendered = format!("{error} {error:?}");

    assert!(matches!(error, LoginError::DeviceCodeUnsupported));
    assert!(!rendered.contains("authority-body-secret"));
    assert!(!rendered.contains("access-token-secret"));
    assert!(!rendered.contains("private.invalid"));
    assert!(load_auth_dot_json(&root, AuthCredentialsStoreMode::File)?.is_none());
    Ok(())
}

#[tokio::test]
async fn unsafe_terminal_code_is_rejected_before_presentation() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = NornAuthRoot::try_from(directory.path())?;
    let server = MockServer::start().await;
    mount_user_code(&server, "CODE\u{1b}[31m", "1").await;
    let presenter = Arc::new(CapturingPresenter::default());

    let result = run_device_login_with_hooks(
        options(root.clone(), &server, Arc::clone(&presenter)),
        |_| Ok(()),
        || Ok(()),
    )
    .await;

    assert!(matches!(
        result,
        Err(LoginError::DeviceCodeMalformed { stage: "user-code" })
    ));
    assert!(presenter.prompts.lock().is_empty());
    assert!(load_auth_dot_json(&root, AuthCredentialsStoreMode::File)?.is_none());
    Ok(())
}

#[tokio::test]
async fn named_device_login_publishes_only_after_durable_save() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = NornAuthRoot::try_from(directory.path())?;
    let NamedLoginPreparation::Pending(pending) =
        prepare_named_login(&base, "remote", OAuthHttpOptions::default())?
    else {
        return Err(std::io::Error::other("named login was unexpectedly recovered").into());
    };
    let slot = pending.auth_root().clone();
    let server = MockServer::start().await;
    mount_success(&server).await;
    let presenter = Arc::new(CapturingPresenter::default());

    run_device_login_with_hooks(
        options(slot, &server, presenter),
        |_| Ok(()),
        move || {
            pending.commit().map_err(|_error| LoginError::Storage {
                kind: LoginStorageFailureKind::Coordination,
                reason: "named account publication failed".to_owned(),
            })
        },
    )
    .await?;

    let accounts = list_accounts(&base)?;
    assert!(
        accounts
            .iter()
            .any(|account| account.alias == "remote" && account.active)
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn one_deadline_covers_sequential_authority_stages() -> TestResult {
    let deadline = DeviceDeadline::start(Duration::from_secs(5));
    deadline
        .run(async {
            tokio::time::sleep(Duration::from_secs(4)).await;
            Ok::<_, LoginError>(())
        })
        .await?;
    let result = deadline
        .run(async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok::<_, LoginError>(())
        })
        .await;

    assert!(matches!(result, Err(LoginError::DeviceCodeExpired)));
    assert_eq!(
        tokio::time::Instant::now() - deadline.started,
        Duration::from_secs(5)
    );
    Ok(())
}

#[test]
fn every_device_secret_is_redacted_from_debug() {
    let pending = PendingDeviceCode {
        device_auth_id: "device-auth-secret".to_owned(),
        user_code: "user-code-secret".to_owned(),
        interval: Duration::from_secs(1),
    };
    let response = TokenPollResponse {
        authorization_code: "authorization-code-secret".to_owned(),
        code_challenge: "challenge-secret".to_owned(),
        code_verifier: "verifier-secret".to_owned(),
    };
    let rendered = format!("{pending:?} {response:?}");

    for secret in [
        "device-auth-secret",
        "user-code-secret",
        "authorization-code-secret",
        "challenge-secret",
        "verifier-secret",
    ] {
        assert!(!rendered.contains(secret));
    }
    assert!(rendered.contains("[REDACTED]"));
}

fn assert_json_body(request: &Request, expected: &serde_json::Value) -> TestResult {
    let actual = serde_json::from_slice::<serde_json::Value>(&request.body)?;
    assert_eq!(&actual, expected);
    Ok(())
}
