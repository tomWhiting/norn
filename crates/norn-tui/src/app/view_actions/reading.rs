//! Explicit source-bound search demand and supervised original-selection export; never paint work.

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use norn::session::store::{EventStore, HistoryDirection, HistoryPage, HistoryRead};
use norn::session_view::{BodyRef, CoverageGap, ItemId, ViewSource};

use crate::TuiError;
use crate::app::export::{ExportError, ExportMode, ExportReceipt, export_original};
use crate::app::render::interaction;
use crate::app::search::{
    SearchBody, SearchHistoryCoverage, SearchQuery, SearchReport, SearchScope, SearchSummary,
};
use crate::app::selection::Selection;
use crate::app::state::AppState;
use crate::app::viewport::{AnchorPosition, ViewAnchor};

#[derive(Clone)]
struct Hit {
    item: ItemId,
    reference: BodyRef,
    range: Range<usize>,
}

/// Only hit identities/ranges survive a search, never another body store.
pub(in crate::app) struct SearchState {
    hits: Vec<Hit>,
    current: Option<usize>,
    summary: Option<SearchSummary>,
    older: Option<OlderSearch>,
}

struct OlderSearch {
    query: String,
    phase: OlderPhase,
    items: Vec<ItemId>,
}

enum OlderPhase {
    Requested,
    History,
    Bodies,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            hits: Vec::new(),
            current: None,
            summary: None,
            older: None,
        }
    }
}

/// Exact operator-approved export scope; complete means selected bytes, not the session.
#[derive(Debug)]
pub(in crate::app) struct ExportScope {
    pub source: ViewSource,
    pub item: ItemId,
    pub reference: BodyRef,
    pub original_range: Range<usize>,
    pub whole_body_loaded: bool,
}

pub(in crate::app) type ExportResult = Result<ExportReceipt<ExportScope>, ExportError>;
pub(in crate::app) type HistoryResult =
    Result<(HistoryRead, Result<HistoryPage, TuiError>), tokio::task::JoinError>;

pub(super) fn search(
    state: &mut AppState,
    scope: SearchScope,
    query: &str,
) -> Result<(), TuiError> {
    SearchQuery::new(query).map_err(interaction)?;
    if matches!(scope, SearchScope::RequestedOlderHistory) {
        if state.screen.search.older.is_some() {
            return Err(interaction(std::io::Error::other(
                "an older-page search is already pending",
            )));
        }
        if !state.transcript.has_older {
            return Err(interaction(std::io::Error::other(
                "no older history is advertised by the accepted page; use /view search loaded <query>",
            )));
        }
        state.screen.search.older = Some(OlderSearch {
            query: query.to_owned(),
            phase: OlderPhase::Requested,
            items: Vec::new(),
        });
        state.screen.feedback = Some("Search requested one older history page and one configured prefix per body; remaining ranges will be reported".to_owned());
        super::pin_visible(state)?;
        state.screen.allow_body_load = true;
        return Ok(());
    }
    let items = if matches!(scope, SearchScope::SelectedBody) {
        super::selected_text(state)?;
        Some(vec![state.screen.selection_item.clone().ok_or_else(
            || {
                interaction(std::io::Error::other(
                    "select an original body before searching selected scope",
                ))
            },
        )?])
    } else {
        None
    };
    scan(state, scope, query, items.as_deref())
}

fn scan(
    state: &mut AppState,
    scope: SearchScope,
    query: &str,
    included: Option<&[ItemId]>,
) -> Result<(), TuiError> {
    let query = SearchQuery::new(query).map_err(interaction)?;
    let projection = &state.transcript.projection;
    let gaps = &projection.coverage().gaps;
    let mut report = SearchReport::new(
        projection.source(),
        scope,
        query,
        SearchHistoryCoverage {
            older_history_not_loaded: state.transcript.has_older,
            live_coverage_uncertain: gaps.contains(&CoverageGap::BroadcastLag)
                || gaps.contains(&CoverageGap::IncompleteAssociation)
                || gaps.contains(&CoverageGap::Interrupted),
        },
    );
    let selected_reference = state.screen.selection.as_ref().map(Selection::reference);
    let mut hits = Vec::new();
    let mut examined_bytes = 0usize;
    for item in projection
        .items()
        .filter(|item| included.is_none_or(|ids| ids.contains(&item.id)))
    {
        for reference in &item.bodies {
            if scope == SearchScope::SelectedBody
                && selected_reference.is_some_and(|selected| selected != reference)
            {
                continue;
            }
            let Some(body) = state.transcript.body(reference) else {
                report
                    .unavailable(&item.id, reference)
                    .map_err(interaction)?;
                continue;
            };
            let mut scan = SearchBody {
                item: &item.id,
                reference,
                original: &body.original,
                complete: body.next_offset.is_none(),
            }
            .matches(query, projection.source())
            .map_err(interaction)?;
            for range in scan.by_ref() {
                hits.push(Hit {
                    item: item.id.clone(),
                    reference: reference.clone(),
                    range,
                });
            }
            let observed = scan.coverage();
            examined_bytes = examined_bytes
                .checked_add(observed.examined.len())
                .ok_or_else(|| {
                    interaction(std::io::Error::other("search examined byte count overflow"))
                })?;
            report.observe(scan).map_err(interaction)?;
        }
    }
    let summary = report.summary();
    state.screen.search.hits = hits;
    state.screen.search.current = None;
    state.screen.search.summary = Some(summary);
    state.screen.feedback = Some(format!(
        "{} · {examined_bytes} original bytes examined",
        summary_text(summary)
    ));
    if !state.screen.search.hits.is_empty() {
        next_hit(state, false)?;
    }
    state.screen.dirty = true;
    Ok(())
}

