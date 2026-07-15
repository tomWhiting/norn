//! Exact-generation named-account logout coordination.

use uuid::Uuid;

use super::super::auth_root::NornAuthRoot;
use super::super::credential_lock_timing::CredentialLockTiming;
use super::super::options::OAuthHttpOptions;
use super::super::revoke::PreparedLogout;
use super::login_lock::LoginSlotLock;
use super::{
    ACCOUNT_OPERATION_LOCK_FILE, AccountAlias, AccountCatalogError, AccountState, io_layer,
    slot_auth_root, slot_relative_path, validated_timing,
};

pub(crate) struct NamedAccountLogoutReservation {
    base_root: NornAuthRoot,
    alias_key: String,
    storage_id: Uuid,
    auth_root: NornAuthRoot,
    slot_lock: LoginSlotLock,
    operation_lock: Option<LoginSlotLock>,
    timing: CredentialLockTiming,
}

impl NamedAccountLogoutReservation {
    pub(crate) fn auth_root(&self) -> &NornAuthRoot {
        &self.auth_root
    }

    pub(crate) fn finish(self, mut prepared: PreparedLogout) -> PreparedLogout {
        if !prepared.local_succeeded() {
            return prepared;
        }
        let retired = io_layer::mutate_catalog(&self.base_root, self.timing, |catalog| {
            let index = catalog
                .records
                .iter()
                .position(|record| {
                    record.alias_key == self.alias_key
                        && record.storage_id == self.storage_id
                        && matches!(record.state, AccountState::Ready | AccountState::Pending)
                })
                .ok_or(AccountCatalogError::ReservationLost)?;
            catalog.records.remove(index);
            if catalog.active == Some(self.storage_id) {
                catalog.active = None;
            }
            Ok(())
        });
        if retired.is_err() {
            prepared.catalog_retirement_failed();
            return prepared;
        }

        let relative = slot_relative_path(self.storage_id);
        drop(self.slot_lock);
        drop(self.operation_lock);
        if let Err(error) = io_layer::remove_slot(&self.base_root, &relative) {
            tracing::warn!(%error, "retired named-account slot could not be removed");
            prepared.slot_cleanup_failed();
        }
        prepared
    }
}

pub(crate) fn prepare_named_account_logout(
    base_root: &NornAuthRoot,
    alias: &str,
    options: OAuthHttpOptions,
) -> Result<NamedAccountLogoutReservation, AccountCatalogError> {
    let alias = AccountAlias::parse(alias)?;
    let timing = validated_timing(options)?;
    let operation_lock =
        LoginSlotLock::acquire_named(base_root, timing, ACCOUNT_OPERATION_LOCK_FILE)?;
    let catalog = io_layer::load_catalog(base_root)?;
    let record = catalog
        .ready_record(alias.key())
        .ok_or(AccountCatalogError::AliasNotFound)?;
    prepare_exact(
        base_root,
        record.alias_key.clone(),
        record.storage_id,
        timing,
        Some(operation_lock),
    )?
    .ok_or(AccountCatalogError::ReservationLost)
}

pub(super) fn prepare_exact(
    base_root: &NornAuthRoot,
    alias_key: String,
    storage_id: Uuid,
    timing: CredentialLockTiming,
    operation_lock: Option<LoginSlotLock>,
) -> Result<Option<NamedAccountLogoutReservation>, AccountCatalogError> {
    let auth_root = slot_auth_root(base_root, storage_id)?;
    let slot_lock = LoginSlotLock::acquire(&auth_root, timing)?;
    let latest = io_layer::load_catalog(base_root)?;
    let still_exact = latest
        .records
        .iter()
        .any(|record| record.alias_key == alias_key && record.storage_id == storage_id);
    if !still_exact {
        drop(slot_lock);
        io_layer::remove_slot(base_root, &slot_relative_path(storage_id))?;
        return Ok(None);
    }
    Ok(Some(NamedAccountLogoutReservation {
        base_root: base_root.clone(),
        alias_key,
        storage_id,
        auth_root,
        slot_lock,
        operation_lock,
        timing,
    }))
}
