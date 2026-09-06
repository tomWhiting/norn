//! Visible semantic rows and demanded-body display cache; no persistence or terminal I/O.

use std::sync::Arc;

use norn::session_view::{BodyRef, ItemDirection, ItemInclusion, ViewItem, ViewItemKind};

use crate::TuiError;
use crate::app::state::AppState;
use crate::app::viewport::{AnchorPosition, AnchorState, ViewAnchor};
use crate::render::frame::{Frame, PaintRow};
use crate::render::layout::Rect;
use crate::render::retained_markdown::{RenderedMarkdown, SourceMapping};
use crate::render::retained_text::TextRow;

use super::transcript_items::{RowGroup, item_groups};
use super::{interaction, push_text};

type LogicalRow = (ViewAnchor, Arc<RenderedMarkdown>, TextRow, Option<BodyRef>);

pub(super) fn conversation(
    state: &mut AppState,
    frame: &mut Frame,
    area: Rect,
) -> Result<(), TuiError> {
    let status = super::composer::activity_status(state);
    let status_area = status.as_ref().filter(|_| area.height > 1).map(|_| Rect {
        row: area.row + area.height - 1,
        height: 1,
        ..area
    });
    let area = Rect {
        height: area.height.saturating_sub(u16::from(status_area.is_some())),
        ..area
    };
    let reconciliation = state
        .screen
        .viewport
        .reconcile(&state.transcript.projection)
        .map_err(interaction)?;
    if matches!(
        reconciliation.anchor,
        Some(AnchorState::BodyStale | AnchorState::ItemUnavailable)
    ) {
        push_text(
            frame,
            "Pinned content revision is no longer current. Its original selection remains pinned; use /view follow to return to live content.",
            area,
            false,
            false,
        )?;
        return Ok(());
    }
    super::navigation::apply(state)?;
    let anchor = state.screen.viewport.anchor().cloned();
    let follows = state.screen.viewport.follows_tail();
    let mut visible = window(
        state,
        area.width,
        if follows { None } else { anchor.as_ref() },
        follows,
        usize::from(area.height),
        false,
    )?;
    if follows {
        visible.reverse();
    }
    for (index, (anchor, text, geometry, body)) in visible.into_iter().enumerate() {
        let selected = state.screen.viewport.selected() == Some(&anchor.item);
        let input = state
            .transcript
            .projection
            .item(&anchor.item)
            .is_some_and(|item| matches!(item.kind, ViewItemKind::Input));
        let area = if input && body.is_some() {
            super::composer::input_margin(
                frame,
                area,
                index,
                matches!(
                    &anchor.position,
                    AnchorPosition::Body {
                        original_offset: 0,
                        ..
                    }
                ),
            )?
        } else {
            area
        };
        let selection = super::hit::selection_ranges(
            state,
            &anchor.item,
            body.as_ref(),
            &text,
            geometry.bytes(),
        );
        state.screen.hit_rows.push(super::hit::HitRow {
            area,
            row: u16::try_from(index).map_err(|source| TuiError::FrameCoordinate {
                value: index,
                source,
            })?,
            anchor: anchor.clone(),
            body,
            text: Arc::clone(&text),
            geometry: geometry.clone(),
        });
        state.screen.visible.push(anchor);
        frame.rows.push(PaintRow {
            area,
            row: u16::try_from(index).map_err(|source| TuiError::FrameCoordinate {
                value: index,
                source,
            })?,
            text,
            geometry,
            selected,
            selection,
            composer: false,
        });
    }
    if let (Some(status), Some(area)) = (status, status_area) {
        push_text(frame, &status, area, false, false)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct RowWindow<'a> {
    anchor: Option<&'a ViewAnchor>,
    cached_position: Option<(usize, usize)>,
    backwards: bool,
    limit: usize,
    exclusive: bool,
}

fn window(
    state: &mut AppState,
    columns: u16,
    anchor: Option<&ViewAnchor>,
    backwards: bool,
    limit: usize,
    exclusive: bool,
) -> Result<Vec<LogicalRow>, TuiError> {
    let items: Box<dyn Iterator<Item = &ViewItem> + '_> = if let Some(anchor) = anchor {
        Box::new(state.transcript.projection.items_from(
            &anchor.item,
            if backwards {
                ItemDirection::Earlier
            } else {
                ItemDirection::Later
            },
            ItemInclusion::Inclusive,
        )?)
    } else if backwards {
        Box::new(state.transcript.projection.items().rev())
    } else {
        Box::new(state.transcript.projection.items())
    };
    let selected = state.screen.viewport.selected().cloned();
    let visible = |item: &ViewItem| {
        !(matches!(item.kind, ViewItemKind::Metadata) && !state.transcript.config.expanded_tools
            || matches!(item.kind, ViewItemKind::Thinking)
                && !state.display_toggles.thinking_visible
            || state.transcript.completion_hidden(&item.id)
                && selected.as_ref() != Some(&item.id)
                && anchor.is_none_or(|anchor| anchor.item != item.id))
    };
    let mut has_earlier = if let Some(anchor) = anchor {
        state
            .transcript
            .projection
            .items_from(
                &anchor.item,
                ItemDirection::Earlier,
                ItemInclusion::Exclusive,
            )?
            .any(visible)
    } else {
        false
    };
    let mut items = items.filter(|item| visible(item)).peekable();
    let mut rows = Vec::new();
    while let Some(item) = items.next() {
        let separator = if backwards {
            items.peek().is_some()
        } else {
            has_earlier
        };
        has_earlier = true;
        let groups = item_groups(
            &state.transcript,
            &mut state.screen,
            item,
            if matches!(item.kind, ViewItemKind::Input) {
                columns.saturating_sub(2).max(1)
            } else {
                columns
            },
            state.display_toggles.secondary_fields_visible,
            separator,
        )?;
        let requested = RowWindow {
            anchor: anchor.filter(|anchor| anchor.item == item.id),
            cached_position: super::navigation::locate_cursor(
                &state.screen,
                item,
                &groups,
                columns,
            ),
            backwards,
            limit: limit.saturating_sub(rows.len()),
            exclusive,
        };
        collect_rows(
            &groups,
            item,
            requested,
            &mut rows,
            &mut state.screen.demands,
        );
        if rows.len() == limit {
            break;
        }
    }
    Ok(rows)
}

