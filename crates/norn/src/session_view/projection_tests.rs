//! Live/committed reducer regressions for identity, retry, authority and missing coverage.

use std::num::NonZeroUsize;
use std::sync::Arc;

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use super::contract_tests::{TestResult, assistant, call, cursor, model, response, source};
use super::{
    BodyRange, CoverageGap, ItemDirection, ItemId, ItemInclusion, SessionProjection, ToolState,
    ViewError, ViewItemKind, project_committed,
};
use crate::provider::McpChannelDeliveryEvent;
use crate::provider::agent_event::{
    AgentCompaction, AgentEvent, AgentEventKind, AgentMessageLifecycle, AgentStreamRetry,
    AgentUsageEstimate, CompactionSummaryKind, SubagentDescriptor, SubagentKind, SubagentLifecycle,
};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::openai::response_stream_event::ResponseStreamEvent;
use crate::provider::reasoning::{ReasoningItem, ReasoningSummaryPart};
use crate::provider::request::ToolCallKind;
use crate::provider::response_audio::ResponseAudioEvent;
use crate::provider::usage::Usage;
use crate::session::events::{EventBase, EventId, SessionEvent};

fn first(view: &SessionProjection) -> Result<&super::ViewItem, Box<dyn std::error::Error>> {
    view.items()
        .next()
        .ok_or_else(|| "fixture view is empty".into())
}

fn tagged(view: &SessionProjection, event: AgentEventKind) -> AgentEvent {
    AgentEvent {
        agent_id: view.source().agent_id,
        agent_role: Arc::from("root"),
        event,
    }
}

fn live(
    view: &mut SessionProjection,
    event: ProviderEvent,
) -> Result<super::LiveReduction, ViewError> {
    view.apply_live(&tagged(view, AgentEventKind::Provider(event)))
}

fn complete(call_id: &str, arguments: &str) -> ProviderEvent {
    ProviderEvent::ToolCallComplete {
        call_id: call_id.to_owned(),
        name: "same-name".to_owned(),
        arguments: arguments.to_owned(),
        kind: ToolCallKind::Custom,
    }
}

fn done() -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        response_id: Some("response-one".to_owned()),
    }
}

#[test]
fn indexed_traversal_crosses_committed_and_live_rows_with_explicit_inclusion() -> TestResult {
    let owner = source();
    let mut view = SessionProjection::new(owner.clone());
    let mut committed = Vec::new();
    for ordinal in 0..2 {
        let event = SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: format!("message {ordinal}"),
        };
        let record = project_committed(&cursor(&owner, ordinal, &event), &event)?;
        committed.push(
            record
                .items()
                .first()
                .ok_or("input item missing")?
                .id
                .clone(),
        );
        view.apply_history_record(&record)?;
    }
    let local = view.record_notice(ViewItemKind::Notice, "local")?;
    view.begin_execution(Uuid::new_v4(), model()?)?;
    live(
        &mut view,
        ProviderEvent::TextDelta {
            text: "stream".to_owned(),
        },
    )?;
    let provisional = view
        .items()
        .find(|row| matches!(row.id, ItemId::Provisional(_)))
        .ok_or("stream item missing")?
        .id
        .clone();
    let ids = |anchor: &ItemId, direction, inclusion| -> Result<Vec<ItemId>, ViewError> {
        Ok(view
            .items_from(anchor, direction, inclusion)?
            .map(|row| row.id.clone())
            .collect())
    };
    assert_eq!(
        ids(
            &committed[1],
            ItemDirection::Later,
            ItemInclusion::Inclusive
        )?,
        vec![committed[1].clone(), local.clone(), provisional.clone()]
    );
    assert_eq!(
        ids(
            &committed[1],
            ItemDirection::Later,
            ItemInclusion::Exclusive
        )?,
        vec![local.clone(), provisional.clone()]
    );
    assert_eq!(
        ids(
            &committed[1],
            ItemDirection::Earlier,
            ItemInclusion::Inclusive
        )?,
        vec![committed[1].clone(), committed[0].clone()]
    );
    assert_eq!(
        ids(
            &committed[1],
            ItemDirection::Earlier,
            ItemInclusion::Exclusive
        )?,
        vec![committed[0].clone()]
    );
    assert_eq!(
        ids(&local, ItemDirection::Earlier, ItemInclusion::Inclusive)?,
        vec![local.clone(), committed[1].clone(), committed[0].clone()]
    );
    assert!(
        ids(
            &committed[0],
            ItemDirection::Earlier,
            ItemInclusion::Exclusive
        )?
        .is_empty()
    );
    assert!(ids(&provisional, ItemDirection::Later, ItemInclusion::Exclusive)?.is_empty());
    let mut both = view.items_from(
        &committed[1],
        ItemDirection::Later,
        ItemInclusion::Inclusive,
    )?;
    assert_eq!(both.next_back().map(|row| &row.id), Some(&provisional));
    assert_eq!(both.next().map(|row| &row.id), Some(&committed[1]));
    assert_eq!(both.next().map(|row| &row.id), Some(&local));
    assert!(both.next().is_none());
    Ok(())
}

#[test]
fn indexed_traversal_refuses_missing_foreign_and_unresolved_alias_anchors() -> TestResult {
    let owner = source();
    let mut view = SessionProjection::new(owner.clone());
    let missing = ItemId::Local {
        source: owner.clone(),
        ordinal: 9,
    };
    assert!(
        matches!(view.items_from(&missing, ItemDirection::Later, ItemInclusion::Inclusive), Err(ViewError::ItemUnavailable { item }) if *item == missing)
    );
    let foreign = ItemId::Local {
        source: source(),
        ordinal: 0,
    };
    assert!(
        matches!(view.items_from(&foreign, ItemDirection::Earlier, ItemInclusion::Exclusive), Err(ViewError::SourceMismatch { expected, .. }) if *expected == owner)
    );
    view.begin_execution(Uuid::new_v4(), model()?)?;
    live(
        &mut view,
        ProviderEvent::ToolCallDelta {
            item_id: "item".to_owned(),
            call_id: Some("call".to_owned()),
            name: Some("same-name".to_owned()),
            arguments_delta: "arg".to_owned(),
            kind: ToolCallKind::Custom,
        },
    )?;
    let previous = first(&view)?.id.clone();
    live(&mut view, complete("call", "argument"))?;
    assert!(view.alias(&previous).is_some());
    assert!(
        matches!(view.items_from(&previous, ItemDirection::Later, ItemInclusion::Inclusive), Err(ViewError::ItemUnavailable { item }) if *item == previous)
    );
    let resolved = view.alias(&previous).ok_or("proven alias missing")?;
    assert_eq!(
        view.items_from(resolved, ItemDirection::Later, ItemInclusion::Inclusive)?
            .next()
            .map(|row| &row.id),
        Some(resolved)
    );
    Ok(())
}

