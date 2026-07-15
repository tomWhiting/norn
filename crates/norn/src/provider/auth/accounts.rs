//! Public named-account operations at the provider boundary.

use std::path::PathBuf;

use super::super::openai_oauth::{
    AccountCatalogError, AccountSummary, AuthCredentialsStoreMode, CLIENT_ID, LoginError,
    LoginStorageFailureKind, LogoutReport, NamedLoginPreparation, NornAuthRoot, NornAuthRootError,
    OAuthHttpOptions, ServerOptions, complete_prepared_logout,
    list_accounts as list_account_catalog, prepare_all_account_logout, prepare_default_login,
    prepare_local_logout, prepare_named_account_logout, prepare_named_login, resolve_account_root,
    resolve_norn_auth_root, run_login_server, use_account, validate_default_login_identity,
};
use super::{LoginConfig, command_norn_auth_root, map_login_error};
use crate::error::{ConfigError, NornError, ProviderError};

/// Run browser login for one Norn-owned named account.
pub async fn login_named(config: LoginConfig, alias: &str) -> Result<(), NornError> {
    login_account(config, Some(alias)).await
}

pub(super) async fn login_account(
    config: LoginConfig,
    alias: Option<&str>,
) -> Result<(), NornError> {
    if config.device_code {
        return Err(NornError::Config(ConfigError::InvalidConfig {
            reason: "device code login is not yet supported; use the browser PKCE flow".to_owned(),
        }));
    }
    let base_root = command_norn_auth_root(config.auth_root)?;
    let http = OAuthHttpOptions::default();
    match alias {
        Some(alias) => login_named_at(&base_root, alias, http).await,
        None => login_default_at(&base_root, http).await,
    }
}

async fn login_default_at(
    base_root: &NornAuthRoot,
    http: OAuthHttpOptions,
) -> Result<(), NornError> {
    let _reservation =
        prepare_default_login(base_root, http).map_err(|error| catalog_error(&error))?;
    let server =
        run_login_server(server_options(base_root.clone(), http)).map_err(map_login_error)?;
    let validation_root = base_root.clone();
    server
        .block_until_done_with_hooks(
            move |auth| {
                validate_default_login_identity(&validation_root, auth)
                    .map_err(|error| catalog_login_error(&error))
            },
            || Ok(()),
        )
        .await
        .map_err(map_login_error)
}

async fn login_named_at(
    base_root: &NornAuthRoot,
    alias: &str,
    http: OAuthHttpOptions,
) -> Result<(), NornError> {
    let prepared =
        prepare_named_login(base_root, alias, http).map_err(|error| catalog_error(&error))?;
    let NamedLoginPreparation::Pending(pending) = prepared else {
        return Ok(());
    };
    let server = match run_login_server(server_options(pending.auth_root().clone(), http)) {
        Ok(server) => server,
        Err(error) => {
            abort_named_login(pending);
            return Err(map_login_error(error));
        }
    };
    let mut pending = Some(pending);
    let result = server
        .block_until_done_with_commit(|| {
            pending
                .take()
                .ok_or_else(named_commit_unavailable)?
                .commit()
                .map_err(|error| catalog_login_error(&error))
        })
        .await;
    if let Some(pending) = pending {
        abort_named_login(pending);
    }
    result.map_err(map_login_error)
}

fn server_options(auth_root: NornAuthRoot, http: OAuthHttpOptions) -> ServerOptions {
    ServerOptions::new(
        auth_root,
        CLIENT_ID.to_owned(),
        AuthCredentialsStoreMode::File,
        http,
    )
}

fn catalog_login_error(error: &AccountCatalogError) -> LoginError {
    tracing::debug!(%error, "named-account login finalization failed");
    LoginError::Storage {
        kind: LoginStorageFailureKind::Coordination,
        reason: "named-account publication could not be completed".to_owned(),
    }
}

fn named_commit_unavailable() -> LoginError {
    LoginError::Storage {
        kind: LoginStorageFailureKind::Coordination,
        reason: "named-account publication state was unavailable".to_owned(),
    }
}

fn abort_named_login(pending: Box<super::super::openai_oauth::NamedLoginReservation>) {
    if let Err(error) = pending.abort() {
        tracing::warn!(%error, "failed to retire named OAuth login reservation");
    }
}