fn collect_rows(
    groups: &[RowGroup],
    item: &ViewItem,
    requested: RowWindow<'_>,
    output: &mut Vec<LogicalRow>,
    demands: &mut Vec<(norn::session_view::ItemId, BodyRef)>,
) {
    let RowWindow {
        anchor,
        cached_position,
        backwards,
        limit,
        exclusive,
    } = requested;
    let anchor_position = cached_position
        .or_else(|| anchor.and_then(|anchor| locate_anchor(groups, &anchor.position)));
    // A headerless first item has a virtual start before its first original row.
    if backwards
        && anchor_position.is_none()
        && anchor.is_some_and(|anchor| {
            matches!(
                anchor.position,
                AnchorPosition::Header | AnchorPosition::BeforeItem
            )
        })
    {
        return;
    }
    let group_order: Box<dyn Iterator<Item = (usize, &RowGroup)> + '_> = if backwards {
        Box::new(groups.iter().enumerate().rev())
    } else {
        Box::new(groups.iter().enumerate())
    };
    let mut added = 0;
    for (group_index, group) in group_order {
        let (start, end) = match anchor_position {
            Some((anchor_group, anchor_row)) if group_index == anchor_group => {
                if backwards {
                    (0, anchor_row)
                } else {
                    (
                        anchor_row
                            .saturating_add(usize::from(exclusive))
                            .min(group.rows.len()),
                        group.rows.len(),
                    )
                }
            }
            Some((anchor_group, _))
                if (backwards && group_index > anchor_group)
                    || (!backwards && group_index < anchor_group) =>
            {
                continue;
            }
            _ => (0, group.rows.len()),
        };
        let rows: Box<dyn Iterator<Item = &TextRow> + '_> = if backwards {
            Box::new(group.rows[start..end].iter().rev())
        } else {
            Box::new(group.rows[start..end].iter())
        };
        for row in rows {
            if added == limit {
                return;
            }
            if let Some(reference) = &group.reference {
                let demand = (item.id.clone(), reference.clone());
                if !demands.contains(&demand) {
                    demands.push(demand);
                }
            }
            output.push((
                ViewAnchor {
                    item: item.id.clone(),
                    position: row_position(group, row),
                },
                Arc::clone(&group.text),
                row.clone(),
                group
                    .reference
                    .clone()
                    .filter(|_| group.fixed_offset.is_none()),
            ));
            added += 1;
        }
    }
}