#[test]
fn indexed_traversal_visits_requested_rows_without_seeking_through_retained_history() -> TestResult
{
    for retained in [64, 8192] {
        let mut view = SessionProjection::new(source());
        let mut ids = Vec::new();
        for ordinal in 0..retained {
            ids.push(view.record_notice(ViewItemKind::Notice, &format!("row {ordinal}"))?);
        }
        let anchor = &ids[retained / 2];
        for direction in [ItemDirection::Earlier, ItemDirection::Later] {
            view.items.traversal_visits.set(0);
            let rows: Vec<_> = view
                .items_from(anchor, direction, ItemInclusion::Inclusive)?
                .take(5)
                .collect();
            assert_eq!(rows.len(), 5);
            assert_eq!(view.items.traversal_visits.get(), 5);
            assert!(std::ptr::eq(
                rows[0],
                view.item(anchor).ok_or("anchor disappeared")?
            ));
            assert_eq!(rows[0].id, *anchor);
            let last = match direction {
                ItemDirection::Earlier => retained / 2 - 4,
                ItemDirection::Later => retained / 2 + 4,
            };
            assert_eq!(rows[4].id, ids[last]);
        }
    }
    Ok(())
}

struct ReusedCallHistory {
    view: SessionProjection,
    identities: Vec<(EventId, EventId)>,
}

fn assert_flat_fixture_rows(view: &SessionProjection, pairs: usize) {
    // The shared assistant fixture includes flat text and a thinking summary.
    // Keep those unrelated rows in the index-work workload and count them.
    assert_eq!(
        view.items()
            .filter(|row| matches!(row.kind, ViewItemKind::Text))
            .count(),
        pairs
    );
    assert_eq!(
        view.items()
            .filter(|row| matches!(row.kind, ViewItemKind::Thinking))
            .count(),
        pairs
    );
}

fn reused_call_history(
    pairs: usize,
    parent_first: bool,
    exact_parent: bool,
) -> Result<ReusedCallHistory, Box<dyn std::error::Error>> {
    let owner = source();
    let mut view = SessionProjection::new(owner.clone());
    let mut identities = Vec::new();
    for pair in 0..pairs {
        let invocation = assistant(
            Vec::new(),
            vec![call("reused", "edit", json!({}), ToolCallKind::Function)],
        );
        let result = SessionEvent::ToolResult {
            base: EventBase::new(exact_parent.then(|| invocation.base().id.clone())),
            tool_call_id: "reused".to_owned(),
            tool_name: "edit".to_owned(),
            output: json!({"committed":true}),
            spool_ref: None,
            duration_ms: 2,
        };
        let invocation_record =
            project_committed(&cursor(&owner, pair * 2, &invocation), &invocation)?;
        let result_record = project_committed(&cursor(&owner, pair * 2 + 1, &result), &result)?;
        if parent_first {
            view.apply_history_record(&invocation_record)?;
            view.apply_history_record(&result_record)?;
        } else {
            view.apply_history_record(&result_record)?;
            view.apply_history_record(&invocation_record)?;
        }
        identities.push((invocation.base().id.clone(), result.base().id.clone()));
    }
    Ok(ReusedCallHistory { view, identities })
}

#[test]
fn exact_parent_joins_do_linear_index_work_when_every_call_id_is_reused() -> TestResult {
    for parent_first in [false, true] {
        let mut visits = Vec::new();
        for pairs in [32, 256] {
            let ReusedCallHistory { view, identities } =
                reused_call_history(pairs, parent_first, true)?;
            assert_eq!(view.items().len(), pairs * 3);
            assert_flat_fixture_rows(&view, pairs);
            let tools: Vec<_> = view
                .items()
                .filter_map(|row| match &row.kind {
                    ViewItemKind::Tool(tool) => Some(tool),
                    _ => None,
                })
                .collect();
            assert_eq!(tools.len(), pairs);
            for (tool, (invocation_id, result_id)) in tools.into_iter().zip(identities) {
                assert_eq!(tool.invocation_event.as_ref(), Some(&invocation_id));
                assert_eq!(tool.result_event.as_ref(), Some(&result_id));
                assert_eq!(tool.state, ToolState::Completed);
            }
            // Counts actual visited invocation/pending index entries, including
            // the late-parent and newly complete-prefix notification paths.
            let inspected = view.items.tool_lookup_visits.get();
            assert!(
                inspected <= 5 * pairs,
                "{inspected} entries for {pairs} pairs"
            );
            visits.push(inspected);
        }
        assert_eq!(visits[1], visits[0] * 8);
    }
    Ok(())
}

