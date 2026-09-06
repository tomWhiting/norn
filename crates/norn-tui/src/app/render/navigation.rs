//! Ordered finite-batch navigation over cached rows; original-byte anchors remain authoritative.

use std::sync::{Arc, Weak};

use norn::session_view::{ItemDirection, ItemInclusion, ViewItem, ViewItemKind, ViewSource};

use crate::TuiError;
use crate::app::state::AppState;
use crate::app::viewport::{AnchorPosition, ViewAnchor};
use crate::render::layout::{Layout, UpperLayout, UpperPane};
use crate::render::retained_markdown::RenderedMarkdown;

use super::transcript::{locate_anchor, row_position};
use super::transcript_items::{RowGroup, item_groups};
use super::{ScreenState, interaction};

#[derive(Debug, thiserror::Error)]
enum NavigationError {
    #[error("scroll source {actual:?} no longer matches pending source {expected:?}")]
    Source {
        expected: Box<ViewSource>,
        actual: Box<ViewSource>,
    },
    #[error("scroll row count exceeds usize for the current finite input batch")]
    Count,
}

struct Motion {
    backwards: bool,
    rows: usize,
    columns: u16,
}

pub(in crate::app) struct PendingNavigation {
    source: ViewSource,
    motions: Vec<Motion>,
}

/// An exact cached display row disambiguates several rows mapping to the same original byte.
/// It is never a copy/export capability and does not retain an evicted body.
pub(in crate::app) struct RowCursor {
    anchor: ViewAnchor,
    text: Weak<RenderedMarkdown>,
    display_start: usize,
    columns: u16,
}

pub(in crate::app) fn queue(
    state: &mut AppState,
    backwards: bool,
    rows: usize,
) -> Result<(), TuiError> {
    if rows == 0 {
        return Ok(());
    }
    state.transcript.cancel_latest();
    let columns = match state.screen.layout {
        Layout::Ready {
            upper: UpperLayout::Split { conversation, .. },
            ..
        }
        | Layout::Ready {
            upper:
                UpperLayout::Single {
                    pane: UpperPane::Conversation,
                    area: conversation,
                },
            ..
        } => conversation.width,
        _ => return Ok(()),
    };
    if state.screen.navigation.is_none() {
        if state.screen.viewport.follows_tail() || state.screen.viewport.anchor().is_none() {
            if let Some(hit) = state.screen.hit_rows.first() {
                state
                    .screen
                    .viewport
                    .scroll_to(hit.anchor.clone(), &state.transcript.projection)
                    .map_err(interaction)?;
                state.screen.row_cursor = Some(RowCursor {
                    anchor: hit.anchor.clone(),
                    text: Arc::downgrade(&hit.text),
                    display_start: hit.geometry.bytes().start,
                    columns,
                });
            } else {
                state.screen.viewport.pin();
            }
        }
        state.screen.navigation = Some(PendingNavigation {
            source: state.transcript.projection.source().clone(),
            motions: Vec::new(),
        });
    }
    let plan = state
        .screen
        .navigation
        .as_mut()
        .ok_or_else(|| interaction(NavigationError::Count))?;
    if plan.source != *state.transcript.projection.source() {
        return Err(interaction(NavigationError::Source {
            expected: Box::new(plan.source.clone()),
            actual: Box::new(state.transcript.projection.source().clone()),
        }));
    }
    if let Some(last) = plan.motions.last_mut()
        && last.backwards == backwards
        && last.columns == columns
    {
        last.rows = last
            .rows
            .checked_add(rows)
            .ok_or_else(|| interaction(NavigationError::Count))?;
    } else {
        plan.motions.push(Motion {
            backwards,
            rows,
            columns,
        });
    }
    Ok(())
}

pub(in crate::app) fn apply(state: &mut AppState) -> Result<(), TuiError> {
    let Some(plan) = state.screen.navigation.take() else {
        return Ok(());
    };
    if plan.source != *state.transcript.projection.source() {
        return Err(interaction(NavigationError::Source {
            expected: Box::new(plan.source),
            actual: Box::new(state.transcript.projection.source().clone()),
        }));
    }
    for motion in plan.motions {
        advance(state, &motion)?;
    }
    Ok(())
}

