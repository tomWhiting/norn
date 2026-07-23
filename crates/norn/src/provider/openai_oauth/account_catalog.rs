//! Norn-owned named OAuth account catalog and slot selection.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::auth_root::NornAuthRoot;
use super::credential_lock_timing::CredentialLockTiming;
use super::credential_transaction::{CredentialDocument, CredentialTransaction};
use super::options::OAuthHttpOptions;
#[path = "account_identity.rs"]
mod identity;
#[path = "account_catalog_io.rs"]
mod io_layer;
#[path = "account_login_lock.rs"]
mod login_lock;
#[path = "account_logout.rs"]
mod logout;
#[path = "account_logout_all.rs"]
mod logout_all;
#[path = "account_catalog_types.rs"]
mod types;

use identity::AccountIdentityFingerprint;
pub use io_layer::AccountCatalogError;
pub use login_lock::DefaultLoginReservation;
use login_lock::{LoginSlotLock, default_login_reservation};
pub(crate) use logout::prepare_named_account_logout;
pub use logout_all::{
    AccountLogoutTarget, AllAccountLogoutReservation, prepare_all_account_logout,
};
pub use types::{AccountAlias, AccountSummary};
/// Compatibility alias for the legacy Norn-owned credential slot.
pub const DEFAULT_ACCOUNT_ALIAS: &str = "default";
const ACCOUNTS_DIRECTORY: &str = "accounts";
pub(super) const LOGIN_LOCK_FILE: &str = ".norn-login.lock";
pub(super) const ACCOUNT_OPERATION_LOCK_FILE: &str = ".norn-account-operation.lock";
const CATALOG_VERSION: u32 = 1;

/// Result of preparing a named browser login.
pub enum NamedLoginPreparation {
    /// A previously interrupted login had already durably saved credentials;
    /// its catalog publication was recovered without another authority exchange.
    Recovered,
    /// The caller must run browser login against this reserved account root.
    Pending(Box<NamedLoginReservation>),
}

/// Exclusive reservation for one named browser login.
pub struct NamedLoginReservation {
    base_root: NornAuthRoot,
    alias: AccountAlias,
    storage_id: Uuid,
    auth_root: NornAuthRoot,
    slot_lock: LoginSlotLock,
    options: OAuthHttpOptions,
}

impl std::fmt::Debug for NamedLoginReservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NamedLoginReservation")
            .finish_non_exhaustive()
    }
}

impl NamedLoginReservation {
    /// Root into which the browser flow must durably save its credential.
    #[must_use]
    pub fn auth_root(&self) -> &NornAuthRoot {
        &self.auth_root
    }

    /// Publish the reserved account after its credential is durable.
    pub fn commit(self) -> Result<(), AccountCatalogError> {
        let result = self.commit_inner();
        if let Err(error) = result {
            if let Err(cleanup_error) = self.abort() {
                tracing::warn!(%cleanup_error, "failed to retire an unpublished named credential");
            }
            return Err(error);
        }
        Ok(())
    }

    fn commit_inner(&self) -> Result<(), AccountCatalogError> {
        let timing = validated_timing(self.options)?;
        let default_lock = LoginSlotLock::acquire(&self.base_root, timing)?;
        let snapshot = CredentialTransaction::inspect(&self.auth_root)
            .map_err(|error| AccountCatalogError::coordination(&error))?;
        let CredentialDocument::Parsed(auth) = snapshot.document else {
            return Err(AccountCatalogError::CredentialNotDurable);
        };
        let identity = AccountIdentityFingerprint::from_auth(&auth)
            .ok_or(AccountCatalogError::CredentialNotDurable)?;
        if default_identity(&self.base_root)? == Some(identity) {
            return Err(AccountCatalogError::DuplicateIdentity);
        }
        io_layer::mutate_catalog(&self.base_root, timing, |catalog| {
            if catalog.records.iter().any(|record| {
                record.storage_id != self.storage_id
                    && record.state == AccountState::Ready
                    && record.identity == Some(identity)
            }) {
                return Err(AccountCatalogError::DuplicateIdentity);
            }
            let record = catalog
                .records
                .iter_mut()
                .find(|record| record.alias_key == self.alias.key())
                .ok_or(AccountCatalogError::ReservationLost)?;
            if record.storage_id != self.storage_id {
                return Err(AccountCatalogError::ReservationLost);
            }
            record.state = AccountState::Ready;
            record.identity = Some(identity);
            catalog.active = Some(self.storage_id);
            Ok(())
        })?;
        drop(default_lock);
        Ok(())
    }

