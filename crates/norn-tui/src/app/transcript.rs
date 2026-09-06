//! Frontend semantic ownership, explicit history demand and revision-bound body cache.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use norn::model_selection::ModelRuntime;
use norn::provider::AgentEvent;
use norn::session::store::{
    BodyPage, BodyRead, EventStore, HistoryAnchor, HistoryDirection, HistoryPage, HistoryRead,
};
use norn::session_view::{
    AcceptedModel, AttemptKey, BodyOrigin, BodyRange, BodyRef, BodyRepresentation, HistoryCursor,
    HistoryPosition, ItemId, LiveReduction, SessionProjection, ViewError, ViewItemKind, ViewSource,
};
use uuid::Uuid;

use super::view_config::ViewConfig;
use crate::TuiError;

pub(crate) mod publication;

/// Loaded original bytes for one approved body revision, before display escaping.
#[derive(Clone, Debug)]
pub struct CachedBody {
    /// Demanded prefix of the original body; wrapping is never stored here.
    pub original: String,
    /// Explicit next load offset, absent only when this revision is complete.
    pub next_offset: Option<usize>,
}

/// One loaded original-byte range; a provisional total may still be unknown.
#[derive(Clone, Debug)]
pub struct LoadedBody {
    /// Requested opaque revision.
    pub reference: BodyRef,
    /// Actual original-byte range.
    pub range: std::ops::Range<usize>,
    /// Approved original bytes.
    pub text: String,
    /// Explicit continuation, absent only for a complete revision.
    pub next_offset: Option<usize>,
}

impl From<BodyPage> for LoadedBody {
    fn from(page: BodyPage) -> Self {
        Self {
            reference: page.reference,
            range: page.range,
            text: page.text,
            next_offset: page.next_offset,
        }
    }
}

/// An explicit item/body request retained through asynchronous store work.
#[derive(Clone, Debug)]
pub struct BodyDemand {
    /// Item that owns this body; aliases are validated on completion.
    pub item: ItemId,
    /// Exact original body revision and requested byte range.
    pub read: BodyRead,
}

/// One frontend's projection and demanded content; no terminal coordinates.
pub struct Transcript {
    /// Shared semantic reducer used by the local frontend and later session host.
    pub projection: SessionProjection,
    /// Frontend-local declared read/detail preferences.
    pub config: ViewConfig,
    bodies: HashMap<BodyRef, CachedBody>,
    pending_bodies: HashSet<BodyRef>,
    failed_bodies: HashSet<BodyRef>,
    oldest: Option<HistoryCursor>,
    newest: Option<HistoryCursor>,
    /// Earlier events exist outside the loaded window.
    pub has_older: bool,
    /// More accepted events existed after the last requested page.
    pub has_newer: bool,
    /// Accepted events observed by the latest page, not a durable watermark.
    pub observed_events: usize,
    configuration_revision: u64,
    /// Explicit body reads owned by this frontend; completion wakes the event loop.
    pub body_tasks: tokio::task::JoinSet<(BodyDemand, Result<LoadedBody, TuiError>)>,
    /// Explicit history-page work, observed through a completion event.
    pub history_tasks: tokio::task::JoinSet<(HistoryRead, Result<HistoryPage, TuiError>)>,
    pending_history: bool,
    latest: super::view_actions::latest::LatestHistory,
    publication: publication::PublicationState,
    /// Exact accepted steer lookups owned through completion.
    pub(crate) input_tasks: tokio::task::JoinSet<publication::InputRead>,
}

impl Transcript {
    /// Create a view only from the source supplied by the actual store owner.
    #[must_use]
    pub fn new(source: ViewSource) -> Self {
        Self {
            projection: SessionProjection::new(source),
            config: ViewConfig::default(),
            bodies: HashMap::new(),
            pending_bodies: HashSet::new(),
            failed_bodies: HashSet::new(),
            oldest: None,
            newest: None,
            has_older: false,
            has_newer: false,
            observed_events: 0,
            configuration_revision: 0,
            body_tasks: tokio::task::JoinSet::new(),
            history_tasks: tokio::task::JoinSet::new(),
            pending_history: false,
            latest: super::view_actions::latest::LatestHistory::default(),
            publication: publication::PublicationState::default(),
            input_tasks: tokio::task::JoinSet::new(),
        }
    }

