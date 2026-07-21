//! Child event-store seeding for [`crate::tools::agent::fork_tool::ForkTool`].
//!
//! Houses the R2 seeding step of the fork pipeline: copying the parent's
//! events into the fork's child [`EventStore`] and closing every orphan
//! `tool_call` — including the fork call itself — that has neither a legacy
//! `ToolResult` nor a canonical function/custom call-output item. It appends
//! a synthetic `ToolResult` so the child context never reaches the provider
//! API with unanswered tool calls. The child store arrives sink-equipped from
//! [`SessionBinding::branch_child`](crate::session::SessionBinding::branch_child)
//! for persistent forks, so every seeded event is written through to the
//! fork's own on-disk timeline. Split out of the fork pipeline (now [`super::fork_context`] /
//! [`super::fork_outcome`]) to
//! keep both files inside the per-file 500-line production-code limit
//! (CO5).

use uuid::Uuid;

use crate::agent::fork::OrphanToolCall;
use crate::error::{NornError, SessionError};
use crate::session::events::{EventBase, SessionEvent};
use crate::session::store::EventStore;
use crate::session::{response_publication_group_len, validate_provider_state_provenance};

/// Truncate a parent-history snapshot at the branch anchor the
/// reservation recorded: everything AFTER the anchor — the reservation
/// `ChildBranch` itself and any events concurrent tasks appended between
/// the mint and the snapshot — is excluded, so the seed matches the
/// recorded branch point exactly (F4).
///
/// `None` anchor = the parent log was empty at branch time; the seed is
/// empty.
///
/// # Errors
///
/// A snapshot that does not contain the anchor violates the append-only
/// contract (the snapshot was taken after the anchor was recorded) and
/// is refused typed — never silently seeded wrong.
pub(super) fn truncate_seed_at_anchor(
    mut snapshot: Vec<SessionEvent>,
    anchor: Option<&crate::session::events::EventId>,
) -> Result<Vec<SessionEvent>, NornError> {
    let Some(anchor) = anchor else {
        return Ok(Vec::new());
    };
    match snapshot.iter().position(|e| e.base().id == *anchor) {
        Some(pos) => {
            snapshot.truncate(pos + 1);
            Ok(snapshot)
        }
        None => Err(NornError::Session(SessionError::StorageError {
            reason: format!(
                "fork: branch anchor {anchor} missing from the parent-history \
                 snapshot taken after the branch — the append-only contract is \
                 violated; refusing to seed a history that cannot match the \
                 recorded branch point"
            ),
        })),
    }
}

/// Seed the fork's child [`EventStore`] with the anchor-truncated parent
/// events plus the synthetic tool results that close every orphan
/// `tool_call` — including the fork call itself (R2).
pub(super) fn seed_fork_events(
    child_store: &EventStore,
    parent_events: &[SessionEvent],
    fork_call_id: Option<&str>,
    fork_id: Uuid,
) -> Result<(), NornError> {
    let mut events = parent_events.to_vec();
    let all_orphans = find_all_orphan_tool_calls(&events);
    if !all_orphans.is_empty() {
        let ids: Vec<&str> = all_orphans.iter().map(|o| o.id.as_str()).collect();
        tracing::info!(
            fork_id = %fork_id,
            fork_call_id = ?fork_call_id,
            orphan_count = all_orphans.len(),
            orphan_ids = ?ids,
            "fork: closing orphan tool_calls in child context",
        );
    }
    for orphan in &all_orphans {
        let is_fork_call = fork_call_id.is_some_and(|fid| fid == orphan.id);
        let output = if is_fork_call {
            serde_json::json!({
                "agent_id": fork_id.to_string(),
                "status": "active",
                "message": crate::agent::fork::FORK_SYNTHETIC_RESULT_MESSAGE,
            })
        } else {
            serde_json::json!({
                "status": "in_progress",
                "message": "executing on parent agent",
            })
        };
        let tool_name = if is_fork_call {
            "fork".to_owned()
        } else {
            orphan.name.clone()
        };
        let parent_id = events.last().map(|e| e.base().id.clone());
        events.push(SessionEvent::ToolResult {
            base: EventBase::new(parent_id),
            tool_call_id: orphan.id.clone(),
            tool_name,
            output,
            spool_ref: None,
            duration_ms: 0,
        });
    }
    validate_provider_state_provenance(&events).map_err(|_error| {
        NornError::Session(SessionError::EventAppendFailed {
            reason: "fork seed contains invalid provider-state provenance".to_owned(),
        })
    })?;

    let mut index = 0;
    while index < events.len() {
        if let Some(group_len) =
            response_publication_group_len(&events, index).map_err(|_error| {
                NornError::Session(SessionError::EventAppendFailed {
                    reason: "fork seed contains invalid provider-state provenance".to_owned(),
                })
            })?
        {
            child_store
                .append_batch(&events[index..index.saturating_add(group_len)])
                .map_err(|error| map_seed_append_error(&error))?;
            index = index.saturating_add(group_len);
        } else {
            child_store
                .append(events[index].clone())
                .map_err(|error| map_seed_append_error(&error))?;
            index = index.saturating_add(1);
        }
    }
    Ok(())
}