fn summary_text(summary: SearchSummary) -> String {
    format!(
        "{:?}: {} matches in {} body scans; {} partial, {} unavailable; older history {}, live coverage {}; complete within declared scope: {}",
        summary.scope,
        summary.matches_found,
        summary.body_scans,
        summary.partial_body_scans,
        summary.unavailable_bodies,
        if summary.history.older_history_not_loaded {
            "not loaded"
        } else {
            "accepted range loaded"
        },
        if summary.history.live_coverage_uncertain {
            "uncertain"
        } else {
            "no observed gap"
        },
        summary.complete_within_scope()
    )
}

pub(super) fn next_hit(state: &mut AppState, backwards: bool) -> Result<(), TuiError> {
    let count = state.screen.search.hits.len();
    if count == 0 {
        return Err(interaction(std::io::Error::other(
            "no retained search hits; inspect search scope/partial coverage before treating this as no match",
        )));
    }
    let next = match state.screen.search.current {
        None => {
            if backwards {
                count - 1
            } else {
                0
            }
        }
        Some(index) if backwards => index
            .checked_sub(1)
            .ok_or_else(|| interaction(std::io::Error::other("already at first retained match")))?,
        Some(index) if index + 1 < count => index + 1,
        Some(_) => {
            return Err(interaction(std::io::Error::other(
                "already at last retained match",
            )));
        }
    };
    let hit = state.screen.search.hits[next].clone();
    let original = super::original_for(state, &hit.item, &hit.reference)?;
    let selection = Selection::from_original(
        state.transcript.projection.source(),
        original,
        hit.range.clone(),
    )
    .map_err(interaction)?;
    let item = state
        .transcript
        .projection
        .alias(&hit.item)
        .unwrap_or(&hit.item)
        .clone();
    state
        .screen
        .viewport
        .scroll_to(
            ViewAnchor {
                item: item.clone(),
                position: AnchorPosition::Body {
                    reference: hit.reference,
                    original_offset: hit.range.start,
                },
            },
            &state.transcript.projection,
        )
        .map_err(interaction)?;
    state
        .screen
        .viewport
        .select(item.clone(), &state.transcript.projection)
        .map_err(interaction)?;
    state.screen.tool_overrides.insert(item.clone(), true);
    state.screen.display_selection = None;
    state.screen.selection = Some(selection);
    state.screen.selection_item = Some(item);
    state.screen.search.current = Some(next);
    state.screen.dirty = true;
    state.screen.feedback = Some(format!(
        "Match {}/{count} · original bytes {:?} · {}",
        next + 1,
        hit.range,
        state
            .screen
            .search
            .summary
            .map_or_else(|| "coverage unavailable".to_owned(), summary_text)
    ));
    Ok(())
}

/// Run explicit older-search requests outside paint; pin only currently requested prefixes.
pub(in crate::app) fn load_requests(
    state: &mut AppState,
    store: &Arc<EventStore>,
    pinned: &mut HashSet<BodyRef>,
) -> Result<(), TuiError> {
    let Some(mut older) = state.screen.search.older.take() else {
        return Ok(());
    };
    match older.phase {
        OlderPhase::Requested => {
            if state.transcript.load_older(store)? {
                older.phase = OlderPhase::History;
            }
            state.screen.search.older = Some(older);
        }
        OlderPhase::History => state.screen.search.older = Some(older),
        OlderPhase::Bodies => {
            let bodies: Vec<_> = older
                .items
                .iter()
                .filter_map(|id| state.transcript.projection.item(id))
                .flat_map(|item| {
                    item.bodies
                        .iter()
                        .map(|reference| (item.id.clone(), reference.clone()))
                })
                .collect();
            for (item, reference) in &bodies {
                pinned.insert(reference.clone());
                state.transcript.load_body(store, item, reference, false)?;
            }
            if state.transcript.body_tasks.is_empty() {
                scan(
                    state,
                    SearchScope::RequestedOlderHistory,
                    &older.query,
                    Some(&older.items),
                )?;
            } else {
                state.screen.search.older = Some(older);
            }
        }
    }
    Ok(())
}

