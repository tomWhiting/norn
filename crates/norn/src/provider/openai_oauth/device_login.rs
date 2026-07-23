//! Headless OAuth device-code authorization.

use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::auth_root::NornAuthRoot;
use super::code_exchange::exchange_code_async;
use super::endpoints::{
    DEVICE_REDIRECT_URI, DEVICE_TOKEN_URL, DEVICE_USER_CODE_URL, DEVICE_VERIFICATION_URL, TOKEN_URL,
};
use super::login_commit::{inspect_login_revision_async, persist_prepared_login};
use super::login_prompt::{LoginPrompt, LoginPromptPresenter};
use super::login_server::{LoginError, LoginStorageFailureKind};
use super::options::OAuthHttpOptions;
use super::pkce::challenge_for;
use super::storage::AuthCredentialsStoreMode;
use super::types::AuthDotJson;

#[derive(Clone)]
pub(crate) struct DeviceLoginOptions {
    auth_root: NornAuthRoot,
    client_id: String,
    mode: AuthCredentialsStoreMode,
    http: OAuthHttpOptions,
    presenter: Arc<dyn LoginPromptPresenter>,
    endpoints: DeviceEndpoints,
}

impl std::fmt::Debug for DeviceLoginOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceLoginOptions")
            .field("auth_root", &self.auth_root)
            .field("client_id", &"[REDACTED]")
            .field("mode", &self.mode)
            .field("http", &self.http)
            .field("presenter", &"[REDACTED]")
            .field("endpoints", &"[COMPILED AUTHORITY]")
            .finish()
    }
}

impl DeviceLoginOptions {
    pub(crate) fn new(
        auth_root: NornAuthRoot,
        client_id: String,
        mode: AuthCredentialsStoreMode,
        http: OAuthHttpOptions,
        presenter: Arc<dyn LoginPromptPresenter>,
    ) -> Self {
        Self {
            auth_root,
            client_id,
            mode,
            http,
            presenter,
            endpoints: DeviceEndpoints::production(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_authority(mut self, base_url: &str) -> Self {
        self.endpoints = DeviceEndpoints::test_authority(base_url);
        self
    }
}

#[derive(Clone)]
struct DeviceEndpoints {
    user_code: String,
    poll: String,
    verification: String,
    token: String,
    redirect_uri: String,
}

impl DeviceEndpoints {
    fn production() -> Self {
        Self {
            user_code: DEVICE_USER_CODE_URL.to_owned(),
            poll: DEVICE_TOKEN_URL.to_owned(),
            verification: DEVICE_VERIFICATION_URL.to_owned(),
            token: TOKEN_URL.to_owned(),
            redirect_uri: DEVICE_REDIRECT_URI.to_owned(),
        }
    }

    #[cfg(test)]
    fn test_authority(base_url: &str) -> Self {
        let base_url = base_url.trim_end_matches('/');
        Self {
            user_code: format!("{base_url}/api/accounts/deviceauth/usercode"),
            poll: format!("{base_url}/api/accounts/deviceauth/token"),
            verification: format!("{base_url}/codex/device"),
            token: format!("{base_url}/oauth/token"),
            redirect_uri: format!("{base_url}/deviceauth/callback"),
        }
    }
}

impl std::fmt::Debug for DeviceEndpoints {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DeviceEndpoints([REDACTED])")
    }
}

#[derive(Serialize)]
struct UserCodeRequest<'a> {
    client_id: &'a str,
}

#[derive(Deserialize)]
struct UserCodeResponse {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(deserialize_with = "deserialize_interval")]
    interval: Duration,
}

#[derive(Serialize)]
struct TokenPollRequest<'a> {
    device_auth_id: &'a str,
    user_code: &'a str,
}

#[derive(Deserialize)]
struct TokenPollResponse {
    authorization_code: String,
    code_challenge: String,
    code_verifier: String,
}

enum TokenPollEnvelope {
    Success(TokenPollResponse),
    Error,
}

