use std::sync::Arc;

use parking_lot::Mutex;
use uuid::Uuid;

use crate::session::events::{EventBase, SessionEvent};
use crate::session::persistence::index::publish_new_child_session;
use crate::session::persistence::types::{
    ResumeFidelity, SESSION_FORMAT_VERSION, SessionIndexEntry, SessionPersistError,
    SessionRecordOrigin, SessionStatus,
};
use crate::session::spool::SpoolWriter;
use crate::session::store::{EventStore, JsonlSink};

use super::{ChildBranchRequest, Persistence, SessionBinding, SessionBrancher};

pub(super) fn materialize_child(
    brancher: &Arc<SessionBrancher>,
    parent: &SessionIndexEntry,
    path_address: &str,
    request: &ChildBranchRequest,
    reservation: &SessionEvent,
) -> Result<(EventStore, SessionBinding), SessionPersistError> {
    let provenance = child_provenance(reservation)?;
    let now = chrono::Utc::now();
    let candidate = SessionIndexEntry {
        id: request.child_session_id.clone(),
        generation: Uuid::new_v4(),
        name: Some(path_address.to_owned()),
        model: request.model.clone(),
        working_dir: request.working_dir.clone(),
        created_at: now,
        updated_at: now,
        event_count: 0,
        status: SessionStatus::Active,
        format_version: SESSION_FORMAT_VERSION,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        rel_path: Some(brancher.child_rel_path(path_address)),
        parent_id: Some(parent.id.clone()),
        fidelity: ResumeFidelity::Canonical,
        origin: SessionRecordOrigin::Native,
    };
    let entry = publish_new_child_session(
        brancher.manager.data_dir(),
        &candidate,
        std::slice::from_ref(&provenance),
        parent.generation,
        brancher.manager.index_lock_deadline(),
    )?;
    let sink = JsonlSink::open_registered(
        brancher.manager.data_dir(),
        &entry,
        brancher.durability,
        brancher.manager.index_lock_deadline(),
    )?;
    let mut store = EventStore::with_sink_and_events(Box::new(sink), vec![provenance]);
    store.attach_spool(SpoolWriter::for_session(
        brancher.manager.data_dir(),
        &entry,
        brancher.durability,
        brancher.manager.index_lock_deadline(),
    ));
    let binding = SessionBinding {
        path_address: path_address.to_owned(),
        persistence: Persistence::Persistent {
            brancher: Arc::clone(brancher),
            registered: Box::new(entry),
        },
        used_names: Mutex::new(std::collections::HashSet::new()),
    };
    Ok((store, binding))
}

fn child_provenance(reservation: &SessionEvent) -> Result<SessionEvent, SessionPersistError> {
    let SessionEvent::ChildBranch {
        parent_session_id,
        child_session_id,
        path_address,
        parent_event_anchor,
        kind,
        ..
    } = reservation
    else {
        return Err(SessionPersistError::EventStore(
            "child materialization requires a ChildBranch reservation".to_owned(),
        ));
    };
    Ok(SessionEvent::ChildBranch {
        base: EventBase::new(None),
        parent_session_id: parent_session_id.clone(),
        child_session_id: child_session_id.clone(),
        path_address: path_address.clone(),
        parent_event_anchor: parent_event_anchor.clone(),
        kind: *kind,
    })
}