#[test]
fn ambiguous_parentless_results_never_rescan_completed_call_history() -> TestResult {
    for parent_first in [false, true] {
        let mut visits = Vec::new();
        for pairs in [32, 256] {
            let ReusedCallHistory { view, identities } =
                reused_call_history(pairs, parent_first, false)?;
            assert_eq!(view.items().len(), pairs * 4 - 1);
            assert_flat_fixture_rows(&view, pairs);
            assert_eq!(
                view.items()
                    .filter(|row| matches!(row.kind, ViewItemKind::Tool(_)))
                    .count(),
                pairs * 2 - 1
            );
            let joined: Vec<_> = view
                .items()
                .filter_map(|row| match &row.kind {
                    ViewItemKind::Tool(tool)
                        if tool.invocation_event.is_some() && tool.result_event.is_some() =>
                    {
                        Some((tool.invocation_event.clone(), tool.result_event.clone()))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                joined,
                vec![(Some(identities[0].0.clone()), Some(identities[0].1.clone()))]
            );
            let inspected = view.items.tool_lookup_visits.get();
            assert!(
                inspected <= 5 * pairs,
                "{inspected} entries for {pairs} pairs"
            );
            visits.push(inspected);
        }
        // The first unambiguous result reads one predecessor; every later
        // result reads exactly two, regardless of retained completed history.
        assert_eq!(visits[1] + 1, (visits[0] + 1) * 8);
    }
    Ok(())
}

fn reused_live_call_history(
    pairs: usize,
    commit_before_result: bool,
) -> Result<SessionProjection, Box<dyn std::error::Error>> {
    let owner = source();
    let mut view = SessionProjection::new(owner.clone());
    view.begin_execution(Uuid::new_v4(), model()?)?;
    for pair in 0..pairs {
        live(
            &mut view,
            ProviderEvent::ToolCallDelta {
                item_id: format!("item-{pair}"),
                call_id: Some("reused".to_owned()),
                name: Some("same-name".to_owned()),
                arguments_delta: "arg".to_owned(),
                kind: ToolCallKind::Custom,
            },
        )?;
        live(&mut view, complete("reused", "argument"))?;
        let attempt = live(&mut view, done())?
            .completed_attempt
            .ok_or("response completion missing")?;
        let invocation = assistant(
            Vec::new(),
            vec![call(
                "reused",
                "same-name",
                json!("argument"),
                ToolCallKind::Custom,
            )],
        );
        let invocation_record =
            project_committed(&cursor(&owner, pair * 2, &invocation), &invocation)?;
        if commit_before_result {
            view.reconcile_history_record(&attempt, &invocation_record)?;
        }
        live(
            &mut view,
            ProviderEvent::ToolResult {
                tool_call_id: "reused".to_owned(),
                tool_name: "same-name".to_owned(),
                output: json!({"pair":pair}),
                duration_ms: 2,
            },
        )?;
        if !commit_before_result {
            view.reconcile_history_record(&attempt, &invocation_record)?;
        }
        let result = SessionEvent::ToolResult {
            base: EventBase::new(Some(invocation.base().id.clone())),
            tool_call_id: "reused".to_owned(),
            tool_name: "same-name".to_owned(),
            output: json!({"pair":pair}),
            spool_ref: None,
            duration_ms: 2,
        };
        view.apply_history_record(&project_committed(
            &cursor(&owner, pair * 2 + 1, &result),
            &result,
        )?)?;
        let invocation_id = invocation_record
            .items()
            .iter()
            .find(|row| matches!(row.kind, ViewItemKind::Tool(_)))
            .map(|row| &row.id)
            .ok_or("fixture invocation record has no tool")?;
        let row = view.item(invocation_id).ok_or("invocation absent")?;
        let ViewItemKind::Tool(tool) = &row.kind else {
            return Err("invocation is not a tool".into());
        };
        assert_eq!(tool.result_event.as_ref(), Some(&result.base().id));
        assert_eq!(tool.invocation_attempt.as_ref(), Some(&attempt));
        assert_eq!(tool.state, ToolState::Completed);
    }
    Ok(view)
}

#[test]
fn live_reused_calls_do_linear_index_work_across_response_and_commit_boundaries() -> TestResult {
    for commit_before_result in [false, true] {
        let mut visits = Vec::new();
        for pairs in [32, 256] {
            let view = reused_live_call_history(pairs, commit_before_result)?;
            assert_eq!(view.items().len(), pairs * 4);
            assert_flat_fixture_rows(&view, pairs);
            assert_eq!(
                view.items()
                    .filter(|row| matches!(row.kind, ViewItemKind::Tool(_)))
                    .count(),
                pairs
            );
            assert_eq!(
                view.items()
                    .filter(|row| matches!(row.kind, ViewItemKind::Metadata))
                    .count(),
                pairs
            );
            // Per pair: one current-attempt alias, one pending result target,
            // and three entries in the exact committed-result correlation.
            let inspected = view.items.tool_lookup_visits.get();
            assert_eq!(inspected, 5 * pairs);
            visits.push(inspected);
        }
        assert_eq!(visits[1], visits[0] * 8);
    }
    Ok(())
}

#[test]
fn unresolved_live_call_ambiguity_only_inspects_two_pending_invocations() -> TestResult {
    let mut view = SessionProjection::new(source());
    view.begin_execution(Uuid::new_v4(), model()?)?;
    for _ in 0..256 {
        live(&mut view, complete("reused", "argument"))?;
        live(&mut view, done())?;
    }
    let prior_visits = view.items.tool_lookup_visits.get();
    live(
        &mut view,
        ProviderEvent::ToolResult {
            tool_call_id: "reused".to_owned(),
            tool_name: "same-name".to_owned(),
            output: json!({"ambiguous":true}),
            duration_ms: 2,
        },
    )?;
    assert_eq!(view.items.tool_lookup_visits.get() - prior_visits, 2);
    assert_eq!(view.items().len(), 513);
    assert_eq!(
        view.items()
            .filter(|row| matches!(row.kind, ViewItemKind::Tool(_)))
            .count(),
        257
    );
    assert_eq!(
        view.items()
            .filter(|row| matches!(row.kind, ViewItemKind::Metadata))
            .count(),
        256
    );
    for (row, tool) in view.items().filter_map(|row| match &row.kind {
        ViewItemKind::Tool(tool) => Some((row, tool)),
        _ => None,
    }) {
        if matches!(row.id, ItemId::Local { .. }) {
            assert!(tool.result.is_some());
            assert!(tool.invocation_attempt.is_none());
            assert_eq!(tool.state, ToolState::Incomplete);
        } else {
            assert!(tool.result.is_none());
        }
    }
    Ok(())
}

#[test]
fn missing_parent_page_never_joins_a_reused_call_to_an_older_invocation() -> TestResult {
    let owner = source();
    let older = assistant(
        Vec::new(),
        vec![call(
            "reused",
            "edit",
            json!({"tool_use_description":"older"}),
            ToolCallKind::Function,
        )],
    );
    let newer = assistant(
        Vec::new(),
        vec![call(
            "reused",
            "edit",
            json!({"tool_use_description":"newer"}),
            ToolCallKind::Function,
        )],
    );
    let result = SessionEvent::ToolResult {
        base: EventBase::new(Some(newer.base().id.clone())),
        tool_call_id: "reused".to_owned(),
        tool_name: "edit".to_owned(),
        output: json!({"committed":true}),
        spool_ref: None,
        duration_ms: 2,
    };
    let mut view = SessionProjection::new(owner.clone());
    view.apply_history_record(&project_committed(&cursor(&owner, 0, &older), &older)?)?;
    view.apply_history_record(&project_committed(&cursor(&owner, 3, &result), &result)?)?;
    let old_tool = view
        .items()
        .find_map(|row| match &row.kind {
            ViewItemKind::Tool(tool)
                if tool.invocation_event.as_ref() == Some(&older.base().id) =>
            {
                Some(tool)
            }
            _ => None,
        })
        .ok_or("older tool absent")?;
    assert!(old_tool.result.is_none());
    view.apply_history_record(&project_committed(&cursor(&owner, 2, &newer), &newer)?)?;
    for row in view.items() {
        if let ViewItemKind::Tool(tool) = &row.kind {
            if tool.invocation_event.as_ref() == Some(&older.base().id) {
                assert!(tool.result.is_none());
            }
            if tool.invocation_event.as_ref() == Some(&newer.base().id) {
                assert_eq!(tool.result_event.as_ref(), Some(&result.base().id));
            }
        }
    }
    Ok(())
}

#[test]
fn early_tool_alias_reaches_committed_anchor_and_later_retry_keeps_prior_result() -> TestResult {
    for commit_before_result in [false, true] {
        let owner = source();
        let mut view = SessionProjection::new(owner.clone());
        view.begin_execution(Uuid::new_v4(), model()?)?;
        live(
            &mut view,
            ProviderEvent::ToolCallDelta {
                item_id: "stream-item".to_owned(),
                call_id: Some("call".to_owned()),
                name: Some("same-name".to_owned()),
                arguments_delta: "a".to_owned(),
                kind: ToolCallKind::Custom,
            },
        )?;
        let early = first(&view)?.id.clone();
        live(&mut view, complete("call", "argument"))?;
        let attempt = live(&mut view, done())?
            .completed_attempt
            .ok_or("no completed attempt")?;
        let event = assistant(
            Vec::new(),
            vec![call(
                "call",
                "same-name",
                json!("argument"),
                ToolCallKind::Custom,
            )],
        );
        let record = project_committed(&cursor(&owner, 0, &event), &event)?;
        if commit_before_result {
            view.reconcile_history_record(&attempt, &record)?;
        }
        live(
            &mut view,
            ProviderEvent::ToolResult {
                tool_call_id: "call".to_owned(),
                tool_name: "same-name".to_owned(),
                output: json!({"ok":true}),
                duration_ms: 2,
            },
        )?;
        let result = view
            .items()
            .find_map(|row| match &row.kind {
                ViewItemKind::Tool(tool) => tool.result.clone(),
                _ => None,
            })
            .ok_or("result absent")?;
        live(
            &mut view,
            ProviderEvent::TextDelta {
                text: "next response".to_owned(),
            },
        )?;
        view.apply_live(&tagged(
            &view,
            AgentEventKind::StreamRetry(AgentStreamRetry {
                attempt: 2,
                max_attempts: Some(2),
                delay_ms: 1,
                error_class: "timeout".to_owned(),
            }),
        ))?;
        let chunk = view.read_provisional(
            &result,
            BodyRange {
                offset: 0,
                max_bytes: NonZeroUsize::new(64).ok_or("zero demand")?,
            },
        )?;
        assert_eq!(chunk.text.as_str(), "{\"ok\":true}");
        if !commit_before_result {
            view.reconcile_history_record(&attempt, &record)?;
        }
        assert!(matches!(view.alias(&early), Some(ItemId::Committed { .. })));
    }
    Ok(())
}

#[test]
fn completed_reasoning_does_not_suppress_the_following_answer_stream() -> TestResult {
    let mut view = SessionProjection::new(source());
    view.begin_execution(Uuid::new_v4(), model()?)?;
    let reasoning = response(
        json!({"type":"reasoning","id":"reasoning","summary":[{"type":"summary_text","text":"summary"}],"encrypted_content":"excluded"}),
    )?;
    live(
        &mut view,
        ProviderEvent::ResponseItemDone { item: reasoning },
    )?;
    live(
        &mut view,
        ProviderEvent::TextDelta {
            text: "visible before completion".to_owned(),
        },
    )?;
    let answer = view
        .items()
        .find(|row| matches!(row.kind, ViewItemKind::Text))
        .and_then(|row| row.bodies.first())
        .ok_or("answer preview suppressed")?;
    assert_eq!(
        view.read_provisional(
            answer,
            BodyRange {
                offset: 0,
                max_bytes: NonZeroUsize::new(64).ok_or("zero demand")?
            }
        )?
        .text
        .as_str(),
        "visible before completion"
    );
    let message = response(
        json!({"type":"message","id":"answer","role":"assistant","status":"completed","content":[{"type":"output_text","text":"final answer","annotations":[],"logprobs":[]}]}),
    )?;
    live(&mut view, ProviderEvent::ResponseItemDone { item: message })?;
    assert_eq!(
        view.items()
            .filter(|row| matches!(row.kind, ViewItemKind::Thinking))
            .count(),
        1
    );
    assert_eq!(
        view.items()
            .filter(|row| matches!(row.kind, ViewItemKind::Text))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn exact_channel_event_reconciles_in_either_arrival_order() -> TestResult {
    for history_first in [false, true] {
        let owner = source();
        let mut view = SessionProjection::new(owner.clone());
        let event = SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "persisted attributed frame".to_owned(),
        };
        let record = project_committed(&cursor(&owner, 0, &event), &event)?;
        let observation = tagged(
            &view,
            AgentEventKind::McpChannel(McpChannelDeliveryEvent {
                event_id: event.base().id.clone(),
                message_id: Uuid::new_v4(),
                recipient_id: owner.agent_id,
                source: "messages".to_owned(),
                generation: 4,
                sequence: 8,
                content: "raw external message".to_owned(),
            }),
        );
        if history_first {
            view.apply_history_record(&record)?;
        }
        view.apply_live(&observation)?;
        view.apply_history_record(&record)?;
        view.apply_live(&observation)?;
        assert_eq!(view.items().len(), 1);
        let row = first(&view)?;
        assert!(matches!(row.id, ItemId::Committed { .. }));
        assert!(matches!(row.kind, ViewItemKind::ExternalInput));
        let body = row.bodies.first().ok_or("external body absent")?;
        assert_eq!(
            view.read_provisional(
                body,
                BodyRange {
                    offset: 0,
                    max_bytes: NonZeroUsize::new(64).ok_or("zero demand")?
                }
            )?
            .text
            .as_str(),
            "raw external message"
        );
    }
    Ok(())
}

#[test]
fn overlapping_tools_join_call_ids_and_preserve_freeform_and_description() -> TestResult {
    let mut view = SessionProjection::new(source());
    view.begin_execution(Uuid::new_v4(), model()?)?;
    live(
        &mut view,
        ProviderEvent::ToolCallDelta {
            item_id: "item-a".to_owned(),
            call_id: Some("call-a".to_owned()),
            name: Some("same-name".to_owned()),
            arguments_delta: "first ".to_owned(),
            kind: ToolCallKind::Custom,
        },
    )?;
    let assembling = first(&view)?.id.clone();
    live(&mut view, complete("call-b", "second input"))?;
    live(&mut view, complete("call-a", "first input"))?;
    assert_eq!(view.items().len(), 2);
    assert!(view.alias(&assembling).is_some());
    live(
        &mut view,
        ProviderEvent::ToolResult {
            tool_call_id: "call-b".to_owned(),
            tool_name: "same-name".to_owned(),
            output: json!({"committed":true,"error":{"kind":"validation_failed","message":"diagnostic failure"}}),
            duration_ms: 7,
        },
    )?;
    let tools: Vec<_> = view
        .items()
        .filter_map(|row| match &row.kind {
            ViewItemKind::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect();
    let first = tools
        .iter()
        .find(|tool| tool.call_id.as_deref() == Some("call-a"))
        .ok_or("first call absent")?;
    let second = tools
        .iter()
        .find(|tool| tool.call_id.as_deref() == Some("call-b"))
        .ok_or("second call absent")?;
    assert_eq!(first.state, ToolState::Running);
    assert_eq!(second.state, ToolState::Failed);
    assert_eq!(second.committed, Some(true));
    assert!(first.description.is_none());
    assert!(second.description.is_none());
    let body = first.arguments.as_ref().ok_or("arguments absent")?;
    let bytes = view.read_provisional(
        body,
        BodyRange {
            offset: 0,
            max_bytes: NonZeroUsize::new(64).ok_or("zero demand")?,
        },
    )?;
    assert_eq!(bytes.text.as_str(), "first input");
    Ok(())
}

#[test]
fn orphan_results_rejoin_explicit_history_without_losing_failure_facts() -> TestResult {
    let owner = source();
    let mut view = SessionProjection::new(owner.clone());
    let invocation = assistant(
        Vec::new(),
        vec![call(
            "call",
            "edit",
            json!({"tool_use_description":"original description","description":"ordinary tool argument","old_string":"old","new_string":"new"}),
            ToolCallKind::Function,
        )],
    );
    let result = SessionEvent::ToolResult {
        base: EventBase::new(Some(invocation.base().id.clone())),
        tool_call_id: "call".to_owned(),
        tool_name: "edit".to_owned(),
        output: json!({"committed":true,"error":{"kind":"validation_failed"}}),
        spool_ref: None,
        duration_ms: 12,
    };
    let result_record = project_committed(&cursor(&owner, 1, &result), &result)?;
    view.apply_history_record(&result_record)?;
    let orphan = first(&view)?.id.clone();
    let ViewItemKind::Tool(tool) = &first(&view)?.kind else {
        return Err("orphan was not a tool".into());
    };
    assert!(tool.arguments.is_none());
    assert_eq!(tool.state, ToolState::Incomplete);
    assert_eq!(tool.result_state, Some(ToolState::Failed));
    let invocation_record = project_committed(&cursor(&owner, 0, &invocation), &invocation)?;
    view.apply_history_record(&invocation_record)?;
    view.apply_history_record(&result_record)?;
    view.apply_history_record(&invocation_record)?;
    let tools: Vec<_> = view
        .items()
        .filter_map(|row| match &row.kind {
            ViewItemKind::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].state, ToolState::Failed);
    assert_eq!(tools[0].committed, Some(true));
    assert_eq!(
        tools[0]
            .description
            .as_ref()
            .map(super::DisplayText::as_str),
        Some("original description")
    );
    assert!(view.alias(&orphan).is_some());
    assert!(
        !view
            .coverage()
            .gaps
            .contains(&CoverageGap::OlderHistoryMissing)
    );
    Ok(())
}

#[test]
fn retry_invalidates_only_current_attempt_and_stales_selected_body() -> TestResult {
    let owner = source();
    let mut view = SessionProjection::new(owner.clone());
    view.begin_execution(Uuid::new_v4(), model()?)?;
    live(&mut view, complete("first-response-call", "keep this"))?;
    let first = live(&mut view, done())?
        .completed_attempt
        .ok_or("first response not completed")?;
    let committed = assistant(
        Vec::new(),
        vec![call(
            "first-response-call",
            "same-name",
            json!("keep this"),
            ToolCallKind::Custom,
        )],
    );
    view.reconcile_history_record(
        &first,
        &project_committed(&cursor(&owner, 0, &committed), &committed)?,
    )?;
    live(
        &mut view,
        ProviderEvent::TextDelta {
            text: "failed-attempt".to_owned(),
        },
    )?;
    let selected = view
        .items()
        .find(|row| {
            matches!(row.id, ItemId::Provisional(_)) && matches!(row.kind, ViewItemKind::Text)
        })
        .and_then(|row| row.bodies.first())
        .ok_or("partial body absent")?
        .clone();
    let retry = tagged(
        &view,
        AgentEventKind::StreamRetry(AgentStreamRetry {
            attempt: 2,
            max_attempts: Some(3),
            delay_ms: 25,
            error_class: "timeout".to_owned(),
        }),
    );
    view.apply_live(&retry)?;
    assert!(matches!(
        view.read_provisional(
            &selected,
            BodyRange {
                offset: 0,
                max_bytes: NonZeroUsize::new(64).ok_or("zero demand")?
            }
        ),
        Err(ViewError::StaleBody { .. })
    ));
    assert!(
        view.items()
            .any(|row| matches!(row.id, ItemId::Committed { .. })
                && matches!(row.kind, ViewItemKind::Tool(_)))
    );
    assert_eq!(
        view.current_attempt()
            .map(|attempt| (attempt.response, attempt.attempt)),
        Some((1, 2))
    );
    live(
        &mut view,
        ProviderEvent::TextDelta {
            text: "new attempt".to_owned(),
        },
    )?;
    view.end_execution(true)?;
    assert!(view.coverage().gaps.contains(&CoverageGap::Interrupted));
    Ok(())
}

#[test]
fn authoritative_items_replace_preview_and_commit_aliases_without_flat_duplicate() -> TestResult {
    let owner = source();
    let mut view = SessionProjection::new(owner.clone());
    let attempt = view.begin_execution(Uuid::new_v4(), model()?)?;
    live(
        &mut view,
        ProviderEvent::TextDelta {
            text: "preview".to_owned(),
        },
    )?;
    let item = response(
        json!({"type":"message","id":"message-one","role":"assistant","status":"completed","content":[{"type":"output_text","text":"authoritative","annotations":[],"logprobs":[]},{"type":"refusal","refusal":"refused"}]}),
    )?;
    live(
        &mut view,
        ProviderEvent::ResponseItemDone { item: item.clone() },
    )?;
    live(
        &mut view,
        ProviderEvent::TextComplete {
            text: "flat duplicate".to_owned(),
        },
    )?;
    assert_eq!(view.items().len(), 2);
    let previous: Vec<_> = view.items().map(|row| row.id.clone()).collect();
    let event = assistant(vec![item], Vec::new());
    view.reconcile_history_record(
        &attempt,
        &project_committed(&cursor(&owner, 0, &event), &event)?,
    )?;
    assert_eq!(view.items().len(), 2);
    assert!(previous.iter().all(|id| view.alias(id).is_some()));
    assert!(view.items().all(|row| {
        row.model
            .as_ref()
            .is_some_and(|model| model.model.as_str() == "fixture-model")
    }));
    view.mark_lagged(4)?;
    assert!(view.coverage().gaps.contains(&CoverageGap::BroadcastLag));
    assert_eq!(view.coverage().missed_live_events, 4);
    Ok(())
}

#[test]
fn all_provider_variants_have_exhaustive_safe_dispositions() -> TestResult {
    let raw = ResponseStreamEvent::from_raw(
        json!({"type":"response.created","sequence_number":0,"private_transport":"raw-secret"}),
    )?;
    let item = response(
        json!({"type":"message","id":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"visible","annotations":[],"logprobs":[]}]}),
    )?;
    let events = vec![
        ProviderEvent::ResponseStreamEvent {
            event: Box::new(raw.clone()),
        },
        ProviderEvent::ResponseAudioFrame {
            stream_event: Box::new(raw),
            event: ResponseAudioEvent::AudioDelta {
                sequence_number: 0,
                bytes: b"audio-secret".to_vec(),
            },
        },
        ProviderEvent::TextDelta {
            text: "text".to_owned(),
        },
        ProviderEvent::RefusalDelta {
            item_id: "refusal".to_owned(),
            output_index: 0,
            content_index: 0,
            refusal: "partial".to_owned(),
        },
        ProviderEvent::RefusalComplete {
            item_id: "refusal".to_owned(),
            output_index: 0,
            content_index: 0,
            refusal: "complete".to_owned(),
        },
        ProviderEvent::ThinkingDelta {
            text: "summary".to_owned(),
        },
        ProviderEvent::ToolCallDelta {
            item_id: "item".to_owned(),
            call_id: None,
            name: None,
            arguments_delta: "{".to_owned(),
            kind: ToolCallKind::Function,
        },
        ProviderEvent::TextComplete {
            text: "complete text".to_owned(),
        },
        ProviderEvent::ThinkingComplete {
            text: "complete summary".to_owned(),
        },
        ProviderEvent::ReasoningItemDone {
            item: ReasoningItem {
                id: "reasoning".to_owned(),
                summary: vec![ReasoningSummaryPart::SummaryText {
                    text: "safe summary".to_owned(),
                }],
                content: None,
                encrypted_content: Some("reasoning-secret".to_owned()),
            },
        },
        ProviderEvent::ResponseItemDone { item },
        complete("call", "freeform"),
        ProviderEvent::ToolResult {
            tool_call_id: "call".to_owned(),
            tool_name: "tool".to_owned(),
            output: json!({"ok":true}),
            duration_ms: 3,
        },
        ProviderEvent::Compaction {
            item_type: "compaction".to_owned(),
            encrypted_content: Some("compaction-secret".to_owned()),
        },
        done(),
        ProviderEvent::Error {
            error: crate::error::ProviderError::RateLimited { retry_after: None },
        },
    ];
    let mut dispositions = std::collections::BTreeSet::new();
    for event in events {
        let disposition = match &event {
            ProviderEvent::ResponseStreamEvent { .. } => "raw",
            ProviderEvent::ResponseAudioFrame { .. } => "audio",
            ProviderEvent::TextDelta { .. } => "text_delta",
            ProviderEvent::RefusalDelta { .. } => "refusal_delta",
            ProviderEvent::RefusalComplete { .. } => "refusal_complete",
            ProviderEvent::ThinkingDelta { .. } => "thinking_delta",
            ProviderEvent::ToolCallDelta { .. } => "tool_delta",
            ProviderEvent::TextComplete { .. } => "text_complete",
            ProviderEvent::ThinkingComplete { .. } => "thinking_complete",
            ProviderEvent::ReasoningItemDone { .. } => "reasoning",
            ProviderEvent::ResponseItemDone { .. } => "item",
            ProviderEvent::ToolCallComplete { .. } => "tool_complete",
            ProviderEvent::ToolResult { .. } => "result",
            ProviderEvent::Compaction { .. } => "compaction",
            ProviderEvent::Done { .. } => "done",
            ProviderEvent::Error { .. } => "error",
        };
        assert!(dispositions.insert(disposition));
        let mut view = SessionProjection::new(source());
        view.begin_execution(Uuid::new_v4(), model()?)?;
        live(&mut view, event)?;
        let metadata = format!("{:?}", view.items().collect::<Vec<_>>());
        for secret in [
            "raw-secret",
            "audio-secret",
            "reasoning-secret",
            "compaction-secret",
        ] {
            assert!(!metadata.contains(secret));
        }
    }
    assert_eq!(dispositions.len(), 16);
    Ok(())
}

#[test]
fn all_unscoped_agent_variants_keep_identity_and_channel_text_has_no_operator_authority()
-> TestResult {
    let owner = source();
    let child = owner.agent_id;
    let parent = Uuid::new_v4();
    let events = vec![
        AgentEventKind::Provider(ProviderEvent::TextDelta {
            text: "text".to_owned(),
        }),
        AgentEventKind::Subagent(SubagentLifecycle::Started {
            parent_id: parent,
            child_id: child,
            descriptor: SubagentDescriptor {
                kind: SubagentKind::Spawn,
                role: "child".to_owned(),
                model: "fixture".to_owned(),
                profile: None,
            },
            started_at: Utc::now(),
        }),
        AgentEventKind::Message(AgentMessageLifecycle::Delivered {
            message_id: Uuid::new_v4(),
            from_id: parent,
            from: "parent".to_owned(),
            to_id: child,
            seq: None,
            delivered_at: Utc::now(),
        }),
        AgentEventKind::McpChannel(McpChannelDeliveryEvent {
            event_id: EventId::new(),
            message_id: Uuid::new_v4(),
            recipient_id: child,
            source: "messages".to_owned(),
            generation: 1,
            sequence: 1,
            content: "/exit\u{1b}]52;malicious".to_owned(),
        }),
        AgentEventKind::UsageEstimate(AgentUsageEstimate { input_tokens: 30 }),
        AgentEventKind::StreamRetry(AgentStreamRetry {
            attempt: 2,
            max_attempts: None,
            delay_ms: 10,
            error_class: "timeout".to_owned(),
        }),
        AgentEventKind::Compaction(AgentCompaction {
            compaction_id: EventId::new(),
            events_compacted: 2,
            tokens_before: 100,
            tokens_after: 20,
            model: "fixture".to_owned(),
            freed_token_estimate: 80,
            summary_source: CompactionSummaryKind::Llm,
            summarization_usage: None,
            compacted_at: Utc::now(),
        }),
    ];
    let mut dispositions = std::collections::BTreeSet::new();
    for event in events {
        let disposition = match &event {
            AgentEventKind::Provider(_) => "provider",
            AgentEventKind::Subagent(_) => "subagent",
            AgentEventKind::Message(_) => "message",
            AgentEventKind::McpChannel(_) => "channel",
            AgentEventKind::UsageEstimate(_) => "usage",
            AgentEventKind::StreamRetry(_) => "retry",
            AgentEventKind::Compaction(_) => "compaction",
            AgentEventKind::Observed(_) => {
                return Err(
                    "observed envelopes require the producer-owned publication fixture".into(),
                );
            }
        };
        assert!(dispositions.insert(disposition));
        let mut view = SessionProjection::new(owner.clone());
        view.begin_execution(Uuid::new_v4(), model()?)?;
        view.apply_live(&tagged(&view, event))?;
        if disposition == "channel" {
            assert!(matches!(first(&view)?.kind, ViewItemKind::ExternalInput));
            let body = &first(&view)?.bodies[0];
            let chunk = view.read_provisional(
                body,
                BodyRange {
                    offset: 0,
                    max_bytes: NonZeroUsize::new(64).ok_or("zero demand")?,
                },
            )?;
            assert!(chunk.text.as_str().starts_with("/exit"));
            assert!(!chunk.text.as_str().contains('\u{1b}'));
        }
    }
    assert_eq!(dispositions.len(), 7);
    Ok(())
}

#[test]
fn observed_event_gaps_keep_later_commits_after_prior_local_errors_and_completions() -> TestResult {
    let owner = source();
    let mut view = SessionProjection::new(owner.clone());
    let initial = SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "first".to_owned(),
    };
    view.apply_history_record(&project_committed(&cursor(&owner, 0, &initial), &initial)?)?;
    let error = view.record_local_body(
        ViewItemKind::Error,
        "prior error",
        "retained details",
        super::BodyRepresentation::Text,
    )?;
    let old_body = view.item(&error).ok_or("missing error")?.bodies[0].clone();
    let complete = view.record_notice(ViewItemKind::Notice, "prior completion")?;
    let next = SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "later prompt".to_owned(),
    };
    let next_record = project_committed(&cursor(&owner, 1, &next), &next)?;
    view.apply_history_record(&next_record)?;
    let response = assistant(Vec::new(), Vec::new());
    let response_record = project_committed(&cursor(&owner, 2, &response), &response)?;
    view.apply_history_record(&response_record)?;
    let final_notice = view.record_notice(ViewItemKind::Notice, "later completion")?;
    let later: Vec<_> = view
        .items_from(&error, ItemDirection::Later, ItemInclusion::Inclusive)?
        .map(|row| row.id.clone())
        .collect();
    let expected: Vec<_> = [error.clone(), complete]
        .into_iter()
        .chain(next_record.items().iter().map(|row| row.id.clone()))
        .chain(response_record.items().iter().map(|row| row.id.clone()))
        .chain([final_notice])
        .collect();
    assert_eq!(later, expected);
    assert_eq!(
        view.item(&error).ok_or("lost pinned error")?.bodies[0],
        old_body
    );
    assert_eq!(
        view.read_provisional(
            &old_body,
            BodyRange {
                offset: 0,
                max_bytes: NonZeroUsize::new(32).ok_or("zero demand")?,
            }
        )?
        .original_text,
        "retained details"
    );
    Ok(())
}

#[test]
fn tail_zero_display_records_advance_observed_gap_and_older_pages_never_rewind_it() -> TestResult {
    let owner = source();
    let mut view = SessionProjection::new(owner.clone());
    let metadata = SessionEvent::Custom {
        base: EventBase::new(None),
        event_type: "metadata".to_owned(),
        data: json!({}),
    };
    let mut tail = project_committed(&cursor(&owner, 100, &metadata), &metadata)?;
    // Exercise the compact-record contract independently of today's metadata
    // display policy: an owner may supply a valid cursor without visible rows.
    tail.items.clear();
    view.apply_history_record(&tail)?;
    let first = view.record_notice(ViewItemKind::Notice, "after initial tail")?;
    let old = SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "older".to_owned(),
    };
    let older = project_committed(&cursor(&owner, 2, &old), &old)?;
    view.apply_history_record(&older)?;
    let second = view.record_notice(ViewItemKind::Notice, "after older page")?;
    let next = SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "next".to_owned(),
    };
    let next_record = project_committed(&cursor(&owner, 101, &next), &next)?;
    view.apply_history_record(&next_record)?;
    assert_eq!(
        view.items().map(|row| row.id.clone()).collect::<Vec<_>>(),
        vec![
            older.items()[0].id.clone(),
            first,
            second,
            next_record.items()[0].id.clone()
        ]
    );
    Ok(())
}