/// A requested page supplies exact item identities; no position/content inference.
pub(in crate::app) fn finish_history(
    state: &mut AppState,
    result: HistoryResult,
) -> Result<(), TuiError> {
    let accepted_items = match &result {
        Ok((request, Ok(page)))
            if request.direction == HistoryDirection::Before
                && &page.source == state.transcript.projection.source() =>
        {
            Some(
                page.records
                    .iter()
                    .flat_map(|record| record.items().iter().map(|item| item.id.clone()))
                    .collect(),
            )
        }
        _ => None,
    };
    let failed = matches!(&result, Err(_) | Ok((_, Err(_))));
    let accepted = state.transcript.finish_history(result)?;
    if let Some(older) = state.screen.search.older.as_mut() {
        if accepted && matches!(older.phase, OlderPhase::History) {
            if let Some(items) = accepted_items {
                older.items = items;
                older.phase = OlderPhase::Bodies;
            }
        } else if failed {
            state.screen.search.older = None;
            state.screen.feedback = Some(
                "Older-history search failed; its unsearched range remains unavailable".to_owned(),
            );
        }
    }
    state.screen.allow_body_load = true;
    state.screen.dirty = true;
    Ok(())
}

/// Capture fresh approved bytes once, then observe the filesystem worker through completion.
pub(super) fn export(
    state: &mut AppState,
    destination: &str,
    mode: ExportMode,
) -> Result<(), TuiError> {
    let path = PathBuf::from(destination);
    let destination = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let bytes = super::selected_text(state)?.as_bytes().to_vec();
    let selection =
        state.screen.selection.as_ref().ok_or_else(|| {
            interaction(std::io::Error::other("export has no original selection"))
        })?;
    let item = state
        .screen
        .selection_item
        .clone()
        .ok_or_else(|| interaction(std::io::Error::other("export has no selected owner")))?;
    let scope = ExportScope {
        source: state.transcript.projection.source().clone(),
        item,
        reference: selection.reference().clone(),
        original_range: selection.range(),
        whole_body_loaded: state
            .transcript
            .body(selection.reference())
            .is_some_and(|body| body.next_offset.is_none()),
    };
    state.screen.feedback = Some(format!(
        "Export requested to {} ({mode:?}); completion pending",
        destination.display()
    ));
    state
        .export_tasks
        .spawn_blocking(move || export_original(&destination, &bytes, mode, scope));
    Ok(())
}

pub(in crate::app) fn finish_export(
    state: &mut AppState,
    result: Result<ExportResult, tokio::task::JoinError>,
) -> Result<(), TuiError> {
    let message = match result {
        Ok(Ok(receipt)) => {
            tracing::info!(destination = %receipt.destination.display(), bytes = receipt.bytes_written, scope = ?receipt.scope, "original selection export completed");
            format!(
                "Exported {} original bytes to {} · {:?} · {:?} · source {:?}, item {:?}, body {:?}, range {:?}, whole body loaded: {}",
                receipt.bytes_written,
                receipt.destination.display(),
                receipt.mode,
                receipt.synchronization,
                receipt.scope.source,
                receipt.scope.item,
                receipt.scope.reference,
                receipt.scope.original_range,
                receipt.scope.whole_body_loaded
            )
        }
        Ok(Err(error)) => {
            tracing::error!(%error, "original selection export failed");
            format!("Export failed ({}): {error}", error.publication())
        }
        Err(error) => {
            tracing::error!(%error, "export worker completion unavailable");
            format!("Export worker failed: {error}; publication state unavailable")
        }
    };
    crate::app::notices::notice(state, "Original selection export", Some(&message))?;
    state.screen.feedback = Some(message);
    Ok(())
}

/// An accepted file write is joined on every orderly/error exit; never called cancelled by drop.
pub(in crate::app) async fn drain_exports(state: &mut AppState) -> Result<(), TuiError> {
    let mut failure = None;
    while let Some(result) = state.export_tasks.join_next().await {
        if let Err(error) = finish_export(state, result) {
            if failure.is_none() {
                failure = Some(error);
            } else {
                tracing::error!(%error, "additional export completion reporting failure");
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
#[path = "reading_tests.rs"]
mod tests;