pub(super) fn locate_anchor(
    groups: &[RowGroup],
    position: &AnchorPosition,
) -> Option<(usize, usize)> {
    match position {
        AnchorPosition::BeforeItem => groups
            .iter()
            .position(|group| group.before_item)
            .map(|index| (index, 0)),
        AnchorPosition::Header => groups
            .iter()
            .position(|group| group.reference.is_none() && !group.before_item)
            .or_else(|| groups.iter().position(|group| group.before_item))
            .map(|index| (index, 0)),
        AnchorPosition::Body {
            reference,
            original_offset,
        } => {
            // The actual original body precedes any continuation notice for the same ref.
            let (group_index, group) = groups
                .iter()
                .enumerate()
                .find(|(_, group)| group.reference.as_ref() == Some(reference))?;
            let row_index = group.rows.partition_point(|row| matches!(row_position(group,row),AnchorPosition::Body { original_offset: offset, .. } if offset <= *original_offset)).saturating_sub(1);
            Some((group_index, row_index))
        }
    }
}

pub(super) fn row_position(group: &RowGroup, row: &TextRow) -> AnchorPosition {
    if group.before_item {
        return AnchorPosition::BeforeItem;
    }
    let Some(reference) = &group.reference else {
        return AnchorPosition::Header;
    };
    if let Some(original_offset) = group.fixed_offset {
        return AnchorPosition::Body {
            reference: reference.clone(),
            original_offset,
        };
    }
    let bytes = row.bytes();
    let first_span = group
        .text
        .spans
        .partition_point(|span| span.display.end <= bytes.start);
    let offset = group.text.spans[first_span..]
        .iter()
        .take_while(|span| span.display.start < bytes.end)
        .find_map(|span| {
            if span.display.end <= bytes.start || span.display.start >= bytes.end {
                return None;
            }
            match &span.source {
                SourceMapping::Exact { original } => {
                    Some(original.start + bytes.start.saturating_sub(span.display.start))
                }
                SourceMapping::Transformed { original } => Some(original.start),
                SourceMapping::Generated => None,
            }
        });
    // Generated-only blank rows attach to the following/preceding original boundary,
    // never become selected original bytes themselves (selection uses the full map).
    let original_offset = offset
        .or_else(|| {
            group.text.spans[..first_span]
                .iter()
                .rev()
                .find_map(|span| {
                    if span.display.end > bytes.start {
                        return None;
                    }
                    match &span.source {
                        SourceMapping::Exact { original }
                        | SourceMapping::Transformed { original } => Some(original.end),
                        SourceMapping::Generated => None,
                    }
                })
        })
        .unwrap_or(0);
    AnchorPosition::Body {
        reference: reference.clone(),
        original_offset,
    }
}

#[cfg(test)]
mod tests {
    use super::super::ScreenState;
    use super::*;
    use crate::app::transcript::{LoadedBody, Transcript};
    use norn::provider::request::{ToolCallCaller, ToolCallKind};
    use norn::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
    use norn::session::{EventStore, SessionBinding};
    use uuid::Uuid;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    #[test]
    fn coalesced_input_preserves_reads_until_current_frame_demands_exist() -> TestResult {
        let store = Arc::new(EventStore::new());
        let source =
            store.bind_view_source(&SessionBinding::ephemeral_root(), Uuid::new_v4(), None)?;
        let mut state = AppState::new(
            crate::terminal::caps::TerminalCaps::baseline(),
            crate::input::history::InputHistory::in_memory(),
            norn::agent::registry::AgentRegistry::shared(),
            source,
            crate::render::fixed_panel::StatusBar::default(),
        );
        let item = crate::app::notices::notice(
            &mut state,
            "Selected completion",
            Some("Exact source and usage details"),
        )?;
        let reference = state
            .transcript
            .projection
            .item(&item)
            .and_then(|item| item.bodies.first())
            .ok_or("notice body absent")?
            .clone();
        state
            .screen
            .viewport
            .select(item, &state.transcript.projection)?;
        state.screen.terminal_event(1);
        state.screen.request_older = true;
        state.screen.request_more = true;
        super::super::load_visible(&mut state, &store)?;
        assert!(state.screen.request_older);
        assert!(state.screen.request_more);
        assert!(state.screen.allow_body_load);
        assert!(state.transcript.history_tasks.is_empty());
        assert!(state.transcript.body_tasks.is_empty());
        assert!(state.transcript.body(&reference).is_none());

        crate::app::event_loop::insert_paste_text(&mut state, "draft survives")?;
        // Arrivals beyond the captured frontier cannot postpone this frame.
        state.screen.terminal_event(99);
        assert_eq!(state.screen.ready_batch_remaining, 0);
        let frame = super::super::prepare(&mut state, 80, 24)?;
        assert!(!frame.rows.is_empty());
        assert!(
            state
                .screen
                .demands
                .iter()
                .any(|(_, body)| body == &reference)
        );
        super::super::load_visible(&mut state, &store)?;
        assert!(!state.screen.request_older);
        assert!(!state.screen.request_more);
        assert!(!state.screen.allow_body_load);
        assert_eq!(state.input_editor.text(), "draft survives");
        assert_eq!(
            state
                .transcript
                .body(&reference)
                .ok_or("selected body not loaded")?
                .original,
            "Exact source and usage details"
        );
        Ok(())
    }