impl<'de> Deserialize<'de> for TokenPollEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let object = raw
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("device poll response must be an object"))?;
        let has_error = object.contains_key("error");
        let has_success = ["authorization_code", "code_challenge", "code_verifier"]
            .iter()
            .any(|field| object.contains_key(*field));
        if has_error && has_success {
            return Err(serde::de::Error::custom(
                "device poll response mixed success and error fields",
            ));
        }
        if has_error {
            return Ok(Self::Error);
        }
        serde_json::from_value(raw)
            .map(Self::Success)
            .map_err(serde::de::Error::custom)
    }
}

struct PendingDeviceCode {
    device_auth_id: String,
    user_code: String,
    interval: Duration,
}

impl std::fmt::Debug for PendingDeviceCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingDeviceCode")
            .field("device_auth_id", &"[REDACTED]")
            .field("user_code", &"[REDACTED]")
            .field("interval", &self.interval)
            .finish()
    }
}

impl std::fmt::Debug for TokenPollResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenPollResponse")
            .field("authorization_code", &"[REDACTED]")
            .field("code_challenge", &"[REDACTED]")
            .field("code_verifier", &"[REDACTED]")
            .finish()
    }
}

pub(crate) async fn run_device_login_with_hooks<V, F>(
    opts: DeviceLoginOptions,
    validate: V,
    commit: F,
) -> Result<(), LoginError>
where
    V: FnOnce(&AuthDotJson) -> Result<(), LoginError> + Send + 'static,
    F: FnOnce() -> Result<(), LoginError> + Send + 'static,
{
    if opts.http.device_code_timeout.is_zero() {
        return Err(LoginError::DeviceCodeConfiguration);
    }
    let timing = opts
        .http
        .credential_lock_timing()
        .map_err(|error| LoginError::Storage {
            kind: LoginStorageFailureKind::Coordination,
            reason: error.to_string(),
        })?;
    let expected_revision = inspect_login_revision_async(opts.auth_root.clone()).await?;
    // The advertised device-code lifetime covers the complete authority
    // exchange. Local credential inspection happens before that clock starts.
    let deadline = DeviceDeadline::start(opts.http.device_code_timeout);
    let client = crate::provider::http_client::build_bounded_client(opts.http.request_timeout)
        .map_err(|_error| LoginError::DeviceCodeTransport)?;
    let pending = deadline.run(request_user_code(&client, &opts)).await?;
    opts.presenter
        .present(LoginPrompt::DeviceCode {
            verification_url: &opts.endpoints.verification,
            user_code: &pending.user_code,
            expires_after: opts.http.device_code_timeout,
        })
        .map_err(|_error| LoginError::Presentation)?;
    let code = poll_for_authorization(&client, &opts, &pending, &deadline).await?;
    validate_authorization_response(&code)?;
    let auth = deadline
        .run(exchange_code_async(
            &client,
            &opts.endpoints.token,
            &opts.client_id,
            &opts.endpoints.redirect_uri,
            &code.code_verifier,
            &code.authorization_code,
        ))
        .await?;
    persist_prepared_login(
        opts.auth_root,
        expected_revision,
        opts.mode,
        timing,
        auth,
        validate,
        commit,
    )
    .await
}

async fn request_user_code(
    client: &reqwest::Client,
    opts: &DeviceLoginOptions,
) -> Result<PendingDeviceCode, LoginError> {
    let _permit = acquire_http_permit()?;
    let response = client
        .post(&opts.endpoints.user_code)
        .json(&UserCodeRequest {
            client_id: &opts.client_id,
        })
        .send()
        .await
        .map_err(|_error| LoginError::DeviceCodeTransport)?;
    if response.status() == StatusCode::NOT_FOUND {
        return Err(LoginError::DeviceCodeUnsupported);
    }
    if !response.status().is_success() {
        return Err(authority_status(
            "requesting a user code",
            response.status(),
        ));
    }
    let response = response
        .json::<UserCodeResponse>()
        .await
        .map_err(|_error| LoginError::DeviceCodeMalformed { stage: "user-code" })?;
    if !terminal_safe_code(&response.user_code)
        || !valid_opaque_value(&response.device_auth_id)
        || response.interval.is_zero()
    {
        return Err(LoginError::DeviceCodeMalformed { stage: "user-code" });
    }
    Ok(PendingDeviceCode {
        device_auth_id: response.device_auth_id,
        user_code: response.user_code,
        interval: response.interval,
    })
}

