//! Borrowed presentation of one conversation; no provider, composer or execution authority.

use norn::session_view::{BodyRef, ItemId, ViewError};

use crate::TuiError;
use crate::events::DisplayToggles;

use super::render::{ScreenState, interaction};
use super::selection::OriginalBody;
use super::state::AppState;
use super::transcript::Transcript;

/// A matched conversation and its frontend presentation, borrowed for one operation.
/// No store is opened and no history or draft is cloned by constructing this view.
pub(in crate::app) struct ConversationView<'a> {
    pub transcript: &'a mut Transcript,
    pub screen: &'a mut ScreenState,
    pub display_toggles: DisplayToggles,
}

impl<'a> ConversationView<'a> {
    /// Refuse a mismatched view before allowing rendering or navigation to mutate it.
    pub fn new(
        transcript: &'a mut Transcript,
        screen: &'a mut ScreenState,
        display_toggles: DisplayToggles,
    ) -> Result<Self, TuiError> {
        validate_source(transcript, screen)?;
        Ok(Self {
            transcript,
            screen,
            display_toggles,
        })
    }

    /// The current root adapter; selection must supply its own matched pair explicitly.
    pub fn root(state: &'a mut AppState) -> Result<Self, TuiError> {
        Self::new(
            &mut state.transcript,
            &mut state.screen,
            state.display_toggles,
        )
    }
}

fn validate_source(transcript: &Transcript, screen: &ScreenState) -> Result<(), TuiError> {
    if screen.viewport.source() != transcript.projection.source() {
        return Err(ViewError::SourceMismatch {
            expected: Box::new(screen.viewport.source().clone()),
            actual: Box::new(transcript.projection.source().clone()),
        }
        .into());
    }
    Ok(())
}

/// Revalidate source, item and revision before lending original bytes to the frontend.
pub(in crate::app) fn original_for<'a>(
    transcript: &'a Transcript,
    screen: &ScreenState,
    item: &ItemId,
    reference: &'a BodyRef,
) -> Result<OriginalBody<'a>, TuiError> {
    validate_source(transcript, screen)?;
    let projection = &transcript.projection;
    let current_id = projection.alias(item).unwrap_or(item);
    if projection
        .item(current_id)
        .is_none_or(|item| !item.bodies.contains(reference))
    {
        return Err(interaction(std::io::Error::other(format!(
            "selected original body revision is no longer current for {item:?}"
        ))));
    }
    let body = transcript.body(reference).ok_or_else(|| {
        interaction(std::io::Error::other(format!(
            "selected original body is not loaded for {item:?}"
        )))
    })?;
    Ok(OriginalBody::new(
        reference,
        &body.original,
        body.next_offset.is_none(),
    ))
}

/// Original selection is checked against this exact conversation, not the running agent.
pub(in crate::app) fn selected_text<'a>(
    transcript: &'a Transcript,
    screen: &'a ScreenState,
) -> Result<&'a str, TuiError> {
    let selection = screen.selection.as_ref().ok_or_else(|| {
        interaction(std::io::Error::other(
            "no original text selection; drag text or use /view select",
        ))
    })?;
    let item = screen
        .selection_item
        .as_ref()
        .ok_or_else(|| interaction(std::io::Error::other("selection has no item owner")))?;
    let original = original_for(transcript, screen, item, selection.reference())?;
    selection
        .read(transcript.projection.source(), Some(original))
        .map_err(interaction)
}

#[cfg(test)]
#[path = "conversation_view_tests.rs"]
mod tests;
