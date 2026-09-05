//! Publication-owned inherited spool permissions; appendable timeline data grants no authority.

use std::collections::{BTreeMap, HashSet};
use std::io::BufReader;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{SpoolWriter, registered_root_session_id, validate_spool_ref};
use crate::session::branch::ROOT_PATH_ADDRESS;
use crate::session::events::{ChildBranchKind, EventId, SessionEvent};
use crate::session::persistence::index::with_registered_spool_entries;
use crate::session::persistence::{SessionIndexEntry, SessionPersistError};
use crate::util::PrivateRoot;

const INHERITANCE_FILE: &str = "spool-inheritance.json";
const INHERITANCE_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    id: String,
    generation: Uuid,
    rel_path: Option<String>,
}

impl Owner {
    fn from_entry(entry: &SessionIndexEntry) -> Self {
        Self {
            id: entry.id.clone(),
            generation: entry.generation,
            rel_path: entry.rel_path.clone(),
        }
    }

    fn matches(&self, entry: &SessionIndexEntry) -> bool {
        self.id == entry.id
            && self.generation == entry.generation
            && self.rel_path == entry.rel_path
    }

    fn root(&self) -> &str {
        self.rel_path
            .as_deref()
            .and_then(|path| path.split('/').next())
            .unwrap_or(&self.id)
    }

