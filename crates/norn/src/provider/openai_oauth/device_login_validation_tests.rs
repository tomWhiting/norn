use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::provider::openai_oauth::storage::save_auth_dot_json;
use crate::provider::openai_oauth::{ChatGptTokens, IdTokenInfo, load_auth_dot_json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

struct AcceptingPresenter;

impl LoginPromptPresenter for AcceptingPresenter {
    fn present(
        &self,
        _prompt: LoginPrompt<'_>,
    ) -> Result<(), super::super::login_prompt::LoginPromptError> {
        Ok(())
    }
}

struct RejectingPresenter;

impl LoginPromptPresenter for RejectingPresenter {
    fn present(
        &self,
        _prompt: LoginPrompt<'_>,
    ) -> Result<(), super::super::login_prompt::LoginPromptError> {
        Err(super::super::login_prompt::LoginPromptError::terminal_output_unavailable())
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
    presenter: Arc<dyn LoginPromptPresenter>,
) -> DeviceLoginOptions {
    DeviceLoginOptions::new(
        root,
        "device-client-id".to_owned(),
        AuthCredentialsStoreMode::File,
        OAuthHttpOptions {
            request_timeout: Duration::from_secs(2),
            device_code_timeout: Duration::from_secs(5),
            ..OAuthHttpOptions::default()
        },
        presenter,
    )
    .with_test_authority(&server.uri())
}

async fn mount_user_code(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_auth_id": "device-auth-secret",
            "user_code": "CODE-1234",
            "interval": "1"
        })))
        .expect(1)
        .mount(server)
        .await;
}

fn valid_poll_response() -> serde_json::Value {
    let verifier = "device-code-verifier-secret";
    serde_json::json!({
        "authorization_code": "authorization-code-secret",
        "code_challenge": challenge_for(verifier),
        "code_verifier": verifier
    })
}

#[tokio::test]
async fn malformed_poll_successes_never_reach_token_exchange() -> TestResult {
    let cases = [
        serde_json::json!({
            "authorization_code": "authorization-code-secret",
            "code_challenge": "mismatched-challenge-secret",
            "code_verifier": "device-code-verifier-secret"
        }),
        serde_json::json!({
            "authorization_code": "",
            "code_challenge": "challenge-secret",
            "code_verifier": "verifier-secret"
        }),
        serde_json::json!({
            "authorization_code": "authorization-code-secret\nforged",
            "code_challenge": "challenge-secret",
            "code_verifier": "verifier-secret"
        }),
    ];
    for poll_body in cases {
        let directory = tempfile::tempdir()?;
        let root = NornAuthRoot::try_from(directory.path())?;
        let server = MockServer::start().await;
        mount_user_code(&server).await;
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(poll_body))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let result = run_device_login_with_hooks(
            options(root.clone(), &server, Arc::new(AcceptingPresenter)),
            |_| Ok(()),
            || Ok(()),
        )
        .await;
        let Err(error) = result else {
            return Err(std::io::Error::other("malformed poll response succeeded").into());
        };
        let rendered = format!("{error} {error:?}");

        assert!(matches!(
            error,
            LoginError::DeviceCodeMalformed { stage: "poll" }
        ));
        for secret in [
            "authorization-code-secret",
            "mismatched-challenge-secret",
            "challenge-secret",
            "device-code-verifier-secret",
            "verifier-secret",
            "forged",
        ] {
            assert!(!rendered.contains(secret));
        }
        assert!(load_auth_dot_json(&root, AuthCredentialsStoreMode::File)?.is_none());
    }
    Ok(())
}

#[tokio::test]
async fn malformed_poll_json_is_payload_free_and_stops_before_exchange() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = NornAuthRoot::try_from(directory.path())?;
    let server = MockServer::start().await;
    mount_user_code(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_string("malformed-poll-body-secret"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let result = run_device_login_with_hooks(
        options(root.clone(), &server, Arc::new(AcceptingPresenter)),
        |_| Ok(()),
        || Ok(()),
    )
    .await;
    let Err(error) = result else {
        return Err(std::io::Error::other("malformed poll JSON succeeded").into());
    };
    let rendered = format!("{error} {error:?}");

    assert!(matches!(
        error,
        LoginError::DeviceCodeMalformed { stage: "poll" }
    ));
    assert!(!rendered.contains("malformed-poll-body-secret"));
    assert!(load_auth_dot_json(&root, AuthCredentialsStoreMode::File)?.is_none());
    Ok(())
}

#[tokio::test]
async fn token_redirect_is_not_followed_or_disclosed() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = NornAuthRoot::try_from(directory.path())?;
    let source = MockServer::start().await;
    let target = MockServer::start().await;
    mount_user_code(&source).await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(valid_poll_response()))
        .expect(1)
        .mount(&source)
        .await;
    Mock::given(method("POST"))
        .and(path("/redirect-target"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&target)
        .await;
    let location = format!(
        "{}/redirect-target?secret=token-redirect-secret",
        target.uri()
    );
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("Location", location.as_str())
                .set_body_string("token-redirect-body-secret"),
        )
        .expect(1)
        .mount(&source)
        .await;

    let result = run_device_login_with_hooks(
        options(root.clone(), &source, Arc::new(AcceptingPresenter)),
        |_| Ok(()),
        || Ok(()),
    )
    .await;
    let Err(error) = result else {
        return Err(std::io::Error::other("redirected token exchange succeeded").into());
    };
    let rendered = format!("{error} {error:?}");

    assert!(matches!(error, LoginError::TokenExchange(_)));
    assert!(!rendered.contains("token-redirect-secret"));
    assert!(!rendered.contains("token-redirect-body-secret"));
    let target_requests = target
        .received_requests()
        .await
        .ok_or_else(|| std::io::Error::other("token redirect requests were not recorded"))?;
    assert!(target_requests.is_empty());
    assert!(load_auth_dot_json(&root, AuthCredentialsStoreMode::File)?.is_none());
    Ok(())
}

#[tokio::test]
async fn presentation_failure_preserves_existing_bytes_and_stops_before_poll() -> TestResult {
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
    let auth_path = root.as_path().join(super::super::storage::AUTH_JSON_FILE);
    let before = std::fs::read(&auth_path)?;
    let server = MockServer::start().await;
    mount_user_code(&server).await;
    for endpoint in ["/api/accounts/deviceauth/token", "/oauth/token"] {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
    }

    let result = run_device_login_with_hooks(
        options(root, &server, Arc::new(RejectingPresenter)),
        |_| Ok(()),
        || Ok(()),
    )
    .await;

    assert!(matches!(result, Err(LoginError::Presentation)));
    assert_eq!(std::fs::read(auth_path)?, before);
    Ok(())
}
