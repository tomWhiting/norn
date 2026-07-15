//! Bounded local preparation for clearing every Norn-owned OAuth account.

use uuid::Uuid;

use super::super::auth_root::NornAuthRoot;
use super::super::options::OAuthHttpOptions;
use super::super::revoke::{PreparedLogout, prepare_local_logout};
use super::super::storage::AuthCredentialsStoreMode;
use super::login_lock::LoginSlotLock;
use super::logout::prepare_exact;
use super::{
    ACCOUNT_OPERATION_LOCK_FILE, AccountCatalogError, LOGIN_LOCK_FILE, io_layer, slot_auth_root,
    validated_timing,
};

/// One slot included in an all-account logout snapshot.
#[derive(Clone, Eq, PartialEq)]
pub struct AccountLogoutTarget {
    alias_key: String,
    storage_id: Uuid,
    auth_root: NornAuthRoot,
}

impl std::fmt::Debug for AccountLogoutTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountLogoutTarget")
            .finish_non_exhaustive()
    }
}

impl AccountLogoutTarget {
    /// Concrete credential root pinned by this target.
    #[must_use]
    pub fn auth_root(&self) -> &NornAuthRoot {
        &self.auth_root
    }
}

/// Exclusive catalog snapshot for one all-account logout.
pub struct AllAccountLogoutReservation {
    base_root: NornAuthRoot,
    targets: Vec<AccountLogoutTarget>,
    operation_lock: LoginSlotLock,
    options: OAuthHttpOptions,
}

impl std::fmt::Debug for AllAccountLogoutReservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AllAccountLogoutReservation")
            .field("target_count", &self.targets.len())
            .finish_non_exhaustive()
    }
}

impl AllAccountLogoutReservation {
    /// Stable named-slot snapshot taken while new reservations are blocked.
    #[must_use]
    pub fn targets(&self) -> &[AccountLogoutTarget] {
        &self.targets
    }

    pub(crate) fn prepare_local_logouts(
        self,
        mode: AuthCredentialsStoreMode,
    ) -> Vec<PreparedLogout> {
        self.prepare_local_logouts_with(mode, || {})
    }

    #[cfg(test)]
    pub(super) fn prepare_local_logouts_observed(
        self,
        mode: AuthCredentialsStoreMode,
        before_named_slot: impl FnMut(),
    ) -> Vec<PreparedLogout> {
        self.prepare_local_logouts_with(mode, before_named_slot)
    }

    fn prepare_local_logouts_with(
        self,
        mode: AuthCredentialsStoreMode,
        mut before_named_slot: impl FnMut(),
    ) -> Vec<PreparedLogout> {
        let Ok(timing) = validated_timing(self.options) else {
            return vec![PreparedLogout::coordination_failure()];
        };
        let mut prepared = Vec::with_capacity(self.targets.len() + 1);
        match LoginSlotLock::acquire_named(&self.base_root, timing, LOGIN_LOCK_FILE) {
            Ok(legacy_lock) => {
                prepared.push(prepare_local_logout(&self.base_root, mode, timing));
                drop(legacy_lock);
            }
            Err(error) => {
                tracing::debug!(%error, "all-account logout could not reserve the default slot");
                prepared.push(PreparedLogout::coordination_failure());
                return prepared;
            }
        }

        for target in self.targets {
            before_named_slot();
            let reservation = prepare_exact(
                &self.base_root,
                target.alias_key,
                target.storage_id,
                timing,
                None,
            );
            let Some(reservation) = (match reservation {
                Ok(reservation) => reservation,
                Err(error) => {
                    tracing::debug!(%error, "all-account logout could not reserve a named slot");
                    prepared.push(PreparedLogout::coordination_failure());
                    continue;
                }
            }) else {
                continue;
            };
            let local = prepare_local_logout(reservation.auth_root(), mode, timing);
            prepared.push(reservation.finish(local));
        }
        drop(self.operation_lock);
        prepared
    }
}

/// Block new account reservations and snapshot every current named slot.
pub fn prepare_all_account_logout(
    base_root: &NornAuthRoot,
    options: OAuthHttpOptions,
) -> Result<AllAccountLogoutReservation, AccountCatalogError> {
    let timing = validated_timing(options)?;
    let operation_lock =
        LoginSlotLock::acquire_named(base_root, timing, ACCOUNT_OPERATION_LOCK_FILE)?;
    let catalog = io_layer::load_catalog(base_root)?;
    let targets = catalog
        .records
        .iter()
        .map(|record| {
            slot_auth_root(base_root, record.storage_id).map(|auth_root| AccountLogoutTarget {
                alias_key: record.alias_key.clone(),
                storage_id: record.storage_id,
                auth_root,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AllAccountLogoutReservation {
        base_root: base_root.clone(),
        targets,
        operation_lock,
        options,
    })
}