    /// Remove an uncommitted reservation after an interactive login fails.
    pub fn abort(self) -> Result<(), AccountCatalogError> {
        self.abort_after_slot_scrub(|| Ok(()))
    }

    fn abort_after_slot_scrub(
        self,
        after_slot_scrub: impl FnOnce() -> Result<(), AccountCatalogError>,
    ) -> Result<(), AccountCatalogError> {
        let timing = validated_timing(self.options)?;
        let relative = slot_relative_path(self.storage_id);
        // Preserve the lock identities until the exact pending record is gone,
        // but durably remove every credential-bearing entry first.
        io_layer::scrub_slot_credentials(&self.base_root, &relative)?;
        after_slot_scrub()?;
        io_layer::mutate_catalog(&self.base_root, timing, |catalog| {
            catalog.records.retain(|record| {
                record.alias_key != self.alias.key() || record.storage_id != self.storage_id
            });
            if catalog.active == Some(self.storage_id) {
                catalog.active = None;
            }
            Ok(())
        })?;
        drop(self.slot_lock);
        io_layer::remove_slot(&self.base_root, &relative)
    }
}

/// Prepare a named account without retaining the catalog lock during browser login.
pub fn prepare_named_login(
    base_root: &NornAuthRoot,
    alias: &str,
    options: OAuthHttpOptions,
) -> Result<NamedLoginPreparation, AccountCatalogError> {
    let alias = AccountAlias::parse(alias)?;
    let timing = validated_timing(options)?;
    let operation_lock =
        LoginSlotLock::acquire_named(base_root, timing, ACCOUNT_OPERATION_LOCK_FILE)?;
    loop {
        let catalog = io_layer::load_catalog(base_root)?;
        match catalog.record(alias.key()) {
            Some(record) if record.state == AccountState::Ready => {
                return Err(AccountCatalogError::AliasExists);
            }
            Some(record) => {
                let storage_id = record.storage_id;
                let auth_root = slot_auth_root(base_root, storage_id)?;
                let slot_lock = LoginSlotLock::acquire(&auth_root, timing)?;
                let latest = io_layer::load_catalog(base_root)?;
                let Some(latest_record) = latest.record(alias.key()) else {
                    continue;
                };
                if latest_record.storage_id != storage_id {
                    continue;
                }
                if latest_record.state == AccountState::Ready {
                    return Err(AccountCatalogError::AliasExists);
                }
                let snapshot = CredentialTransaction::inspect(&auth_root)
                    .map_err(|error| AccountCatalogError::coordination(&error))?;
                if matches!(snapshot.document, CredentialDocument::Parsed(_)) {
                    let reservation = NamedLoginReservation {
                        base_root: base_root.clone(),
                        alias,
                        storage_id,
                        auth_root,
                        slot_lock,
                        options,
                    };
                    reservation.commit()?;
                    drop(operation_lock);
                    return Ok(NamedLoginPreparation::Recovered);
                }
                return Ok(NamedLoginPreparation::Pending(Box::new(
                    NamedLoginReservation {
                        base_root: base_root.clone(),
                        alias,
                        storage_id,
                        auth_root,
                        slot_lock,
                        options,
                    },
                )));
            }
            None => {
                let storage_id = Uuid::new_v4();
                let auth_root = slot_auth_root(base_root, storage_id)?;
                let slot_lock = LoginSlotLock::acquire(&auth_root, timing)?;
                let inserted = io_layer::mutate_catalog(base_root, timing, |catalog| {
                    if catalog.record(alias.key()).is_some() {
                        return Ok(false);
                    }
                    catalog
                        .records
                        .push(AccountRecord::pending(&alias, storage_id));
                    Ok(true)
                })?;
                if !inserted {
                    drop(slot_lock);
                    continue;
                }
                return Ok(NamedLoginPreparation::Pending(Box::new(
                    NamedLoginReservation {
                        base_root: base_root.clone(),
                        alias,
                        storage_id,
                        auth_root,
                        slot_lock,
                        options,
                    },
                )));
            }
        }
    }
}

