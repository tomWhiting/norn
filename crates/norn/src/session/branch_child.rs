//! The generation-bound child-mint transaction.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use uuid::Uuid;

use crate::session::events::{EventBase, SessionEvent};
use crate::session::persistence::index::revalidate_registered_entry;
use crate::session::persistence::types::SessionPersistError;
use crate::session::store::EventStore;
use crate::util::PrivateRoot;

use super::materialize::materialize_child;
use super::{BranchedChild, ChildBranchRequest, ChildDurability, Persistence, SessionBinding};

impl SessionBinding {
    /// Mint a child session under this agent, preserving parent-first ordering.
    pub fn branch_child(
        &self,
        parent_store: &EventStore,
        request: &ChildBranchRequest,
    ) -> Result<BranchedChild, SessionPersistError> {
        crate::session::persistence::io::ensure_session_id_path_safe(&request.child_session_id)?;
        let mut used = self.used_names.lock();
        let registered = self.revalidate_parent()?;

        let persistent = match (&self.persistence, request.durability) {
            (Persistence::Ephemeral, ChildDurability::Persist) => {
                return Err(SessionPersistError::EphemeralParent {
                    parent_path: self.path_address.clone(),
                });
            }
            (
                Persistence::Ephemeral | Persistence::Persistent { .. },
                ChildDurability::Ephemeral,
            ) => None,
            (Persistence::Persistent { brancher, .. }, ChildDurability::Persist) => Some((
                Arc::clone(brancher),
                registered.ok_or_else(|| {
                    SessionPersistError::EventStore(
                        "persistent binding lost its registered generation".to_owned(),
                    )
                })?,
            )),
        };

        let name = mint_child_name(&request.name_stem, &used);
        let path_address = format!("{}/{name}", self.path_address);
        if let Some((brancher, _)) = &persistent {
            let rel_path = brancher.child_rel_path(&path_address);
            let _permit = crate::session::persistence::acquire_private_fs()?;
            let root = PrivateRoot::create(brancher.manager.data_dir())?;
            if root.regular_file_exists(Path::new(&rel_path))? {
                return Err(SessionPersistError::ChildPathOccupied { rel_path });
            }
        }

        let anchor = parent_store.last_event_id();
        let reservation = SessionEvent::ChildBranch {
            base: EventBase::new(anchor.clone()),
            parent_session_id: self.session_id().map(str::to_owned),
            child_session_id: persistent
                .is_some()
                .then(|| request.child_session_id.clone()),
            path_address: path_address.clone(),
            parent_event_anchor: anchor.clone(),
            kind: request.kind,
        };
        parent_store.append(reservation.clone())?;
        used.insert(name);

        let Some((brancher, parent)) = persistent else {
            return Ok(BranchedChild {
                store: Arc::new(EventStore::new()),
                binding: Arc::new(Self {
                    path_address: path_address.clone(),
                    persistence: Persistence::Ephemeral,
                    used_names: Mutex::new(HashSet::new()),
                }),
                path_address,
                session_id: None,
                parent_event_anchor: anchor,
            });
        };

        let (store, binding) =
            materialize_child(&brancher, &parent, &path_address, request, &reservation)?;
        Ok(BranchedChild {
            store: Arc::new(store),
            binding: Arc::new(binding),
            path_address,
            session_id: Some(request.child_session_id.clone()),
            parent_event_anchor: anchor,
        })
    }

    fn revalidate_parent(
        &self,
    ) -> Result<Option<crate::session::SessionIndexEntry>, SessionPersistError> {
        let Persistence::Persistent {
            brancher,
            registered,
        } = &self.persistence
        else {
            return Ok(None);
        };
        revalidate_registered_entry(
            brancher.manager.data_dir(),
            registered,
            brancher.manager.index_lock_deadline(),
        )
        .map(Some)
    }
}

/// Mint `{stem}-{8 hex}` until it clears the ever-used set.
pub(super) fn mint_child_name(stem: &str, used: &HashSet<String>) -> String {
    loop {
        let hex = Uuid::new_v4().simple().to_string();
        let candidate = format!("{stem}-{}", &hex[..8]);
        if !used.contains(&candidate) {
            return candidate;
        }
    }
}
