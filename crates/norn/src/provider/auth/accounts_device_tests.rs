use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::Notify;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::provider::openai_oauth::{AuthDotJson, ChatGptTokens, IdTokenInfo};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

struct NoopPresenter;

impl super::super::super::openai_oauth::LoginPromptPresenter for NoopPresenter {
    fn present(
        &self,
        _prompt: super::super::super::openai_oauth::LoginPrompt<'_>,
    ) -> Result<(), super::super::super::openai_oauth::LoginPromptError> {
        Ok(())
    }
}

struct RejectingPresenter;

impl super::super::super::openai_oauth::LoginPromptPresenter for RejectingPresenter {
    fn present(
        &self,
        _prompt: super::super::super::openai_oauth::LoginPrompt<'_>,
    ) -> Result<(), super::super::super::openai_oauth::LoginPromptError> {
        Err(super::super::super::openai_oauth::LoginPromptError::terminal_output_unavailable())
    }
}

#[derive(Clone)]
struct NotifyingPendingResponse {
    observed: Arc<Notify>,
}

struct ObservedDrop<T> {
    value: Option<T>,
    observed: Arc<Notify>,
}

impl<T> ObservedDrop<T> {
    fn new(value: T, observed: Arc<Notify>) -> Self {
        Self {
            value: Some(value),
            observed,
        }
    }

    fn take(&mut self) -> Option<T> {
        self.value.take()
    }
}

impl<T> Drop for ObservedDrop<T> {
    fn drop(&mut self) {
        drop(self.value.take());
        self.observed.notify_one();
    }
}

impl Respond for NotifyingPendingResponse {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        self.observed.notify_one();
        ResponseTemplate::new(403)
    }
}

fn test_auth() -> AuthDotJson {
    AuthDotJson::from_tokens(ChatGptTokens {
        id_token: IdTokenInfo::create_for_testing("account-fixture"),
        access_token: "access-token-secret".to_owned(),
        refresh_token: "refresh-token-secret".to_owned(),
        account_id: Some("account-fixture".to_owned()),
        additional_fields: BTreeMap::new(),
    })
}

fn missing_commit_state() -> LoginError {
    LoginError::Storage {
        kind: LoginStorageFailureKind::Coordination,
        reason: "test commit state was unavailable".to_owned(),
    }
}

#[test]
fn dropping_named_login_guard_retires_pending_catalog_slot() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = NornAuthRoot::try_from(directory.path())?;
    let NamedLoginPreparation::Pending(reservation) =
        prepare_named_login(&base, "cancelled", OAuthHttpOptions::default())?
    else {
        return Err(std::io::Error::other("new named login was unexpectedly recovered").into());
    };

    let original_root = reservation.auth_root().clone();
    let guard = PendingNamedLogin::new(reservation);
    assert!(
        list_account_catalog(&base)?
            .iter()
            .all(|account| account.alias != "cancelled")
    );
    drop(guard);

    assert!(
        list_account_catalog(&base)?
            .iter()
            .all(|account| account.alias != "cancelled")
    );
    assert!(!original_root.as_path().exists());

    let NamedLoginPreparation::Pending(replacement) =
        prepare_named_login(&base, "cancelled", OAuthHttpOptions::default())?
    else {
        return Err(std::io::Error::other("cancelled alias was not freshly reservable").into());
    };
    assert_ne!(replacement.auth_root(), &original_root);
    replacement.abort()?;
    Ok(())
}

#[tokio::test]
async fn cancelling_named_device_poll_retires_slot_without_credentials() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = NornAuthRoot::try_from(directory.path())?;
    let NamedLoginPreparation::Pending(reservation) =
        prepare_named_login(&base, "remote", OAuthHttpOptions::default())?
    else {
        return Err(std::io::Error::other("new named login was unexpectedly recovered").into());
    };
    let old_root = reservation.auth_root().clone();
    let guard = PendingNamedLogin::new(reservation);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_auth_id": "device-auth-secret",
            "user_code": "CODE-1234",
            "interval": "30"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let poll_observed = Arc::new(Notify::new());
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(NotifyingPendingResponse {
            observed: Arc::clone(&poll_observed),
        })
        .expect(1)
        .mount(&server)
        .await;
    let commit_called = Arc::new(AtomicBool::new(false));
    let commit_observer = Arc::clone(&commit_called);
    let options = DeviceLoginOptions::new(
        old_root.clone(),
        CLIENT_ID.to_owned(),
        AuthCredentialsStoreMode::File,
        OAuthHttpOptions {
            request_timeout: Duration::from_secs(2),
            device_code_timeout: Duration::from_secs(60),
            ..OAuthHttpOptions::default()
        },
        Arc::new(NoopPresenter),
    )
    .with_test_authority(&server.uri());
    let task = tokio::spawn(run_device_login_with_hooks(
        options,
        |_| Ok(()),
        move || {
            commit_observer.store(true, Ordering::Relaxed);
            guard.commit()
        },
    ));

    tokio::time::timeout(Duration::from_secs(2), poll_observed.notified()).await?;
    task.abort();
    let cancellation = task.await;

    assert!(cancellation.is_err_and(|error| error.is_cancelled()));
    assert!(!commit_called.load(Ordering::Relaxed));
    assert!(
        super::super::super::openai_oauth::load_auth_dot_json(
            &old_root,
            AuthCredentialsStoreMode::File,
        )?
        .is_none()
    );
    assert!(!old_root.as_path().exists());
    let NamedLoginPreparation::Pending(replacement) =
        prepare_named_login(&base, "remote", OAuthHttpOptions::default())?
    else {
        return Err(std::io::Error::other("cancelled alias was not freshly reservable").into());
    };
    assert_ne!(replacement.auth_root(), &old_root);
    replacement.abort()?;
    Ok(())
}

