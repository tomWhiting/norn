use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use super::{AgentBuilder, SessionSpec};
use crate::error::{NornError, ProviderError};
use crate::provider::auth::{ApiKeyAuthProvider, AuthProvider, AuthSource, OAuthAuthProvider};
use crate::provider::mock::MockProvider;
use crate::provider::openai::OpenAiProvider;
use crate::provider::openai_oauth::{
    AuthDotJson, AuthManager, ChatGptTokens, CodexAuth, IdTokenInfo, OAuthHttpOptions,
};
use crate::provider::{
    Provider, ProviderCapabilities, ProviderConfig, ProviderStateIdentity, SecretString,
};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::{DurabilityPolicy, SessionManager};
use wiremock::MockServer;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn provider(identity: ProviderStateIdentity) -> Arc<dyn Provider> {
    Arc::new(MockProvider::new(Vec::new()).with_state_identity(identity))
}

fn provider_config(auth_source: AuthSource, base_url: Option<String>) -> ProviderConfig {
    ProviderConfig {
        auth_source,
        base_url,
        timeout: Duration::from_secs(5),
        max_retries: 0,
        provider_options: None,
        debug_dump_file: None,
        rate_limit: None,
        rate_limit_interval: None,
        retry_backoff: None,
        retry_after_ceiling: None,
    }
}

fn oauth_provider(
    account_id: &str,
    user_id: &str,
    access_token: &str,
) -> TestResult<Arc<dyn Provider>> {
    let mut id_token = IdTokenInfo::create_for_testing(account_id);
    id_token.chatgpt_user_id = Some(user_id.to_owned());
    let auth = CodexAuth::ChatGpt(Box::new(AuthDotJson::from_tokens(ChatGptTokens {
        id_token,
        access_token: access_token.to_owned(),
        refresh_token: "refresh-token".to_owned(),
        account_id: Some(account_id.to_owned()),
        additional_fields: BTreeMap::new(),
    })));
    let manager = AuthManager::from_static_auth(auth, OAuthHttpOptions::default())?;
    let auth_provider: Arc<dyn AuthProvider> = Arc::new(OAuthAuthProvider::from_manager(manager));
    Ok(Arc::new(OpenAiProvider::with_auth_provider(
        provider_config(AuthSource::oauth_default(), None),
        auth_provider,
    )?))
}

fn api_key_provider(key: &str, base_url: String) -> TestResult<Arc<dyn Provider>> {
    let auth_source = AuthSource::ApiKey {
        key: SecretString::new(key),
    };
    let auth_provider: Arc<dyn AuthProvider> =
        Arc::new(ApiKeyAuthProvider::new(SecretString::new(key)));
    Ok(Arc::new(OpenAiProvider::with_auth_provider(
        provider_config(auth_source, Some(base_url)),
        auth_provider,
    )?))
}

async fn request_count(server: &MockServer) -> TestResult<usize> {
    let requests = server
        .received_requests()
        .await
        .ok_or_else(|| io::Error::other("wiremock request recording is disabled"))?;
    Ok(requests.len())
}

fn build_managed(
    provider: Arc<dyn Provider>,
    manager: &SessionManager,
    spec: SessionSpec,
    working_dir: &std::path::Path,
) -> Result<crate::agent::Agent, NornError> {
    let model = crate::model_catalog::default_selection().model;
    AgentBuilder::new(provider)
        .model(model)
        .context_window_limit(272_000)
        .working_dir(working_dir)
        .allowed_tools(&[])
        .open_session(manager, spec, DurabilityPolicy::Flush)
        .build()
}

