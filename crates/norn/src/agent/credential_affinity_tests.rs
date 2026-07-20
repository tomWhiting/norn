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

fn managed_state(manager: &SessionManager) -> TestResult<Vec<u8>> {
    let mut snapshot = Vec::new();
    for entry in manager.list()? {
        let durable = crate::session::read_session_events_for_entry(manager.data_dir(), &entry)?;
        snapshot.push(serde_json::json!({
            "entry": entry,
            "events": durable.events,
        }));
    }
    Ok(serde_json::to_vec(&snapshot)?)
}

fn assert_identity_mismatch(
    result: Result<crate::agent::Agent, NornError>,
    operation: &str,
) -> TestResult {
    match result {
        Err(NornError::Provider(ProviderError::ProviderStateIdentityMismatch)) => Ok(()),
        Err(other) => {
            Err(io::Error::other(format!("{operation} returned the wrong error: {other}")).into())
        }
        Ok(_) => Err(io::Error::other(format!(
            "{operation} accepted a different provider-state identity"
        ))
        .into()),
    }
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

    let before = serde_json::to_vec(&manager.list()?)?;
    let mismatch = build_managed(
        provider(other_identity),
        &manager,
        SessionSpec::resume(&entry.id),
        working_dir.path(),
    );
    match mismatch {
        Err(NornError::Provider(ProviderError::ProviderStateIdentityMismatch)) => {}
        Err(other) => {
            return Err(io::Error::other(format!(
                "expected typed provider-state mismatch, got {other}"
            ))
            .into());
        }
        Ok(_) => {
            return Err(io::Error::other("mismatched managed resume returned an agent").into());
        }
    }
    assert_eq!(
        serde_json::to_vec(&manager.list()?)?,
        before,
        "a mismatched managed resume must not mutate the session index",
    );

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

    let before_resume_mismatch = managed_state(&manager)?;
    assert_identity_mismatch(
        build_managed(
            provider(different),
            &manager,
            SessionSpec::resume_latest(working_dir.display().to_string()),
            &working_dir,
        ),
        "latest resume",
    )?;
    assert_eq!(managed_state(&manager)?, before_resume_mismatch);

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

    let before_fork_mismatch = managed_state(&manager)?;
    assert_identity_mismatch(
        build_managed(
            provider(different),
            &manager,
            SessionSpec::fork_latest(working_dir.display().to_string()),
            &working_dir,
        ),
        "latest fork",
    )?;
    assert_eq!(
        managed_state(&manager)?,
        before_fork_mismatch,
        "a rejected latest fork must not publish a child or mutate its source",
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

    let before = managed_state(&manager)?;
    assert_identity_mismatch(
        build_managed(
            oauth_provider("shared-account", "user-b", "access-b")?,
            &manager,
            SessionSpec::resume(&session_id),
            working_dir.path(),
        ),
        "same-account different-user OAuth resume",
    )?;
    assert_eq!(
        managed_state(&manager)?,
        before,
        "a different OAuth principal must not mutate the bound timeline",
    );

    assert_identity_mismatch(
        build_managed(
            oauth_provider("other-account", "user-a", "access-c")?,
            &manager,
            SessionSpec::resume(&session_id),
            working_dir.path(),
        ),
        "different-account same-user OAuth resume",
    )?;
    assert_eq!(
        managed_state(&manager)?,
        before,
        "a different OAuth account must not mutate the bound timeline",
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

    let before = managed_state(&manager)?;
    assert_identity_mismatch(
        build_managed(
            api_key_provider("second-key", first_authority.uri())?,
            &manager,
            SessionSpec::resume(&session_id),
            working_dir.path(),
        ),
        "API-key rotation",
    )?;
    assert_eq!(managed_state(&manager)?, before);

    assert_identity_mismatch(
        build_managed(
            api_key_provider("first-key", other_authority.uri())?,
            &manager,
            SessionSpec::resume(&session_id),
            working_dir.path(),
        ),
        "normalized endpoint rotation",
    )?;
    assert_eq!(managed_state(&manager)?, before);
    assert_eq!(request_count(&first_authority).await?, 0);
    assert_eq!(request_count(&other_authority).await?, 0);
    Ok(())
}
