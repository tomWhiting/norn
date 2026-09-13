//! Selected history isolation, live append continuity, producer binding and store rotation.

use std::num::NonZeroUsize;
use std::sync::Arc;

use uuid::Uuid;

use super::{ActionLogTree, AgentHistoryError};
use crate::session::SessionBinding;
use crate::session::action_log::ActionLog;
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::{
    BodyRead, EventStore, HistoryAnchor, HistoryDirection, HistoryPage, HistoryRead,
    HistoryReadError, SessionHistoryReader,
};
use crate::session_view::{BodyRange, ViewError};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn demand(value: usize) -> Result<NonZeroUsize, std::io::Error> {
    NonZeroUsize::new(value).ok_or_else(|| std::io::Error::other("nonzero fixture demand required"))
}

fn user(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: text.to_owned(),
    }
}

fn register(
    tree: &ActionLogTree,
    agent: Uuid,
    parent: Option<Uuid>,
) -> Result<Arc<EventStore>, HistoryReadError> {
    let store = Arc::new(EventStore::new());
    store.bind_view_source(&SessionBinding::ephemeral_root(), agent, parent)?;
    tree.register(agent, parent, Arc::new(ActionLog::new(Arc::clone(&store))));
    Ok(store)
}

fn request(reader: &SessionHistoryReader) -> Result<HistoryRead, std::io::Error> {
    Ok(HistoryRead {
        source: reader.source().clone(),
        anchor: HistoryAnchor::Start,
        direction: HistoryDirection::After,
        max_events: demand(1)?,
    })
}

fn body(page: &HistoryPage) -> Result<BodyRead, std::io::Error> {
    let reference = page
        .records
        .first()
        .and_then(|record| record.items().first())
        .and_then(|item| item.bodies.first())
        .ok_or_else(|| std::io::Error::other("fixture page must carry an approved body"))?;
    Ok(BodyRead {
        reference: reference.clone(),
        range: BodyRange {
            offset: 0,
            max_bytes: demand(1024)?,
        },
    })
}

#[test]
fn history_reader_limits_selection_to_registered_caller_subtree() -> TestResult {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let sibling = Uuid::new_v4();
    let grandchild = Uuid::new_v4();
    let absent = Uuid::new_v4();
    let tree = ActionLogTree::new(root);
    register(&tree, root, None)?;
    register(&tree, child, Some(root))?;
    register(&tree, sibling, Some(root))?;
    register(&tree, grandchild, Some(child))?;
    for target in [child, grandchild] {
        assert_eq!(
            tree.history_reader(child, target)?.source().agent_id,
            target
        );
    }
    for target in [root, sibling] {
        assert!(matches!(
            tree.history_reader(child, target),
            Err(AgentHistoryError::OutsideSubtree { caller, target: denied })
                if caller == child && denied == target
        ));
    }
    for (caller, target) in [(absent, root), (root, absent), (absent, absent)] {
        assert!(matches!(
            tree.history_reader(caller, target),
            Err(AgentHistoryError::Unregistered { agent_id }) if agent_id == absent
        ));
    }
    Ok(())
}

#[test]
fn history_reader_does_not_bind_or_relabel_unbound_and_miswired_stores() -> TestResult {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let tree = ActionLogTree::new(root);
    register(&tree, root, None)?;
    let store = Arc::new(EventStore::new());
    tree.register(
        child,
        Some(root),
        Arc::new(ActionLog::new(Arc::clone(&store))),
    );
    assert!(matches!(
        tree.history_reader(root, child),
        Err(AgentHistoryError::History { agent_id, source: HistoryReadError::Unbound { .. } })
            if agent_id == child
    ));
    let source = store.bind_view_source(&SessionBinding::ephemeral_root(), child, Some(root))?;
    assert_eq!(tree.history_reader(root, child)?.source(), &source);
    for (bound_agent, bound_parent) in [(root, Some(root)), (child, None)] {
        let wrong = Arc::new(EventStore::new());
        wrong.bind_view_source(&SessionBinding::ephemeral_root(), bound_agent, bound_parent)?;
        let alternate = ActionLogTree::new(root);
        register(&alternate, root, None)?;
        alternate.register(child, Some(root), Arc::new(ActionLog::new(wrong)));
        assert!(matches!(
            alternate.history_reader(root, child),
            Err(AgentHistoryError::SourceMismatch { target, .. }) if target == child
        ));
    }
    Ok(())
}