/// List the compatibility slot and all published named accounts.
pub fn list_auth_accounts() -> Result<Vec<AccountSummary>, NornError> {
    let base_root = command_norn_auth_root(None)?;
    list_account_catalog(&base_root).map_err(|error| catalog_error(&error))
}

/// Select an account for subsequently constructed OAuth providers.
pub fn use_auth_account(alias: &str) -> Result<(), NornError> {
    let base_root = command_norn_auth_root(None)?;
    use_account(&base_root, alias, OAuthHttpOptions::default())
        .map_err(|error| catalog_error(&error))
}

/// Durably clear the compatibility slot plus every ready or pending named slot.
pub async fn logout_all_auth_accounts() -> Result<Vec<LogoutReport>, NornError> {
    let base_root = command_norn_auth_root(None)?;
    let options = OAuthHttpOptions::default();
    let reservation =
        prepare_all_account_logout(&base_root, options).map_err(|error| catalog_error(&error))?;
    let prepared = tokio::task::spawn_blocking(move || {
        reservation.prepare_local_logouts(AuthCredentialsStoreMode::File)
    })
    .await
    .map_err(|error| {
        NornError::Config(ConfigError::InvalidConfig {
            reason: format!("all-account local logout task failed: {error}"),
        })
    })?;
    let mut reports = Vec::with_capacity(prepared.len());
    for logout in prepared {
        reports.push(complete_prepared_logout(logout, options).await);
    }
    Ok(reports)
}

/// Durably retire one exact named account generation before remote revocation.
pub async fn logout_named(config: LoginConfig, alias: &str) -> Result<LogoutReport, NornError> {
    if alias.eq_ignore_ascii_case(super::super::openai_oauth::DEFAULT_ACCOUNT_ALIAS) {
        return super::logout(config).await;
    }
    let base_root = command_norn_auth_root(config.auth_root)?;
    let options = OAuthHttpOptions::default();
    let timing = options.credential_lock_timing().map_err(|error| {
        NornError::Config(ConfigError::InvalidConfig {
            reason: error.to_string(),
        })
    })?;
    let alias = alias.to_owned();
    let prepared = tokio::task::spawn_blocking(move || {
        let reservation = prepare_named_account_logout(&base_root, &alias, options)?;
        let local = prepare_local_logout(
            reservation.auth_root(),
            AuthCredentialsStoreMode::File,
            timing,
        );
        Ok::<_, AccountCatalogError>(reservation.finish(local))
    })
    .await
    .map_err(|error| {
        NornError::Config(ConfigError::InvalidConfig {
            reason: format!("named-account local logout task failed: {error}"),
        })
    })?
    .map_err(|error| catalog_error(&error))?;
    Ok(complete_prepared_logout(prepared, options).await)
}

/// Resolve a CLI account name against the trusted Norn auth root.
pub fn command_account_root(alias: Option<&str>) -> Result<NornAuthRoot, NornError> {
    let base_root = command_norn_auth_root(None)?;
    match alias {
        Some(alias) => {
            resolve_account_root(&base_root, Some(alias)).map_err(|error| catalog_error(&error))
        }
        None => Ok(base_root),
    }
}

/// Resolve and pin the account root used by a newly constructed provider.
pub fn provider_account_root(alias: Option<&str>) -> Result<PathBuf, ProviderError> {
    let base_root = resolve_norn_auth_root(None).map_err(norn_auth_root_error)?;
    resolve_account_root(&base_root, alias)
        .map(NornAuthRoot::into_path_buf)
        .map_err(|error| ProviderError::AuthenticationFailed {
            reason: error.to_string(),
        })
}

pub(super) fn provider_root_from_override(
    override_path: Option<PathBuf>,
) -> Result<NornAuthRoot, ProviderError> {
    let has_override = override_path.is_some();
    let base_root = resolve_norn_auth_root(override_path).map_err(norn_auth_root_error)?;
    if has_override {
        return Ok(base_root);
    }
    resolve_account_root(&base_root, None).map_err(|error| ProviderError::AuthenticationFailed {
        reason: error.to_string(),
    })
}

fn norn_auth_root_error(error: NornAuthRootError) -> ProviderError {
    ProviderError::AuthenticationFailed {
        reason: error.to_string(),
    }
}

pub(super) fn catalog_error(error: &AccountCatalogError) -> NornError {
    NornError::Config(ConfigError::InvalidConfig {
        reason: error.to_string(),
    })
}