#[test]
fn managed_open_validates_affinity_before_returning_an_agent() -> TestResult {
    let working_dir = tempfile::tempdir()?;
    let session_dir = tempfile::tempdir()?;
    let manager = SessionManager::new(session_dir.path());
    let first_identity = ProviderStateIdentity::derive(
        "norn.agent-builder.affinity-test",
        b"first-provider-fixture",
    );
    let other_identity = ProviderStateIdentity::derive(
        "norn.agent-builder.affinity-test",
        b"other-provider-fixture",
    );

    let created = build_managed(
        provider(first_identity),
        &manager,
        SessionSpec::Create { name: None },
        working_dir.path(),
    )?;
    let entry = created
        .session_entry()
        .ok_or_else(|| io::Error::other("managed create did not surface its index entry"))?
        .clone();
    assert_eq!(entry.provider_state_identity, Some(first_identity));
    drop(created);

    let resumed = build_managed(
        provider(first_identity),
        &manager,
        SessionSpec::resume(&entry.id),
        working_dir.path(),
    )?;
    assert_eq!(
        resumed
            .session_entry()
            .and_then(|resumed_entry| resumed_entry.provider_state_identity),
        Some(first_identity),
    );
    drop(resumed);

    // A different credential of the same operator rebinds instead of
    // locking the session out (owner ruling 2026-07-25: sessions are not
    // locked to an account): the managed open returns an agent whose
    // session row carries the new identity behind a durable epoch
    // boundary that retires the previous identity's anchors.
    let rebound = build_managed(
        provider(other_identity),
        &manager,
        SessionSpec::resume(&entry.id),
        working_dir.path(),
    )?;
    assert_eq!(
        rebound
            .session_entry()
            .and_then(|rebound_entry| rebound_entry.provider_state_identity),
        Some(other_identity),
    );
    drop(rebound);

    let before = serde_json::to_vec(&manager.list()?)?;
    let absent = build_managed(
        Arc::new(MockProvider::new(Vec::new())),
        &manager,
        SessionSpec::resume(&entry.id),
        working_dir.path(),
    );
    match absent {
        Err(NornError::Provider(ProviderError::ProviderStateIdentityMismatch)) => {}
        Err(other) => {
            return Err(io::Error::other(format!(
                "expected absent provider-state identity to fail closed, got {other}"
            ))
            .into());
        }
        Ok(_) => {
            return Err(io::Error::other("identity-less managed resume returned an agent").into());
        }
    }
    assert_eq!(
        serde_json::to_vec(&manager.list()?)?,
        before,
        "an identity-less managed resume must not mutate the session index",
    );
    Ok(())
}

#[test]
fn threaded_provider_without_identity_creates_no_managed_session() -> TestResult {
    let working_dir = tempfile::tempdir()?;
    let session_dir = tempfile::tempdir()?;
    let manager = SessionManager::new(session_dir.path());
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::with_capabilities(
        Vec::new(),
        ProviderCapabilities::openai_responses(),
    ));

    let result = build_managed(
        provider,
        &manager,
        SessionSpec::Create { name: None },
        working_dir.path(),
    );
    match result {
        Err(NornError::Provider(ProviderError::ProviderStateIdentityRequired)) => {}
        Err(other) => {
            return Err(io::Error::other(format!(
                "expected required provider-state identity, got {other}"
            ))
            .into());
        }
        Ok(_) => {
            return Err(io::Error::other("identity-less threaded provider built an agent").into());
        }
    }
    assert!(
        std::fs::read_dir(session_dir.path())?.next().is_none(),
        "identity validation must precede all managed-session filesystem mutation",
    );
    Ok(())
}