#[test]
fn history_reader_shares_future_appends_and_enforces_body_and_page_isolation() -> TestResult {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let tree = ActionLogTree::new(root);
    register(&tree, root, None)?.append(user("root private text"))?;
    let child_store = register(&tree, child, Some(root))?;
    let reader = tree.history_reader(root, child)?;
    let read = request(&reader)?;
    assert!(reader.history_page(&read)?.records.is_empty());
    child_store.append(user("Aé🙂Z"))?;
    child_store.append(user("second child message"))?;
    let first = reader.history_page(&read)?;
    assert_eq!(first.total_events, 2);
    assert_eq!(first.records.len(), 1);
    assert!(first.has_after);
    let mut ranged = body(&first)?;
    ranged.range.offset = 1;
    ranged.range.max_bytes = demand(5)?;
    let chunk = reader.read_body(&ranged)?;
    assert_eq!(chunk.text, "é");
    assert_eq!(chunk.next_offset, Some(3));
    let root_reader = tree.history_reader(root, root)?;
    let root_page = root_reader.history_page(&request(&root_reader)?)?;
    assert!(matches!(
        reader.read_body(&body(&root_page)?),
        Err(HistoryReadError::View(ViewError::SourceMismatch { .. }))
    ));
    assert!(matches!(
        reader.history_page(&request(&root_reader)?),
        Err(HistoryReadError::View(ViewError::SourceMismatch { .. }))
    ));
    let next = reader.history_page(&HistoryRead {
        anchor: HistoryAnchor::At(first.next.ok_or("expected first cursor")?),
        ..read
    })?;
    assert_eq!(
        reader.read_body(&body(&next)?)?.text,
        "second child message"
    );
    Ok(())
}

#[test]
fn history_reader_remains_with_its_source_after_root_rotation() -> TestResult {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let tree = ActionLogTree::new(root);
    let original = register(&tree, root, None)?;
    original.append(user("before rotation"))?;
    register(&tree, child, Some(root))?.append(user("retained child"))?;
    let old_reader = tree.history_reader(root, root)?;
    let child_reader = tree.history_reader(root, child)?;
    let replacement = Arc::new(EventStore::new());
    replacement.bind_view_source(&SessionBinding::ephemeral_root(), root, None)?;
    replacement.append(user("after rotation"))?;
    tree.replace_root_log(Arc::new(ActionLog::new(replacement)));
    let new_reader = tree.history_reader(root, root)?;
    assert_ne!(old_reader.source(), new_reader.source());
    for (reader, expected) in [
        (&old_reader, "before rotation"),
        (&new_reader, "after rotation"),
        (&child_reader, "retained child"),
    ] {
        let page = reader.history_page(&request(reader)?)?;
        assert_eq!(reader.read_body(&body(&page)?)?.text, expected);
    }
    assert_eq!(
        tree.history_reader(root, child)?.source(),
        child_reader.source()
    );
    assert!(new_reader.history_page(&request(&old_reader)?).is_err());
    Ok(())
}

#[test]
fn history_reader_uses_child_source_bound_by_runtime_branch_allocation() -> TestResult {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let tree = ActionLogTree::new(root);
    let root_store = register(&tree, root, None)?;
    let branch = crate::tools::agent::delegation::branch_child_off_executor(
        &SessionBinding::ephemeral_root(),
        &root_store,
        &crate::session::ChildBranchRequest {
            child_session_id: child.to_string(),
            name_stem: "history-fixture".to_owned(),
            kind: crate::session::events::ChildBranchKind::Spawn,
            durability: crate::session::ChildDurability::Ephemeral,
            model: "test".to_owned(),
            working_dir: "/fixture".to_owned(),
        },
        root,
    )?;
    branch.store.append(user("actual child store"))?;
    tree.register(child, Some(root), Arc::new(ActionLog::new(branch.store)));
    let reader = tree.history_reader(root, child)?;
    assert_eq!(reader.source().agent_id, child);
    assert_eq!(reader.source().parent_agent_id, Some(root));
    let page = reader.history_page(&request(&reader)?)?;
    assert_eq!(reader.read_body(&body(&page)?)?.text, "actual child store");
    Ok(())
}