    fn validate_current(&self, entries: &[SessionIndexEntry]) -> Result<(), SessionPersistError> {
        if entries.iter().any(|entry| self.matches(entry)) {
            Ok(())
        } else {
            Err(SessionPersistError::GenerationChanged {
                id: self.id.clone(),
            })
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    event_id: EventId,
    reference: String,
    owner: Owner,
}

/// Closed sidecar and journal metadata, created only by the fork publication owner.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpoolInheritance {
    version: u32,
    destination: Owner,
    source: Owner,
    branch_event_id: EventId,
    parent_event_anchor: EventId,
    grants: Vec<Grant>,
}

pub(crate) fn inheritance_path(root_session_id: &str) -> PathBuf {
    PathBuf::from(root_session_id).join(INHERITANCE_FILE)
}

impl SpoolInheritance {
    /// Prepare exact inherited permissions while the publication owner holds the index lock.
    pub(crate) fn prepare(
        root: &PrivateRoot,
        entries: &[SessionIndexEntry],
        source: &SessionIndexEntry,
        destination: &SessionIndexEntry,
        events: &[SessionEvent],
    ) -> Result<Option<Self>, SessionPersistError> {
        if !events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::ToolResult {
                    spool_ref: Some(_),
                    ..
                }
            )
        }) {
            return Ok(None);
        }
        let Some(branch) = events.last() else {
            return Err(invalid(&destination.id, "fork has no ChildBranch"));
        };
        let SessionEvent::ChildBranch {
            base,
            parent_session_id: Some(parent),
            child_session_id: Some(child),
            path_address,
            parent_event_anchor: Some(anchor),
            kind: ChildBranchKind::Fork,
        } = branch
        else {
            return Err(invalid(
                &destination.id,
                "fork does not end with its ChildBranch",
            ));
        };
        if parent != &source.id
            || child != &destination.id
            || path_address != ROOT_PATH_ADDRESS
            || base.parent_id.as_ref() != Some(anchor)
        {
            return Err(invalid(
                &destination.id,
                "fork ChildBranch ownership or anchor differs",
            ));
        }
        let inherited = Self::read(root, entries, source)?;
        if let Some(manifest) = &inherited {
            manifest.validate_history(source, &events[..events.len() - 1])?;
        }
        let mut grants = BTreeMap::new();
        for event in &events[..events.len() - 1] {
            let SessionEvent::ToolResult {
                base,
                spool_ref: Some(reference),
                ..
            } = event
            else {
                continue;
            };
            validate_spool_ref(reference)?;
            let owned = format!(
                "{}/spool/{}.bin",
                registered_root_session_id(source),
                base.id
            );
            let grant = if reference == &owned {
                Some(Grant {
                    event_id: base.id.clone(),
                    reference: reference.clone(),
                    owner: Owner::from_entry(source),
                })
            } else if let Some(manifest) = &inherited {
                manifest
                    .grants
                    .iter()
                    .find(|grant| grant.event_id == base.id && grant.reference == *reference)
                    .cloned()
            } else {
                None
            };
            // Old cross-root references with no publication-owned generation evidence
            // remain unavailable. No current-index lookup invents historical authority.
            if let Some(grant) = grant
                && grants.insert(base.id.to_string(), grant).is_some()
            {
                return Err(invalid(
                    &destination.id,
                    "duplicate inherited spool EventId",
                ));
            }
        }
        if grants.is_empty() {
            return Ok(None);
        }
        let manifest = Self {
            version: INHERITANCE_VERSION,
            destination: Owner::from_entry(destination),
            source: Owner::from_entry(source),
            branch_event_id: base.id.clone(),
            parent_event_anchor: anchor.clone(),
            grants: grants.into_values().collect(),
        };
        manifest.validate_destination(destination)?;
        manifest.validate_sources(entries)?;
        manifest.validate_history(destination, events)?;
        Ok(Some(manifest))
    }

    pub(crate) fn validate_destination(
        &self,
        destination: &SessionIndexEntry,
    ) -> Result<(), SessionPersistError> {
        if self.version != INHERITANCE_VERSION
            || !self.destination.matches(destination)
            || destination.rel_path.is_some()
            || self.grants.is_empty()
        {
            return Err(invalid(
                &destination.id,
                "manifest destination, version or grants differ",
            ));
        }
        if self.destination.generation.get_version_num() != 4
            || self.source.generation.get_version_num() != 4
        {
            return Err(invalid(
                &destination.id,
                "manifest generation is not a UUID v4",
            ));
        }
        let mut ids = HashSet::new();
        let mut previous: Option<String> = None;
        for grant in &self.grants {
            let id = grant.event_id.to_string();
            if !ids.insert(&grant.event_id) || previous.as_ref().is_some_and(|prior| prior >= &id) {
                return Err(invalid(
                    &destination.id,
                    "manifest grants are duplicated or unsorted",
                ));
            }
            validate_spool_ref(&grant.reference)?;
            if grant.reference != format!("{}/spool/{}.bin", grant.owner.root(), grant.event_id)
                || grant.owner.generation.get_version_num() != 4
            {
                return Err(invalid(
                    &destination.id,
                    "manifest grant does not name its exact owner and event",
                ));
            }
            previous = Some(id);
        }
        Ok(())
    }

    pub(crate) fn validate_sources(
        &self,
        entries: &[SessionIndexEntry],
    ) -> Result<(), SessionPersistError> {
        self.source.validate_current(entries)?;
        for grant in &self.grants {
            grant.owner.validate_current(entries)?;
        }
        Ok(())
    }

    pub(crate) fn read(
        root: &PrivateRoot,
        entries: &[SessionIndexEntry],
        registered: &SessionIndexEntry,
    ) -> Result<Option<Self>, SessionPersistError> {
        let owner_root = registered_root_session_id(registered);
        let file = match root.open_read(&inheritance_path(owner_root)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let manifest: Self = serde_json::from_reader(BufReader::new(file))
            .map_err(|error| malformed(&registered.id, &error))?;
        let destination = entries
            .iter()
            .find(|entry| entry.id == owner_root)
            .ok_or_else(|| SessionPersistError::GenerationChanged {
                id: owner_root.to_owned(),
            })?;
        manifest.validate_destination(destination)?;
        manifest.validate_sources(entries)?;
        Ok(Some(manifest))
    }

    pub(crate) fn validate_history(
        &self,
        registered: &SessionIndexEntry,
        events: &[SessionEvent],
    ) -> Result<(), SessionPersistError> {
        // Child timelines share their root's artifact authority; their selected event
        // is still checked against the exact grant before any range is opened.
        if registered.id != self.destination.id {
            return Ok(());
        }
        let mut positions = std::collections::HashMap::new();
        for (ordinal, event) in events.iter().enumerate() {
            if positions.insert(event.base().id.clone(), ordinal).is_some() {
                return Err(invalid(&registered.id, "history repeats an EventId"));
            }
        }
        let branch_position = *positions
            .get(&self.branch_event_id)
            .ok_or_else(|| invalid(&registered.id, "manifest ChildBranch is absent"))?;
        let SessionEvent::ChildBranch {
            base,
            parent_session_id: Some(parent),
            child_session_id: Some(child),
            path_address,
            parent_event_anchor: Some(anchor),
            kind: ChildBranchKind::Fork,
        } = &events[branch_position]
        else {
            return Err(invalid(&registered.id, "manifest branch is not a fork"));
        };
        if parent != &self.source.id
            || child != &self.destination.id
            || path_address != ROOT_PATH_ADDRESS
            || anchor != &self.parent_event_anchor
            || base.parent_id.as_ref() != Some(anchor)
            || branch_position
                .checked_sub(1)
                .and_then(|index| events.get(index))
                .map(|event| &event.base().id)
                != Some(anchor)
        {
            return Err(invalid(
                &registered.id,
                "manifest branch or parent anchor differs",
            ));
        }
        for grant in &self.grants {
            let ordinal = *positions.get(&grant.event_id).ok_or_else(|| {
                invalid(
                    &registered.id,
                    "manifest event is absent from copied history",
                )
            })?;
            if ordinal >= branch_position
                || !matches!(&events[ordinal], SessionEvent::ToolResult { spool_ref: Some(reference), .. } if reference == &grant.reference)
            {
                return Err(invalid(
                    &registered.id,
                    "manifest grant differs from copied history",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn authorizes(&self, event: &SessionEvent, reference: &str) -> bool {
        self.grants
            .iter()
            .any(|grant| grant.event_id == event.base().id && grant.reference == reference)
    }
}

impl SpoolWriter {
    /// Validate publication-owned inheritance against this registered timeline on open.
    pub(crate) fn validate_inherited_history(
        &self,
        events: &[SessionEvent],
    ) -> Result<(), SessionPersistError> {
        with_registered_spool_entries(
            &self.data_dir,
            &self.registered,
            self.index_lock_deadline,
            |root, entries| {
                if let Some(manifest) = SpoolInheritance::read(root, entries, &self.registered)? {
                    manifest.validate_history(&self.registered, events)?;
                }
                Ok(())
            },
        )
    }
}

fn invalid(session_id: &str, reason: &'static str) -> SessionPersistError {
    SessionPersistError::EventStore(format!(
        "spool inheritance for session {session_id}: {reason}"
    ))
}

pub(crate) fn malformed(session_id: &str, error: &serde_json::Error) -> SessionPersistError {
    SessionPersistError::EventStore(format!(
        "spool inheritance for session {session_id} is malformed ({:?}, line {}, column {})",
        error.classify(),
        error.line(),
        error.column()
    ))
}

#[cfg(test)]
#[path = "inheritance_tests.rs"]
mod tests;