#[test]
fn latest_resume_and_fork_enforce_affinity_before_mutation_or_publication() -> TestResult {
    let working_dir = tempfile::tempdir()?;
    let working_dir = working_dir.path().canonicalize()?;
    let session_dir = tempfile::tempdir()?;
    let manager = SessionManager::new(session_dir.path());
    let selected = ProviderStateIdentity::derive(
        "norn.agent-builder.latest-affinity-test",
        b"selected-provider",
    );
    let different = ProviderStateIdentity::derive(
        "norn.agent-builder.latest-affinity-test",
        b"different-provider",
    );

    let created = build_managed(
        provider(selected),
        &manager,
        SessionSpec::Create { name: None },
        &working_dir,
    )?;
    let source_id = created
        .session_entry()
        .ok_or_else(|| io::Error::other("managed create did not return an index entry"))?
        .id
        .clone();
    created
        .into_parts()
        .event_store
        .append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "forkable history".to_owned(),
        })?;

    let resumed = build_managed(
        provider(selected),
        &manager,
        SessionSpec::resume_latest(working_dir.display().to_string()),
        &working_dir,
    )?;
    assert_eq!(
        resumed.session_entry().map(|entry| entry.id.as_str()),
        Some(source_id.as_str()),
    );
    drop(resumed);

    // A latest-resume under a different credential rebinds the row
    // (owner ruling 2026-07-25: sessions are not locked to an account).
    let rebound = build_managed(
        provider(different),
        &manager,
        SessionSpec::resume_latest(working_dir.display().to_string()),
        &working_dir,
    )?;
    assert_eq!(
        rebound
            .session_entry()
            .and_then(|entry| entry.provider_state_identity),
        Some(different),
    );
    drop(rebound);
    // Move the row back so the fork arms below exercise both directions
    // from a `selected`-bound source.
    drop(build_managed(
        provider(selected),
        &manager,
        SessionSpec::resume_latest(working_dir.display().to_string()),
        &working_dir,
    )?);

    let forked = build_managed(
        provider(selected),
        &manager,
        SessionSpec::fork_latest(working_dir.display().to_string()),
        &working_dir,
    )?;
    let fork_id = forked
        .session_entry()
        .ok_or_else(|| io::Error::other("latest fork did not return an index entry"))?
        .id
        .clone();
    assert_ne!(fork_id, source_id);
    assert_eq!(
        forked
            .session_entry()
            .and_then(|entry| entry.provider_state_identity),
        Some(selected),
    );
    drop(forked);

    // A cross-credential latest-fork publishes a CHILD that adopts the
    // forker's identity behind its own epoch boundary; the SOURCE row is
    // never mutated by a fork.
    let cross_fork = build_managed(
        provider(different),
        &manager,
        SessionSpec::fork_latest(working_dir.display().to_string()),
        &working_dir,
    )?;
    assert_eq!(
        cross_fork
            .session_entry()
            .and_then(|entry| entry.provider_state_identity),
        Some(different),
    );
    drop(cross_fork);
    // Whichever row "latest" resolved to as the fork source, no existing
    // row's binding moved: a fork never mutates a source.
    assert_eq!(
        manager.resolve(&source_id)?.provider_state_identity,
        Some(selected),
        "a cross-credential fork must never mutate the original session's identity",
    );
    assert_eq!(
        manager.resolve(&fork_id)?.provider_state_identity,
        Some(selected),
        "a cross-credential fork must never mutate its immediate source's identity",
    );
    Ok(())
}