#[test]
fn exact_completion_rekey_visits_only_its_bucket_and_keeps_ids_bodies_and_memberships() -> TestResult
{
    let owner = source();
    let mut view = SessionProjection::new(owner.clone());
    let execution = Uuid::new_v4();
    let mut completions = Vec::new();
    for response in 0..256 {
        let attempt = super::AttemptKey {
            execution,
            response,
            attempt: 1,
        };
        let id = view.record_local_body(
            ViewItemKind::Notice,
            "done",
            "exact detail",
            super::BodyRepresentation::Text,
        )?;
        view.items.bind_completion(&id, &attempt)?;
        completions.push((attempt, id));
    }
    let (attempt, selected) = &completions[137];
    let before = view
        .item(selected)
        .ok_or("missing selected completion")?
        .clone();
    // Updating the exact existing row must retain its completion membership.
    view.items.insert(before.clone())?;
    let event = assistant(Vec::new(), Vec::new());
    let position = cursor(&owner, 20, &event);
    view.apply_history_record(&project_committed(&position, &event)?)?;
    view.items.completion_relocations.set(0);
    view.items.place_completions_after(attempt, &position)?;
    assert_eq!(view.items.completion_relocations.get(), 1);
    assert_eq!(view.items().next_back().map(|row| &row.id), Some(selected));
    let after = view.item(selected).ok_or("lost completion")?;
    assert_eq!(after.id, before.id);
    assert_eq!(after.bodies, before.bodies);
    assert_eq!(
        view.read_provisional(
            &after.bodies[0],
            BodyRange {
                offset: 0,
                max_bytes: NonZeroUsize::new(32).ok_or("zero demand")?,
            }
        )?
        .original_text,
        "exact detail"
    );
    view.items.place_completions_after(attempt, &position)?;
    assert_eq!(view.items.completion_relocations.get(), 1);
    for (remaining, item) in &completions {
        if item != selected {
            view.items.place_completions_after(remaining, &position)?;
        }
    }
    assert_eq!(view.items.completion_relocations.get(), completions.len());
    assert_eq!(
        view.items()
            .filter(|row| matches!(row.kind, ViewItemKind::Notice))
            .count(),
        completions.len()
    );
    Ok(())
}