async fn poll_for_authorization(
    client: &reqwest::Client,
    opts: &DeviceLoginOptions,
    pending: &PendingDeviceCode,
    deadline: &DeviceDeadline,
) -> Result<TokenPollResponse, LoginError> {
    loop {
        let response = deadline.run(poll_once(client, opts, pending)).await?;
        if let Some(code) = response {
            return Ok(code);
        }
        deadline
            .run(async {
                tokio::time::sleep(pending.interval).await;
                Ok(())
            })
            .await?;
    }
}

struct DeviceDeadline {
    started: tokio::time::Instant,
    total: Duration,
}

impl DeviceDeadline {
    fn start(total: Duration) -> Self {
        Self {
            started: tokio::time::Instant::now(),
            total,
        }
    }

    async fn run<F, T>(&self, future: F) -> Result<T, LoginError>
    where
        F: std::future::Future<Output = Result<T, LoginError>>,
    {
        let remaining = self.total.saturating_sub(self.started.elapsed());
        if remaining.is_zero() {
            return Err(LoginError::DeviceCodeExpired);
        }
        tokio::time::timeout(remaining, future)
            .await
            .map_err(|_elapsed| LoginError::DeviceCodeExpired)?
    }
}

async fn poll_once(
    client: &reqwest::Client,
    opts: &DeviceLoginOptions,
    pending: &PendingDeviceCode,
) -> Result<Option<TokenPollResponse>, LoginError> {
    let _permit = acquire_http_permit()?;
    let response = client
        .post(&opts.endpoints.poll)
        .json(&TokenPollRequest {
            device_auth_id: &pending.device_auth_id,
            user_code: &pending.user_code,
        })
        .send()
        .await
        .map_err(|_error| LoginError::DeviceCodeTransport)?;
    match response.status() {
        StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => Ok(None),
        status if status.is_success() => {
            let envelope = response
                .json::<TokenPollEnvelope>()
                .await
                .map_err(|_error| LoginError::DeviceCodeMalformed { stage: "poll" })?;
            match envelope {
                TokenPollEnvelope::Success(response) => Ok(Some(response)),
                TokenPollEnvelope::Error => Err(LoginError::DeviceCodeMalformed { stage: "poll" }),
            }
        }
        status => Err(authority_status("polling for authorization", status)),
    }
}

fn validate_authorization_response(response: &TokenPollResponse) -> Result<(), LoginError> {
    if !valid_opaque_value(&response.authorization_code)
        || !valid_opaque_value(&response.code_verifier)
        || !valid_opaque_value(&response.code_challenge)
        || challenge_for(&response.code_verifier) != response.code_challenge
    {
        return Err(LoginError::DeviceCodeMalformed { stage: "poll" });
    }
    Ok(())
}

fn acquire_http_permit() -> Result<crate::resource::DescriptorPermit, LoginError> {
    let governor = crate::resource::DescriptorGovernor::global()
        .map_err(|error| LoginError::DescriptorAdmission(Box::new(error)))?;
    governor
        .try_acquire(crate::resource::HTTP_REQUEST_PEAK)
        .map_err(|error| LoginError::DescriptorAdmission(Box::new(error)))
}

fn authority_status(stage: &'static str, status: StatusCode) -> LoginError {
    LoginError::DeviceCodeAuthority {
        stage,
        status: status.as_u16(),
    }
}

fn terminal_safe_code(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_graphic())
}

fn valid_opaque_value(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_control)
}

fn deserialize_interval<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    raw.trim()
        .parse::<u64>()
        .map(Duration::from_secs)
        .map_err(serde::de::Error::custom)
}

#[cfg(test)]
#[path = "device_login_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "device_login_failure_tests.rs"]
mod failure_tests;

#[cfg(test)]
#[path = "device_login_validation_tests.rs"]
mod validation_tests;