pub(super) fn locate_cursor(
    screen: &ScreenState,
    item: &ViewItem,
    groups: &[RowGroup],
    columns: u16,
) -> Option<(usize, usize)> {
    let cursor = screen.row_cursor.as_ref()?;
    if cursor.columns != columns
        || cursor.anchor.item != item.id
        || screen.viewport.anchor() != Some(&cursor.anchor)
    {
        return None;
    }
    let text = cursor.text.upgrade()?;
    groups.iter().enumerate().find_map(|(group_index, group)| {
        if !Arc::ptr_eq(&text, &group.text) {
            return None;
        }
        group
            .rows
            .iter()
            .position(|row| row.bytes().start == cursor.display_start)
            .map(|row| (group_index, row))
    })
}

fn advance(state: &mut AppState, motion: &Motion) -> Result<(), TuiError> {
    let anchor = state.screen.viewport.anchor().cloned();
    let direction = if motion.backwards {
        ItemDirection::Earlier
    } else {
        ItemDirection::Later
    };
    let items: Box<dyn Iterator<Item = &ViewItem>> = match &anchor {
        Some(anchor) => Box::new(state.transcript.projection.items_from(
            &anchor.item,
            direction,
            ItemInclusion::Inclusive,
        )?),
        None if motion.backwards => Box::new(state.transcript.projection.items().rev()),
        None => Box::new(state.transcript.projection.items()),
    };
    let selected = state.screen.viewport.selected().cloned();
    let visible = |item: &&ViewItem| {
        !(matches!(item.kind, ViewItemKind::Metadata) && !state.transcript.config.expanded_tools
            || matches!(item.kind, ViewItemKind::Thinking)
                && !state.display_toggles.thinking_visible
            || state.transcript.completion_hidden(&item.id)
                && selected.as_ref() != Some(&item.id)
                && anchor.as_ref().is_none_or(|anchor| anchor.item != item.id))
    };
    let mut earlier = match &anchor {
        Some(anchor) => state
            .transcript
            .projection
            .items_from(
                &anchor.item,
                ItemDirection::Earlier,
                ItemInclusion::Exclusive,
            )?
            .any(|item| visible(&item)),
        None => false,
    };
    let mut items = items.filter(visible).peekable();
    let mut remaining = motion.rows;
    let mut target = None;
    while let Some(item) = items.next() {
        let separator = if motion.backwards {
            items.peek().is_some()
        } else {
            earlier
        };
        earlier = true;
        let width = if matches!(item.kind, ViewItemKind::Input) {
            motion.columns.saturating_sub(2).max(1)
        } else {
            motion.columns
        };
        let groups = item_groups(
            &state.transcript,
            &mut state.screen,
            item,
            width,
            state.display_toggles.secondary_fields_visible,
            separator,
        )?;
        let local_anchor = anchor.as_ref().filter(|anchor| anchor.item == item.id);
        let position = locate_cursor(&state.screen, item, &groups, motion.columns)
            .or_else(|| local_anchor.and_then(|anchor| locate_anchor(&groups, &anchor.position)));
        if motion.backwards
            && position.is_none()
            && local_anchor.is_some_and(|anchor| {
                matches!(
                    anchor.position,
                    AnchorPosition::Header | AnchorPosition::BeforeItem
                )
            })
        {
            continue;
        }
        let indices: Box<dyn Iterator<Item = usize>> = if motion.backwards {
            Box::new((0..groups.len()).rev())
        } else {
            Box::new(0..groups.len())
        };
        for index in indices {
            let group = &groups[index];
            let (start, end) = match position {
                Some((current, row)) if current == index => {
                    if motion.backwards {
                        (0, row)
                    } else {
                        (
                            row.saturating_add(1).min(group.rows.len()),
                            group.rows.len(),
                        )
                    }
                }
                Some((current, _))
                    if motion.backwards && index > current
                        || !motion.backwards && index < current =>
                {
                    continue;
                }
                _ => (0, group.rows.len()),
            };
            let count = end - start;
            if count == 0 {
                continue;
            }
            let travelled = remaining.min(count);
            let row_index = if motion.backwards {
                end - travelled
            } else {
                start + travelled - 1
            };
            let row = &group.rows[row_index];
            target = Some(RowCursor {
                anchor: ViewAnchor {
                    item: item.id.clone(),
                    position: row_position(group, row),
                },
                text: Arc::downgrade(&group.text),
                display_start: row.bytes().start,
                columns: motion.columns,
            });
            remaining -= travelled;
            if remaining == 0 {
                break;
            }
        }
        if remaining == 0 {
            break;
        }
    }
    if let Some(target) = target {
        state
            .screen
            .viewport
            .scroll_to(target.anchor.clone(), &state.transcript.projection)
            .map_err(interaction)?;
        state.screen.row_cursor = Some(target);
    }
    Ok(())
}

#[cfg(test)]
#[path = "navigation_tests.rs"]
mod tests;
