use std::collections::BTreeMap;

use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::super::auth_root::resolve_norn_auth_root;
use super::super::foreign_home_test_support::{
    populate_foreign_home, snapshot_foreign_home, verify_foreign_home_unchanged,
};
use super::super::storage::{AuthCredentialsStoreMode, load_auth_dot_json, save_auth_dot_json};
use super::super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
#[serial_test::serial]
async fn refresh_leaves_foreign_home_unchanged_at_completion() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(refresh_response()))
        .mount(&server)
        .await;

    let norn_home = tempfile::tempdir()?;
    let codex_home = tempfile::tempdir()?;
    let norn_auth_root = norn_home.path().join("auth");
    save_auth_dot_json(&norn_auth_root, &auth_document("old-access", "old-refresh"))?;
    populate_foreign_home(codex_home.path())?;
    let foreign_before = snapshot_foreign_home(codex_home.path())?;

    temp_env::async_with_vars(
        [
            ("NORN_HOME", Some(norn_home.path().as_os_str())),
            ("CODEX_HOME", Some(codex_home.path().as_os_str())),
        ],
        async {
            let expected_root = NornAuthRoot::try_from(norn_home.path().join("auth"))?;
            let root = resolve_norn_auth_root(None)?;
            assert_eq!(root, expected_root);
            let manager = AuthManager::shared_for_tests(
                root.clone(),
                AuthCredentialsStoreMode::File,
                server.uri(),
            )
            .await?;
            manager.refresh_token_from_authority().await?;
            let rotated = load_auth_dot_json(&root, AuthCredentialsStoreMode::File)?
                .ok_or_else(|| std::io::Error::other("rotated credential is missing"))?;
            let tokens = rotated
                .tokens
                .ok_or_else(|| std::io::Error::other("rotated token bundle is missing"))?;
            assert_eq!(tokens.access_token, "new-access");
            assert_eq!(tokens.refresh_token, "new-refresh");
            Ok::<(), Box<dyn std::error::Error>>(())
        },
    )
    .await?;

    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| std::io::Error::other("request recording is unavailable"))?;
    assert_eq!(requests.len(), 1);
    verify_foreign_home_unchanged(codex_home.path(), &foreign_before)?;
    Ok(())
}

fn auth_document(access_token: &str, refresh_token: &str) -> AuthDotJson {
    AuthDotJson::from_tokens(ChatGptTokens {
        id_token: IdTokenInfo::create_for_testing("foreign-home-account"),
        access_token: access_token.to_owned(),
        refresh_token: refresh_token.to_owned(),
        account_id: Some("foreign-home-account".to_owned()),
        additional_fields: BTreeMap::new(),
    })
}

fn refresh_response() -> serde_json::Value {
    serde_json::json!({
        "id_token": IdTokenInfo::create_for_testing("foreign-home-account").raw_jwt,
        "access_token": "new-access",
        "refresh_token": "new-refresh",
        "account_id": "foreign-home-account",
    })
}