/// Reserve the compatibility slot for one browser login.
pub fn prepare_default_login(
    base_root: &NornAuthRoot,
    options: OAuthHttpOptions,
) -> Result<DefaultLoginReservation, AccountCatalogError> {
    let timing = validated_timing(options)?;
    default_login_reservation(base_root, timing)
}

/// Reject a default-slot login that duplicates a published named identity.
pub fn validate_default_login_identity(
    base_root: &NornAuthRoot,
    auth: &super::types::AuthDotJson,
) -> Result<(), AccountCatalogError> {
    let identity = AccountIdentityFingerprint::from_auth(auth)
        .ok_or(AccountCatalogError::CredentialNotDurable)?;
    let catalog = io_layer::load_catalog(base_root)?;
    if catalog
        .records
        .iter()
        .any(|record| record.state == AccountState::Ready && record.identity == Some(identity))
    {
        return Err(AccountCatalogError::DuplicateIdentity);
    }
    Ok(())
}

/// List the legacy slot and every published named account.
pub fn list_accounts(base_root: &NornAuthRoot) -> Result<Vec<AccountSummary>, AccountCatalogError> {
    let catalog = io_layer::load_catalog(base_root)?;
    let mut accounts = vec![AccountSummary {
        alias: DEFAULT_ACCOUNT_ALIAS.to_owned(),
        active: catalog.active.is_none(),
        legacy_default: true,
    }];
    accounts.extend(catalog.records.iter().filter_map(|record| {
        if record.state == AccountState::Ready {
            Some(AccountSummary {
                alias: record.alias.clone(),
                active: catalog.active == Some(record.storage_id),
                legacy_default: false,
            })
        } else {
            None
        }
    }));
    accounts[1..].sort_by_key(|account| account.alias.to_ascii_lowercase());
    Ok(accounts)
}

/// Select a published account for subsequently constructed OAuth providers.
pub fn use_account(
    base_root: &NornAuthRoot,
    alias: &str,
    options: OAuthHttpOptions,
) -> Result<(), AccountCatalogError> {
    let timing = validated_timing(options)?;
    if alias.eq_ignore_ascii_case(DEFAULT_ACCOUNT_ALIAS) {
        return io_layer::mutate_catalog(base_root, timing, |catalog| {
            catalog.active = None;
            Ok(())
        });
    }
    let alias = AccountAlias::parse(alias)?;
    io_layer::mutate_catalog(base_root, timing, |catalog| {
        let record = catalog
            .records
            .iter()
            .find(|record| record.alias_key == alias.key() && record.state == AccountState::Ready)
            .ok_or(AccountCatalogError::AliasNotFound)?;
        catalog.active = Some(record.storage_id);
        Ok(())
    })
}

