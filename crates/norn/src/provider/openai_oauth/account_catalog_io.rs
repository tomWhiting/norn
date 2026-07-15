//! Private durable I/O for the named-account catalog.

use std::io::{Read as _, Write as _};
use std::path::Path;

use uuid::Uuid;

use super::super::auth_root::NornAuthRoot;
use super::super::credential_lock_timing::CredentialLockTiming;
use super::super::credential_transaction::CredentialTransaction;
use super::AccountCatalog;
use crate::util::PrivateRoot;

const CATALOG_FILE: &str = "accounts.json";

/// Named-account validation, coordination, or durable-storage failure.
#[derive(Debug, thiserror::Error)]
pub enum AccountCatalogError {
    /// Alias did not match `[A-Za-z0-9][A-Za-z0-9._-]*`.
    #[error("account alias must match [A-Za-z0-9][A-Za-z0-9._-]*")]
    InvalidAlias,
    /// `default` names the compatibility slot and cannot be created.
    #[error("the account alias 'default' is reserved for the legacy credential slot")]
    ReservedAlias,
    /// A published or reserved account already uses this case-insensitive alias.
    #[error("an account with that alias already exists")]
    AliasExists,
    /// A different alias already publishes the same validated remote identity.
    #[error("that OAuth identity is already published under another account alias")]
    DuplicateIdentity,
    /// No published account uses this alias.
    #[error("no account with that alias exists")]
    AliasNotFound,
    /// The catalog schema version is not supported by this binary.
    #[error("the named-account catalog version is not supported")]
    UnsupportedVersion,
    /// Catalog fields failed structural validation.
    #[error("the named-account catalog is malformed")]
    MalformedCatalog,
    /// Lock timing supplied by the embedder was invalid.
    #[error("named-account lock timing is invalid")]
    InvalidTiming,
    /// Another process retained a required lock past the configured deadline.
    #[error("named-account coordination timed out")]
    LockTimeout,
    /// Descriptor admission, locking, or credential inspection failed.
    #[error("named-account operation could not be coordinated")]
    Coordination,
    /// The login reservation changed before publication.
    #[error("named-account login reservation changed before publication")]
    ReservationLost,
    /// Login publication was requested without a usable durable credential.
    #[error("named-account login did not produce a durable usable credential")]
    CredentialNotDurable,
    /// Private filesystem operation failed.
    #[error("named-account private storage failed: {0}")]
    Io(#[from] std::io::Error),
    /// Catalog JSON could not be decoded or encoded.
    #[error("named-account catalog JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

impl AccountCatalogError {
    pub(super) fn coordination(error: &impl std::fmt::Display) -> Self {
        tracing::debug!(%error, "named-account credential coordination failed");
        Self::Coordination
    }
}

pub(super) fn load_catalog(
    base_root: &NornAuthRoot,
) -> Result<AccountCatalog, AccountCatalogError> {
    let descriptor_permit = crate::resource::acquire_private_fs()
        .map_err(|error| AccountCatalogError::coordination(&error))?;
    let result =
        match PrivateRoot::open_read_observational(base_root.as_path(), Path::new(CATALOG_FILE)) {
            Ok(mut file) => {
                let mut raw = Vec::new();
                file.read_to_end(&mut raw)?;
                let catalog: AccountCatalog = serde_json::from_slice(&raw)?;
                catalog.validate()?;
                Ok(catalog)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(AccountCatalog::default())
            }
            Err(error) => Err(AccountCatalogError::Io(error)),
        };
    drop(descriptor_permit);
    result
}

pub(super) fn mutate_catalog<T>(
    base_root: &NornAuthRoot,
    timing: CredentialLockTiming,
    mutate: impl FnOnce(&mut AccountCatalog) -> Result<T, AccountCatalogError>,
) -> Result<T, AccountCatalogError> {
    let transaction = CredentialTransaction::acquire(base_root, timing)
        .map_err(|error| AccountCatalogError::coordination(&error))?;
    let descriptor_permit = crate::resource::acquire_private_fs()
        .map_err(|error| AccountCatalogError::coordination(&error))?;
    let root = PrivateRoot::create_with_durable_ancestors(base_root.as_path())?;
    let mut catalog = load_from_root(&root)?;
    let result = mutate(&mut catalog)?;
    catalog.validate()?;
    save_to_root(&root, &catalog)?;
    drop(root);
    drop(descriptor_permit);
    drop(transaction);
    Ok(result)
}

pub(super) fn remove_slot(
    base_root: &NornAuthRoot,
    relative: &Path,
) -> Result<(), AccountCatalogError> {
    let descriptor_permit = crate::resource::acquire_private_fs()
        .map_err(|error| AccountCatalogError::coordination(&error))?;
    let root = match PrivateRoot::open(base_root.as_path()) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    match root.remove_dir_all(relative) {
        Ok(()) => root.sync_dir(Path::new(""))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    drop(root);
    drop(descriptor_permit);
    Ok(())
}

fn load_from_root(root: &PrivateRoot) -> Result<AccountCatalog, AccountCatalogError> {
    let mut file = match root.open_read(Path::new(CATALOG_FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AccountCatalog::default());
        }
        Err(error) => return Err(error.into()),
    };
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)?;
    let catalog: AccountCatalog = serde_json::from_slice(&raw)?;
    catalog.validate()?;
    Ok(catalog)
}

fn save_to_root(root: &PrivateRoot, catalog: &AccountCatalog) -> Result<(), AccountCatalogError> {
    let mut raw = serde_json::to_vec_pretty(catalog)?;
    raw.push(b'\n');
    let temporary = format!("{CATALOG_FILE}.{}.tmp", Uuid::new_v4());
    let temporary = Path::new(&temporary);
    let result = (|| -> Result<(), AccountCatalogError> {
        let mut file = root.create_new(temporary)?;
        file.write_all(&raw)?;
        file.sync_all()?;
        drop(file);
        root.rename(temporary, Path::new(CATALOG_FILE))?;
        root.sync_dir(Path::new(""))?;
        Ok(())
    })();
    if result.is_err()
        && let Err(error) = root.remove_file(temporary)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "failed to remove named-account catalog temporary file");
    }
    result
}
