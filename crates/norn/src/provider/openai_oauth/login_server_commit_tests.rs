use super::super::types::CodexAuth;
use super::*;

fn test_auth_document() -> Result<AuthDotJson, std::io::Error> {
    match CodexAuth::create_dummy_chatgpt_auth_for_testing() {
        CodexAuth::ChatGpt(auth) => Ok(*auth),
        CodexAuth::ApiKey(_) => Err(std::io::Error::other(
            "dummy ChatGPT credential returned an API key",
        )),
    }
}

#[test]
fn credential_transaction_failures_map_to_structural_storage_kinds()
-> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (
            CredentialTransactionError::Conflict,
            LoginStorageFailureKind::Conflict,
        ),
        (
            CredentialTransactionError::VerificationConflict,
            LoginStorageFailureKind::Conflict,
        ),
        (
            CredentialTransactionError::RecoveryIncomplete(std::io::Error::other("restore failed")),
            LoginStorageFailureKind::Conflict,
        ),
        (
            CredentialTransactionError::DeletedButUndurable(std::io::Error::other(
                "directory sync failed",
            )),
            LoginStorageFailureKind::Undurable,
        ),
        (
            CredentialTransactionError::OpenRoot(std::io::Error::other("root unavailable")),
            LoginStorageFailureKind::Coordination,
        ),
        (
            CredentialTransactionError::OpenLock(std::io::Error::other("lock unavailable")),
            LoginStorageFailureKind::Coordination,
        ),
        (
            CredentialTransactionError::LockTimeout {
                waited: Duration::from_millis(1),
            },
            LoginStorageFailureKind::Coordination,
        ),
        (
            CredentialTransactionError::Lock(std::io::Error::other("lock failed")),
            LoginStorageFailureKind::Coordination,
        ),
        (
            CredentialTransactionError::Storage(super::super::storage::StorageError::Io(
                std::io::Error::other("storage failed"),
            )),
            LoginStorageFailureKind::Coordination,
        ),
    ];

    for (source, expected_kind) in cases {
        let LoginError::Storage { kind, .. } = map_credential_transaction_error(source) else {
            return Err(std::io::Error::other(
                "credential transaction failure lost its storage classification",
            )
            .into());
        };
        assert_eq!(kind, expected_kind);
    }
    Ok(())
}

#[test]
fn dropping_login_server_cancels_a_waiting_callback_worker()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let auth_root = NornAuthRoot::try_from(directory.path())?;
    let (prepared_sender, prepared) = oneshot::channel();
    let (acknowledgement, acknowledgement_receiver) = oneshot::channel();
    let (finished_sender, finished) = oneshot::channel();
    let lifecycle = Arc::new(AtomicU8::new(LOGIN_WAITING));
    let server = LoginServer {
        prepared,
        acknowledgement: Some(acknowledgement),
        finished,
        auth_root,
        expected_revision: None,
        mode: AuthCredentialsStoreMode::File,
        credential_lock_timeout: OAuthHttpOptions::DEFAULT_CREDENTIAL_LOCK_TIMEOUT,
        lifecycle: Arc::clone(&lifecycle),
    };
    drop(server);
    drop(prepared_sender);
    drop(finished_sender);
    assert_eq!(lifecycle.load(Ordering::Acquire), LOGIN_CANCELED);
    assert_eq!(
        acknowledgement_receiver.blocking_recv()?,
        CommitAcknowledgement::Canceled
    );
    Ok(())
}