#[test]
fn managed_oauth_session_distinguishes_users_in_the_same_account() -> TestResult {
    let working_dir = tempfile::tempdir()?;
    let session_dir = tempfile::tempdir()?;
    let manager = SessionManager::new(session_dir.path());

    let created = build_managed(
        oauth_provider("shared-account", "user-a", "access-a")?,
        &manager,
        SessionSpec::Create { name: None },
        working_dir.path(),
    )?;
    let session_id = created
        .session_entry()
        .ok_or_else(|| io::Error::other("OAuth create did not return an index entry"))?
        .id
        .clone();
    drop(created);

    let refreshed = build_managed(
        oauth_provider("shared-account", "user-a", "rotated-access")?,
        &manager,
        SessionSpec::resume(&session_id),
        working_dir.path(),
    )?;
    drop(refreshed);

    // Distinct principals still derive DISTINCT identities — that
    // distinctness is what forces every credential change through an
    // audited epoch boundary. Under the 2026-07-25 owner ruling the
    // change no longer locks the session: a different principal rebinds,
    // and the boundary retires the previous principal's provider-side
    // anchors BEFORE the new identity publishes, so one principal can
    // never replay another's server-side state.
    let original_principal_identity = manager.resolve(&session_id)?.provider_state_identity;
    let user_b = build_managed(
        oauth_provider("shared-account", "user-b", "access-b")?,
        &manager,
        SessionSpec::resume(&session_id),
        working_dir.path(),
    )?;
    let second_user_identity = user_b
        .session_entry()
        .and_then(|entry| entry.provider_state_identity);
    assert!(second_user_identity.is_some());
    assert_ne!(
        second_user_identity, original_principal_identity,
        "distinct users in one account must derive distinct identities",
    );
    drop(user_b);

    let other_account = build_managed(
        oauth_provider("other-account", "user-a", "access-c")?,
        &manager,
        SessionSpec::resume(&session_id),
        working_dir.path(),
    )?;
    let other_account_identity = other_account
        .session_entry()
        .and_then(|entry| entry.provider_state_identity);
    assert!(other_account_identity.is_some());
    assert_ne!(
        other_account_identity, original_principal_identity,
        "distinct accounts must derive distinct identities",
    );
    assert_ne!(
        other_account_identity, second_user_identity,
        "account and user axes must both feed the identity",
    );
    Ok(())
}

#[tokio::test]
async fn managed_api_key_or_endpoint_rotation_rejects_before_wire_dispatch() -> TestResult {
    let first_authority = MockServer::start().await;
    let other_authority = MockServer::start().await;
    let working_dir = tempfile::tempdir()?;
    let session_dir = tempfile::tempdir()?;
    let manager = SessionManager::new(session_dir.path());

    let created = build_managed(
        api_key_provider("first-key", format!("{}/", first_authority.uri()))?,
        &manager,
        SessionSpec::Create { name: None },
        working_dir.path(),
    )?;
    let session_id = created
        .session_entry()
        .ok_or_else(|| io::Error::other("API-key create did not return an index entry"))?
        .id
        .clone();
    drop(created);

    let normalized_resume = build_managed(
        api_key_provider("first-key", first_authority.uri())?,
        &manager,
        SessionSpec::resume(&session_id),
        working_dir.path(),
    )?;
    drop(normalized_resume);

    // Rotating the API key or endpoint derives a DIFFERENT identity, and
    // under the 2026-07-25 owner ruling the resume rebinds through an
    // epoch boundary instead of rejecting. The property that must hold is
    // unchanged: the rebind is a LOCAL index transaction — no request
    // reaches either authority during the open, so nothing can thread the
    // old server-side state against the rotated credential.
    let original_identity = manager.resolve(&session_id)?.provider_state_identity;
    let rotated_key = build_managed(
        api_key_provider("second-key", first_authority.uri())?,
        &manager,
        SessionSpec::resume(&session_id),
        working_dir.path(),
    )?;
    let rotated_key_identity = rotated_key
        .session_entry()
        .and_then(|entry| entry.provider_state_identity);
    assert!(rotated_key_identity.is_some());
    assert_ne!(
        rotated_key_identity, original_identity,
        "an API-key rotation must derive a distinct identity",
    );
    drop(rotated_key);

    let rotated_endpoint = build_managed(
        api_key_provider("first-key", other_authority.uri())?,
        &manager,
        SessionSpec::resume(&session_id),
        working_dir.path(),
    )?;
    let rotated_endpoint_identity = rotated_endpoint
        .session_entry()
        .and_then(|entry| entry.provider_state_identity);
    assert!(rotated_endpoint_identity.is_some());
    assert_ne!(
        rotated_endpoint_identity, original_identity,
        "an endpoint rotation must derive a distinct identity",
    );
    drop(rotated_endpoint);

    assert_eq!(request_count(&first_authority).await?, 0);
    assert_eq!(request_count(&other_authority).await?, 0);
    Ok(())
}
