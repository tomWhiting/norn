use std::sync::Arc;
use std::sync::atomic::AtomicU8;

use tokio::sync::oneshot;

use super::super::auth_root::resolve_norn_auth_root;
use super::super::foreign_home_test_support::{
    populate_foreign_home, snapshot_foreign_home, verify_foreign_home_unchanged,
};
use super::super::types::CodexAuth;
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
#[serial_test::serial]
async fn login_commit_leaves_foreign_home_unchanged_at_completion() -> TestResult {
    let norn_home = tempfile::tempdir()?;
    let codex_home = tempfile::tempdir()?;
    populate_foreign_home(codex_home.path())?;
    let foreign_before = snapshot_foreign_home(codex_home.path())?;

    temp_env::async_with_vars(
        [
            ("NORN_HOME", Some(norn_home.path().as_os_str())),
            ("CODEX_HOME", Some(codex_home.path().as_os_str())),
        ],
        async {
            let expected_root = NornAuthRoot::try_from(norn_home.path().join("auth"))?;
            let resolved_root = resolve_norn_auth_root(None)?;
            assert_eq!(resolved_root, expected_root);
            commit_fixture_login(resolved_root).await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        },
    )
    .await?;

    verify_foreign_home_unchanged(codex_home.path(), &foreign_before)?;
    assert!(norn_home.path().join("auth/auth.json").is_file());
    Ok(())
}

async fn commit_fixture_login(auth_root: NornAuthRoot) -> TestResult {
    let (prepared_sender, prepared) = oneshot::channel();
    let (acknowledgement, acknowledgement_receiver) = oneshot::channel();
    let (finished_sender, finished) = oneshot::channel();
    let server = LoginServer {
        prepared,
        acknowledgement: Some(acknowledgement),
        finished,
        auth_root,
        expected_revision: None,
        mode: AuthCredentialsStoreMode::File,
        credential_lock_timing: OAuthHttpOptions::default().credential_lock_timing()?,
        lifecycle: Arc::new(AtomicU8::new(LOGIN_CALLBACK_CLAIMED)),
    };
    let auth = match CodexAuth::create_dummy_chatgpt_auth_for_testing() {
        CodexAuth::ChatGpt(auth) => *auth,
        CodexAuth::ApiKey(_) => {
            return Err(
                std::io::Error::other("dummy ChatGPT credential returned an API key").into(),
            );
        }
    };
    if prepared_sender.send(Ok(auth)).is_err() {
        return Err(std::io::Error::other("prepared login receiver closed").into());
    }
    let worker = tokio::spawn(async move {
        let acknowledgement = acknowledgement_receiver
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        finished_sender
            .send(())
            .map_err(|()| std::io::Error::other("login completion receiver closed"))?;
        Ok::<_, std::io::Error>(acknowledgement)
    });

    server.block_until_done().await?;
    let acknowledgement = worker
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))??;
    if acknowledgement != CommitAcknowledgement::Committed {
        return Err(std::io::Error::other("login was not durably committed").into());
    }
    Ok(())
}