#[tokio::test]
async fn dropping_completion_before_commit_never_persists_prepared_auth()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let auth_root = NornAuthRoot::try_from(directory.path())?;
    let (prepared_sender, prepared) = oneshot::channel();
    let (acknowledgement, acknowledgement_receiver) = oneshot::channel();
    let (finished_sender, finished) = oneshot::channel();
    let lifecycle = Arc::new(AtomicU8::new(LOGIN_CALLBACK_CLAIMED));
    let server = LoginServer {
        prepared,
        acknowledgement: Some(acknowledgement),
        finished,
        auth_root,
        expected_revision: None,
        mode: AuthCredentialsStoreMode::File,
        credential_lock_timeout: OAuthHttpOptions::DEFAULT_CREDENTIAL_LOCK_TIMEOUT,
        lifecycle,
    };
    prepared_sender
        .send(Ok(test_auth_document()?))
        .map_err(|prepared| {
            drop(prepared);
            std::io::Error::other("prepared receiver closed")
        })?;

    let completion = server.block_until_done();
    drop(completion);
    drop(finished_sender);

    assert_eq!(
        acknowledgement_receiver.await?,
        CommitAcknowledgement::Canceled
    );
    assert!(!directory.path().join("auth.json").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_completion_during_transaction_acquisition_never_commits()
-> Result<(), Box<dyn std::error::Error>> {
    const PENDING_ACQUIRE_TIMEOUT: Duration = Duration::from_millis(250);
    const DRAIN_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);
    const DRAIN_HOLD_INTERVAL: Duration = Duration::from_millis(500);

    let directory = tempfile::tempdir()?;
    let auth_path = directory.path().join("auth.json");
    let auth_root = NornAuthRoot::try_from(directory.path())?;
    let held_transaction = CredentialTransaction::acquire(&auth_root, DRAIN_ACQUIRE_TIMEOUT)?;
    let (prepared_sender, prepared) = oneshot::channel();
    let (acknowledgement, acknowledgement_receiver) = oneshot::channel();
    let (finished_sender, finished) = oneshot::channel();
    let lifecycle = Arc::new(AtomicU8::new(LOGIN_CALLBACK_CLAIMED));
    let server = LoginServer {
        prepared,
        acknowledgement: Some(acknowledgement),
        finished,
        auth_root: auth_root.clone(),
        expected_revision: None,
        mode: AuthCredentialsStoreMode::File,
        credential_lock_timeout: PENDING_ACQUIRE_TIMEOUT,
        lifecycle: Arc::clone(&lifecycle),
    };
    prepared_sender
        .send(Ok(test_auth_document()?))
        .map_err(|prepared| {
            drop(prepared);
            std::io::Error::other("prepared receiver closed")
        })?;

    let mut completion = Box::pin(server.block_until_done());
    for _ in 0..2 {
        let completion_state = std::future::poll_fn(|context| {
            std::task::Poll::Ready(std::future::Future::poll(completion.as_mut(), context))
        })
        .await;
        if let std::task::Poll::Ready(result) = completion_state {
            return Err(std::io::Error::other(format!(
                "completion resolved while the credential transaction was held: {result:?}"
            ))
            .into());
        }
        tokio::task::yield_now().await;
    }

    drop(completion);
    assert_eq!(lifecycle.load(Ordering::Acquire), LOGIN_CALLBACK_CLAIMED);
    assert_eq!(
        acknowledgement_receiver.await?,
        CommitAcknowledgement::Canceled
    );

    drop(held_transaction);
    let drain_root = auth_root;
    let drain_guard = tokio::task::spawn_blocking(move || {
        CredentialTransaction::acquire(&drain_root, DRAIN_ACQUIRE_TIMEOUT)
    })
    .await
    .map_err(|error| std::io::Error::other(format!("drain task failed: {error}")))??;

    // If the canceled acquisition wins the gate, this begins after it drops.
    // Otherwise, holding the gate past its deadline drains it by timeout.
    tokio::time::sleep(DRAIN_HOLD_INTERVAL).await;
    drop(drain_guard);
    drop(finished_sender);

    assert!(!auth_path.exists());
    Ok(())
}

#[tokio::test]
async fn durable_save_precedes_commit_acknowledgement() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let auth_path = directory.path().join("auth.json");
    let auth_root = NornAuthRoot::try_from(directory.path())?;
    let (prepared_sender, prepared) = oneshot::channel();
    let (acknowledgement, acknowledgement_receiver) = oneshot::channel();
    let (finished_sender, finished) = oneshot::channel();
    let lifecycle = Arc::new(AtomicU8::new(LOGIN_CALLBACK_CLAIMED));
    let server = LoginServer {
        prepared,
        acknowledgement: Some(acknowledgement),
        finished,
        auth_root,
        expected_revision: None,
        mode: AuthCredentialsStoreMode::File,
        credential_lock_timeout: OAuthHttpOptions::DEFAULT_CREDENTIAL_LOCK_TIMEOUT,
        lifecycle,
    };
    prepared_sender
        .send(Ok(test_auth_document()?))
        .map_err(|prepared| {
            drop(prepared);
            std::io::Error::other("prepared receiver closed")
        })?;
    let worker = tokio::spawn(async move {
        let acknowledgement = acknowledgement_receiver
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let credential_was_durable = auth_path.is_file();
        finished_sender
            .send(())
            .map_err(|()| std::io::Error::other("completion receiver closed"))?;
        Ok::<_, std::io::Error>((acknowledgement, credential_was_durable))
    });

    server.block_until_done().await?;
    let (acknowledgement, credential_was_durable) = worker
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))??;

    assert_eq!(acknowledgement, CommitAcknowledgement::Committed);
    assert!(credential_was_durable);
    Ok(())
}

#[tokio::test]
async fn storage_failure_sends_cancel_acknowledgement() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let auth_root_path = directory.path().join("not-a-directory");
    std::fs::write(&auth_root_path, b"not a directory")?;
    let auth_root = NornAuthRoot::try_from(auth_root_path.as_path())?;
    let (prepared_sender, prepared) = oneshot::channel();
    let (acknowledgement, acknowledgement_receiver) = oneshot::channel();
    let (finished_sender, finished) = oneshot::channel();
    let lifecycle = Arc::new(AtomicU8::new(LOGIN_CALLBACK_CLAIMED));
    let server = LoginServer {
        prepared,
        acknowledgement: Some(acknowledgement),
        finished,
        auth_root,
        expected_revision: None,
        mode: AuthCredentialsStoreMode::File,
        credential_lock_timeout: OAuthHttpOptions::DEFAULT_CREDENTIAL_LOCK_TIMEOUT,
        lifecycle,
    };
    prepared_sender
        .send(Ok(test_auth_document()?))
        .map_err(|prepared| {
            drop(prepared);
            std::io::Error::other("prepared receiver closed")
        })?;
    let worker = tokio::spawn(async move {
        let acknowledgement = acknowledgement_receiver
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        finished_sender
            .send(())
            .map_err(|()| std::io::Error::other("completion receiver closed"))?;
        Ok::<_, std::io::Error>(acknowledgement)
    });

    let result = server.block_until_done().await;
    let acknowledgement = worker
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))??;

    assert!(matches!(result, Err(LoginError::Storage { .. })));
    assert_eq!(acknowledgement, CommitAcknowledgement::Canceled);
    Ok(())
}