fn map_seed_append_error(error: &SessionError) -> NornError {
    NornError::Session(SessionError::EventAppendFailed {
        reason: error.to_string(),
    })
}

/// Scan the durable effective prompt view for `tool_call`s without either
/// local result representation anywhere after them. Returns every visible
/// orphan across the entire history, not just the latest turn. Suppressed and
/// compacted calls stay hidden instead of gaining a new visible synthetic
/// output. This is the unconditional safety net that ensures the child context
/// never reaches the API with orphans.
fn find_all_orphan_tool_calls(events: &[SessionEvent]) -> Vec<OrphanToolCall> {
    crate::session::unresolved_effective_local_tool_calls(events)
        .into_iter()
        .map(|call| OrphanToolCall {
            id: call.call_id,
            name: call.name,
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn user(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
        }
    }

    /// F4: the seed is cut at the recorded anchor — the reservation and
    /// any concurrently appended events after it never leak into the
    /// fork's inherited history.
    #[test]
    fn truncate_seed_cuts_at_anchor() {
        let a = user("a");
        let b = user("b");
        let concurrent = user("appended concurrently");
        let anchor = b.base().id.clone();
        let seed =
            truncate_seed_at_anchor(vec![a, b, concurrent], Some(&anchor)).expect("anchor present");
        assert_eq!(seed.len(), 2, "everything after the anchor is dropped");
        assert_eq!(seed[1].base().id, anchor);
    }

    /// `None` anchor = empty parent log at branch time = empty seed.
    #[test]
    fn truncate_seed_none_anchor_is_empty() {
        let seed = truncate_seed_at_anchor(vec![user("late")], None).expect("empty seed");
        assert!(seed.is_empty());
    }

    /// F4 end-to-end shape: events appended to the parent BETWEEN the
    /// mint (which records the anchor under the allocation lock) and the
    /// post-mint snapshot — the real concurrent-wrapper-task race — are
    /// excluded from the seed, as is the reservation itself.
    #[test]
    fn concurrent_parent_append_never_leaks_into_the_seed() {
        use crate::session::manager::{CreateSessionOptions, SessionManager};
        use crate::session::store::DurabilityPolicy;
        use crate::session::{
            ChildBranchRequest, ChildDurability, SessionBinding, SessionBrancher,
        };
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager
            .create(
                CreateSessionOptions {
                    model: "m".to_owned(),
                    working_dir: "/w".to_owned(),
                    name: None,
                },
                DurabilityPolicy::Flush,
            )
            .unwrap();
        let root_id = opened.entry.id.clone();
        let root_entry = opened.entry.clone();
        let store = opened.store;
        store.append(user("pre-branch a")).unwrap();
        store.append(user("pre-branch b")).unwrap();

        let binding = SessionBinding::persistent_root(
            Arc::new(SessionBrancher::new(
                manager,
                root_id,
                DurabilityPolicy::Flush,
            )),
            &root_entry,
            &[],
        );
        let branched = binding
            .branch_child(
                &store,
                &ChildBranchRequest {
                    child_session_id: Uuid::new_v4().to_string(),
                    name_stem: "fork".to_owned(),
                    kind: crate::session::events::ChildBranchKind::Fork,
                    durability: ChildDurability::Persist,
                    model: "m".to_owned(),
                    working_dir: "/w".to_owned(),
                },
            )
            .unwrap();

        // The concurrent append lands between the mint and the snapshot.
        store.append(user("concurrent append")).unwrap();

        let seed = truncate_seed_at_anchor(store.events(), branched.parent_event_anchor.as_ref())
            .expect("anchor present in the post-mint snapshot");
        assert_eq!(seed.len(), 2, "exactly the pre-branch history: {seed:?}");
        assert!(
            seed.iter()
                .all(|e| !matches!(e, SessionEvent::ChildBranch { .. })),
            "the reservation never leaks into the seed",
        );
        assert!(
            seed.iter().all(|e| !matches!(
                e,
                SessionEvent::UserMessage { content, .. } if content == "concurrent append"
            )),
            "the concurrent append never leaks into the seed",
        );
    }

    #[test]
    fn persistent_fork_seeds_framed_response_group_without_splitting_it() -> TestResult {
        use std::sync::Arc;

        use crate::session::events::{EventUsage, ProviderEpochBoundaryReason};
        use crate::session::manager::{CreateSessionOptions, SessionManager};
        use crate::session::store::DurabilityPolicy;
        use crate::session::{
            ChildBranchRequest, ChildDurability, ProviderStateProvenance, SessionBinding,
            SessionBrancher, validate_provider_state_provenance,
        };

        let temp = tempfile::tempdir()?;
        let manager = SessionManager::new(temp.path());
        let opened = manager.create(
            CreateSessionOptions {
                model: "m".to_owned(),
                working_dir: "/w".to_owned(),
                name: None,
            },
            DurabilityPolicy::Flush,
        )?;
        let root_id = opened.entry.id.clone();
        let root_entry = opened.entry.clone();
        let parent = opened.store;
        parent.append(user("prompt"))?;

        let boundary = SessionEvent::ProviderEpochBoundary {
            base: EventBase::new(parent.last_event_id()),
            reason: ProviderEpochBoundaryReason::ResponseStatePublication,
        };
        let assistant_id = crate::session::events::EventId::new();
        let provenance = ProviderStateProvenance::new(assistant_id.clone(), true)
            .into_custom_event(EventBase::new(Some(boundary.base().id.clone())))?;
        let mut assistant_base = EventBase::new(Some(provenance.base().id.clone()));
        assistant_base.id = assistant_id;
        let assistant = SessionEvent::AssistantMessage {
            base: assistant_base,
            response_items: Vec::new(),
            content: "answer".to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: "end_turn".to_owned(),
            response_id: Some("resp_fork-seed".to_owned()),
        };
        parent.append_batch(&[boundary, provenance, assistant])?;

        let binding = SessionBinding::persistent_root(
            Arc::new(SessionBrancher::new(
                manager.clone(),
                root_id,
                DurabilityPolicy::Flush,
            )),
            &root_entry,
            &[],
        );
        let child_id = Uuid::new_v4().to_string();
        let branched = binding.branch_child(
            &parent,
            &ChildBranchRequest {
                child_session_id: child_id.clone(),
                name_stem: "fork".to_owned(),
                kind: crate::session::events::ChildBranchKind::Fork,
                durability: ChildDurability::Persist,
                model: "m".to_owned(),
                working_dir: "/w".to_owned(),
            },
        )?;
        let seed = truncate_seed_at_anchor(parent.events(), branched.parent_event_anchor.as_ref())?;
        seed_fork_events(&branched.store, &seed, None, Uuid::new_v4())?;

        let resumed = manager.resume(&child_id, DurabilityPolicy::Flush)?;
        validate_provider_state_provenance(&resumed.store.events())?;
        assert!(resumed.store.events().windows(3).any(|events| matches!(
            events,
            [
                SessionEvent::ProviderEpochBoundary {
                    reason: ProviderEpochBoundaryReason::ResponseStatePublication,
                    ..
                },
                SessionEvent::Custom { .. },
                SessionEvent::AssistantMessage { .. },
            ]
        )));
        Ok(())
    }

    /// A missing anchor is an append-only violation and refuses typed.
    #[test]
    fn truncate_seed_missing_anchor_is_typed_error() {
        let ghost = user("ghost");
        let err = truncate_seed_at_anchor(vec![user("a")], Some(&ghost.base().id))
            .expect_err("missing anchor must refuse");
        assert!(matches!(
            err,
            NornError::Session(SessionError::StorageError { .. })
        ));
    }
}

#[cfg(test)]
mod effective_view_tests;