#[test]
fn routine_live_metadata_stays_retained_while_errors_refusals_and_external_input_stay_distinct()
-> TestResult {
    let mut view = SessionProjection::new(source());
    view.begin_execution(Uuid::new_v4(), model()?)?;
    view.apply_live(&tagged(
        &view,
        AgentEventKind::UsageEstimate(AgentUsageEstimate { input_tokens: 73 }),
    ))?;
    let completion = live(&mut view, done())?
        .completion_item
        .ok_or("missing retained Done item")?;
    assert!(matches!(
        view.item(&completion).ok_or("missing Done metadata")?.kind,
        ViewItemKind::Metadata
    ));
    assert_eq!(
        view.items()
            .filter(|row| matches!(row.kind, ViewItemKind::Metadata))
            .count(),
        2
    );
    assert!(view.items().any(|row| row.label.as_str().contains("73")));
    live(
        &mut view,
        ProviderEvent::Error {
            error: crate::error::ProviderError::RateLimited { retry_after: None },
        },
    )?;
    live(
        &mut view,
        ProviderEvent::RefusalComplete {
            item_id: "refusal".to_owned(),
            output_index: 0,
            content_index: 0,
            refusal: "cannot complete".to_owned(),
        },
    )?;
    view.apply_live(&tagged(
        &view,
        AgentEventKind::McpChannel(McpChannelDeliveryEvent {
            event_id: EventId::new(),
            message_id: Uuid::new_v4(),
            recipient_id: view.source().agent_id,
            source: "messages".to_owned(),
            generation: 1,
            sequence: 1,
            content: "external message".to_owned(),
        }),
    ))?;
    assert!(
        view.items()
            .any(|row| matches!(row.kind, ViewItemKind::Error))
    );
    assert!(
        view.items()
            .any(|row| matches!(row.kind, ViewItemKind::Refusal))
    );
    assert!(
        view.items()
            .any(|row| matches!(row.kind, ViewItemKind::ExternalInput))
    );
    Ok(())
}