#[tokio::test]
async fn named_presentation_failure_retires_slot_before_polling() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = NornAuthRoot::try_from(directory.path())?;
    let NamedLoginPreparation::Pending(reservation) =
        prepare_named_login(&base, "remote", OAuthHttpOptions::default())?
    else {
        return Err(std::io::Error::other("new named login was unexpectedly recovered").into());
    };
    let old_root = reservation.auth_root().clone();
    let pending = PendingNamedLogin::new(reservation);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_auth_id": "device-auth-secret",
            "user_code": "CODE-1234",
            "interval": "30"
        })))
        .expect(1)
        .mount(&server)
        .await;
    for endpoint in ["/api/accounts/deviceauth/token", "/oauth/token"] {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
    }
    let options = DeviceLoginOptions::new(
        old_root.clone(),
        CLIENT_ID.to_owned(),
        AuthCredentialsStoreMode::File,
        OAuthHttpOptions::default(),
        Arc::new(RejectingPresenter),
    )
    .with_test_authority(&server.uri());

    let result = run_device_login_with_hooks(options, |_| Ok(()), move || pending.commit()).await;

    assert!(matches!(result, Err(LoginError::Presentation)));
    assert!(!old_root.as_path().exists());
    let NamedLoginPreparation::Pending(replacement) =
        prepare_named_login(&base, "remote", OAuthHttpOptions::default())?
    else {
        return Err(std::io::Error::other("failed presentation left the alias reserved").into());
    };
    assert_ne!(replacement.auth_root(), &old_root);
    replacement.abort()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_default_commit_while_lock_waits_never_writes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = NornAuthRoot::try_from(directory.path())?;
    let reservation = prepare_default_login(&base, OAuthHttpOptions::default())?;
    let held = crate::provider::openai_oauth::CredentialTransaction::acquire(
        &base,
        OAuthHttpOptions::default().credential_lock_timing()?,
    )?;
    let cleanup_observed = Arc::new(Notify::new());
    let mut commit_state = ObservedDrop::new(reservation, Arc::clone(&cleanup_observed));
    let mut completion = Box::pin(crate::provider::openai_oauth::persist_prepared_login(
        base.clone(),
        None,
        AuthCredentialsStoreMode::File,
        OAuthHttpOptions::default().credential_lock_timing()?,
        test_auth(),
        |_| Ok(()),
        move || {
            let reservation = commit_state.take().ok_or_else(missing_commit_state)?;
            drop(reservation);
            Ok(())
        },
    ));

    for _ in 0..2 {
        let state = std::future::poll_fn(|context| {
            std::task::Poll::Ready(std::future::Future::poll(completion.as_mut(), context))
        })
        .await;
        if let std::task::Poll::Ready(result) = state {
            return Err(std::io::Error::other(format!(
                "default commit resolved while the credential lock was held: {result:?}"
            ))
            .into());
        }
        tokio::task::yield_now().await;
    }

    drop(completion);
    drop(held);
    tokio::time::timeout(Duration::from_secs(2), cleanup_observed.notified()).await?;

    assert!(
        crate::provider::openai_oauth::load_auth_dot_json(&base, AuthCredentialsStoreMode::File,)?
            .is_none()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_named_commit_while_lock_waits_retires_slot() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = NornAuthRoot::try_from(directory.path())?;
    let NamedLoginPreparation::Pending(reservation) =
        prepare_named_login(&base, "remote", OAuthHttpOptions::default())?
    else {
        return Err(std::io::Error::other("new named login was unexpectedly recovered").into());
    };
    let old_root = reservation.auth_root().clone();
    let pending = PendingNamedLogin::new(reservation);
    let held = crate::provider::openai_oauth::CredentialTransaction::acquire(
        &old_root,
        OAuthHttpOptions::default().credential_lock_timing()?,
    )?;
    let cleanup_observed = Arc::new(Notify::new());
    let mut commit_state = ObservedDrop::new(pending, Arc::clone(&cleanup_observed));
    let mut completion = Box::pin(crate::provider::openai_oauth::persist_prepared_login(
        old_root.clone(),
        None,
        AuthCredentialsStoreMode::File,
        OAuthHttpOptions::default().credential_lock_timing()?,
        test_auth(),
        |_| Ok(()),
        move || {
            let pending = commit_state.take().ok_or_else(missing_commit_state)?;
            pending.commit()
        },
    ));

    for _ in 0..2 {
        let state = std::future::poll_fn(|context| {
            std::task::Poll::Ready(std::future::Future::poll(completion.as_mut(), context))
        })
        .await;
        if let std::task::Poll::Ready(result) = state {
            return Err(std::io::Error::other(format!(
                "named commit resolved while the credential lock was held: {result:?}"
            ))
            .into());
        }
        tokio::task::yield_now().await;
    }

    drop(completion);
    drop(held);
    tokio::time::timeout(Duration::from_secs(2), cleanup_observed.notified()).await?;

    assert!(!old_root.as_path().exists());
    let NamedLoginPreparation::Pending(replacement) =
        prepare_named_login(&base, "remote", OAuthHttpOptions::default())?
    else {
        return Err(std::io::Error::other("cancelled alias was not freshly reservable").into());
    };
    assert_ne!(replacement.auth_root(), &old_root);
    replacement.abort()?;
    Ok(())
}
