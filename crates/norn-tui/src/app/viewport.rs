//! Source-bound logical viewport state; no geometry, body reads or execution effects.

use norn::session_view::{BodyOrigin, BodyRef, ItemId, SessionProjection, ViewItem, ViewSource};

/// The caller supplies an original-byte position obtained from its current hit map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AnchorPosition {
    Header,
    Body {
        reference: BodyRef,
        original_offset: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ViewAnchor {
    pub item: ItemId,
    pub position: AnchorPosition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ItemState {
    Current,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AnchorState {
    Current,
    ItemUnavailable,
    /// Keep the original capability and byte offset for a named stale selection.
    BodyStale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ViewportReconciliation {
    pub anchor: Option<AnchorState>,
    pub selected: Option<ItemState>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ViewportError {
    #[error("viewport source {expected:?} differs from requested source {actual:?}")]
    SourceMismatch {
        expected: Box<ViewSource>,
        actual: Box<ViewSource>,
    },
    #[error("viewport item {item:?} is unavailable in the bound projection")]
    ItemUnavailable { item: Box<ItemId> },
    #[error("viewport body {reference:?} is not current for item {item:?}")]
    BodyNotCurrent {
        item: Box<ItemId>,
        reference: Box<BodyRef>,
    },
}

/// Resize changes the caller's layout only; this state has no terminal coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Viewport {
    source: ViewSource,
    anchor: Option<ViewAnchor>,
    selected: Option<ItemId>,
    follow_tail: bool,
}

impl Viewport {
    pub fn new(source: ViewSource, follow_tail: bool) -> Self {
        Self {
            source,
            anchor: None,
            selected: None,
            follow_tail,
        }
    }

    pub const fn source(&self) -> &ViewSource {
        &self.source
    }

    pub const fn anchor(&self) -> Option<&ViewAnchor> {
        self.anchor.as_ref()
    }

    pub const fn selected(&self) -> Option<&ItemId> {
        self.selected.as_ref()
    }

    pub const fn follows_tail(&self) -> bool {
        self.follow_tail
    }

    /// Admit an explicit current hit-tested anchor without reading or clamping bytes.
    pub fn scroll_to(
        &mut self,
        mut anchor: ViewAnchor,
        projection: &SessionProjection,
    ) -> Result<(), ViewportError> {
        self.check_source(projection.source())?;
        self.check_source(item_source(&anchor.item))?;
        let item = current_item(projection, &anchor.item).ok_or_else(|| {
            ViewportError::ItemUnavailable {
                item: Box::new(anchor.item.clone()),
            }
        })?;
        if let AnchorPosition::Body { reference, .. } = &anchor.position {
            self.check_source(body_source(reference))?;
            if !item.bodies.contains(reference) {
                return Err(ViewportError::BodyNotCurrent {
                    item: Box::new(item.id.clone()),
                    reference: Box::new(reference.clone()),
                });
            }
        }
        anchor.item.clone_from(&item.id);
        self.anchor = Some(anchor);
        self.pin();
        Ok(())
    }

    pub fn select(
        &mut self,
        item: ItemId,
        projection: &SessionProjection,
    ) -> Result<(), ViewportError> {
        self.check_source(projection.source())?;
        self.check_source(item_source(&item))?;
        let current =
            current_item(projection, &item).ok_or_else(|| ViewportError::ItemUnavailable {
                item: Box::new(item),
            })?;
        self.selected = Some(current.id.clone());
        self.pin();
        Ok(())
    }

    /// Typing, selecting and explicit browsing can pin without inventing an anchor.
    pub const fn pin(&mut self) {
        self.follow_tail = false;
    }

    /// Only an explicit return-to-live action clears historical navigation state.
    pub fn follow_tail(&mut self) {
        self.anchor = None;
        self.selected = None;
        self.follow_tail = true;
    }

    /// A real source replacement clears source-specific state and requires explicit follow.
    pub fn replace_source(&mut self, source: ViewSource) -> bool {
        if self.source == source {
            return false;
        }
        *self = Self::new(source, false);
        true
    }

    /// Follow only owner-proven aliases. Never replace a body capability or its byte offset.
    pub fn reconcile(
        &mut self,
        projection: &SessionProjection,
    ) -> Result<ViewportReconciliation, ViewportError> {
        self.check_source(projection.source())?;
        let anchor = self.anchor.as_mut().map(|anchor| {
            let Some(item) = current_item(projection, &anchor.item) else {
                return AnchorState::ItemUnavailable;
            };
            anchor.item.clone_from(&item.id);
            match &anchor.position {
                AnchorPosition::Header => AnchorState::Current,
                AnchorPosition::Body { reference, .. } if item.bodies.contains(reference) => {
                    AnchorState::Current
                }
                AnchorPosition::Body { .. } => AnchorState::BodyStale,
            }
        });
        let selected = self.selected.as_mut().map(|selected| {
            let Some(item) = current_item(projection, selected) else {
                return ItemState::Unavailable;
            };
            selected.clone_from(&item.id);
            ItemState::Current
        });
        Ok(ViewportReconciliation { anchor, selected })
    }

    fn check_source(&self, actual: &ViewSource) -> Result<(), ViewportError> {
        if actual == &self.source {
            Ok(())
        } else {
            Err(ViewportError::SourceMismatch {
                expected: Box::new(self.source.clone()),
                actual: Box::new(actual.clone()),
            })
        }
    }
}

fn current_item<'a>(projection: &'a SessionProjection, id: &ItemId) -> Option<&'a ViewItem> {
    let current = projection.alias(id).unwrap_or(id);
    projection.item(current)
}

fn item_source(item: &ItemId) -> &ViewSource {
    match item {
        ItemId::Committed { cursor, .. } => cursor.source(),
        ItemId::Provisional(key) => &key.source,
        ItemId::Local { source, .. } => source,
    }
}

fn body_source(reference: &BodyRef) -> &ViewSource {
    match reference.origin() {
        BodyOrigin::Committed { cursor, .. } => cursor.source(),
        BodyOrigin::Provisional { key, .. } => &key.source,
        BodyOrigin::Local { source, .. } => source,
    }
}

#[cfg(test)]
#[path = "viewport_tests.rs"]
mod tests;