    fn tool_fixture() -> TestResult<(Transcript, ViewItem)> {
        let store = EventStore::new();
        let source =
            store.bind_view_source(&SessionBinding::ephemeral_root(), Uuid::new_v4(), None)?;
        let base = EventBase::new(None);
        let parent = base.id.clone();
        store.append(SessionEvent::AssistantMessage {
            base, response_items: Vec::new(), content: String::new(), thinking: String::new(), reasoning: Vec::new(),
            tool_calls: vec![ToolCallEvent { call_id: "call".to_owned(), name: "edit".to_owned(), arguments: serde_json::json!({"path":"source.rs","old_string":"abc","new_string":"xyz","tool_use_description":"A deliberately long descriptive tool header that must never consume all terminal rows"}), kind: ToolCallKind::Function, caller: ToolCallCaller::Absent }],
            usage: EventUsage::default(), stop_reason: "tool_use".to_owned(), response_id: None,
        })?;
        store.append(SessionEvent::ToolResult {
            base: EventBase::new(Some(parent)),
            tool_call_id: "call".to_owned(),
            tool_name: "edit".to_owned(),
            output: serde_json::json!({"committed":true,"path":"source.rs"}),
            spool_ref: None,
            duration_ms: 7,
        })?;
        let mut transcript = Transcript::new(source);
        transcript.accept_history(&store.history_page(&transcript.initial_history()?)?)?;
        let item = transcript
            .projection
            .items()
            .find(|item| matches!(item.kind, ViewItemKind::Tool(_)))
            .ok_or("tool absent")?
            .clone();
        assert_eq!(item.bodies.len(), 2);
        for reference in &item.bodies {
            let demand = transcript
                .demand_body(&item.id, reference, false)?
                .ok_or("demand absent")?;
            transcript.accept_body(&demand, LoadedBody::from(store.read_body(&demand.read)?))?;
        }
        Ok((transcript, item))
    }

    #[test]
    fn backwards_argument_anchor_excludes_later_result_body() -> TestResult {
        let (transcript, item) = tool_fixture()?;
        let mut screen = ScreenState::new(transcript.projection.source().clone());
        screen.tool_overrides.insert(item.id.clone(), true);
        let groups = item_groups(&transcript, &mut screen, &item, 12, false, false)?;
        let argument_group = groups.get(1).ok_or("argument group absent")?;
        let anchor = ViewAnchor {
            item: item.id.clone(),
            position: row_position(
                argument_group,
                argument_group
                    .rows
                    .get(2)
                    .ok_or("wrapped argument absent")?,
            ),
        };
        let mut rows = Vec::new();
        let mut demands = Vec::new();
        collect_rows(
            &groups,
            &item,
            RowWindow {
                cached_position: None,
                anchor: Some(&anchor),
                backwards: true,
                limit: 4,
                exclusive: true,
            },
            &mut rows,
            &mut demands,
        );
        assert!(!rows.is_empty());
        for (position, _, _, _) in rows {
            if let AnchorPosition::Body { reference, .. } = position.position {
                assert_eq!(reference, item.bodies[0]);
            }
        }
        assert!(
            !demands
                .iter()
                .any(|(_, reference)| reference == &item.bodies[1])
        );
        Ok(())
    }

    #[test]
    fn header_is_one_row_and_body_anchor_survives_reflow() -> TestResult {
        let (transcript, item) = tool_fixture()?;
        let mut screen = ScreenState::new(transcript.projection.source().clone());
        screen.tool_overrides.insert(item.id.clone(), true);
        let narrow = item_groups(&transcript, &mut screen, &item, 8, false, false)?;
        assert_eq!(narrow[0].rows.len(), 1);
        let anchor = row_position(
            &narrow[1],
            narrow[1].rows.get(3).ok_or("narrow row absent")?,
        );
        let wide = item_groups(&transcript, &mut screen, &item, 18, false, false)?;
        assert_eq!(wide[0].rows.len(), 1);
        let (group, row) = locate_anchor(&wide, &anchor).ok_or("reflow anchor absent")?;
        assert_eq!(group, 1);
        let AnchorPosition::Body {
            reference,
            original_offset,
        } = anchor
        else {
            return Err("expected body anchor".into());
        };
        let AnchorPosition::Body {
            reference: actual,
            original_offset: actual_offset,
        } = row_position(&wide[group], &wide[group].rows[row])
        else {
            return Err("expected mapped body".into());
        };
        assert_eq!(actual, reference);
        assert!(actual_offset <= original_offset);
        Ok(())
    }