fn description_projection_tools(
    raw: &str,
    kind: ToolCallKind,
    legacy: Option<serde_json::Value>,
) -> Result<Vec<super::ToolView>, Box<dyn std::error::Error>> {
    let owner = source();
    let item = response(match kind {
        ToolCallKind::Function => json!({
            "type":"function_call", "id":"item", "call_id":"description-call",
            "name":"edit", "arguments":raw
        }),
        ToolCallKind::Custom => json!({
            "type":"custom_tool_call", "id":"item", "call_id":"description-call",
            "name":"edit", "input":raw
        }),
    })?;
    let mut tools = Vec::new();
    for event in [
        ProviderEvent::ToolCallComplete {
            call_id: "description-call".to_owned(),
            name: "edit".to_owned(),
            arguments: raw.to_owned(),
            kind,
        },
        ProviderEvent::ResponseItemDone { item: item.clone() },
    ] {
        let mut view = SessionProjection::new(owner.clone());
        view.begin_execution(Uuid::new_v4(), model()?)?;
        live(&mut view, event)?;
        let tool = projected_description_tool(&view)?;
        let original = view.read_provisional(
            tool.arguments.as_ref().ok_or("live argument body absent")?,
            BodyRange {
                offset: 0,
                max_bytes: NonZeroUsize::new(raw.len()).ok_or("empty fixture arguments")?,
            },
        )?;
        assert_eq!(original.original_text, raw);
        tools.push(tool.clone());
    }
    let modern = assistant(vec![item], Vec::new());
    let legacy = legacy.map(|arguments| {
        assistant(
            Vec::new(),
            vec![call("description-call", "edit", arguments, kind)],
        )
    });
    for event in std::iter::once(modern).chain(legacy) {
        let mut view = SessionProjection::new(owner.clone());
        view.apply_history_record(&project_committed(&cursor(&owner, 0, &event), &event)?)?;
        tools.push(projected_description_tool(&view)?.clone());
    }
    Ok(tools)
}