    /// Capture the actual accepted model at this locally admitted execution.
    pub fn begin_execution(&mut self, model: &ModelRuntime) -> Result<AttemptKey, TuiError> {
        Ok(self.projection.begin_execution(
            Uuid::new_v4(),
            AcceptedModel::capture(model, self.configuration_revision),
        )?)
    }

    /// Advance only after a model/effort/tier change was accepted and published.
    pub fn model_changed(&mut self) -> Result<(), TuiError> {
        self.configuration_revision =
            self.configuration_revision
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted {
                    counter: "frontend model configuration",
                })?;
        Ok(())
    }

    /// Reduce one typed root event; child events retain their separate owner.
    pub fn apply_live(&mut self, event: &AgentEvent) -> Result<LiveReduction, TuiError> {
        Ok(self.projection.apply_live(event)?)
    }

    /// Retain a compact local notice and one original-text body when supplied.
    pub fn notice(
        &mut self,
        kind: ViewItemKind,
        label: &str,
        body: Option<&str>,
    ) -> Result<ItemId, TuiError> {
        Ok(match body {
            Some(body) => {
                self.projection
                    .record_local_body(kind, label, body, BodyRepresentation::Text)?
            }
            None => self.projection.record_notice(kind, label)?,
        })
    }

    /// Build the initial tail demand without cloning or reading any store history.
    pub fn initial_history(&self) -> Result<HistoryRead, TuiError> {
        self.history_request(HistoryAnchor::End, HistoryDirection::Before)
    }

    /// Build an explicit older-history demand, preserving source-bound cursors.
    pub fn older_history(&self) -> Result<HistoryRead, TuiError> {
        self.history_request(
            self.oldest
                .clone()
                .map_or(HistoryAnchor::End, HistoryAnchor::At),
            HistoryDirection::Before,
        )
    }

    /// Request accepted records after the highest observed event, not from paint.
    pub fn newer_history(&self) -> Result<HistoryRead, TuiError> {
        self.history_request(
            self.newest
                .clone()
                .map_or(HistoryAnchor::Start, HistoryAnchor::At),
            HistoryDirection::After,
        )
    }

    fn history_request(
        &self,
        anchor: HistoryAnchor,
        direction: HistoryDirection,
    ) -> Result<HistoryRead, TuiError> {
        Ok(HistoryRead {
            source: self.projection.source().clone(),
            anchor,
            direction,
            max_events: self.config.history_demand()?,
        })
    }

    /// Apply an owner-minted page; a completed read from a retired source is stale.
    /// Returns false for that explicitly detected rotation race.
    pub fn accept_history(&mut self, page: &HistoryPage) -> Result<bool, TuiError> {
        if &page.source != self.projection.source() {
            return Ok(false);
        }
        for record in &page.records {
            self.projection.apply_history_record(record)?;
        }
        if let Some(first) = page.records.first()
            && self
                .oldest
                .as_ref()
                .is_none_or(|old| ordinal(first.cursor()) <= ordinal(old))
        {
            self.oldest = Some(first.cursor().clone());
            self.has_older = page.has_before;
        }
        if let Some(last) = page.records.last() {
            if self
                .newest
                .as_ref()
                .is_none_or(|new| ordinal(last.cursor()) >= ordinal(new))
            {
                self.newest = Some(last.cursor().clone());
                self.has_newer = page.has_after;
            }
        } else if page.total_events == 0 {
            self.has_older = false;
            self.has_newer = false;
        }
        self.observed_events = self.observed_events.max(page.total_events);
        Ok(true)
    }

    /// Request one earlier owner-bound page; concurrent duplicate requests are coalesced.
    pub fn load_older(&mut self, store: &Arc<EventStore>) -> Result<bool, TuiError> {
        if self.pending_history || !self.has_older {
            return Ok(false);
        }
        let request = self.older_history()?;
        let store = Arc::clone(store);
        self.pending_history = true;
        self.history_tasks.spawn(async move {
            let result = read_history(store, request.clone()).await;
            (request, result)
        });
        Ok(true)
    }

    /// Request current accepted history without changing provider/runtime ownership.
    pub(super) fn request_latest(&mut self) {
        self.latest.begin(self.projection.source());
    }

    /// Navigation cancels follow intent; an admitted history job may still complete.
    pub(super) fn cancel_latest(&mut self) {
        self.latest.cancel();
    }

    pub(super) fn latest_pending(&self) -> bool {
        self.latest.pending()
    }

    /// Schedule one configured page; completion, not a timer, advances the captured frontier.
    pub(super) fn load_latest(&mut self, store: &Arc<EventStore>) -> Result<bool, TuiError> {
        if self.pending_history || !self.latest.pending() {
            return Ok(false);
        }
        let request = if self.newest.is_none() {
            self.initial_history()?
        } else {
            self.newer_history()?
        };
        if !self.latest.start() {
            return Ok(false);
        }
        self.pending_history = true;
        let store = Arc::clone(store);
        self.history_tasks.spawn(async move {
            let result = read_history(store, request.clone()).await;
            (request, result)
        });
        Ok(true)
    }

    /// Accept one explicit history completion without asserting a durable live watermark.
    pub fn finish_history(
        &mut self,
        result: Result<(HistoryRead, Result<HistoryPage, TuiError>), tokio::task::JoinError>,
    ) -> Result<bool, TuiError> {
        let (request, page) = match result {
            Ok(result) => result,
            Err(source) => {
                self.pending_history = false;
                self.latest.failed();
                return Err(TuiError::ViewTask {
                    operation: "history completion",
                    source,
                });
            }
        };
        if &request.source != self.projection.source() {
            return Ok(false);
        }
        self.pending_history = false;
        match page {
            Ok(page) => {
                let coverage = self.latest.observe(&request, &page);
                if request.direction == HistoryDirection::After
                    && page.records.is_empty()
                    && matches!(&request.anchor, HistoryAnchor::At(cursor) if self.newest.as_ref().is_some_and(|newest| ordinal(cursor) == ordinal(newest)))
                {
                    self.has_newer = page.has_after;
                }
                let accepted = self.accept_history(&page)?;
                coverage?;
                Ok(accepted)
            }
            Err(error) => {
                self.latest.failed();
                self.notice(
                    ViewItemKind::Unavailable,
                    "Requested history page unavailable",
                    Some(&error.to_string()),
                )?;
                Ok(false)
            }
        }
    }

    /// Cached original body data only; this accessor performs no store reads.
    #[must_use]
    pub fn body(&self, reference: &BodyRef) -> Option<&CachedBody> {
        self.bodies.get(reference)
    }

    /// Keep only body revisions explicitly pinned by visibility, expansion or selection.
    /// Semantic rows remain retained when a body is evicted.
    pub fn retain_bodies(&mut self, pinned: &HashSet<BodyRef>) {
        self.bodies
            .retain(|reference, _| pinned.contains(reference));
    }

    /// Create one explicit initial/continuation demand, deduplicating in-flight reads.
    /// A body must belong to the exact current item revision.
    pub fn demand_body(
        &mut self,
        item: &ItemId,
        reference: &BodyRef,
        more: bool,
    ) -> Result<Option<BodyDemand>, TuiError> {
        if more {
            self.failed_bodies.remove(reference);
        }
        if !self.owns_body(item, reference)
            || self.pending_bodies.contains(reference)
            || self.failed_bodies.contains(reference)
        {
            return Ok(None);
        }
        let offset = match self.bodies.get(reference) {
            Some(body) if more => match body.next_offset {
                Some(offset) => offset,
                None => return Ok(None),
            },
            Some(_) => return Ok(None),
            None => 0,
        };
        let demand = BodyDemand {
            item: item.clone(),
            read: BodyRead {
                reference: reference.clone(),
                range: BodyRange {
                    offset,
                    max_bytes: self.config.body_demand()?,
                },
            },
        };
        self.pending_bodies.insert(reference.clone());
        Ok(Some(demand))
    }

    /// Read an explicitly demanded local/provisional range in its owning reducer.
    /// The copy is limited to the demand; committed bodies use the store worker.
    pub fn read_local_body(&self, demand: &BodyDemand) -> Result<LoadedBody, TuiError> {
        let chunk = self
            .projection
            .read_provisional(&demand.read.reference, demand.read.range)?;
        Ok(LoadedBody {
            reference: chunk.body,
            range: chunk.offset..chunk.next_offset,
            text: chunk.original_text,
            next_offset: (!chunk.complete).then_some(chunk.next_offset),
        })
    }

    /// Clear an observed failed read so an explicit retry can be requested.
    pub fn body_failed(&mut self, reference: &BodyRef) {
        self.pending_bodies.remove(reference);
    }

    /// Accept only the requested item/body revision and contiguous original byte range.
    /// Returns false when a newer revision or session rotation retired this request.
    pub fn accept_body(&mut self, demand: &BodyDemand, page: LoadedBody) -> Result<bool, TuiError> {
        self.pending_bodies.remove(&demand.read.reference);
        if !self.owns_body(&demand.item, &demand.read.reference) {
            return Ok(false);
        }
        if page.reference != demand.read.reference
            || page.range.start != demand.read.range.offset
            || page.range.end.checked_sub(page.range.start) != Some(page.text.len())
            || page.text.len() > demand.read.range.max_bytes.get()
            || page
                .next_offset
                .is_some_and(|next| next != page.range.end || next <= page.range.start)
        {
            return Err(TuiError::InvalidBodyPage {
                item: Box::new(demand.item.clone()),
                offset: demand.read.range.offset,
            });
        }
        let body = self
            .bodies
            .entry(page.reference)
            .or_insert_with(|| CachedBody {
                original: String::new(),
                next_offset: Some(0),
            });
        if body.original.len() != page.range.start {
            return Err(TuiError::InvalidBodyPage {
                item: Box::new(demand.item.clone()),
                offset: page.range.start,
            });
        }
        body.original.push_str(&page.text);
        body.next_offset = page.next_offset;
        Ok(true)
    }

    /// Schedule an explicit visible-body demand outside paint and resize.
    pub fn load_body(
        &mut self,
        store: &Arc<EventStore>,
        item: &ItemId,
        reference: &BodyRef,
        more: bool,
    ) -> Result<(), TuiError> {
        let Some(demand) = self.demand_body(item, reference, more)? else {
            return Ok(());
        };
        if is_committed(reference) {
            let store = Arc::clone(store);
            self.body_tasks.spawn(async move {
                let result = read_committed_body(store, demand.clone())
                    .await
                    .map(|(_, page)| page);
                (demand, result)
            });
        } else {
            match self.read_local_body(&demand) {
                Ok(page) => {
                    self.accept_body(&demand, page)?;
                }
                Err(error) => {
                    self.body_failed(reference);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// Accept one explicitly scheduled read or retain its named error.
    pub fn finish_body(
        &mut self,
        result: Result<(BodyDemand, Result<LoadedBody, TuiError>), tokio::task::JoinError>,
    ) -> Result<(), TuiError> {
        let (demand, page) = result.map_err(|source| TuiError::ViewTask {
            operation: "body completion",
            source,
        })?;
        match page {
            Ok(page) => {
                self.accept_body(&demand, page)?;
            }
            Err(error) => {
                self.body_failed(&demand.read.reference);
                self.failed_bodies.insert(demand.read.reference.clone());
                if self.owns_body(&demand.item, &demand.read.reference) {
                    self.notice(
                        ViewItemKind::Unavailable,
                        &format!("Body unavailable for {:?}", demand.item),
                        Some(&error.to_string()),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn owns_body(&self, item: &ItemId, reference: &BodyRef) -> bool {
        let id = self.projection.alias(item).unwrap_or(item);
        self.projection.item(id).is_some_and(|row| row.bodies.contains(reference)
            || matches!(&row.kind, ViewItemKind::Tool(tool) if tool.arguments.as_ref() == Some(reference) || tool.result.as_ref() == Some(reference)))
    }
}

/// Run only explicit history work off the terminal executor.
pub async fn read_history(
    store: Arc<EventStore>,
    request: HistoryRead,
) -> Result<HistoryPage, TuiError> {
    tokio::task::spawn_blocking(move || store.history_page(&request))
        .await
        .map_err(|source| TuiError::ViewTask {
            operation: "history page",
            source,
        })?
        .map_err(TuiError::from)
}

/// Run only an approved committed body demand off the terminal executor.
pub async fn read_committed_body(
    store: Arc<EventStore>,
    demand: BodyDemand,
) -> Result<(BodyDemand, LoadedBody), TuiError> {
    tokio::task::spawn_blocking(move || {
        let page = store.read_body(&demand.read)?;
        Ok((demand, LoadedBody::from(page)))
    })
    .await
    .map_err(|source| TuiError::ViewTask {
        operation: "body range",
        source,
    })?
}

/// Whether the store rather than the projection owns this body capability.
#[must_use]
pub fn is_committed(reference: &BodyRef) -> bool {
    matches!(reference.origin(), BodyOrigin::Committed { .. })
}

fn ordinal(cursor: &HistoryCursor) -> Option<usize> {
    match cursor.position() {
        HistoryPosition::Empty => None,
        HistoryPosition::Event { ordinal, .. } => Some(*ordinal),
    }
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod tests;