    #[test]
    fn unchanged_body_and_width_reuse_wrapped_geometry() -> TestResult {
        let (transcript, item) = tool_fixture()?;
        let mut screen = ScreenState::new(transcript.projection.source().clone());
        screen.tool_overrides.insert(item.id.clone(), true);
        let first = item_groups(&transcript, &mut screen, &item, 40, false, false)?;
        let second = item_groups(&transcript, &mut screen, &item, 40, false, false)?;
        assert!(Arc::ptr_eq(&first[1].text, &second[1].text));
        assert!(Arc::ptr_eq(&first[1].rows, &second[1].rows));
        let mut visible = Vec::new();
        let mut demands = Vec::new();
        collect_rows(
            &second,
            &item,
            RowWindow {
                cached_position: None,
                anchor: None,
                backwards: true,
                limit: 2,
                exclusive: false,
            },
            &mut visible,
            &mut demands,
        );
        assert_eq!(visible.len(), 2);
        Ok(())
    }
    #[test]
    fn one_row_navigation_advances_past_header_and_repeated_body_rows() -> TestResult {
        let (transcript, item) = tool_fixture()?;
        let mut screen = ScreenState::new(transcript.projection.source().clone());
        screen.tool_overrides.insert(item.id.clone(), true);
        let groups = item_groups(&transcript, &mut screen, &item, 8, false, false)?;
        let mut anchor = ViewAnchor {
            item: item.id.clone(),
            position: AnchorPosition::Header,
        };
        let mut positions = Vec::new();
        for _ in 0..10 {
            let mut rows = Vec::new();
            let mut demands = Vec::new();
            collect_rows(
                &groups,
                &item,
                RowWindow {
                    cached_position: None,
                    anchor: Some(&anchor),
                    backwards: false,
                    limit: 1,
                    exclusive: true,
                },
                &mut rows,
                &mut demands,
            );
            let (next, _, _, _) = rows.first().ok_or("one-row move did not advance")?;
            assert_ne!(next, &anchor);
            assert!(!positions.contains(next));
            positions.push(next.clone());
            anchor = next.clone();
        }
        Ok(())
    }

    fn readable_state() -> TestResult<AppState> {
        let (mut transcript, tool) = tool_fixture()?;
        transcript.config.expanded_tools = false;
        let mut failed = tool.kind;
        if let ViewItemKind::Tool(tool) = &mut failed {
            tool.state = norn::session_view::ToolState::Failed;
            tool.result_state = Some(norn::session_view::ToolState::Failed);
        }
        transcript.notice(failed, "", None)?;
        for (kind, label, body) in [
            (ViewItemKind::Text, "Assistant", "Answer in ordinary prose"),
            (
                ViewItemKind::Thinking,
                "Thinking",
                "Consider **carefully** and `code`",
            ),
            (ViewItemKind::Error, "Error", "Visible failure detail"),
            (ViewItemKind::Input, "You", "Original α input"),
        ] {
            let id = transcript.notice(kind, label, Some(body))?;
            let reference = transcript
                .projection
                .item(&id)
                .and_then(|item| item.bodies.first())
                .ok_or("readability body absent")?
                .clone();
            let demand = transcript
                .demand_body(&id, &reference, false)?
                .ok_or("local demand absent")?;
            let page = transcript.read_local_body(&demand)?;
            assert!(transcript.accept_body(&demand, page)?);
        }
        let mut caps = crate::terminal::caps::TerminalCaps::baseline();
        caps.true_colour = true;
        caps.italic_support = true;
        let mut state = AppState::new(
            caps,
            crate::input::history::InputHistory::in_memory(),
            norn::agent::registry::AgentRegistry::shared(),
            transcript.projection.source().clone(),
            crate::render::fixed_panel::StatusBar::default(),
        );
        state.transcript = transcript;
        Ok(state)
    }

