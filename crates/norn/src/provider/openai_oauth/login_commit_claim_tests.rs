use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::*;
use crate::provider::openai_oauth::{ChatGptTokens, IdTokenInfo, OAuthHttpOptions};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn test_auth() -> AuthDotJson {
    AuthDotJson::from_tokens(ChatGptTokens {
        id_token: IdTokenInfo::create_for_testing("account-fixture"),
        access_token: "access-token-secret".to_owned(),
        refresh_token: "refresh-token-secret".to_owned(),
        account_id: Some("account-fixture".to_owned()),
        additional_fields: BTreeMap::new(),
    })
}

fn commit_test_error(reason: &'static str) -> LoginError {
    LoginError::Storage {
        kind: LoginStorageFailureKind::Coordination,
        reason: reason.to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_after_commit_claim_cannot_split_save_and_publication() -> TestResult {
    let directory = tempfile::tempdir()?;
    let auth_root = NornAuthRoot::try_from(directory.path())?;
    let timing = OAuthHttpOptions::default().credential_lock_timing()?;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    let committed = Arc::new(AtomicBool::new(false));
    let commit_observer = Arc::clone(&committed);
    let task = tokio::spawn(persist_prepared_login(
        auth_root.clone(),
        None,
        AuthCredentialsStoreMode::File,
        timing,
        test_auth(),
        |_| Ok(()),
        move || {
            started_tx
                .send(())
                .map_err(|()| commit_test_error("commit start observer was unavailable"))?;
            release_rx
                .recv()
                .map_err(|_error| commit_test_error("commit release was unavailable"))?;
            commit_observer.store(true, Ordering::Release);
            finished_tx
                .send(())
                .map_err(|()| commit_test_error("commit finish observer was unavailable"))?;
            Ok(())
        },
    ));

    started_rx.await?;
    assert!(auth_root.as_path().join("auth.json").is_file());
    task.abort();
    release_tx.send(())?;
    tokio::time::timeout(Duration::from_secs(2), finished_rx).await??;
    let task_result = task.await;

    assert!(task_result.is_ok() || task_result.is_err_and(|error| error.is_cancelled()));
    assert!(committed.load(Ordering::Acquire));
    assert!(auth_root.as_path().join("auth.json").is_file());
    Ok(())
}