fn projected_description_tool(
    view: &SessionProjection,
) -> Result<&super::ToolView, Box<dyn std::error::Error>> {
    let mut tools = view.items().filter_map(|item| match &item.kind {
        ViewItemKind::Tool(tool) => Some(tool.as_ref()),
        _ => None,
    });
    let tool = tools.next().ok_or("projected tool absent")?;
    assert!(
        tools.next().is_none(),
        "one invocation became multiple tool rows"
    );
    assert!(
        tool.arguments.is_some(),
        "description extraction lost original arguments"
    );
    Ok(tool)
}

#[test]
fn tool_envelope_description_wins_over_ordinary_argument_in_every_projection() -> TestResult {
    let original = "  inspect configuration\nthen edit 🦀  ";
    let arguments = json!({
        "description":"ordinary argument must not become the row",
        "tool_use_description": original,
        "path":"config.json"
    });
    let tools = description_projection_tools(
        &arguments.to_string(),
        ToolCallKind::Function,
        Some(arguments),
    )?;
    assert_eq!(
        tools.len(),
        4,
        "native live, Responses live, modern replay and legacy replay"
    );
    for tool in tools {
        assert_eq!(
            tool.description.as_ref().map(super::DisplayText::as_str),
            Some(original)
        );
        assert!(tool.description_error.is_none());
    }
    Ok(())
}