    #[test]
    fn mixed_frame_restores_spacing_typed_colours_and_full_width_composer() -> TestResult {
        let mut state = readable_state()?;
        let frame = super::super::prepare(&mut state, 100, 40)?;
        let crate::render::layout::Layout::Ready { composer, .. } = frame.layout else {
            return Err("readability fixture has no composer".into());
        };
        assert_eq!(composer.column, 0);
        assert_eq!(composer.width, 100);
        let items = state.transcript.projection.items().count();
        let separators: Vec<_> = state
            .screen
            .hit_rows
            .iter()
            .filter(|row| row.anchor.position == AnchorPosition::BeforeItem)
            .collect();
        assert_eq!(separators.len(), items - 1);
        assert!(separators.iter().all(|row| row.body.is_none()
            && row.text.spans.is_empty()
            && row.geometry.bytes().is_empty()));
        let first = state
            .screen
            .hit_rows
            .first()
            .ok_or("first visible row absent")?;
        assert_ne!(first.anchor.position, AnchorPosition::BeforeItem);
        assert!(state.screen.demands.iter().all(|(id, _)| {
            state
                .transcript
                .projection
                .item(id)
                .is_some_and(|item| !matches!(item.kind, ViewItemKind::Tool(_)))
        }));
        let painted = String::from_utf8(frame.encode(&state.terminal_caps)?)?;
        for control in [
            "\x1b[38;2;80;160;220m",
            "\x1b[38;2;200;80;80m",
            "\x1b[2m",
            "\x1b[3m",
        ] {
            assert!(
                painted.contains(control),
                "actual frame omitted {control:?}"
            );
        }
        assert!(state.screen.hit_rows.iter().any(|row| {
            row.text
                .styled
                .text()
                .starts_with("edit: A deliberately long descriptive tool header")
        }));
        assert!(
            !state
                .screen
                .hit_rows
                .iter()
                .any(|row| row.text.styled.text() == "Assistant")
        );
        assert!(state.screen.hit_rows.iter().any(|row| {
            row.text.styled.text() == "Original α input"
                && row
                    .text
                    .styled
                    .spans()
                    .iter()
                    .all(|span| span.style.foreground == Some([80, 160, 220]))
        }));
        assert!(state.transcript.body_tasks.is_empty());
        assert!(state.transcript.history_tasks.is_empty());
        Ok(())
    }

    #[test]
    fn one_row_navigation_crosses_distinct_spacers_in_both_directions() -> TestResult {
        let mut state = readable_state()?;
        let all = window(&mut state, 100, None, false, 100, false)?;
        assert!(!all.is_empty());
        for pair in all.windows(2) {
            let first = &pair[0].0;
            let second = &pair[1].0;
            assert_ne!(first, second);
            let forward = window(&mut state, 100, Some(first), false, 1, true)?;
            assert_eq!(forward.first().map(|row| &row.0), Some(second));
            let backward = window(&mut state, 100, Some(second), true, 1, true)?;
            assert_eq!(backward.first().map(|row| &row.0), Some(first));
        }
        let body = all
            .iter()
            .find(|row| {
                row.3.is_some()
                    && state
                        .transcript
                        .projection
                        .item(&row.0.item)
                        .is_some_and(|item| matches!(item.kind, ViewItemKind::Input))
            })
            .ok_or("body row absent")?;
        let anchor = body.0.clone();
        let reference = body.3.as_ref().ok_or("body ref absent")?.clone();
        let original = state
            .transcript
            .body(&reference)
            .ok_or("loaded body absent")?
            .original
            .clone();
        let source = state.transcript.projection.source().clone();
        let selection = crate::app::selection::Selection::from_original(
            &source,
            crate::app::selection::OriginalBody::new(&reference, &original, true),
            0..original.len(),
        )?;
        state
            .screen
            .viewport
            .scroll_to(anchor.clone(), &state.transcript.projection)?;
        let narrow = window(&mut state, 12, Some(&anchor), false, 5, false)?;
        assert!(!narrow.is_empty());
        assert_eq!(state.screen.viewport.anchor(), Some(&anchor));
        assert_eq!(
            selection.read(
                &source,
                Some(crate::app::selection::OriginalBody::new(
                    &reference,
                    &state
                        .transcript
                        .body(&reference)
                        .ok_or("selected original lost during reflow")?
                        .original,
                    true,
                ))
            )?,
            original
        );
        assert_eq!(
            state
                .transcript
                .body(&reference)
                .ok_or("reflow lost body")?
                .original,
            original
        );
        Ok(())
    }
}