/// Resolve an explicit alias, or the active/default account when none is supplied.
pub fn resolve_account_root(
    base_root: &NornAuthRoot,
    alias: Option<&str>,
) -> Result<NornAuthRoot, AccountCatalogError> {
    if alias.is_some_and(|value| value.eq_ignore_ascii_case(DEFAULT_ACCOUNT_ALIAS)) {
        return Ok(base_root.clone());
    }
    let parsed_alias = alias.map(AccountAlias::parse).transpose()?;
    let catalog = io_layer::load_catalog(base_root)?;
    let storage_id = parsed_alias.as_ref().map_or(catalog.active, |alias| {
        catalog
            .ready_record(alias.key())
            .map(|record| record.storage_id)
    });
    if parsed_alias.is_some() && storage_id.is_none() {
        return Err(AccountCatalogError::AliasNotFound);
    }
    storage_id.map_or_else(|| Ok(base_root.clone()), |id| slot_auth_root(base_root, id))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AccountState {
    Pending,
    Ready,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountRecord {
    pub(super) alias: String,
    pub(super) alias_key: String,
    pub(super) storage_id: Uuid,
    pub(super) state: AccountState,
    pub(super) identity: Option<AccountIdentityFingerprint>,
}

impl AccountRecord {
    fn pending(alias: &AccountAlias, storage_id: Uuid) -> Self {
        Self {
            alias: alias.as_str().to_owned(),
            alias_key: alias.key().to_owned(),
            storage_id,
            state: AccountState::Pending,
            identity: None,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountCatalog {
    pub(super) version: u32,
    pub(super) active: Option<Uuid>,
    pub(super) records: Vec<AccountRecord>,
}

impl Default for AccountCatalog {
    fn default() -> Self {
        Self {
            version: CATALOG_VERSION,
            active: None,
            records: Vec::new(),
        }
    }
}

impl AccountCatalog {
    pub(super) fn validate(&self) -> Result<(), AccountCatalogError> {
        if self.version != CATALOG_VERSION {
            return Err(AccountCatalogError::UnsupportedVersion);
        }
        let mut aliases = HashSet::new();
        let mut ids = HashSet::new();
        let mut identities = HashSet::new();
        for record in &self.records {
            let alias = AccountAlias::parse(&record.alias)?;
            if alias.key() != record.alias_key
                || record.storage_id.is_nil()
                || !aliases.insert(record.alias_key.clone())
                || !ids.insert(record.storage_id)
                || (record.state == AccountState::Pending && record.identity.is_some())
                || (record.state == AccountState::Ready
                    && !record
                        .identity
                        .is_some_and(|identity| identities.insert(identity)))
            {
                return Err(AccountCatalogError::MalformedCatalog);
            }
        }
        if let Some(active) = self.active
            && !self
                .records
                .iter()
                .any(|record| record.storage_id == active && record.state == AccountState::Ready)
        {
            return Err(AccountCatalogError::MalformedCatalog);
        }
        Ok(())
    }

    fn record(&self, key: &str) -> Option<&AccountRecord> {
        self.records.iter().find(|record| record.alias_key == key)
    }

    pub(super) fn ready_record(&self, key: &str) -> Option<&AccountRecord> {
        self.record(key)
            .filter(|record| record.state == AccountState::Ready)
    }
}

fn validated_timing(
    options: OAuthHttpOptions,
) -> Result<CredentialLockTiming, AccountCatalogError> {
    options.credential_lock_timing().map_err(|error| {
        tracing::debug!(%error, "named-account lock timing was rejected");
        AccountCatalogError::InvalidTiming
    })
}

fn default_identity(
    base_root: &NornAuthRoot,
) -> Result<Option<AccountIdentityFingerprint>, AccountCatalogError> {
    let snapshot = CredentialTransaction::inspect(base_root)
        .map_err(|error| AccountCatalogError::coordination(&error))?;
    match snapshot.document {
        CredentialDocument::Parsed(auth) => Ok(AccountIdentityFingerprint::from_auth(&auth)),
        CredentialDocument::Missing => Ok(None),
        CredentialDocument::Malformed(_) => Err(AccountCatalogError::CredentialNotDurable),
    }
}

pub(super) fn slot_relative_path(storage_id: Uuid) -> PathBuf {
    Path::new(ACCOUNTS_DIRECTORY).join(storage_id.hyphenated().to_string())
}

pub(super) fn slot_auth_root(
    base_root: &NornAuthRoot,
    storage_id: Uuid,
) -> Result<NornAuthRoot, AccountCatalogError> {
    NornAuthRoot::try_from(base_root.as_path().join(slot_relative_path(storage_id))).map_err(
        |error| {
            tracing::debug!(%error, "named-account storage id produced an invalid root");
            AccountCatalogError::MalformedCatalog
        },
    )
}

#[cfg(test)]
#[path = "account_catalog_tests.rs"]
mod tests;