#[test]
fn missing_empty_and_invalid_envelope_descriptions_remain_distinct() -> TestResult {
    for (arguments, expected_error) in [
        (json!({"description":"not envelope metadata"}), None),
        (
            json!({"tool_use_description":"", "description":"not fallback"}),
            Some("is empty"),
        ),
        (
            json!({"tool_use_description":" \n\t ", "description":"not fallback"}),
            Some("is empty"),
        ),
        (
            json!({"tool_use_description":null}),
            Some("must be a string"),
        ),
        (
            json!({"tool_use_description":false}),
            Some("must be a string"),
        ),
        (json!({"tool_use_description":42}), Some("must be a string")),
        (
            json!({"tool_use_description":["PRIVATE_DESCRIPTION_VALUE"]}),
            Some("must be a string"),
        ),
        (
            json!({"tool_use_description":{"secret":"PRIVATE_DESCRIPTION_VALUE"}}),
            Some("must be a string"),
        ),
        (
            json!(["PRIVATE_DESCRIPTION_VALUE"]),
            Some("must be an object"),
        ),
    ] {
        let tools = description_projection_tools(
            &arguments.to_string(),
            ToolCallKind::Function,
            Some(arguments),
        )?;
        assert_eq!(tools.len(), 4);
        for tool in tools {
            assert!(tool.description.is_none());
            match (expected_error, tool.description_error.as_ref()) {
                (None, None) => {}
                (Some(expected), Some(error)) => {
                    assert!(error.as_str().contains(expected));
                    assert!(error.as_str().contains("tool_use_description"));
                    assert!(!error.as_str().contains("PRIVATE_DESCRIPTION_VALUE"));
                }
                pair => return Err(format!("wrong description availability: {pair:?}").into()),
            }
        }
    }
    Ok(())
}

#[test]
fn invalid_serialized_function_arguments_are_diagnostic_in_live_and_modern_replay() -> TestResult {
    let tools = description_projection_tools("{broken", ToolCallKind::Function, None)?;
    assert_eq!(tools.len(), 3);
    for tool in tools {
        assert!(tool.description.is_none());
        assert!(
            tool.description_error
                .as_ref()
                .is_some_and(|error| error.as_str().contains("cannot read tool_use_description"))
        );
    }
    Ok(())
}

#[test]
fn custom_payloads_never_become_envelope_descriptions() -> TestResult {
    for raw in [
        r#"{"tool_use_description":"not metadata","description":"also not metadata"}"#,
        "freeform {broken text",
    ] {
        let tools = description_projection_tools(raw, ToolCallKind::Custom, Some(json!(raw)))?;
        assert_eq!(tools.len(), 4);
        for tool in tools {
            assert!(tool.description.is_none());
            assert!(tool.description_error.is_none());
            assert_eq!(tool.kind, Some(ToolCallKind::Custom));
        }
    }
    Ok(())
}
